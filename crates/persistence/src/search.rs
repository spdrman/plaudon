//! The cross-recording full-text index over transcript segments (spec 3.1,
//! 9.8, 17.5).
//!
//! The index is an SQLite FTS5 table maintained by triggers declared in
//! migration `0400`, so any crate that writes a transcript segment keeps the
//! index correct without knowing the index exists. This module owns querying
//! it and repairing it after a restore.
//!
//! Search is local by construction: it is a SQL statement against the user's
//! own database. There is no code path here that could reach the network.

use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
use sqlx::Row;

use crate::db::Database;
use crate::error::{PersistenceError, PersistenceResult};

/// One search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// Recording the hit belongs to.
    pub recording_id: RecordingId,
    /// Transcript revision the matching segment belongs to.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Segment row id.
    pub segment_id: String,
    /// Segment position inside the revision.
    pub seq_index: u32,
    /// Segment text, verbatim.
    pub text: String,
    /// Start offset, when the revision carries real times.
    pub start_ms: Option<u64>,
    /// Alignment origin of the matching revision, as stored on the segment.
    pub alignment_origin: String,
    /// Recording title, for display.
    pub recording_title: String,
    /// BM25 rank. Lower is better, as FTS5 reports it.
    pub rank: f64,
}

/// Query options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchPage {
    /// Maximum hits to return.
    pub limit: u32,
    /// Hits to skip.
    pub offset: u32,
}

impl Default for SearchPage {
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
        }
    }
}

/// Reads and repairs the transcript search index.
#[derive(Debug, Clone)]
pub struct TranscriptSearchRepository {
    db: Database,
}

impl TranscriptSearchRepository {
    /// Wrap an open database.
    #[must_use]
    pub const fn new(db: Database) -> Self {
        Self { db }
    }

    /// Run a prepared FTS5 match expression.
    ///
    /// The caller is responsible for building a safe expression; use
    /// `plaudon_app_core::search::to_match_expression`, which strips every
    /// FTS5 operator. Passing raw user input here is a bug, which is why the
    /// application-level entry point is the one exported to the UI.
    ///
    /// Recordings with a `deleted_at` are excluded, so a tombstoned recording
    /// can never appear in results.
    pub async fn search(
        &self,
        match_expression: &str,
        page: SearchPage,
    ) -> PersistenceResult<Vec<SearchHit>> {
        if match_expression.trim().is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT s.recording_id      AS recording_id,
                    s.transcript_revision_id AS revision_id,
                    s.segment_id        AS segment_id,
                    s.seq_index         AS seq_index,
                    seg.text            AS text,
                    seg.start_ms        AS start_ms,
                    seg.alignment_origin AS alignment_origin,
                    r.title             AS title,
                    bm25(transcript_search) AS rank
             FROM transcript_search s
             JOIN transcript_segments seg ON seg.id = s.segment_id
             JOIN recordings r ON r.id = s.recording_id
             WHERE transcript_search MATCH ?1
               AND r.deleted_at IS NULL
             ORDER BY rank, s.recording_id, s.seq_index
             LIMIT ?2 OFFSET ?3",
        )
        .bind(match_expression)
        .bind(i64::from(page.limit))
        .bind(i64::from(page.offset))
        .fetch_all(self.db.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                let recording_id: String = row.try_get("recording_id")?;
                let revision_id: String = row.try_get("revision_id")?;
                Ok(SearchHit {
                    recording_id: recording_id
                        .parse()
                        .map_err(|_| PersistenceError::IntegrityFailed)?,
                    transcript_revision_id: revision_id
                        .parse()
                        .map_err(|_| PersistenceError::IntegrityFailed)?,
                    segment_id: row.try_get("segment_id")?,
                    seq_index: u32::try_from(row.try_get::<i64, _>("seq_index")?).unwrap_or(0),
                    text: row.try_get("text")?,
                    start_ms: row
                        .try_get::<Option<i64>, _>("start_ms")?
                        .and_then(|v| u64::try_from(v).ok()),
                    alignment_origin: row.try_get("alignment_origin")?,
                    recording_title: row.try_get("title")?,
                    rank: row.try_get("rank")?,
                })
            })
            .collect()
    }

    /// Number of indexed rows, for consistency assertions.
    pub async fn indexed_row_count(&self) -> PersistenceResult<i64> {
        let row = sqlx::query("SELECT count(*) FROM transcript_search")
            .fetch_one(self.db.pool())
            .await?;
        Ok(row.get::<i64, _>(0))
    }

    /// Rebuild the index from the authoritative segment rows.
    ///
    /// Used after a portable restore, where rows arrive through a path that
    /// bypasses the triggers, and as a repair for an index damaged by an
    /// interrupted write.
    pub async fn rebuild(&self) -> PersistenceResult<u64> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("DELETE FROM transcript_search")
            .execute(&mut *tx)
            .await?;
        let result = sqlx::query(
            "INSERT INTO transcript_search(text, segment_id, transcript_revision_id, recording_id, seq_index)
             SELECT s.text, s.id, s.transcript_revision_id, r.recording_id, s.seq_index
             FROM transcript_segments s
             JOIN transcript_revisions r ON r.id = s.transcript_revision_id",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(result.rows_affected())
    }
}
