//! The direct-device ledger (spec §13.2, §13.3, App. B.6).
//!
//! What this module persists is deliberately narrow: identities, observations,
//! transfer accounting and opaque vault references. It never stores a
//! credential, and the schema makes a verified transfer that is not complete
//! impossible to insert.

use plaudon_domain::ids::{DeviceId, TransferId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::time::Timestamp;

use crate::db::Database;
use crate::error::{PersistenceError, PersistenceResult};

/// Trust the application places in a device identity (spec §13.2 `devices`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustState {
    /// Seen, but nothing about its identity is established.
    Unverified,
    /// The device completed the profile's authentication exchange.
    ///
    /// Spec §16.2A: this is `profile-authenticated`, not cryptographically
    /// verified identity.
    ProfileAuthenticated,
}

impl TrustState {
    /// Stored string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::ProfileAuthenticated => "profile_authenticated",
        }
    }

    /// Decode a stored string; anything unknown is the weaker state.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw {
            "profile_authenticated" => Self::ProfileAuthenticated,
            _ => Self::Unverified,
        }
    }
}

/// Lifecycle of one transfer row (spec App. B.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferStatus {
    /// Bytes are arriving or the transfer is paused between attempts.
    InFlight,
    /// The protocol's terminal marker arrived and the range is contiguous.
    Complete,
    /// The transfer ended without completing.
    Interrupted,
    /// The user cancelled.
    Cancelled,
}

impl TransferStatus {
    /// Stored string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InFlight => "in_flight",
            Self::Complete => "complete",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Verification state of a transfer row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferVerification {
    /// Completeness has not been established.
    Unverified,
    /// Every completeness check passed.
    Verified,
}

impl TransferVerification {
    /// Stored string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Verified => "verified",
        }
    }
}

/// A device as the application knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    /// Row identity.
    pub id: DeviceId,
    /// Vendor as observed.
    pub vendor: String,
    /// Model as observed.
    pub model: String,
    /// Stable fingerprint of the host-assigned peripheral identity.
    pub serial_fingerprint: Option<String>,
    /// Name the device advertised. Display metadata only.
    pub discovered_name: Option<String>,
    /// Firmware fingerprint most recently observed.
    pub observed_firmware: Option<String>,
    /// Trust in the identity.
    pub trust_state: TrustState,
}

/// One transfer, as accounted in `device_transfers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRecord {
    /// Row identity.
    pub id: TransferId,
    /// Device the bytes came from.
    pub device_id: DeviceId,
    /// Transport used.
    pub transport: String,
    /// Native format the device reported.
    pub native_format: Option<String>,
    /// Length the device declared, when it declared one.
    pub expected_bytes: Option<u64>,
    /// Bytes actually received.
    pub received_bytes: u64,
    /// Digest of the received bytes, once they exist.
    pub sha256: Option<Sha256Digest>,
    /// Lifecycle state.
    pub status: TransferStatus,
    /// Verification state.
    pub verification: TransferVerification,
}

/// A resume checkpoint (spec §13.3 `device_transfer_checkpoints`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferCheckpointRecord {
    /// Transfer this checkpoint belongs to.
    pub transfer_id: TransferId,
    /// Device-scoped recording identifier.
    pub device_recording_id: String,
    /// Offset the next read would start at.
    pub chunk_offset: u64,
    /// Bytes staged and digested.
    pub verified_bytes: u64,
    /// Length the device declared when the transfer started.
    pub source_size_hint: Option<u64>,
    /// Digest of the staged prefix.
    pub staged_prefix_sha256: String,
    /// Whether the profile's offset semantics were demonstrated.
    pub resume_capability: String,
}

/// Columns of a `devices` row, in the order [`DeviceRepository::get_device`]
/// selects them.
type DeviceRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

