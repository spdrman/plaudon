//! Consent and Local-only Mode enforcement for scripted work (App. A.5).
//!
//! App. A.5: "We will restrict machine-triggered automation to the current
//! signed-in OS user and require explicit cloud consent for any cost-bearing
//! work." The owner check lives in [`crate::bridge`]; this module is the
//! second half.
//!
//! The state read here is the same state the GUI writes: the `consent_grants`
//! table and the `local_only_mode` flag inside the persisted preferences. The
//! CLI must never have its own consent store, because then revoking consent in
//! the app would not revoke it for scripts.

use plaudon_persistence::{Database, PersistenceResult};
use serde::{Deserialize, Serialize};
use sqlx::Row;

/// `app_preferences` key the desktop shell writes its settings under.
pub const PREFERENCES_KEY: &str = "desktop.preferences";

/// Where a requested processing stage would run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    /// Entirely on this machine.
    Local,
    /// A cloud provider, which is cost-bearing and needs consent.
    Cloud,
}

impl Destination {
    /// Whether this destination is cost-bearing.
    #[must_use]
    pub const fn is_cost_bearing(self) -> bool {
        matches!(self, Self::Cloud)
    }
}

/// Why scripted processing was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyRefusal {
    /// Local-only Mode is on, so nothing may leave the machine.
    #[error("Local-only Mode is enabled, so cloud processing was refused")]
    LocalOnlyMode,
    /// No active consent grant covers this work.
    #[error("cloud processing needs consent that has not been granted")]
    ConsentRequired,
}

impl PolicyRefusal {
    /// The stable error code reported in the CLI's JSON envelope.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::LocalOnlyMode => "LOCAL_ONLY_MODE",
            Self::ConsentRequired => "CONSENT_REQUIRED",
        }
    }
}

/// The effective policy for the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectivePolicy {
    /// Whether Local-only Mode is enabled.
    pub local_only_mode: bool,
    /// Identifiers of consent grants that are currently active.
    pub active_consent_ids: Vec<String>,
}

impl EffectivePolicy {
    /// Decide whether work at `destination` may proceed.
    ///
    /// # Errors
    ///
    /// Returns the refusal that applies. Local-only Mode is checked first, so
    /// a user who turned it on gets that explanation rather than being told to
    /// grant consent they already granted.
    pub fn authorize(&self, destination: Destination) -> Result<Option<&str>, PolicyRefusal> {
        if !destination.is_cost_bearing() {
            return Ok(None);
        }
        if self.local_only_mode {
            return Err(PolicyRefusal::LocalOnlyMode);
        }
        self.active_consent_ids
            .first()
            .map(String::as_str)
            .map(Some)
            .ok_or(PolicyRefusal::ConsentRequired)
    }
}

/// Reads the effective policy from the library database.
#[derive(Debug, Clone)]
pub struct PolicyReader {
    db: Database,
}

