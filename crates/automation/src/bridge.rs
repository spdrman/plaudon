//! The enable switch and the owner check for the scripting bridge
//! (spec 14.5, App. A.5).
//!
//! Spec 14.5 permits "an optional, explicitly enabled local scripting bridge".
//! App. A.5 restricts machine-triggered automation to the current signed-in OS
//! user. Both are enforced here, before any command does anything:
//!
//! * The bridge is **off** until a user turns it on in settings. The default
//!   row written by migration `0400` has `enabled = 0`, so a fresh install is
//!   inert even if the binary is on the PATH.
//! * Turning it on records the OS user that did so. A different user running
//!   the same binary against the same library is refused, which is the case
//!   that matters on a shared machine.
//!
//! What this is not: an authentication mechanism. It is a guard against a
//! second account and against accidental activation. Spec 16.2 already treats
//! an attacker with the user's own session as outside what the app can
//! defend.

use plaudon_persistence::{Database, PersistenceResult};
use serde::{Deserialize, Serialize};
use sqlx::Row;

/// State of the bridge for one library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeState {
    /// Whether the user enabled the bridge.
    pub enabled: bool,
    /// Opaque identity of the OS user who enabled it.
    pub owner_user_key: Option<String>,
    /// When it was enabled, in epoch milliseconds.
    pub enabled_at_ms: Option<i64>,
}

impl BridgeState {
    /// The state of a library where the bridge was never turned on.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            owner_user_key: None,
            enabled_at_ms: None,
        }
    }
}

/// What a `bridge` subcommand did, for the output envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BridgeCommandOutcome {
    /// The state was reported unchanged.
    Status(BridgeState),
    /// The bridge was turned on.
    Enabled(BridgeState),
    /// The bridge was turned off.
    Disabled(BridgeState),
}

/// Why the bridge refused to run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BridgeRefusal {
    /// The user has not enabled the scripting bridge.
    #[error("the local scripting bridge is disabled; enable it in Settings first")]
    Disabled,
    /// A different OS user enabled the bridge.
    #[error("the local scripting bridge belongs to a different signed-in user")]
    WrongUser,
}

/// Identity of the OS user running this process.
///
/// On Unix this is the effective uid, which cannot be spoofed by an
/// environment variable the way `$USER` can. On Windows it is the account
/// name, which is the identity the OS itself uses for the user profile.
#[must_use]
#[allow(
    unsafe_code,
    reason = "geteuid is the only way to read the effective uid; it takes no \
              arguments, touches no memory and cannot fail"
)]
pub fn current_user_key() -> String {
    #[cfg(unix)]
    {
        // SAFETY: `geteuid` takes no arguments, touches no memory and cannot
        // fail. It is unsafe only because it is an extern "C" call.
        let uid = unsafe { libc::geteuid() };
        format!("uid:{uid}")
    }
    #[cfg(not(unix))]
    {
        std::env::var("USERNAME")
            .map_or_else(|_| "unknown".to_string(), |name| format!("user:{name}"))
    }
}

/// Reads and writes the bridge state.
#[derive(Debug, Clone)]
pub struct BridgeStateStore {
    db: Database,
}

