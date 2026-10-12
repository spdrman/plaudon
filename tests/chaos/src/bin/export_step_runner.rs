//! Performs a real export, backup, restore or deletion up to a chosen durable
//! step, then aborts the process without unwinding, cleanup or flushing.
//!
//! Usage: `export-step-runner <library-root> <work-dir> <step> [archive-path]`
//!
//! Nothing in the production code path knows this exists: every step below is
//! a public method, and the runner simply stops calling them.

use std::io::Write;
use std::path::{Path, PathBuf};

use plaudon_app_core::deletion::{DeletionConfirmation, DeletionService};
use plaudon_chaos_tests::ExportStep;
use plaudon_domain::ids::{ImportIntentId, RecordingId};
use plaudon_domain::media::{MediaIdentity, Sha256Digest};
use plaudon_domain::time::Timestamp;
use plaudon_export::archive::manifest::ManifestEntry;
use plaudon_export::archive::reader::{self, EntrySink};
use plaudon_export::destination::ExportDestination;
use plaudon_export::error::{ExportError, ExportResult};
use plaudon_persistence::archive::{relative_path, MediaArchive};
use plaudon_persistence::db::Database;
use plaudon_persistence::migrations::MigrationRunner;

const NOW: Timestamp = Timestamp::from_millis(1_700_000_500_000);

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert!(
        args.len() >= 4,
        "usage: export-step-runner <library-root> <work-dir> <step> [archive]"
    );
    let library_root = PathBuf::from(&args[1]);
    let work_dir = PathBuf::from(&args[2]);
    let step = ExportStep::parse(&args[3]).expect("unknown step");
    let archive_path = args.get(4).map(PathBuf::from);

    let db = Database::open(library_root.join("app.sqlite"))
        .await
        .expect("open database");
    MigrationRunner::new().run(&db).await.expect("migrate");
    let media = MediaArchive::open(&library_root).expect("open archive");

    match step {
        ExportStep::ExportPartialWrite => {
            let destination = ExportDestination::new(&work_dir).expect("destination");
            let _ = destination.write_atomic_with("transcript.txt", |file| {
                file.write_all(b"half of a transcript export")
                    .expect("write");
                file.flush().expect("flush");
                crash();
            });
        }
        ExportStep::BackupPartialWrite => {
            let destination = ExportDestination::new(&work_dir).expect("destination");
            let _ = destination.write_atomic_with("library.recording-archive", |file| {
                file.write_all(b"PLAUDARC\x01\x00\x00\x00").expect("write");
                file.flush().expect("flush");
                crash();
            });
        }
        ExportStep::RestoreAfterStaging => {
            let archive = archive_path.expect("archive path");
            let staging = work_dir.join("staging");
            std::fs::create_dir_all(&staging).expect("staging");
            let mut sink = FileSink::new(staging);
            reader::stream(&archive, None, &sink.directory.clone(), &mut sink).expect("stream");
            crash();
        }
        ExportStep::RestoreAfterMediaCommit => {
            let archive = archive_path.expect("archive path");
            let staging = work_dir.join("staging");
            std::fs::create_dir_all(&staging).expect("staging");
            let mut sink = FileSink::new(staging);
            reader::stream(&archive, None, &sink.directory.clone(), &mut sink).expect("stream");
            for (entry, path) in &sink.staged {
                let Some(name) = entry.strip_prefix("media/") else {
                    continue;
                };
                let (stem, extension) = name.split_once('.').expect("media name");
                let digest: Sha256Digest = stem.parse().expect("digest");
                let length = std::fs::metadata(path).expect("metadata").len();
                let identity = MediaIdentity::new(digest, length, String::new());
                let relative = relative_path(&identity, extension);
                let intent = ImportIntentId::new();
                let staging_path = media.staging_path(intent);
                std::fs::create_dir_all(staging_path.parent().expect("parent")).expect("mkdir");
                std::fs::rename(path, &staging_path).expect("rename");
                media
                    .commit_staged(&staging_path, &relative)
                    .expect("commit");
                println!("{relative}");
            }
            std::io::stdout().flush().expect("flush");
            crash();
        }
        ExportStep::DeleteAfterJournal | ExportStep::DeleteAfterRows => {
            let recording_id: RecordingId = sqlx::query_scalar::<_, String>(
                "SELECT id FROM recordings ORDER BY created_at LIMIT 1",
            )
            .fetch_one(db.pool())
            .await
            .expect("recording")
            .parse()
            .expect("recording id");

            let service = DeletionService::new(db.clone(), media);
            let disclosure = service.disclose(recording_id).await.expect("disclose");
            let planned = service
                .plan(
                    recording_id,
                    &DeletionConfirmation {
                        disclosure_digest: disclosure.digest(),
                        reason: "user_requested".into(),
                    },
                    NOW,
                )
                .await
                .expect("plan");
            println!("{}", planned.deletion_id);
            std::io::stdout().flush().expect("flush");
            if step == ExportStep::DeleteAfterJournal {
                crash();
            }
            service
                .remove_rows_for(&planned, NOW)
                .await
                .expect("remove rows");
            crash();
        }
    }
    crash();
}

/// Extracts verified payloads into a directory, exactly as the restore
/// pipeline's own staging sink does.
struct FileSink {
    directory: PathBuf,
    current: Option<(PathBuf, std::fs::File)>,
    staged: Vec<(String, PathBuf)>,
    next: u64,
}

impl FileSink {
    fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            current: None,
            staged: Vec::new(),
            next: 0,
        }
    }
}

impl EntrySink for FileSink {
    fn begin(&mut self, _entry: &ManifestEntry) -> ExportResult<()> {
        let path = self.directory.join(format!("entry-{:08}", self.next));
        self.next += 1;
        let file = std::fs::File::create(&path)?;
        self.current = Some((path, file));
        Ok(())
    }

    fn chunk(&mut self, bytes: &[u8]) -> ExportResult<()> {
        let (_, file) = self.current.as_mut().ok_or(ExportError::IntegrityFailed)?;
        file.write_all(bytes)?;
        Ok(())
    }

    fn commit(&mut self, entry: &ManifestEntry) -> ExportResult<()> {
        let (path, file) = self.current.take().ok_or(ExportError::IntegrityFailed)?;
        file.sync_all()?;
        drop(file);
        self.staged.push((entry.path.clone(), path));
        Ok(())
    }
}

fn _unused(_: &Path) {}

/// Terminate immediately: no destructors, no buffered writes, no unwinding.
fn crash() -> ! {
    std::process::abort();
}
