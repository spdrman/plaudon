//! Crash-harness worker (SPEC v1.3 §14.6; EPIC C task C2.2).
//!
//! A child process that drives a scripted job lifecycle against a real SQLite
//! database with one crash point armed through `PLAUDON_CRASH_POINT`. The
//! parent kills nothing itself: the armed point aborts the process from inside,
//! which reproduces a hard kill (no unwinding, no flush, no destructors) at an
//! exact durable boundary.
//!
//! Only built with `--features fault-injection`; release builds contain neither
//! this binary nor any crash point.
//!
//! Usage:
//!   plaudon-chaos-worker --db <path> --staging <dir> --scenario <name>
//!
//! Scenarios are listed in [`SCENARIOS`]. The worker prints
//! `PLAUDON_WORKER_FINISHED <scenario> <job-id>` on stdout when it ran to
//! completion without the crash point firing.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use plaudon_domain::ids::RecordingId;
use plaudon_jobs::cancel::InterruptionCause;
use plaudon_jobs::engine::{EngineConfig, JobEngine};
use plaudon_jobs::key::{ImportJobKey, SourceKind};
use plaudon_jobs::scheduler::ResourceClass;
use plaudon_jobs::state::{JobState, TransitionCause};
use plaudon_jobs::store::{EnqueueRequest, JobOutput};
use plaudon_jobs::writer::Database;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::journal::{ImportJournal, JournalState};

/// Every scenario the worker can run.
pub const SCENARIOS: &[&str] = &[
    "import_happy_path",
    "import_with_journal",
    "cloud_dispatch_no_response",
    "cloud_dispatch_with_response",
    "cancel_mid_flight",
    "suspend_mid_stage",
    "consent_revoked_mid_queue",
];

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let get = |flag: &str| -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    let Some(db_path) = get("--db").map(PathBuf::from) else {
        eprintln!("--db <path> is required");
        return ExitCode::from(2);
    };
    let Some(staging) = get("--staging").map(PathBuf::from) else {
        eprintln!("--staging <dir> is required");
        return ExitCode::from(2);
    };
    let Some(archive) = get("--archive").map(PathBuf::from) else {
        eprintln!("--archive <dir> is required");
        return ExitCode::from(2);
    };
    let scenario = get("--scenario").unwrap_or_else(|| "import_happy_path".to_string());
    if !SCENARIOS.contains(&scenario.as_str()) {
        eprintln!("unknown scenario `{scenario}`; known: {SCENARIOS:?}");
        return ExitCode::from(2);
    }
    let seed = get("--seed").unwrap_or_else(|| "0".to_string());

    if let Err(error) = std::fs::create_dir_all(&staging) {
        eprintln!("could not create staging dir: {error}");
        return ExitCode::from(2);
    }

    if let Err(error) = std::fs::create_dir_all(&archive) {
        eprintln!("could not create archive dir: {error}");
        return ExitCode::from(2);
    }

    match run(&db_path, &staging, &archive, &scenario, &seed).await {
        Ok(job) => {
            println!("PLAUDON_WORKER_FINISHED {scenario} {job}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("PLAUDON_WORKER_FAILED {scenario} {error}");
            ExitCode::from(1)
        }
    }
}