impl BridgeStateStore {
    /// Open the store over a database.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Current state.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn load(&self) -> PersistenceResult<BridgeState> {
        let row = sqlx::query(
            "SELECT enabled, owner_user_key, enabled_at FROM automation_bridge_state
             WHERE singleton = 1",
        )
        .fetch_optional(self.db.pool())
        .await?;
        let Some(row) = row else {
            return Ok(BridgeState::disabled());
        };
        Ok(BridgeState {
            enabled: row.try_get::<i64, _>("enabled")? != 0,
            owner_user_key: row.try_get("owner_user_key")?,
            enabled_at_ms: row.try_get("enabled_at")?,
        })
    }

    /// Enable the bridge for the current OS user.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn enable(&self, now_ms: i64) -> PersistenceResult<BridgeState> {
        let owner = current_user_key();
        sqlx::query(
            "UPDATE automation_bridge_state
             SET enabled = 1, owner_user_key = ?1, enabled_at = ?2, updated_at = ?2
             WHERE singleton = 1",
        )
        .bind(&owner)
        .bind(now_ms)
        .execute(self.db.pool())
        .await?;
        self.load().await
    }

    /// Disable the bridge and forget the owner.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn disable(&self, now_ms: i64) -> PersistenceResult<BridgeState> {
        sqlx::query(
            "UPDATE automation_bridge_state
             SET enabled = 0, owner_user_key = NULL, enabled_at = NULL, updated_at = ?1
             WHERE singleton = 1",
        )
        .bind(now_ms)
        .execute(self.db.pool())
        .await?;
        self.load().await
    }

    /// Check that the bridge may run for the current process.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures; a refusal is returned as `Ok(Err(..))`
    /// so a caller distinguishes "database broken" from "not allowed".
    pub async fn authorize(&self) -> PersistenceResult<Result<BridgeState, BridgeRefusal>> {
        let state = self.load().await?;
        if !state.enabled {
            return Ok(Err(BridgeRefusal::Disabled));
        }
        let current = current_user_key();
        if state.owner_user_key.as_deref() != Some(current.as_str()) {
            return Ok(Err(BridgeRefusal::WrongUser));
        }
        Ok(Ok(state))
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

    #[tokio::test]
    async fn a_fresh_library_has_the_bridge_switched_off() {
        let (_dir, db) = open().await;
        let store = BridgeStateStore::new(db.clone());
        let state = store.load().await.unwrap();
        assert!(!state.enabled);
        assert_eq!(state.owner_user_key, None);
        assert_eq!(
            store.authorize().await.unwrap(),
            Err(BridgeRefusal::Disabled)
        );
        db.close().await;
    }

    #[tokio::test]
    async fn enabling_records_the_current_user_and_authorizes_it() {
        let (_dir, db) = open().await;
        let store = BridgeStateStore::new(db.clone());
        let state = store.enable(1_700_000_000_000).await.unwrap();
        assert!(state.enabled);
        assert_eq!(state.owner_user_key, Some(current_user_key()));
        assert_eq!(state.enabled_at_ms, Some(1_700_000_000_000));
        assert!(store.authorize().await.unwrap().is_ok());
        db.close().await;
    }

    #[tokio::test]
    async fn a_different_os_user_is_refused() {
        let (_dir, db) = open().await;
        let store = BridgeStateStore::new(db.clone());
        store.enable(1).await.unwrap();
        sqlx::query("UPDATE automation_bridge_state SET owner_user_key = 'uid:999999'")
            .execute(db.pool())
            .await
            .unwrap();
        assert_eq!(
            store.authorize().await.unwrap(),
            Err(BridgeRefusal::WrongUser)
        );
        db.close().await;
    }

    #[tokio::test]
    async fn disabling_makes_the_bridge_inert_again() {
        let (_dir, db) = open().await;
        let store = BridgeStateStore::new(db.clone());
        store.enable(1).await.unwrap();
        let state = store.disable(2).await.unwrap();
        assert!(!state.enabled);
        assert_eq!(state.owner_user_key, None);
        assert_eq!(
            store.authorize().await.unwrap(),
            Err(BridgeRefusal::Disabled)
        );
        db.close().await;
    }

    #[test]
    fn the_user_key_is_stable_within_a_process_and_not_taken_from_the_environment() {
        let first = current_user_key();
        std::env::set_var("USER", "attacker");
        assert_eq!(current_user_key(), first);
        assert!(!first.contains("attacker"));
    }

    #[tokio::test]
    async fn the_state_row_is_a_singleton() {
        let (_dir, db) = open().await;
        let count: i64 = sqlx::query("SELECT count(*) FROM automation_bridge_state")
            .fetch_one(db.pool())
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, 1);
        let inserted = sqlx::query(
            "INSERT INTO automation_bridge_state (singleton, enabled, updated_at) VALUES (1, 1, 0)",
        )
        .execute(db.pool())
        .await;
        assert!(inserted.is_err(), "a second row must be impossible");
        db.close().await;
    }
}