/// Columns of a `device_transfers` row, in the order
/// [`DeviceRepository::get_transfer`] selects them.
type TransferRow = (
    String,
    String,
    Option<String>,
    Option<i64>,
    i64,
    Option<String>,
    String,
    String,
);

/// Columns of a `device_transfer_checkpoints` row, in the order
/// [`DeviceRepository::checkpoint`] selects them.
type CheckpointRow = (String, i64, i64, Option<i64>, String, String);

/// Reads and writes the direct-device ledger.
#[derive(Debug, Clone)]
pub struct DeviceRepository {
    db: Database,
}

impl DeviceRepository {
    /// Wrap a database.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Insert or refresh a device row.
    pub async fn upsert_device(
        &self,
        device: &DeviceRecord,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO devices
                 (id, vendor, model, serial_fingerprint, discovered_name, observed_firmware,
                  first_seen_at, last_seen_at, trust_state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                 vendor = excluded.vendor,
                 model = excluded.model,
                 serial_fingerprint = excluded.serial_fingerprint,
                 discovered_name = excluded.discovered_name,
                 observed_firmware = excluded.observed_firmware,
                 last_seen_at = excluded.last_seen_at,
                 trust_state = excluded.trust_state",
        )
        .bind(device.id.to_string())
        .bind(&device.vendor)
        .bind(&device.model)
        .bind(device.serial_fingerprint.as_ref())
        .bind(device.discovered_name.as_ref())
        .bind(device.observed_firmware.as_ref())
        .bind(at.as_millis())
        .bind(device.trust_state.as_str())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Read a device row.
    pub async fn get_device(&self, id: DeviceId) -> PersistenceResult<DeviceRecord> {
        let row: Option<DeviceRow> = sqlx::query_as(
            "SELECT vendor, model, serial_fingerprint, discovered_name, observed_firmware,
                        trust_state
                 FROM devices WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(self.db.pool())
        .await?;
        let (vendor, model, serial_fingerprint, discovered_name, observed_firmware, trust_state) =
            row.ok_or(PersistenceError::RecordNotFound)?;
        Ok(DeviceRecord {
            id,
            vendor,
            model,
            serial_fingerprint,
            discovered_name,
            observed_firmware,
            trust_state: TrustState::parse(&trust_state),
        })
    }

    /// Record a firmware observation.
    ///
    /// Returns `true` when the fingerprint differs from the most recent one,
    /// which is the signal to revalidate the compatibility cell rather than
    /// reuse a cached capability (spec App. B.10).
    pub async fn record_firmware_observation(
        &self,
        device_id: DeviceId,
        firmware_fingerprint: &str,
        source: &str,
        capability_profile: &str,
        auth_mode: &str,
        at: Timestamp,
    ) -> PersistenceResult<bool> {
        let previous: Option<(String,)> = sqlx::query_as(
            "SELECT firmware_fingerprint FROM device_firmware_observations
             WHERE device_id = ?1 ORDER BY observed_at DESC, rowid DESC LIMIT 1",
        )
        .bind(device_id.to_string())
        .fetch_optional(self.db.pool())
        .await?;

        sqlx::query(
            "INSERT INTO device_firmware_observations
                 (id, device_id, firmware_fingerprint, observed_at, source,
                  capability_profile, auth_mode)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(device_id.to_string())
        .bind(firmware_fingerprint)
        .bind(at.as_millis())
        .bind(source)
        .bind(capability_profile)
        .bind(auth_mode)
        .execute(self.db.pool())
        .await?;

        Ok(previous.is_some_and(|(seen,)| seen != firmware_fingerprint))
    }

    /// Open a transfer row.
    pub async fn start_transfer(
        &self,
        record: &TransferRecord,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO device_transfers
                 (id, device_id, source_item_id, transport, native_format, expected_bytes,
                  received_bytes, sha256, transfer_status, verification_status, started_at)
             VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(record.id.to_string())
        .bind(record.device_id.to_string())
        .bind(&record.transport)
        .bind(record.native_format.as_ref())
        .bind(record.expected_bytes.map(i64_of))
        .bind(i64_of(record.received_bytes))
        .bind(record.sha256.map(Sha256Digest::to_hex))
        .bind(record.status.as_str())
        .bind(record.verification.as_str())
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Close a transfer row.
    ///
    /// The schema refuses a `verified` row whose transfer is not `complete`,
    /// so an attempt to mark an interrupted transfer verified fails here
    /// rather than producing a playable record (spec §13.2).
    pub async fn finish_transfer(
        &self,
        id: TransferId,
        received_bytes: u64,
        sha256: Option<&Sha256Digest>,
        status: TransferStatus,
        verification: TransferVerification,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "UPDATE device_transfers
             SET received_bytes = ?2, sha256 = ?3, transfer_status = ?4,
                 verification_status = ?5, ended_at = ?6
             WHERE id = ?1",
        )
        .bind(id.to_string())
        .bind(i64_of(received_bytes))
        .bind(sha256.copied().map(Sha256Digest::to_hex))
        .bind(status.as_str())
        .bind(verification.as_str())
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Read a transfer row.
    pub async fn get_transfer(&self, id: TransferId) -> PersistenceResult<TransferRecord> {
        let row: Option<TransferRow> = sqlx::query_as(
            "SELECT device_id, transport, native_format, expected_bytes, received_bytes,
                    sha256, transfer_status, verification_status
             FROM device_transfers WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(self.db.pool())
        .await?;
        let (
            device_id,
            transport,
            native_format,
            expected_bytes,
            received_bytes,
            sha256,
            status,
            verification,
        ) = row.ok_or(PersistenceError::RecordNotFound)?;

        Ok(TransferRecord {
            id,
            device_id: device_id
                .parse()
                .map_err(|_| PersistenceError::RecordNotFound)?,
            transport,
            native_format,
            expected_bytes: expected_bytes.map(u64_of),
            received_bytes: u64_of(received_bytes),
            sha256: sha256.as_deref().and_then(|hex| hex.parse().ok()),
            status: match status.as_str() {
                "complete" => TransferStatus::Complete,
                "cancelled" => TransferStatus::Cancelled,
                "interrupted" => TransferStatus::Interrupted,
                _ => TransferStatus::InFlight,
            },
            verification: if verification == "verified" {
                TransferVerification::Verified
            } else {
                TransferVerification::Unverified
            },
        })
    }

    /// Write or replace a resume checkpoint.
    pub async fn record_checkpoint(
        &self,
        checkpoint: &TransferCheckpointRecord,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO device_transfer_checkpoints
                 (transfer_id, device_recording_id, chunk_offset, verified_bytes,
                  source_size_hint, staged_prefix_sha256, resume_capability, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(transfer_id) DO UPDATE SET
                 device_recording_id = excluded.device_recording_id,
                 chunk_offset = excluded.chunk_offset,
                 verified_bytes = excluded.verified_bytes,
                 source_size_hint = excluded.source_size_hint,
                 staged_prefix_sha256 = excluded.staged_prefix_sha256,
                 resume_capability = excluded.resume_capability,
                 updated_at = excluded.updated_at",
        )
        .bind(checkpoint.transfer_id.to_string())
        .bind(&checkpoint.device_recording_id)
        .bind(i64_of(checkpoint.chunk_offset))
        .bind(i64_of(checkpoint.verified_bytes))
        .bind(checkpoint.source_size_hint.map(i64_of))
        .bind(&checkpoint.staged_prefix_sha256)
        .bind(&checkpoint.resume_capability)
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Read a resume checkpoint.
    pub async fn checkpoint(
        &self,
        transfer_id: TransferId,
    ) -> PersistenceResult<Option<TransferCheckpointRecord>> {
        let row: Option<CheckpointRow> = sqlx::query_as(
            "SELECT device_recording_id, chunk_offset, verified_bytes, source_size_hint,
                    staged_prefix_sha256, resume_capability
             FROM device_transfer_checkpoints WHERE transfer_id = ?1",
        )
        .bind(transfer_id.to_string())
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(
            |(
                device_recording_id,
                chunk_offset,
                verified_bytes,
                source_size_hint,
                staged_prefix_sha256,
                resume_capability,
            )| {
                TransferCheckpointRecord {
                    transfer_id,
                    device_recording_id,
                    chunk_offset: u64_of(chunk_offset),
                    verified_bytes: u64_of(verified_bytes),
                    source_size_hint: source_size_hint.map(u64_of),
                    staged_prefix_sha256,
                    resume_capability,
                }
            },
        ))
    }

    /// Record the outcome of a non-destructive audit for one profile.
    ///
    /// Spec §7.9 and §17.3A: automatic nearby sync stays disabled until an
    /// audit passes for the profile, and the result belongs to the
    /// compatibility cell rather than to one session, so it survives a
    /// restart. Stored in `compatibility_evidence`, which is the table that
    /// already exists for exactly this kind of claim.
    pub async fn record_audit(
        &self,
        profile_key: &str,
        host_os: &str,
        firmware: Option<&str>,
        outcome_code: &str,
        notes: &str,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO compatibility_evidence
                 (id, profile_key, evidence_level, test_run_id, host_os, firmware,
                  recorded_at, notes)
             VALUES (?1, ?2, 'non_destructive_audit', ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(profile_key)
        .bind(outcome_code)
        .bind(host_os)
        .bind(firmware)
        .bind(at.as_millis())
        .bind(notes)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// The most recent audit outcome code for a profile, if any.
    ///
    /// Absence is not a pass: a caller with `None` must treat the profile as
    /// unaudited and leave automatic sync off.
    pub async fn latest_audit(&self, profile_key: &str) -> PersistenceResult<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT test_run_id FROM compatibility_evidence
             WHERE profile_key = ?1 AND evidence_level = 'non_destructive_audit'
             ORDER BY recorded_at DESC, rowid DESC LIMIT 1",
        )
        .bind(profile_key)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(code,)| code))
    }

    /// Record the opaque vault reference for a device credential.
    ///
    /// Only the service and account strings are stored; the credential itself
    /// never leaves the OS vault (spec §16.4, App. B.7).
    pub async fn record_credential_ref(
        &self,
        device_id: DeviceId,
        vault_service: &str,
        vault_account_reference: &str,
        auth_mode: &str,
        at: Timestamp,
    ) -> PersistenceResult<()> {
        sqlx::query(
            "INSERT INTO device_credential_refs
                 (device_id, vault_service, vault_account_reference, auth_mode,
                  created_at, last_verified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(device_id) DO UPDATE SET
                 vault_service = excluded.vault_service,
                 vault_account_reference = excluded.vault_account_reference,
                 auth_mode = excluded.auth_mode,
                 last_verified_at = excluded.last_verified_at",
        )
        .bind(device_id.to_string())
        .bind(vault_service)
        .bind(vault_account_reference)
        .bind(auth_mode)
        .bind(at.as_millis())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Forget the vault reference for a device.
    pub async fn remove_credential_ref(&self, device_id: DeviceId) -> PersistenceResult<()> {
        sqlx::query("DELETE FROM device_credential_refs WHERE device_id = ?1")
            .bind(device_id.to_string())
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    /// Whether a vault reference exists for a device.
    pub async fn has_credential_ref(&self, device_id: DeviceId) -> PersistenceResult<bool> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM device_credential_refs WHERE device_id = ?1")
                .bind(device_id.to_string())
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row.is_some())
    }
}

fn i64_of(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn u64_of(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use plaudon_domain::media::Sha256Digest;

    use super::*;
    use crate::migrations::MigrationRunner;

    async fn fresh() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(dir.path().join("app.sqlite"))
            .await
            .expect("open");
        MigrationRunner::new().run(&db).await.expect("migrate");
        (dir, db)
    }

    fn device() -> DeviceRecord {
        DeviceRecord {
            id: DeviceId::new(),
            vendor: "PLAUD".into(),
            model: "Note".into(),
            serial_fingerprint: Some("fp-1".into()),
            discovered_name: Some("PLAUD_NOTE".into()),
            observed_firmware: Some("V0095".into()),
            trust_state: TrustState::Unverified,
        }
    }

    fn transfer(device_id: DeviceId, expected: Option<u64>) -> TransferRecord {
        TransferRecord {
            id: TransferId::new(),
            device_id,
            transport: "ble".into(),
            native_format: Some("audio/opus".into()),
            expected_bytes: expected,
            received_bytes: 0,
            sha256: None,
            status: TransferStatus::InFlight,
            verification: TransferVerification::Unverified,
        }
    }

    #[tokio::test]
    async fn a_device_round_trips_and_refreshes() {
        let (_dir, db) = fresh().await;
        let repository = DeviceRepository::new(db.clone());
        let mut record = device();
        let at = Timestamp::from_millis(1_760_000_000_000);
        repository.upsert_device(&record, at).await.unwrap();
        assert_eq!(repository.get_device(record.id).await.unwrap(), record);

        record.trust_state = TrustState::ProfileAuthenticated;
        repository.upsert_device(&record, at).await.unwrap();
        assert_eq!(
            repository.get_device(record.id).await.unwrap().trust_state,
            TrustState::ProfileAuthenticated
        );
        db.close().await;
    }

    #[tokio::test]
    async fn a_firmware_change_is_reported_so_the_cell_can_be_revalidated() {
        let (_dir, db) = fresh().await;
        let repository = DeviceRepository::new(db.clone());
        let record = device();
        let at = Timestamp::from_millis(1_760_000_000_000);
        repository.upsert_device(&record, at).await.unwrap();

        let changed = repository
            .record_firmware_observation(
                record.id,
                "V0095",
                "probe",
                "plaud-note-legacy-v0095",
                "legacy_static_token",
                at,
            )
            .await
            .unwrap();
        assert!(!changed, "the first observation has nothing to differ from");

        let same = repository
            .record_firmware_observation(
                record.id,
                "V0095",
                "probe",
                "plaud-note-legacy-v0095",
                "legacy_static_token",
                at,
            )
            .await
            .unwrap();
        assert!(!same);

        let upgraded = repository
            .record_firmware_observation(
                record.id,
                "V0096",
                "probe",
                "plaud-note-legacy-v0095",
                "legacy_static_token",
                at,
            )
            .await
            .unwrap();
        assert!(upgraded, "a changed firmware must demand revalidation");
        db.close().await;
    }

    #[tokio::test]
    async fn an_interrupted_transfer_cannot_be_recorded_as_verified() {
        let (_dir, db) = fresh().await;
        let repository = DeviceRepository::new(db.clone());
        let record = device();
        let at = Timestamp::from_millis(1_760_000_000_000);
        repository.upsert_device(&record, at).await.unwrap();

        let transfer = transfer(record.id, Some(4_096));
        repository.start_transfer(&transfer, at).await.unwrap();

        let digest = Sha256Digest::of(b"partial");
        let refused = repository
            .finish_transfer(
                transfer.id,
                1_000,
                Some(&digest),
                TransferStatus::Interrupted,
                TransferVerification::Verified,
                at,
            )
            .await;
        assert!(
            refused.is_err(),
            "the schema must refuse a verified row for an incomplete transfer"
        );

        repository
            .finish_transfer(
                transfer.id,
                1_000,
                Some(&digest),
                TransferStatus::Interrupted,
                TransferVerification::Unverified,
                at,
            )
            .await
            .unwrap();
        let stored = repository.get_transfer(transfer.id).await.unwrap();
        assert_eq!(stored.status, TransferStatus::Interrupted);
        assert_eq!(stored.verification, TransferVerification::Unverified);
        assert_eq!(stored.received_bytes, 1_000);
        db.close().await;
    }

    #[tokio::test]
    async fn a_checkpoint_round_trips_and_is_replaced_in_place() {
        let (_dir, db) = fresh().await;
        let repository = DeviceRepository::new(db.clone());
        let record = device();
        let at = Timestamp::from_millis(1_760_000_000_000);
        repository.upsert_device(&record, at).await.unwrap();
        let transfer = transfer(record.id, Some(4_096));
        repository.start_transfer(&transfer, at).await.unwrap();

        let mut checkpoint = TransferCheckpointRecord {
            transfer_id: transfer.id,
            device_recording_id: "REC0001".into(),
            chunk_offset: 160,
            verified_bytes: 160,
            source_size_hint: Some(4_096),
            staged_prefix_sha256: "ab".repeat(32),
            resume_capability: "restart_only".into(),
        };
        repository.record_checkpoint(&checkpoint, at).await.unwrap();
        assert_eq!(
            repository.checkpoint(transfer.id).await.unwrap(),
            Some(checkpoint.clone())
        );

        checkpoint.chunk_offset = 320;
        checkpoint.verified_bytes = 320;
        repository.record_checkpoint(&checkpoint, at).await.unwrap();
        assert_eq!(
            repository
                .checkpoint(transfer.id)
                .await
                .unwrap()
                .map(|c| c.verified_bytes),
            Some(320)
        );
        db.close().await;
    }

    #[tokio::test]
    async fn an_audit_outcome_is_recorded_per_profile_and_the_latest_one_wins() {
        let (_dir, db) = fresh().await;
        let repository = DeviceRepository::new(db.clone());
        let profile = "plaud-note-legacy-v0095";
        assert_eq!(repository.latest_audit(profile).await.unwrap(), None);

        repository
            .record_audit(
                profile,
                "macos",
                Some("V0095"),
                "LISTING_CHANGED",
                "REC0001 disappeared after a read",
                Timestamp::from_millis(1_760_000_000_000),
            )
            .await
            .unwrap();
        assert_eq!(
            repository.latest_audit(profile).await.unwrap().as_deref(),
            Some("LISTING_CHANGED")
        );

        repository
            .record_audit(
                profile,
                "macos",
                Some("V0095"),
                "UNCHANGED",
                "two identical passes",
                Timestamp::from_millis(1_760_000_100_000),
            )
            .await
            .unwrap();
        assert_eq!(
            repository.latest_audit(profile).await.unwrap().as_deref(),
            Some("UNCHANGED")
        );
        assert_eq!(
            repository.latest_audit("plaud-note-future").await.unwrap(),
            None,
            "an unaudited profile is unaudited, not passing"
        );
        db.close().await;
    }

    #[tokio::test]
    async fn a_credential_reference_holds_no_credential() {
        let (_dir, db) = fresh().await;
        let repository = DeviceRepository::new(db.clone());
        let record = device();
        let at = Timestamp::from_millis(1_760_000_000_000);
        repository.upsert_device(&record, at).await.unwrap();

        repository
            .record_credential_ref(
                record.id,
                "ai.plaudon.plaudon",
                "device.dev-0123.auth",
                "legacy_static_token",
                at,
            )
            .await
            .unwrap();
        assert!(repository.has_credential_ref(record.id).await.unwrap());

        let stored: (String, String) = sqlx::query_as(
            "SELECT vault_service, vault_account_reference FROM device_credential_refs
             WHERE device_id = ?1",
        )
        .bind(record.id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(stored.0, "ai.plaudon.plaudon");
        assert!(stored.1.starts_with("device."));

        repository.remove_credential_ref(record.id).await.unwrap();
        assert!(!repository.has_credential_ref(record.id).await.unwrap());
        db.close().await;
    }
}
