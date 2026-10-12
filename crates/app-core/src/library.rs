//! The read-side library service shared by the GUI and the scripting bridge
//! (spec 6.3(13), 14.5).
//!
//! Spec 6.3(13): "Native events and optional local scripting invoke the same
//! validated Rust application services as the GUI, not parallel business
//! logic." This module is that service for the three things the CLI needs:
//! listing recordings, resolving a recording's current transcript revision,
//! and reading that revision's segments.
//!
//! Everything here is read-only and local. Tombstoned recordings are excluded
//! at the query, so a deleted recording cannot be listed or exported.

use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_persistence::transcript::{StoredRevision, StoredSegment, TranscriptRepository};
use plaudon_persistence::{Database, PersistenceError, PersistenceResult};
use serde::{Deserialize, Serialize};
use sqlx::Row;

/// One row of the library listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingSummary {
    /// Recording identity.
    pub recording_id: RecordingId,
    /// User-visible title.
    pub title: String,
    /// When the audio was captured, when the source reported it.
    pub captured_at_ms: Option<i64>,
    /// When the recording entered the library.
    pub created_at_ms: i64,
    /// Media duration, when known.
    pub duration_ms: Option<u64>,
    /// Number of transcript revisions.
    pub revision_count: u32,
}

/// A page of the listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LibraryPage {
    /// Maximum rows.
    pub limit: u32,
    /// Rows to skip.
    pub offset: u32,
}

impl Default for LibraryPage {
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
        }
    }
}

/// How the listing is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LibraryOrder {
    /// Newest capture first, falling back to import time.
    RecordedAtDesc,
    /// Oldest capture first.
    RecordedAtAsc,
    /// Alphabetical by title.
    TitleAsc,
}

impl LibraryOrder {
    const fn sql(self) -> &'static str {
        match self {
            Self::RecordedAtDesc => "COALESCE(r.captured_at, r.created_at) DESC, r.id ASC",
            Self::RecordedAtAsc => "COALESCE(r.captured_at, r.created_at) ASC, r.id ASC",
            Self::TitleAsc => "r.title COLLATE NOCASE ASC, r.id ASC",
        }
    }
}

/// One transcript segment as the export surface presents it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportedSegment {
    /// Position in the revision.
    pub seq_index: u32,
    /// Start offset, absent when the revision carries no times.
    pub start_ms: Option<u64>,
    /// End offset, absent when the revision carries no times.
    pub end_ms: Option<u64>,
    /// Segment text, verbatim.
    pub text: String,
    /// Speaker label with its confirmation state, when diarization ran and a
    /// speaker row exists.
    pub speaker: Option<ExportedSpeaker>,
}

/// A speaker as rendered into an export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedSpeaker {
    /// The label or the user-entered name.
    pub label: String,
    /// Whether a human confirmed that this names the speaker.
    ///
    /// Spec 17.6: an inferred cluster label must never be presented as an
    /// identity, so this flag travels with the label into every export.
    pub confirmed: bool,
}

/// A complete transcript export for one recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptExport {
    /// Recording identity.
    pub recording_id: RecordingId,
    /// Recording title.
    pub title: String,
    /// The exact revision exported.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Engine that produced the revision.
    pub engine: String,
    /// Model that produced the revision.
    pub model_id: String,
    /// Where the segment times came from.
    pub alignment_origin: AlignmentOrigin,
    /// Locale, when detected.
    pub locale: Option<String>,
    /// Media duration the engine observed.
    pub duration_ms: Option<u64>,
    /// The segments.
    pub segments: Vec<ExportedSegment>,
}

