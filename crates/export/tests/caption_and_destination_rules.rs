//! Spec 4.6/16.5: the two places an export decides what it is allowed to emit
//! — the cue list it builds from a revision, and the name and folder it writes
//! into.
//!
//! Both are driven from untrusted material: segment offsets come from an
//! engine, the file name comes from a user-entered title. The tests below feed
//! each the inputs that sit exactly on a rule and assert the artifact, never
//! the implementation.

use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
use plaudon_domain::time::Timestamp;
use plaudon_export::bundle::{
    AppStamp, ExportNotices, RecordingExport, RecordingFacts, SegmentFacts, TranscriptFacts,
};
use plaudon_export::captions::{build_cues, eligibility};
use plaudon_export::destination::{build_file_name, ExportDestination};
use plaudon_export::error::{CaptionRefusal, ExportError};

// ---------------------------------------------------------------------------
// Caption fixtures
// ---------------------------------------------------------------------------

fn segment(index: u32, start: Option<u64>, end: Option<u64>, text: &str) -> SegmentFacts {
    SegmentFacts {
        id: format!("segment-{index}"),
        seq_index: index,
        start_ms: start,
        end_ms: end,
        speaker_id: None,
        text: text.to_owned(),
        confidence: None,
        confidence_scale: None,
        words: Vec::new(),
    }
}

fn transcript(segments: Vec<SegmentFacts>) -> TranscriptFacts {
    TranscriptFacts {
        id: TranscriptRevisionId::new(),
        parent_id: None,
        origin: "machine".to_owned(),
        input_sha256: "0".repeat(64),
        engine: "whisper.cpp".to_owned(),
        model_id: "tiny".to_owned(),
        model_version: None,
        locale: None,
        // A native alignment permits times, so every refusal below is about the
        // segments themselves rather than about the alignment origin.
        alignment_label: plaudon_export::bundle::alignment_label("native").to_owned(),
        alignment_origin: "native".to_owned(),
        status: "complete".to_owned(),
        normalization_profile: "default".to_owned(),
        chunking_strategy: "none".to_owned(),
        duration_ms: Some(10_000),
        created_at: Timestamp::from_millis(1),
        engine_backend: None,
        engine_params: None,
        annotations: Vec::new(),
        segments,
    }
}

fn export_of(transcript: TranscriptFacts) -> RecordingExport {
    RecordingExport {
        schema_version: plaudon_export::bundle::EXPORT_SCHEMA_VERSION.to_owned(),
        app: AppStamp::default(),
        exported_at: Timestamp::from_millis(5),
        recording: RecordingFacts {
            id: RecordingId::new(),
            title: "Meeting".to_owned(),
            captured_at: None,
            duration_ms: None,
            created_at: Timestamp::from_millis(1),
            deleted_at: None,
            tags: Vec::new(),
            annotation: None,
            preferred_media_asset_id: None,
        },
        media: Vec::new(),
        sources: Vec::new(),
        speakers: Vec::new(),
        selected_transcript_id: Some(transcript.id),
        transcripts: vec![transcript],
        outputs: Vec::new(),
        notices: ExportNotices::default(),
    }
}

// ---------------------------------------------------------------------------
// Caption eligibility and cue construction
// ---------------------------------------------------------------------------

#[test]
fn a_timed_alignment_with_no_complete_segment_times_refuses_captions() {
    // The alignment origin says times exist, but no segment carries a usable
    // pair. Half a time is not a cue, so the affordance is refused with the
    // reason the UI shows instead of emitting an empty file.
    let facts = transcript(vec![
        segment(0, None, Some(1_000), "half a time"),
        segment(1, Some(2_000), None, "the other half"),
    ]);

    assert_eq!(eligibility(&facts), Err(CaptionRefusal::NoSegments));

    let export = export_of(facts);
    let error = build_cues(&export, export.selected_transcript().unwrap()).unwrap_err();
    match error {
        ExportError::CaptionsUnavailable(refusal) => {
            assert_eq!(refusal, CaptionRefusal::NoSegments);
            assert!(refusal.explanation().contains("no segments"));
        }
        other => panic!("expected a caption refusal, got {other:?}"),
    }
}

