//! Per-job scratch of the resumable speaker-matching pipeline
//! (`VOICE_PROFILES_SPEC` VP-020, VP-100..VP-105; migration 0901).
//!
//! The §4 pipeline is five stages with one checkpoint each, and a checkpoint is
//! only worth writing if the stage's output survives the crash: resuming "at
//! match" means the embed stage's vectors are still there. This module is where
//! they wait.
//!
//! Two rules shape it.
//!
//! **A vector arrives sealed or not at all.** [`ClusterEmbeddingInsert`] takes
//! ciphertext, a nonce and a dimension — exactly like
//! [`crate::voice_profiles::ExemplarInsert`] — so this crate never holds a
//! plaintext embedding and cannot log, export or persist one (VP-002, VP-071).
//!
//! **It is scratch, not a record.** Everything here belongs to one job and is
//! discarded with [`SpeakerMatchScratch::discard`] when that job settles, in
//! whatever way it settles. A cluster embedding never becomes an exemplar by
//! sitting here; promoting one is enrolment, an explicit user action (VP-007).

use plaudon_domain::ids::JobId;
use plaudon_domain::time::Timestamp;
use sqlx::Row;
use uuid::Uuid;

use crate::db::Database;
use crate::error::{PersistenceError, PersistenceResult};

/// One sealed cluster embedding to park until the match stage runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterEmbeddingInsert {
    /// Job that computed it.
    pub job_id: JobId,
    /// Diarization run the cluster belongs to.
    pub diarization_run_id: Uuid,
    /// Cluster within that run.
    pub cluster_key: String,
    /// Clean single-speaker speech behind the vector, in milliseconds.
    pub speech_ms: u64,
    /// Embedding model identity.
    pub model_id: String,
    /// Embedding model version.
    pub model_version: String,
    /// Dimensionality of the sealed vector.
    pub embedding_dim: u32,
    /// XChaCha20-Poly1305 ciphertext of the vector.
    pub embedding_cipher: Vec<u8>,
    /// The 24-byte nonce it was sealed with.
    pub embedding_nonce: Vec<u8>,
}

/// A parked cluster embedding as stored. The vector stays sealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredClusterEmbedding {
    /// Job that computed it.
    pub job_id: JobId,
    /// Diarization run the cluster belongs to.
    pub diarization_run_id: Uuid,
    /// Cluster within that run.
    pub cluster_key: String,
    /// Clean single-speaker speech behind the vector.
    pub speech_ms: u64,
    /// Embedding model identity.
    pub model_id: String,
    /// Embedding model version.
    pub model_version: String,
    /// Dimensionality.
    pub embedding_dim: u32,
    /// Ciphertext.
    pub embedding_cipher: Vec<u8>,
    /// Nonce.
    pub embedding_nonce: Vec<u8>,
    /// When it was parked.
    pub created_at: Timestamp,
}

/// One match report to park until the suggestion stage runs.
///
/// `report_json` is the serialized
/// `plaudon_app_core::voice_profiles::matching::MatchReport`: person ids,
/// scores and bands. That type is serializable exactly because an embedding is
/// not, so nothing biometric can travel through this column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchReportInsert {
    /// Job that produced it.
    pub job_id: JobId,
    /// Diarization run the cluster belongs to.
    pub diarization_run_id: Uuid,
    /// Cluster the report is about.
    pub cluster_key: String,
    /// The serialized report.
    pub report_json: String,
}

/// A parked match report as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMatchReport {
    /// Cluster the report is about.
    pub cluster_key: String,
    /// The serialized report.
    pub report_json: String,
    /// When it was parked.
    pub created_at: Timestamp,
}

/// What discarding a job's scratch destroyed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScratchDiscard {
    /// Sealed cluster embeddings destroyed.
    pub embeddings: u32,
    /// Match reports destroyed.
    pub reports: u32,
}

/// Reads and writes the speaker-matching scratch of one job.
#[derive(Debug, Clone)]
pub struct SpeakerMatchScratch {
    db: Database,
}