impl TranscriptExport {
    /// Render the transcript as plain text, one segment per line.
    ///
    /// A time prefix appears only when the revision carries real times
    /// (spec 8.6A), and a speaker prefix carries its confirmation state.
    #[must_use]
    pub fn to_plain_text(&self) -> String {
        let timed = self.alignment_origin.permits_times();
        self.segments
            .iter()
            .map(|segment| {
                let mut line = String::new();
                if timed {
                    if let Some(start) = segment.start_ms {
                        line.push_str(&format!("[{}] ", format_timestamp(start)));
                    }
                }
                if let Some(speaker) = &segment.speaker {
                    let qualifier = if speaker.confirmed {
                        "confirmed"
                    } else {
                        "inferred cluster"
                    };
                    line.push_str(&format!("{} ({qualifier}): ", speaker.label));
                }
                line.push_str(&segment.text);
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// `hh:mm:ss` for a millisecond offset.
#[must_use]
pub fn format_timestamp(ms: u64) -> String {
    let seconds = ms / 1_000;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
}

/// Read-only access to the library.
#[derive(Debug, Clone)]
pub struct LibraryService {
    db: Database,
    transcripts: TranscriptRepository,
}

impl LibraryService {
    /// Open the service over a database.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self {
            transcripts: TranscriptRepository::new(db.clone()),
            db,
        }
    }

    /// List recordings, excluding tombstoned ones.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn list_recordings(
        &self,
        page: LibraryPage,
        order: LibraryOrder,
    ) -> PersistenceResult<Vec<RecordingSummary>> {
        // The only interpolated fragment is a constant chosen by an enum; every
        // user value is bound.
        let sql = format!(
            "SELECT r.id AS id, r.title AS title, r.captured_at AS captured_at,
                    r.created_at AS created_at, r.duration_ms AS duration_ms,
                    (SELECT count(*) FROM transcript_revisions tr WHERE tr.recording_id = r.id)
                        AS revision_count
             FROM recordings r
             WHERE r.deleted_at IS NULL
             ORDER BY {}
             LIMIT ?1 OFFSET ?2",
            order.sql()
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(i64::from(page.limit))
            .bind(i64::from(page.offset))
            .fetch_all(self.db.pool())
            .await?;

        rows.into_iter()
            .map(|row| {
                let id: String = row.try_get("id")?;
                Ok(RecordingSummary {
                    recording_id: id.parse().map_err(|_| PersistenceError::IntegrityFailed)?,
                    title: row.try_get("title")?,
                    captured_at_ms: row.try_get("captured_at")?,
                    created_at_ms: row.try_get("created_at")?,
                    duration_ms: row
                        .try_get::<Option<i64>, _>("duration_ms")?
                        .and_then(|v| u64::try_from(v).ok()),
                    revision_count: u32::try_from(row.try_get::<i64, _>("revision_count")?)
                        .unwrap_or(0),
                })
            })
            .collect()
    }

    /// Whether a recording exists and has not been deleted.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn is_live(&self, recording_id: RecordingId) -> PersistenceResult<bool> {
        let row = sqlx::query("SELECT deleted_at FROM recordings WHERE id = ?1")
            .bind(recording_id.to_string())
            .fetch_optional(self.db.pool())
            .await?;
        Ok(row.is_some_and(|row| row.get::<Option<i64>, _>(0).is_none()))
    }

    /// The newest transcript revision of a recording.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::RecordNotFound`] when the recording has no
    /// transcript yet.
    pub async fn latest_revision(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<StoredRevision> {
        let ids = self.transcripts.revisions_for(recording_id).await?;
        let newest = ids
            .last()
            .copied()
            .ok_or(PersistenceError::RecordNotFound)?;
        self.transcripts.revision(newest).await
    }

    /// Segments of a revision.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn segments(
        &self,
        revision_id: TranscriptRevisionId,
    ) -> PersistenceResult<Vec<StoredSegment>> {
        self.transcripts.segments(revision_id).await
    }

    /// Build the export view of a recording's newest transcript.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::RecordNotFound`] when the recording is
    /// absent, tombstoned or has no transcript.
    pub async fn export_transcript(
        &self,
        recording_id: RecordingId,
    ) -> PersistenceResult<TranscriptExport> {
        if !self.is_live(recording_id).await? {
            return Err(PersistenceError::RecordNotFound);
        }
        let revision = self.latest_revision(recording_id).await?;
        let segments = self.segments(revision.id).await?;
        let speakers = self.speaker_labels(revision.id).await?;

        let title: String = sqlx::query("SELECT title FROM recordings WHERE id = ?1")
            .bind(recording_id.to_string())
            .fetch_one(self.db.pool())
            .await?
            .try_get("title")?;

        Ok(TranscriptExport {
            recording_id,
            title,
            transcript_revision_id: revision.id,
            engine: revision.engine.clone(),
            model_id: revision.model_id.clone(),
            alignment_origin: revision.alignment_origin,
            locale: revision.locale.clone(),
            duration_ms: revision.duration_ms,
            segments: segments
                .into_iter()
                .map(|segment| ExportedSegment {
                    seq_index: segment.seq_index,
                    start_ms: segment.start_ms,
                    end_ms: segment.end_ms,
                    text: segment.text,
                    speaker: speakers
                        .iter()
                        .find(|(seq, _)| *seq == segment.seq_index)
                        .map(|(_, speaker)| speaker.clone()),
                })
                .collect(),
        })
    }

    /// Speaker labels per segment index, where diarization produced any.
    async fn speaker_labels(
        &self,
        revision_id: TranscriptRevisionId,
    ) -> PersistenceResult<Vec<(u32, ExportedSpeaker)>> {
        let rows = sqlx::query(
            "SELECT seg.seq_index AS seq_index,
                    COALESCE(sp.name, sp.label) AS label,
                    sp.user_confirmed AS user_confirmed
             FROM transcript_segments seg
             JOIN speakers sp ON sp.id = seg.speaker_id
             WHERE seg.transcript_revision_id = ?1
             ORDER BY seg.seq_index",
        )
        .bind(revision_id.to_string())
        .fetch_all(self.db.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok((
                    u32::try_from(row.try_get::<i64, _>("seq_index")?).unwrap_or(0),
                    ExportedSpeaker {
                        label: row.try_get("label")?,
                        confirmed: row.try_get::<i64, _>("user_confirmed")? != 0,
                    },
                ))
            })
            .collect()
    }