impl PolicyReader {
    /// Open the reader.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Load the effective policy.
    ///
    /// `now_ms` is supplied by the caller so the decision is testable and so a
    /// clock change mid-run cannot make a grant flicker.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn load(&self, now_ms: i64) -> PersistenceResult<EffectivePolicy> {
        let local_only_mode = self.local_only_mode().await?;
        let rows = sqlx::query(
            "SELECT id FROM consent_grants
             WHERE revoked_at IS NULL AND approved_at <= ?1
             ORDER BY approved_at",
        )
        .bind(now_ms)
        .fetch_all(self.db.pool())
        .await?;
        let active_consent_ids = rows
            .into_iter()
            .map(|row| row.try_get::<String, _>("id"))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(EffectivePolicy {
            local_only_mode,
            active_consent_ids,
        })
    }

    /// Whether Local-only Mode is on, as persisted by the desktop shell.
    ///
    /// An absent or unreadable preferences blob means the user never changed
    /// the setting, which is the shipped default of "off". A malformed blob is
    /// deliberately not treated as "on": doing so would silently break
    /// scripted local work, and Local-only Mode is enforced by the egress gate
    /// in `plaudon-security` regardless of what this reader concludes.
    async fn local_only_mode(&self) -> PersistenceResult<bool> {
        let row = sqlx::query("SELECT value_json FROM app_preferences WHERE key = ?1")
            .bind(PREFERENCES_KEY)
            .fetch_optional(self.db.pool())
            .await?;
        let Some(row) = row else { return Ok(false) };
        let raw: String = row.try_get("value_json")?;
        Ok(serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|value| {
                value
                    .get("local_only_mode")
                    .and_then(serde_json::Value::as_bool)
            })
            .unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_persistence::migrations::MigrationRunner;

    async fn open() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("library.sqlite"))
            .await
            .unwrap();
        MigrationRunner::new().run(&db).await.unwrap();
        (dir, db)
    }

    async fn set_local_only(db: &Database, enabled: bool) {
        sqlx::query(
            "INSERT INTO app_preferences (key, version, value_json) VALUES (?1, 1, ?2)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
        )
        .bind(PREFERENCES_KEY)
        .bind(format!("{{\"local_only_mode\": {enabled}}}"))
        .execute(db.pool())
        .await
        .unwrap();
    }

    async fn grant(db: &Database, id: &str, revoked_at: Option<i64>) {
        sqlx::query(
            "INSERT INTO consent_grants
                 (id, scope, privacy_policy_version, engine, model_or_provider, approved_at, revoked_at)
             VALUES (?1, 'recording', 'v1', 'openrouter', 'test/model', 1, ?2)",
        )
        .bind(id)
        .bind(revoked_at)
        .execute(db.pool())
        .await
        .unwrap();
    }

    #[test]
    fn local_work_never_needs_consent() {
        let policy = EffectivePolicy {
            local_only_mode: true,
            active_consent_ids: Vec::new(),
        };
        assert_eq!(policy.authorize(Destination::Local), Ok(None));
    }

    #[test]
    fn cloud_work_without_consent_is_refused() {
        let policy = EffectivePolicy {
            local_only_mode: false,
            active_consent_ids: Vec::new(),
        };
        assert_eq!(
            policy.authorize(Destination::Cloud),
            Err(PolicyRefusal::ConsentRequired)
        );
    }

    #[test]
    fn local_only_mode_takes_precedence_over_an_existing_grant() {
        let policy = EffectivePolicy {
            local_only_mode: true,
            active_consent_ids: vec!["grant-1".to_string()],
        };
        assert_eq!(
            policy.authorize(Destination::Cloud),
            Err(PolicyRefusal::LocalOnlyMode)
        );
    }

    #[test]
    fn cloud_work_with_a_grant_is_authorized_and_names_the_grant() {
        let policy = EffectivePolicy {
            local_only_mode: false,
            active_consent_ids: vec!["grant-1".to_string()],
        };
        assert_eq!(policy.authorize(Destination::Cloud), Ok(Some("grant-1")));
    }

    #[tokio::test]
    async fn a_fresh_library_has_no_consent_and_local_only_off() {
        let (_dir, db) = open().await;
        let policy = PolicyReader::new(db.clone()).load(1_000).await.unwrap();
        assert!(!policy.local_only_mode);
        assert_eq!(policy.active_consent_ids, [] as [std::string::String; 0]);
        db.close().await;
    }

    #[tokio::test]
    async fn a_revoked_grant_does_not_authorize_anything() {
        let (_dir, db) = open().await;
        grant(&db, "grant-1", Some(500)).await;
        let policy = PolicyReader::new(db.clone()).load(1_000).await.unwrap();
        assert_eq!(policy.active_consent_ids, [] as [std::string::String; 0]);
        assert_eq!(
            policy.authorize(Destination::Cloud),
            Err(PolicyRefusal::ConsentRequired)
        );
        db.close().await;
    }

    #[tokio::test]
    async fn an_active_grant_authorizes_cloud_work() {
        let (_dir, db) = open().await;
        grant(&db, "grant-1", None).await;
        let policy = PolicyReader::new(db.clone()).load(1_000).await.unwrap();
        assert_eq!(policy.authorize(Destination::Cloud), Ok(Some("grant-1")));
        db.close().await;
    }

    #[tokio::test]
    async fn the_cli_sees_the_same_local_only_flag_the_gui_wrote() {
        let (_dir, db) = open().await;
        grant(&db, "grant-1", None).await;
        set_local_only(&db, true).await;
        let policy = PolicyReader::new(db.clone()).load(1_000).await.unwrap();
        assert!(policy.local_only_mode);
        assert_eq!(
            policy.authorize(Destination::Cloud),
            Err(PolicyRefusal::LocalOnlyMode)
        );

        set_local_only(&db, false).await;
        let policy = PolicyReader::new(db.clone()).load(1_000).await.unwrap();
        assert!(!policy.local_only_mode);
        db.close().await;
    }

    #[tokio::test]
    async fn a_malformed_preferences_blob_does_not_silently_change_the_mode() {
        let (_dir, db) = open().await;
        sqlx::query("INSERT INTO app_preferences (key, version, value_json) VALUES (?1, 1, '{')")
            .bind(PREFERENCES_KEY)
            .execute(db.pool())
            .await
            .unwrap();
        let policy = PolicyReader::new(db.clone()).load(1_000).await.unwrap();
        assert!(!policy.local_only_mode);
        db.close().await;
    }

    #[test]
    fn refusals_carry_stable_codes() {
        assert_eq!(PolicyRefusal::LocalOnlyMode.code(), "LOCAL_ONLY_MODE");
        assert_eq!(PolicyRefusal::ConsentRequired.code(), "CONSENT_REQUIRED");
    }
}