impl SpeakerMatchScratch {
    /// Open the repository.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Park the embed stage's output, replacing anything an earlier attempt of
    /// the same job left behind.
    ///
    /// Re-running an uncommitted embed stage is normal — the previous attempt
    /// crashed — so the write is an upsert on `(job, cluster)` rather than a
    /// conflict.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Invalid`] for a malformed nonce, ciphertext or
    /// dimension; otherwise propagates persistence failures.
    pub async fn put_cluster_embeddings(
        &self,
        embeddings: &[ClusterEmbeddingInsert],
        at: Timestamp,
    ) -> PersistenceResult<u32> {
        for embedding in embeddings {
            validate_cluster_embedding(embedding)?;
        }
        let mut tx = self.db.begin_write().await?;
        let mut written = 0u32;
        for embedding in embeddings {
            sqlx::query(
                "INSERT INTO speaker_match_cluster_embeddings
                     (job_id, diarization_run_id, cluster_key, speech_ms, model_id, model_version,
                      embedding_dim, embedding_cipher, embedding_nonce, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT (job_id, cluster_key) DO UPDATE SET
                     diarization_run_id = excluded.diarization_run_id,
                     speech_ms = excluded.speech_ms,
                     model_id = excluded.model_id,
                     model_version = excluded.model_version,
                     embedding_dim = excluded.embedding_dim,
                     embedding_cipher = excluded.embedding_cipher,
                     embedding_nonce = excluded.embedding_nonce,
                     created_at = excluded.created_at",
            )
            .bind(embedding.job_id.to_string())
            .bind(embedding.diarization_run_id.to_string())
            .bind(&embedding.cluster_key)
            .bind(as_i64(embedding.speech_ms))
            .bind(&embedding.model_id)
            .bind(&embedding.model_version)
            .bind(i64::from(embedding.embedding_dim))
            .bind(embedding.embedding_cipher.as_slice())
            .bind(embedding.embedding_nonce.as_slice())
            .bind(at.as_millis())
            .execute(&mut *tx)
            .await?;
            written += 1;
        }
        tx.commit().await?;
        Ok(written)
    }

    /// The sealed cluster embeddings of one job, in cluster order.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn cluster_embeddings(
        &self,
        job_id: JobId,
    ) -> PersistenceResult<Vec<StoredClusterEmbedding>> {
        let rows = sqlx::query(
            "SELECT job_id, diarization_run_id, cluster_key, speech_ms, model_id, model_version,
                    embedding_dim, embedding_cipher, embedding_nonce, created_at
             FROM speaker_match_cluster_embeddings WHERE job_id = ?1 ORDER BY cluster_key",
        )
        .bind(job_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter().map(decode_cluster_embedding).collect()
    }

    /// Park the match stage's output.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures; the schema rejects a non-object
    /// `report_json`.
    pub async fn put_reports(
        &self,
        reports: &[MatchReportInsert],
        at: Timestamp,
    ) -> PersistenceResult<u32> {
        let mut tx = self.db.begin_write().await?;
        let mut written = 0u32;
        for report in reports {
            sqlx::query(
                "INSERT INTO speaker_match_reports
                     (job_id, diarization_run_id, cluster_key, report_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (job_id, cluster_key) DO UPDATE SET
                     diarization_run_id = excluded.diarization_run_id,
                     report_json = excluded.report_json,
                     created_at = excluded.created_at",
            )
            .bind(report.job_id.to_string())
            .bind(report.diarization_run_id.to_string())
            .bind(&report.cluster_key)
            .bind(&report.report_json)
            .bind(at.as_millis())
            .execute(&mut *tx)
            .await?;
            written += 1;
        }
        tx.commit().await?;
        Ok(written)
    }

    /// The match reports of one job, in cluster order.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn reports(&self, job_id: JobId) -> PersistenceResult<Vec<StoredMatchReport>> {
        let rows = sqlx::query(
            "SELECT cluster_key, report_json, created_at
             FROM speaker_match_reports WHERE job_id = ?1 ORDER BY cluster_key",
        )
        .bind(job_id.to_string())
        .fetch_all(self.db.pool())
        .await?;
        rows.iter()
            .map(|row| {
                Ok(StoredMatchReport {
                    cluster_key: row.try_get("cluster_key")?,
                    report_json: row.try_get("report_json")?,
                    created_at: Timestamp::from_millis(row.try_get::<i64, _>("created_at")?),
                })
            })
            .collect()
    }

    /// Destroy one job's scratch, however the job ended.
    ///
    /// Idempotent: a job whose scratch is already gone reports zero, because
    /// cancellation, completion and a retried cleanup all have to be allowed
    /// to run it.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn discard(&self, job_id: JobId) -> PersistenceResult<ScratchDiscard> {
        let mut tx = self.db.begin_write().await?;
        let embeddings =
            sqlx::query("DELETE FROM speaker_match_cluster_embeddings WHERE job_id = ?1")
                .bind(job_id.to_string())
                .execute(&mut *tx)
                .await?
                .rows_affected();
        let reports = sqlx::query("DELETE FROM speaker_match_reports WHERE job_id = ?1")
            .bind(job_id.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(ScratchDiscard {
            embeddings: u32::try_from(embeddings).unwrap_or(u32::MAX),
            reports: u32::try_from(reports).unwrap_or(u32::MAX),
        })
    }
}

fn validate_cluster_embedding(embedding: &ClusterEmbeddingInsert) -> PersistenceResult<()> {
    if embedding.embedding_nonce.len() != 24 {
        return Err(PersistenceError::Invalid(
            "an XChaCha20-Poly1305 nonce is 24 bytes".to_owned(),
        ));
    }
    if embedding.embedding_cipher.len() < 16 {
        return Err(PersistenceError::Invalid(
            "ciphertext is shorter than one authentication tag".to_owned(),
        ));
    }
    if embedding.embedding_dim == 0 {
        return Err(PersistenceError::Invalid(
            "an embedding has at least one dimension".to_owned(),
        ));
    }
    if embedding.cluster_key.is_empty() {
        return Err(PersistenceError::Invalid(
            "a cluster embedding names its cluster".to_owned(),
        ));
    }
    Ok(())
}

fn decode_cluster_embedding(
    row: &sqlx::sqlite::SqliteRow,
) -> PersistenceResult<StoredClusterEmbedding> {
    let job_id: String = row.try_get("job_id")?;
    let run_id: String = row.try_get("diarization_run_id")?;
    let dim: i64 = row.try_get("embedding_dim")?;
    Ok(StoredClusterEmbedding {
        job_id: job_id
            .parse()
            .map_err(|_| PersistenceError::IntegrityFailed)?,
        diarization_run_id: run_id
            .parse()
            .map_err(|_| PersistenceError::IntegrityFailed)?,
        cluster_key: row.try_get("cluster_key")?,
        speech_ms: u64::try_from(row.try_get::<i64, _>("speech_ms")?)
            .map_err(|_| PersistenceError::IntegrityFailed)?,
        model_id: row.try_get("model_id")?,
        model_version: row.try_get("model_version")?,
        embedding_dim: u32::try_from(dim).map_err(|_| PersistenceError::IntegrityFailed)?,
        embedding_cipher: row.try_get("embedding_cipher")?,
        embedding_nonce: row.try_get("embedding_nonce")?,
        created_at: Timestamp::from_millis(row.try_get::<i64, _>("created_at")?),
    })
}

fn as_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
