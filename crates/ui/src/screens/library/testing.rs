//! Deterministic fixtures for the library's own tests and for the scale runs.
//!
//! Every value is derived from the row index, so a hundred rows, ten thousand
//! rows and a snapshot of either are byte-identical on every machine. Nothing
//! here reaches a port, a clock or a random number generator — the snapshot
//! policy (`docs/EGUI_TESTING.md` §4.5) requires a frame with no minted
//! identifiers in it.

use std::str::FromStr as _;

use plaudon_app::dto::{
    ImportStatus, OutputKind, RecordingDetail, RecordingMediaInfo, RecordingOutput, RecordingPage,
    RecordingSourceRef, RecordingSummary, RevisionKind, SourceKind, TranscriptRevisionRef,
    TranscriptionStatus,
};
use plaudon_domain::ids::{OutputId, RecordingId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::transcript::AlignmentOrigin;

/// A stable identifier for row `index`.
#[must_use]
pub fn recording_id(index: usize) -> RecordingId {
    RecordingId::from_str(&format!("00000000-0000-4000-8000-{index:012x}"))
        .expect("the fixture identifier template is a valid UUID")
}

/// A stable revision identifier for row `index`.
#[must_use]
pub fn revision_id(index: usize) -> TranscriptRevisionId {
    TranscriptRevisionId::from_str(&format!("00000000-0000-4000-9000-{index:012x}"))
        .expect("the fixture identifier template is a valid UUID")
}

fn digest(index: usize) -> Sha256Digest {
    let mut bytes = [0_u8; 32];
    for (slot, byte) in bytes.iter_mut().zip(index.to_be_bytes()) {
        *slot = byte;
    }
    Sha256Digest::from_bytes(bytes)
}

/// Row `index` of a synthetic library: a recording captured on a distinct day
/// with a distinct duration, source, import state and transcription state, so
/// that sorting and filtering have something to bite on.
#[must_use]
pub fn row_at(index: usize) -> RecordingSummary {
    let day = 1 + (index % 28);
    let month = 1 + (index / 28) % 12;
    let (source_kind, source_label) = match index % 4 {
        0 => (SourceKind::ManualImport, "Manual import"),
        1 => (SourceKind::WatchedFolder, "Watched folder"),
        2 => (SourceKind::PlaudCloud, "PLAUD Cloud"),
        _ => (SourceKind::PlaudDevice, "PLAUD Note"),
    };
    let import_status = match index % 7 {
        0 => ImportStatus::Pending,
        1 => ImportStatus::Importing,
        6 => ImportStatus::Failed,
        _ => ImportStatus::Imported,
    };
    let transcription_status = match index % 6 {
        0 => TranscriptionStatus::None,
        1 => TranscriptionStatus::Queued,
        2 => TranscriptionStatus::Running,
        3 => TranscriptionStatus::Completed,
        4 => TranscriptionStatus::Failed,
        _ => TranscriptionStatus::Stale,
    };
    RecordingSummary {
        recording_id: recording_id(index),
        title: format!("Recording {index:04}"),
        // Every fifth recording arrived without a capture instant, which is
        // the case the "never invent a timestamp" rule exists for.
        recorded_at: (!index.is_multiple_of(5))
            .then(|| format!("2026-{month:02}-{day:02}T09:15:00Z")),
        imported_at: format!("2026-{month:02}-{day:02}T10:30:00Z"),
        // Every third recording has no measured duration.
        duration_ms: (!index.is_multiple_of(3)).then(|| ((index as u64 % 90) + 1) * 61_000),
        source: RecordingSourceRef {
            source_kind,
            label: source_label.to_owned(),
        },
        import_status,
        transcription_status,
        processing_engine: index.is_multiple_of(2).then(|| "whisper.cpp".to_owned()),
        last_error: None,
        tags: match index % 3 {
            0 => vec!["work".to_owned()],
            1 => vec!["work".to_owned(), "weekly".to_owned()],
            _ => Vec::new(),
        },
        byte_length: 1_024 * (index as u64 + 1),
        sha256: digest(index),
    }
}

/// `count` synthetic rows.
#[must_use]
pub fn rows(count: usize) -> Vec<RecordingSummary> {
    (0..count).map(row_at).collect()
}

/// One synthetic row with a given title and a stable identity derived from it.
#[must_use]
pub fn row(title: &str) -> RecordingSummary {
    let index = title.bytes().fold(7_usize, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(byte as usize) % 1_000_000
    });
    let mut row = row_at(index);
    title.clone_into(&mut row.title);
    row
}

/// [`row`], then whatever the caller needs changed.
#[must_use]
pub fn row_with(title: &str, edit: impl FnOnce(&mut RecordingSummary)) -> RecordingSummary {
    let mut row = row(title);
    edit(&mut row);
    row
}

/// A page response over `items`.
#[must_use]
pub fn page(items: Vec<RecordingSummary>, next_cursor: Option<&str>, total: u64) -> RecordingPage {
    RecordingPage {
        items,
        next_cursor: next_cursor.map(str::to_owned),
        total,
    }
}

/// The detail view of one summary, with one machine revision and a summary
/// output derived from it.
#[must_use]
pub fn detail(summary: RecordingSummary) -> RecordingDetail {
    let index = summary.byte_length as usize / 1_024;
    let revision = TranscriptRevisionRef {
        revision_id: revision_id(index),
        kind: RevisionKind::Machine,
        created_at: summary.imported_at.clone(),
        engine_id: Some("whisper.cpp".to_owned()),
        model_id: Some("medium.en".to_owned()),
        alignment_origin: AlignmentOrigin::Native,
        segment_count: 42,
        superseded: false,
    };
    let output = RecordingOutput {
        output_id: OutputId::from_str(&format!("00000000-0000-4000-a000-{index:012x}"))
            .expect("the fixture identifier template is a valid UUID"),
        kind: OutputKind::Summary,
        source_revision_id: revision.revision_id,
        created_at: summary.imported_at.clone(),
        stale: false,
    };
    RecordingDetail {
        media: RecordingMediaInfo {
            observed_codec: "wav".to_owned(),
            sample_rate_hz: Some(16_000),
            channels: Some(1),
            original_file_name: format!("{}.wav", summary.title),
        },
        current_revision_id: Some(revision.revision_id),
        revisions: vec![revision],
        outputs: vec![output],
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_index_always_produces_the_same_row() {
        assert_eq!(row_at(17), row_at(17));
        assert_ne!(row_at(17).recording_id, row_at(18).recording_id);
    }

    #[test]
    fn the_fixture_library_contains_the_honesty_cases() {
        let rows = rows(30);
        assert!(rows.iter().any(|row| row.duration_ms.is_none()));
        assert!(rows.iter().any(|row| row.recorded_at.is_none()));
        assert!(rows
            .iter()
            .any(|row| row.import_status == ImportStatus::Failed));
        assert!(rows
            .iter()
            .any(|row| row.transcription_status == TranscriptionStatus::Stale));
    }
}