async fn run(
    db_path: &Path,
    staging: &Path,
    archive: &Path,
    scenario: &str,
    seed: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    plaudon_jobs::store::JobStore::migrate(db_path).await?;
    let db = Database::open(db_path, 2).await?;
    let engine = JobEngine::open(
        db,
        EngineConfig::new(staging).with_archive_root(archive),
        Vec::new(),
    )
    .await?;

    // Startup recovery always runs first, exactly as the application does.
    engine.recover().await?;

    let content_hash = format!("{seed:0>64}");
    let key = ImportJobKey::by_content(SourceKind::LocalFile, content_hash.clone());
    let recording = RecordingId::new();
    let job = engine
        .enqueue(
            EnqueueRequest::new("import", key, ResourceClass::LocalDisk)
                .with_recording(recording)
                .with_consent("consent-1")
                .with_input_hash(content_hash.clone()),
        )
        .await?
        .job_id();
    engine
        .grant_consent("consent-1", 7, "openrouter", "acme/whisper-1")
        .await?;

    run_scenario(
        &engine,
        job,
        archive,
        staging,
        db_path,
        scenario,
        seed,
        content_hash,
    )
    .await?;
    Ok(job.to_string())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_scenario(
    engine: &JobEngine,
    job: plaudon_domain::ids::JobId,
    archive: &Path,
    staging: &Path,
    db_path: &Path,
    scenario: &str,
    seed: &str,
    content_hash: String,
) -> Result<(), Box<dyn std::error::Error>> {
    match scenario {
        "import_happy_path" => {
            advance(engine, job, &[JobState::Preflight, JobState::RunningLocal]).await?;
            engine
                .store()
                .prepare_checkpoint(job, "decode", 0, "in")
                .await?;
            engine
                .store()
                .commit_checkpoint(job, "decode", "pcm/0.raw", None)
                .await?;
            advance(engine, job, &[JobState::Verifying]).await?;
            let (reference, digest, length) = publish_asset(archive, seed)?;
            engine
                .complete(
                    job,
                    vec![JobOutput::new("media_asset", reference).verified(digest, length)],
                )
                .await?;
        }
        "import_with_journal" => {
            advance(engine, job, &[JobState::Preflight, JobState::RunningLocal]).await?;
            // Drive the real six-step durable import order (§13.3A) through
            // its owner, tripping a crash point at each step boundary.
            let media_archive = MediaArchive::open(archive)?;
            let journal = ImportJournal::new(
                plaudon_persistence::Database::open(db_path).await?,
                media_archive,
            );
            let bytes = format!("media-bytes-for-{seed}").into_bytes();

            plaudon_jobs::fault::trip("journal:staged");
            let staged = journal.stage(&mut bytes.as_slice(), "audio/wav")?;
            plaudon_jobs::fault::trip("journal:hashed");
            plaudon_jobs::fault::trip("journal:flushed");

            let at = plaudon_domain::time::Timestamp::now();
            let intent = journal
                .record_intent(
                    &staged,
                    "wav",
                    plaudon_domain::media::QualityClass::UserSuppliedFile,
                    None,
                    Some(job),
                    at,
                )
                .await?;
            plaudon_jobs::fault::trip_dynamic(&plaudon_jobs::fault::journal_point(
                JournalState::IntentRecorded,
            ));

            journal.rename_into_archive(&intent, at).await?;
            plaudon_jobs::fault::trip_dynamic(&plaudon_jobs::fault::journal_point(
                JournalState::Renamed,
            ));

            let asset = journal.commit(&intent, at).await?;
            plaudon_jobs::fault::trip_dynamic(&plaudon_jobs::fault::journal_point(
                JournalState::Committed,
            ));

            advance(engine, job, &[JobState::Verifying]).await?;
            engine
                .complete(
                    job,
                    vec![
                        JobOutput::new("media_asset", intent.target_relative_path.clone())
                            .verified(
                                intent.identity.sha256.to_hex(),
                                i64::try_from(intent.identity.byte_length)?,
                            ),
                    ],
                )
                .await?;
            debug_assert_ne!(asset.to_string(), String::new());
        }
        "cloud_dispatch_no_response" => {
            advance(engine, job, &[JobState::Preflight, JobState::CloudReady]).await?;
            engine
                .begin_dispatch(
                    job,
                    content_hash,
                    "multipart_stt",
                    Some("acme".into()),
                    "whisper-1",
                    None,
                    Some("zdr@1".into()),
                    7,
                )
                .await?;
            // The response never arrives; the process is expected to die here.
            engine.store().get(job).await?;
        }
        "cloud_dispatch_with_response" => {
            advance(engine, job, &[JobState::Preflight, JobState::CloudReady]).await?;
            let attempt = engine
                .begin_dispatch(
                    job,
                    content_hash,
                    "multipart_stt",
                    Some("acme".into()),
                    "whisper-1",
                    None,
                    Some("zdr@1".into()),
                    7,
                )
                .await?;
            engine
                .finish_dispatch(job, &attempt, Some("req-1".into()))
                .await?;
            engine
                .complete(
                    job,
                    vec![JobOutput::new("transcript_revision", format!("rev-{seed}"))],
                )
                .await?;
        }
        "cancel_mid_flight" => {
            advance(engine, job, &[JobState::Preflight, JobState::RunningLocal]).await?;
            let derivative = staging.join(format!("{job}.part"));
            write_staging_file(&derivative)?;
            engine.cancel(job).await?;
            engine.finish_cancellation(job, vec![derivative]).await?;
        }
        "suspend_mid_stage" => {
            advance(engine, job, &[JobState::Preflight, JobState::RunningLocal]).await?;
            engine.interrupt(job, InterruptionCause::OsSleep).await?;
            engine.resume(job).await?;
        }
        "consent_revoked_mid_queue" => {
            advance(engine, job, &[JobState::Preflight, JobState::CloudReady]).await?;
            engine.revoke_consent("consent-1").await?;
            // Must be refused; a success here is a bug the harness asserts on.
            if engine
                .begin_dispatch(
                    job,
                    content_hash,
                    "multipart_stt",
                    Some("acme".into()),
                    "whisper-1",
                    None,
                    None,
                    7,
                )
                .await
                .is_ok()
            {
                return Err("dispatch succeeded after consent revocation".into());
            }
        }
        other => return Err(format!("unknown scenario `{other}`").into()),
    }
    Ok(())
}

async fn advance(
    engine: &JobEngine,
    job: plaudon_domain::ids::JobId,
    states: &[JobState],
) -> Result<(), Box<dyn std::error::Error>> {
    for state in states {
        let cause = if *state == JobState::CloudSending {
            TransitionCause::system("chaos").with_consent_verified()
        } else {
            TransitionCause::system("chaos")
        };
        engine.store().transition(job, *state, cause).await?;
    }
    Ok(())
}

/// Write a staging file, tripping the mid-write crash point between chunks.
fn write_staging_file(path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(b"first-half-of-the-media-bytes")?;
    file.flush()?;
    plaudon_jobs::fault::trip("file:during_write");
    file.write_all(b"second-half-of-the-media-bytes")?;
    file.sync_all()?;
    Ok(())
}

/// Archive-relative reference for a scenario's media asset (§13.4 layout).
fn asset_reference(seed: &str) -> String {
    let digest = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(format!("media-bytes-for-{seed}").as_bytes()))
    };
    format!("media/sha256/{}/{digest}.wav", &digest[..2])
}

/// Write the scenario's media bytes into the content-addressed archive and
/// return `(reference, sha256, byte length)`.
fn publish_asset(
    archive: &Path,
    seed: &str,
) -> Result<(String, String, i64), Box<dyn std::error::Error>> {
    let reference = asset_reference(seed);
    let path = archive.join(&reference);
    std::fs::create_dir_all(path.parent().ok_or("asset has no parent")?)?;
    let bytes = format!("media-bytes-for-{seed}").into_bytes();
    std::fs::write(&path, &bytes)?;
    let digest = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(&bytes))
    };
    Ok((reference, digest, i64::try_from(bytes.len())?))
}
