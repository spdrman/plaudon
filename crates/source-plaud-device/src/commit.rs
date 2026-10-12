//! The durable two-stage commit for a device transfer (spec §4.2A.7, §7.9,
//! §13.3A, App. B.6).
//!
//! A radio transfer cannot use [`ImportJournal::stage`], because the bytes
//! arrive frame by frame rather than from a reader. [`DeviceStaging`] opens
//! the journal's own staging path, the actor streams into it, and the digest
//! is computed on the way past. From there the ordinary journal sequence
//! applies: record the intent, rename within the filesystem, transact the
//! asset, mark the journal committed.
//!
//! The rule the whole module exists to enforce is one line of spec §4.2A.7:
//! we "queue local transcription only after media verification". An
//! acquisition whose [`AcquisitionReport::may_commit`] is false never reaches
//! the archive, so a `Completed` import always points at verified bytes.

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use plaudon_domain::ids::{ImportIntentId, JobId, RecordingId, TransferId};
use plaudon_domain::media::{MediaIdentity, QualityClass};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::archive::sanitize_extension;
use plaudon_persistence::device::{
    DeviceRepository, TransferStatus, TransferVerification as RowVerification,
};
use plaudon_persistence::journal::{ImportJournal, StagedFile};
use plaudon_persistence::PersistenceError;

use crate::transfer::{AcquisitionReport, DerivativeKind, DerivativeOutcome};

/// Why a device import could not be committed.
#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    /// The acquisition did not pass its completeness checks.
    ///
    /// Carries the reason code so the interface can explain which check
    /// failed rather than saying "import failed".
    #[error("the transfer is not verified: {reason}")]
    NotVerified {
        /// Stable reason code from [`crate::TransferVerdict`].
        reason: &'static str,
    },
    /// The staging file could not be created or written.
    #[error("the staging file could not be written")]
    Staging(#[source] std::io::Error),
    /// The journal or the database refused.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
}

/// A staging file opened inside the journal's staging directory.
///
/// Holding this value is what makes the bytes recoverable: the intent
/// identifier is the staging file's name, so the startup reconciler can find
/// an interrupted device transfer exactly as it finds an interrupted download.
#[derive(Debug)]
pub struct DeviceStaging {
    intent_id: ImportIntentId,
    path: PathBuf,
    file: Option<File>,
}

impl DeviceStaging {
    /// Open a new staging file for one transfer.
    pub fn open(journal: &ImportJournal) -> Result<Self, CommitError> {
        let intent_id = ImportIntentId::new();
        let path = journal.archive().staging_path(intent_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(CommitError::Staging)?;
        }
        let file = File::create(&path).map_err(CommitError::Staging)?;
        Ok(Self {
            intent_id,
            path,
            file: Some(file),
        })
    }

    /// The intent this staging file belongs to.
    #[must_use]
    pub const fn intent_id(&self) -> ImportIntentId {
        self.intent_id
    }

    /// Absolute staging path.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Take the writer the actor streams into.
    ///
    /// Taking it once is deliberate: two writers to one staging file would
    /// interleave bytes that the digest could not describe.
    pub fn writer(&mut self) -> Option<File> {
        self.file.take()
    }

    /// Flush and close the file, keeping the bytes on disk.
    pub fn finish(mut self) -> Result<StagedBytes, CommitError> {
        if let Some(mut file) = self.file.take() {
            file.flush().map_err(CommitError::Staging)?;
            file.sync_all().map_err(CommitError::Staging)?;
        }
        Ok(StagedBytes {
            intent_id: self.intent_id,
            path: self.path.clone(),
        })
    }

    /// Remove the staging file, for an abandoned transfer.
    pub fn discard(self) -> Result<(), CommitError> {
        drop(self.file);
        if self.path.exists() {
            std::fs::remove_file(&self.path).map_err(CommitError::Staging)?;
        }
        Ok(())
    }
}

/// Staged bytes waiting for a commit decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedBytes {
    /// Intent the bytes belong to.
    pub intent_id: ImportIntentId,
    /// Absolute staging path.
    pub path: PathBuf,
}