    /// Timestamp helper used by callers that need a consistent clock.
    #[must_use]
    pub fn now() -> Timestamp {
        Timestamp::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_render_as_hours_minutes_seconds() {
        assert_eq!(format_timestamp(0), "00:00:00");
        assert_eq!(format_timestamp(3_723_000), "01:02:03");
    }

    #[test]
    fn plain_text_omits_times_when_the_revision_has_none() {
        let export = TranscriptExport {
            recording_id: RecordingId::new(),
            title: "Meeting".to_string(),
            transcript_revision_id: TranscriptRevisionId::new(),
            engine: "whisper.cpp".to_string(),
            model_id: "base.en".to_string(),
            alignment_origin: AlignmentOrigin::None,
            locale: None,
            duration_ms: None,
            segments: vec![ExportedSegment {
                seq_index: 0,
                start_ms: None,
                end_ms: None,
                text: "Hello".to_string(),
                speaker: None,
            }],
        };
        assert_eq!(export.to_plain_text(), "Hello");
    }

    #[test]
    fn plain_text_carries_the_speaker_confirmation_state() {
        let export = TranscriptExport {
            recording_id: RecordingId::new(),
            title: "Meeting".to_string(),
            transcript_revision_id: TranscriptRevisionId::new(),
            engine: "whisper.cpp".to_string(),
            model_id: "base.en".to_string(),
            alignment_origin: AlignmentOrigin::Native,
            locale: None,
            duration_ms: None,
            segments: vec![
                ExportedSegment {
                    seq_index: 0,
                    start_ms: Some(0),
                    end_ms: Some(1_000),
                    text: "Hello".to_string(),
                    speaker: Some(ExportedSpeaker {
                        label: "Speaker 1".to_string(),
                        confirmed: false,
                    }),
                },
                ExportedSegment {
                    seq_index: 1,
                    start_ms: Some(1_000),
                    end_ms: Some(2_000),
                    text: "Hi".to_string(),
                    speaker: Some(ExportedSpeaker {
                        label: "Priya".to_string(),
                        confirmed: true,
                    }),
                },
            ],
        };
        let text = export.to_plain_text();
        assert!(text.contains("Speaker 1 (inferred cluster): Hello"));
        assert!(text.contains("Priya (confirmed): Hi"));
        assert!(text.contains("[00:00:00]"));
    }
}