#[test]
fn a_segment_missing_one_of_its_times_is_skipped_rather_than_given_one() {
    let facts = transcript(vec![
        segment(0, Some(0), Some(1_000), "complete"),
        segment(1, Some(1_000), None, "no end"),
        segment(2, None, None, "no times at all"),
        segment(3, Some(2_000), Some(2_500), "complete again"),
    ]);
    assert_eq!(eligibility(&facts), Ok(()));

    let export = export_of(facts);
    let cues = build_cues(&export, export.selected_transcript().unwrap()).unwrap();

    assert_eq!(cues.len(), 2, "only the fully timed segments become cues");
    assert_eq!(cues[0].index, 1);
    assert_eq!(cues[0].text, "complete");
    assert_eq!(cues[1].index, 2);
    assert_eq!(cues[1].text, "complete again");
    assert_eq!((cues[1].start_ms, cues[1].end_ms), (2_000, 2_500));
}

#[test]
fn two_segments_starting_at_the_same_offset_never_produce_an_overlap() {
    // Clamping the first cue down to the second cue's start leaves it
    // zero-length, and there is no room to give it the one-millisecond minimum
    // without pushing it over the cue that follows. It stays zero-length
    // rather than overlapping or inventing an offset.
    let facts = transcript(vec![
        segment(0, Some(1_000), Some(5_000), "first"),
        segment(1, Some(1_000), Some(2_000), "second"),
    ]);

    let export = export_of(facts);
    let cues = build_cues(&export, export.selected_transcript().unwrap()).unwrap();

    assert_eq!(cues.len(), 2);
    assert_eq!((cues[0].start_ms, cues[0].end_ms), (1_000, 1_000));
    assert_eq!((cues[1].start_ms, cues[1].end_ms), (1_000, 2_000));
    assert!(cues[0].end_ms <= cues[1].start_ms, "cues must not overlap");
    assert!(
        cues[0].end_ms >= cues[0].start_ms,
        "no cue may run backwards"
    );
}

// ---------------------------------------------------------------------------
// File names
// ---------------------------------------------------------------------------

#[test]
fn a_title_that_is_nothing_but_an_extension_falls_back_to_a_generic_stem() {
    // `....txt` sanitizes to itself, and dropping the extension the title
    // carried leaves only dots. An export is never named for them.
    assert_eq!(build_file_name("....txt", "", "txt"), "recording.txt");
    assert_eq!(
        build_file_name("....txt", "a1b2", "srt"),
        "recording-a1b2.srt"
    );
}

#[test]
fn a_title_whose_first_eighty_characters_are_punctuation_falls_back_too() {
    // The stem is non-empty, but the 80-character budget leaves nothing but
    // trailing dots, which cannot end a file name on Windows.
    let title = format!("{}a b", ".".repeat(85));

    assert_eq!(build_file_name(&title, "7f3a", "txt"), "recording-7f3a.txt");
    assert_eq!(build_file_name(&title, "", "vtt"), "recording.vtt");
}

// ---------------------------------------------------------------------------
// Atomic writes
// ---------------------------------------------------------------------------

#[test]
fn a_temporary_file_that_disappears_before_the_rename_leaves_no_export() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("exports");
    std::fs::create_dir_all(&folder).unwrap();
    let destination = ExportDestination::new(&folder).unwrap();

    let error = destination
        .write_atomic_with("meeting.txt", |file| {
            use std::io::Write as _;
            file.write_all(b"the whole export")?;
            // Simulate the staging file being removed under us between the
            // write and the rename: the export must fail, not land empty.
            for entry in std::fs::read_dir(&folder)? {
                let entry = entry?;
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".plaudon-export-")
                {
                    std::fs::remove_file(entry.path())?;
                }
            }
            Ok(16)
        })
        .unwrap_err();

    assert!(matches!(error, ExportError::Io(_)), "unexpected {error:?}");
    assert_eq!(
        std::fs::read_dir(&folder).unwrap().count(),
        0,
        "neither the export nor a leftover temporary file may survive"
    );
}

#[cfg(unix)]
#[test]
fn an_export_folder_that_cannot_be_opened_for_reading_still_receives_its_file() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("exports");
    std::fs::create_dir_all(&folder).unwrap();
    let destination = ExportDestination::new(&folder).unwrap();

    // Write and traverse, but not list: the post-rename directory fsync cannot
    // open this folder. It is a durability hint, so the export still succeeds.
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o300)).unwrap();
    let written = destination.write_atomic("meeting.txt", b"the whole export");
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();

    let path = written.unwrap();
    assert_eq!(path, destination.directory().join("meeting.txt"));
    assert_eq!(std::fs::read(&path).unwrap(), b"the whole export");
}