/// What a committed device import produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedImport {
    /// Asset holding the exact transferred bytes.
    pub acquired_asset_id: plaudon_domain::ids::MediaAssetId,
    /// Asset holding the playable conversion, when one was produced.
    pub derivative_asset_id: Option<plaudon_domain::ids::MediaAssetId>,
    /// Identity of the transferred bytes.
    pub identity: MediaIdentity,
    /// Whether local transcription may now be queued.
    ///
    /// Always true for a committed import, and false is unreachable: the
    /// commit refuses an unverified acquisition (spec §4.2A.7).
    pub may_queue_transcription: bool,
}

/// One commit request.
///
/// A struct rather than a parameter list: every field is an identity or a
/// borrowed artefact, and two `Option<...Id>` arguments in a row are exactly
/// the shape a caller gets wrong.
#[derive(Debug)]
pub struct DeviceCommitRequest<'a> {
    /// The staged acquired bytes.
    pub staged: &'a StagedBytes,
    /// The acquisition report describing them.
    pub report: &'a AcquisitionReport,
    /// The playable derivative, when one was produced and staged.
    pub derivative: Option<(&'a DerivativeKind, &'a StagedBytes, &'a MediaIdentity)>,
    /// Transfer row to close.
    pub transfer_id: TransferId,
    /// Recording the assets belong to, when one exists already.
    pub recording_id: Option<RecordingId>,
    /// Job that owns the import.
    pub job_id: Option<JobId>,
}

/// Commits verified device transfers into the archive.
#[derive(Debug, Clone)]
pub struct DeviceImportCommitter {
    journal: ImportJournal,
    devices: DeviceRepository,
}

impl DeviceImportCommitter {
    /// Wrap a journal and the device ledger.
    #[must_use]
    pub const fn new(journal: ImportJournal, devices: DeviceRepository) -> Self {
        Self { journal, devices }
    }

    /// The journal this committer writes through.
    #[must_use]
    pub const fn journal(&self) -> &ImportJournal {
        &self.journal
    }

    /// Commit one verified acquisition.
    ///
    /// The sequence is spec §13.3A's, in order: the bytes are already staged
    /// and flushed (1, 2), the intent is recorded (3), the file is renamed
    /// within the archive volume (4), the asset association is transacted
    /// (5), and the journal is closed (6). A failure at any step leaves the
    /// journal row behind for the startup reconciler rather than a half-built
    /// library.
    pub async fn commit(
        &self,
        request: DeviceCommitRequest<'_>,
        at: Timestamp,
    ) -> Result<CommittedImport, CommitError> {
        let DeviceCommitRequest {
            staged,
            report,
            derivative,
            transfer_id,
            recording_id,
            job_id,
        } = request;
        if !report.may_commit() {
            // The staged bytes stay on disk: they are evidence for a resume
            // or for diagnostics, and the reconciler owns their cleanup.
            self.devices
                .finish_transfer(
                    transfer_id,
                    report.verification.received_bytes,
                    Some(&report.identity.sha256),
                    TransferStatus::Interrupted,
                    RowVerification::Unverified,
                    at,
                )
                .await?;
            return Err(CommitError::NotVerified {
                reason: report.verification.verdict.code(),
            });
        }

        let acquired = StagedFile {
            intent_id: staged.intent_id,
            path: staged.path.clone(),
            identity: report.identity.clone(),
        };
        let intent = self
            .journal
            .record_intent(
                &acquired,
                sanitize_extension("asr"),
                QualityClass::CompressedDeviceTransfer,
                recording_id,
                job_id,
                at,
            )
            .await?;
        self.journal.rename_into_archive(&intent, at).await?;
        let acquired_asset_id = self.journal.commit(&intent, at).await?;

        let derivative_asset_id = match derivative {
            Some((kind, staged_derivative, identity)) => {
                let staged_file = StagedFile {
                    intent_id: staged_derivative.intent_id,
                    path: staged_derivative.path.clone(),
                    identity: identity.clone(),
                };
                let intent = self
                    .journal
                    .record_intent(
                        &staged_file,
                        kind.extension(),
                        kind.quality_class(),
                        recording_id,
                        job_id,
                        at,
                    )
                    .await?;
                self.journal.rename_into_archive(&intent, at).await?;
                Some(self.journal.commit(&intent, at).await?)
            }
            None => None,
        };

        self.devices
            .finish_transfer(
                transfer_id,
                report.verification.received_bytes,
                Some(&report.identity.sha256),
                TransferStatus::Complete,
                RowVerification::Verified,
                at,
            )
            .await?;

        Ok(CommittedImport {
            acquired_asset_id,
            derivative_asset_id,
            identity: report.identity.clone(),
            may_queue_transcription: true,
        })
    }

