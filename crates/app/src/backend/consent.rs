//! Durable consent grants over the `consent_grants` table (spec §4.4 #6,
//! §16.3).
//!
//! The table predates the consent model: it has fixed columns for the grant id,
//! disclosure version, engine, model and the two instants, and nowhere to put
//! purpose, content types, routes, scope or the required retention. The
//! complete [`ConsentGrant`] is therefore serialized into the `scope` column
//! (plain TEXT, no constraint), while the columns that already existed keep
//! their meaning so the `inference_attempts.consent_id` foreign key and any SQL
//! reader of those columns stay valid. Rows written by other components with a
//! bare scope label are not grants this module can interpret and are skipped.
//!
//! Revocation is a column (`revoked_at`) and always wins over whatever the
//! serialized grant says, so a revoked grant cannot be resurrected by an
//! upsert.
//!
//! The methods mirror [`plaudon_security::consent::ConsentRepository`] but are
//! `async`: that trait is synchronous and this crate's storage is not.

use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_security::consent::ConsentGrant;
use sqlx::{Row, SqlitePool};

use crate::backend::store::storage_error;
use crate::error::internal;

/// Engine recorded on every grant row.
const GRANT_ENGINE: &str = "openrouter";

/// Reads and writes consent grants.
#[derive(Debug, Clone)]
pub struct ConsentStore {
    pool: SqlitePool,
}

impl ConsentStore {
    /// Wrap the library pool.
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Every grant, revoked ones included.
    pub async fn load_grants(&self) -> Result<Vec<ConsentGrant>, AppError> {
        let rows = sqlx::query(
            "SELECT scope, revoked_at FROM consent_grants ORDER BY approved_at ASC, id ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| storage_error("cannot read consent grants", &err))?;
        let mut grants = Vec::with_capacity(rows.len());
        for row in rows {
            let raw: String = row.get("scope");
            let Ok(mut grant) = serde_json::from_str::<ConsentGrant>(&raw) else {
                continue;
            };
            let revoked_at: Option<i64> = row.get("revoked_at");
            if let Some(millis) = revoked_at {
                grant.revoked_at_unix = Some(millis.div_euclid(1000));
            }
            grants.push(grant);
        }
        Ok(grants)
    }

    /// Insert or replace a grant by id. Never touches `revoked_at`.
    pub async fn upsert_grant(&self, grant: &ConsentGrant) -> Result<(), AppError> {
        let encoded = serde_json::to_string(grant)
            .map_err(|err| internal(format!("cannot serialize a consent grant: {err}")))?;
        let model = grant
            .routes
            .iter()
            .next()
            .map_or_else(String::new, |route| route.model_id.clone());
        sqlx::query(
            "INSERT INTO consent_grants \
             (id, scope, privacy_policy_version, engine, model_or_provider, approved_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(id) DO UPDATE SET scope = excluded.scope, \
                 privacy_policy_version = excluded.privacy_policy_version, \
                 model_or_provider = excluded.model_or_provider",
        )
        .bind(&grant.id)
        .bind(encoded)
        .bind(&grant.disclosure_version)
        .bind(GRANT_ENGINE)
        .bind(model)
        .bind(grant.granted_at_unix.saturating_mul(1000))
        .execute(&self.pool)
        .await
        .map_err(|err| storage_error("cannot write a consent grant", &err))?;
        Ok(())
    }

    /// Record a revocation. Idempotent; the first revocation instant is kept.
    ///
    /// # Errors
    ///
    /// [`AppErrorCode::RecordNotFound`] when no grant has this id.
    pub async fn mark_revoked(&self, grant_id: &str, at_unix: i64) -> Result<(), AppError> {
        let result = sqlx::query(
            "UPDATE consent_grants SET revoked_at = COALESCE(revoked_at, ?2) WHERE id = ?1",
        )
        .bind(grant_id)
        .bind(at_unix.saturating_mul(1000))
        .execute(&self.pool)
        .await
        .map_err(|err| storage_error("cannot revoke a consent grant", &err))?;
        if result.rows_affected() == 0 {
            return Err(AppError::new(
                AppErrorCode::RecordNotFound,
                "no such consent grant",
            ));
        }
        Ok(())
    }
}