    /// Record a cancelled transfer and remove its staging bytes.
    pub async fn cancel(
        &self,
        staging: DeviceStaging,
        transfer_id: TransferId,
        received_bytes: u64,
        at: Timestamp,
    ) -> Result<(), CommitError> {
        staging.discard()?;
        self.devices
            .finish_transfer(
                transfer_id,
                received_bytes,
                None,
                TransferStatus::Cancelled,
                RowVerification::Unverified,
                at,
            )
            .await?;
        Ok(())
    }
}

/// Whether a derivative produced an asset worth committing.
#[must_use]
pub fn derivative_to_commit<'a>(
    outcome: &'a DerivativeOutcome,
    staged: &'a StagedBytes,
) -> Option<(&'a DerivativeKind, &'a StagedBytes, &'a MediaIdentity)> {
    match outcome {
        DerivativeOutcome::Produced { kind, identity, .. } => Some((kind, staged, identity)),
        DerivativeOutcome::Unavailable { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use plaudon_domain::ids::DeviceId;
    use plaudon_persistence::archive::MediaArchive;
    use plaudon_persistence::db::Database;
    use plaudon_persistence::device::{DeviceRecord, TrustState};
    use plaudon_persistence::journal::JournalState;
    use plaudon_persistence::migrations::MigrationRunner;
    use plaudon_plaud_protocol::bulk::BulkFrame;
    use plaudon_plaud_protocol::payload::{DeviceCodec, ListedRecording};
    use plaudon_plaud_protocol::profile::LEGACY_NOTE_V0095;

    use super::*;
    use crate::transfer::StagingAcquisition;

    fn sidecar(packets: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..packets {
            let mut packet = vec![9 << 3];
            packet.extend(std::iter::repeat_n(0x55u8, 40));
            out.extend_from_slice(&u16::try_from(packet.len()).unwrap().to_be_bytes());
            out.extend_from_slice(&packet);
        }
        out
    }

    fn listed(bytes: u64) -> ListedRecording {
        ListedRecording {
            id: "REC0001".into(),
            display_name: "REC0001.ASR".into(),
            declared_byte_length: bytes,
            captured_at_unix: 1_760_000_000,
            codec: DeviceCodec::AsrOpus,
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        db: Database,
        committer: DeviceImportCommitter,
        devices: DeviceRepository,
        device_id: DeviceId,
        transfer_id: TransferId,
        archive: MediaArchive,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(dir.path().join("app.sqlite"))
            .await
            .expect("open");
        MigrationRunner::new().run(&db).await.expect("migrate");
        let archive = MediaArchive::open(dir.path().join("archive")).expect("archive");
        let journal = ImportJournal::new(db.clone(), archive.clone());
        let devices = DeviceRepository::new(db.clone());

        let at = Timestamp::from_millis(1_760_000_000_000);
        let device_id = DeviceId::new();
        devices
            .upsert_device(
                &DeviceRecord {
                    id: device_id,
                    vendor: "PLAUD".into(),
                    model: "Note".into(),
                    serial_fingerprint: Some("fp".into()),
                    discovered_name: Some("PLAUD_NOTE".into()),
                    observed_firmware: Some("V0095".into()),
                    trust_state: TrustState::ProfileAuthenticated,
                },
                at,
            )
            .await
            .expect("device");

        let transfer_id = TransferId::new();
        devices
            .start_transfer(
                &plaudon_persistence::device::TransferRecord {
                    id: transfer_id,
                    device_id,
                    transport: "ble".into(),
                    native_format: Some("audio/opus".into()),
                    expected_bytes: Some(1_025),
                    received_bytes: 0,
                    sha256: None,
                    status: TransferStatus::InFlight,
                    verification: RowVerification::Unverified,
                },
                at,
            )
            .await
            .expect("transfer row");

        Fixture {
            _dir: dir,
            db,
            committer: DeviceImportCommitter::new(journal, devices.clone()),
            devices,
            device_id,
            transfer_id,
            archive,
        }
    }

    /// Stage a transfer of `bytes`, optionally without its terminal marker.
    fn stage(
        fixture: &Fixture,
        bytes: &[u8],
        declared: u64,
        terminal: bool,
    ) -> (StagedBytes, AcquisitionReport) {
        let mut staging = DeviceStaging::open(fixture.committer.journal()).expect("staging");
        let writer = staging.writer().expect("the writer is taken once");
        let mut acquisition =
            StagingAcquisition::begin(&LEGACY_NOTE_V0095, &listed(declared), writer)
                .expect("acquisition");
        for (index, chunk) in bytes.chunks(80).enumerate() {
            acquisition
                .accept(&BulkFrame::Chunk {
                    offset: u32::try_from(index * 80).unwrap(),
                    payload: chunk.to_vec(),
                })
                .expect("accepting a chunk");
        }
        if terminal {
            acquisition
                .accept(&BulkFrame::EndOfTransfer)
                .expect("the terminal marker");
        }
        let report = acquisition.finish().expect("finishing");
        let staged = staging.finish().expect("flushing");
        (staged, report)
    }

    #[tokio::test]
    async fn a_verified_transfer_commits_and_only_then_may_be_transcribed() {
        let fixture = fixture().await;
        let bytes = sidecar(25);
        let (staged, report) = stage(&fixture, &bytes, bytes.len() as u64, true);
        assert!(report.may_commit());

        let at = Timestamp::from_millis(1_760_000_001_000);
        let committed = fixture
            .committer
            .commit(
                DeviceCommitRequest {
                    staged: &staged,
                    report: &report,
                    derivative: None,
                    transfer_id: fixture.transfer_id,
                    recording_id: None,
                    job_id: None,
                },
                at,
            )
            .await
            .expect("committing a verified transfer");

        assert!(committed.may_queue_transcription);
        assert_eq!(committed.identity, report.identity);
        assert!(!staged.path.exists(), "the staging file was renamed away");

        let intent = fixture
            .committer
            .journal()
            .get(staged.intent_id)
            .await
            .expect("the journal row");
        assert_eq!(intent.state, JournalState::Committed);
        assert_eq!(intent.quality_class, QualityClass::CompressedDeviceTransfer);

        let row = fixture
            .devices
            .get_transfer(fixture.transfer_id)
            .await
            .expect("the transfer row");
        assert_eq!(row.status, TransferStatus::Complete);
        assert_eq!(row.verification, RowVerification::Verified);
        assert_eq!(row.received_bytes, bytes.len() as u64);

        fixture
            .archive
            .verify(&intent.target_relative_path, &report.identity)
            .expect("the archived bytes match their identity");
        fixture.db.close().await;
    }

    #[tokio::test]
    async fn an_unverified_transfer_never_reaches_the_archive() {
        let fixture = fixture().await;
        let bytes = sidecar(25);
        // No terminal marker: the stream simply stopped.
        let (staged, report) = stage(&fixture, &bytes, bytes.len() as u64, false);
        assert!(!report.may_commit());

        let at = Timestamp::from_millis(1_760_000_001_000);
        let error = fixture
            .committer
            .commit(
                DeviceCommitRequest {
                    staged: &staged,
                    report: &report,
                    derivative: None,
                    transfer_id: fixture.transfer_id,
                    recording_id: None,
                    job_id: None,
                },
                at,
            )
            .await
            .expect_err("an unverified transfer must not commit");
        assert!(matches!(
            error,
            CommitError::NotVerified {
                reason: "NO_TERMINAL_MARKER"
            }
        ));

        assert!(
            staged.path.exists(),
            "the staged bytes stay for the reconciler, they are not silently lost"
        );
        assert_eq!(
            fixture.archive.list_blobs().expect("listing blobs").len(),
            0,
            "nothing reached the archive"
        );
        let row = fixture
            .devices
            .get_transfer(fixture.transfer_id)
            .await
            .expect("the transfer row");
        assert_eq!(row.status, TransferStatus::Interrupted);
        assert_eq!(row.verification, RowVerification::Unverified);
        fixture.db.close().await;
    }

    #[tokio::test]
    async fn a_playable_derivative_commits_as_a_second_distinct_asset() {
        let fixture = fixture().await;
        let bytes = sidecar(25);
        let (staged, report) = stage(&fixture, &bytes, bytes.len() as u64, true);

        // Produce the derivative into its own staging file.
        let mut derivative_staging =
            DeviceStaging::open(fixture.committer.journal()).expect("derivative staging");
        let mut writer = derivative_staging.writer().expect("writer");
        let outcome = crate::transfer::derive_playable(&report, &mut bytes.as_slice(), &mut writer);
        drop(writer);
        let derivative_staged = derivative_staging.finish().expect("flushing");
        let DerivativeOutcome::Produced { identity, .. } = &outcome else {
            panic!("a valid sidecar remuxes");
        };
        let derivative_identity = identity.clone();

        let at = Timestamp::from_millis(1_760_000_001_000);
        let committed = fixture
            .committer
            .commit(
                DeviceCommitRequest {
                    staged: &staged,
                    report: &report,
                    derivative: derivative_to_commit(&outcome, &derivative_staged),
                    transfer_id: fixture.transfer_id,
                    recording_id: None,
                    job_id: None,
                },
                at,
            )
            .await
            .expect("committing");

        let derivative_id = committed
            .derivative_asset_id
            .expect("the derivative was committed");
        assert_ne!(derivative_id, committed.acquired_asset_id);
        assert_ne!(derivative_identity, report.identity);

        let assets = plaudon_persistence::repo::MediaAssetRepository::new(fixture.db.clone());
        let acquired = assets
            .get(committed.acquired_asset_id)
            .await
            .expect("acquired asset");
        let derived = assets.get(derivative_id).await.expect("derived asset");
        assert_eq!(
            acquired.quality_class,
            QualityClass::CompressedDeviceTransfer
        );
        assert_eq!(derived.quality_class, QualityClass::LocalDerivative);
        assert!(!derived.quality_class.is_original_fidelity());
        assert_eq!(
            std::path::Path::new(&derived.relative_path)
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("opus")
        );
        assert_eq!(
            std::path::Path::new(&acquired.relative_path)
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("asr")
        );
        fixture.db.close().await;
    }

    #[tokio::test]
    async fn a_cancelled_transfer_leaves_no_staging_file_and_no_asset() {
        let fixture = fixture().await;
        let staging = DeviceStaging::open(fixture.committer.journal()).expect("staging");
        let path = staging.path().to_path_buf();
        assert!(path.exists());

        let at = Timestamp::from_millis(1_760_000_001_000);
        fixture
            .committer
            .cancel(staging, fixture.transfer_id, 160, at)
            .await
            .expect("cancelling");
        assert!(!path.exists());

        let row = fixture
            .devices
            .get_transfer(fixture.transfer_id)
            .await
            .expect("the transfer row");
        assert_eq!(row.status, TransferStatus::Cancelled);
        assert_eq!(row.verification, RowVerification::Unverified);
        assert_eq!(
            fixture.archive.list_blobs().expect("listing blobs").len(),
            0
        );
        fixture.db.close().await;
    }

    #[tokio::test]
    async fn a_second_transfer_of_identical_bytes_reuses_the_stored_blob() {
        let fixture = fixture().await;
        let bytes = sidecar(25);
        let at = Timestamp::from_millis(1_760_000_001_000);

        let (first_staged, first_report) = stage(&fixture, &bytes, bytes.len() as u64, true);
        let first = fixture
            .committer
            .commit(
                DeviceCommitRequest {
                    staged: &first_staged,
                    report: &first_report,
                    derivative: None,
                    transfer_id: fixture.transfer_id,
                    recording_id: None,
                    job_id: None,
                },
                at,
            )
            .await
            .expect("first commit");

        let second_transfer = TransferId::new();
        fixture
            .devices
            .start_transfer(
                &plaudon_persistence::device::TransferRecord {
                    id: second_transfer,
                    device_id: fixture.device_id,
                    transport: "ble".into(),
                    native_format: Some("audio/opus".into()),
                    expected_bytes: Some(bytes.len() as u64),
                    received_bytes: 0,
                    sha256: None,
                    status: TransferStatus::InFlight,
                    verification: RowVerification::Unverified,
                },
                at,
            )
            .await
            .expect("second transfer row");

        let (second_staged, second_report) = stage(&fixture, &bytes, bytes.len() as u64, true);
        let second = fixture
            .committer
            .commit(
                DeviceCommitRequest {
                    staged: &second_staged,
                    report: &second_report,
                    derivative: None,
                    transfer_id: second_transfer,
                    recording_id: None,
                    job_id: None,
                },
                at,
            )
            .await
            .expect("second commit");

        assert_eq!(
            first.acquired_asset_id, second.acquired_asset_id,
            "identical bytes share one stored blob"
        );
        assert_eq!(
            fixture.archive.list_blobs().expect("listing blobs").len(),
            1
        );
        assert!(!second_staged.path.exists());
        fixture.db.close().await;
    }
}
