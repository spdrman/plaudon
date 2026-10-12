//! SRT and WebVTT captions, emitted only when real times exist (spec 4.6, 8.6A).
//!
//! The eligibility rule is the whole point of this module. A revision whose
//! alignment origin is `none` has **no** times: the engine returned text only.
//! We refuse rather than interpolate, because a fabricated cue time is
//! indistinguishable from a real one once it is in a caption file. A `derived`
//! alignment is allowed and is labelled as derived inside the file, so a
//! downstream reader knows the offsets came from a separate alignment pass.

use std::fmt::Write as _;

use plaudon_domain::transcript::AlignmentOrigin;

use crate::bundle::{RecordingExport, TranscriptFacts};
use crate::error::{CaptionRefusal, ExportError, ExportResult};

/// Caption container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionFormat {
    /// SubRip (`.srt`), comma decimal separator.
    Srt,
    /// WebVTT (`.vtt`), dot decimal separator and a `WEBVTT` header.
    Vtt,
}

impl CaptionFormat {
    /// File extension.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Vtt => "vtt",
        }
    }
}

/// Whether captions may be produced from a revision, and why not when they
/// may not.
///
/// This is the predicate the UI calls before offering the affordance, so the
/// button can be disabled with the real reason instead of failing on click.
pub fn eligibility(transcript: &TranscriptFacts) -> Result<(), CaptionRefusal> {
    if !transcript.alignment().permits_times() {
        return Err(CaptionRefusal::NoAlignment);
    }
    let timed = transcript
        .segments
        .iter()
        .filter(|segment| segment.start_ms.is_some() && segment.end_ms.is_some())
        .count();
    if timed == 0 {
        return Err(CaptionRefusal::NoSegments);
    }
    Ok(())
}

/// Format a millisecond offset as `HH:MM:SS,mmm` (SRT) or `HH:MM:SS.mmm` (VTT).
#[must_use]
pub fn format_cue_time(ms: u64, format: CaptionFormat) -> String {
    let total_seconds = ms / 1000;
    let millis = ms % 1000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    let separator = match format {
        CaptionFormat::Srt => ',',
        CaptionFormat::Vtt => '.',
    };
    format!("{hours:02}:{minutes:02}:{seconds:02}{separator}{millis:03}")
}

/// Parse `HH:MM:SS,mmm` / `HH:MM:SS.mmm` back to milliseconds.
///
/// Used by the round-trip tests and by any consumer verifying a cue list.
#[must_use]
pub fn parse_cue_time(text: &str) -> Option<u64> {
    let (clock, millis) = text.split_once([',', '.'])?;
    let mut parts = clock.split(':');
    let hours: u64 = parts.next()?.parse().ok()?;
    let minutes: u64 = parts.next()?.parse().ok()?;
    let seconds: u64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || minutes > 59 || seconds > 59 || millis.len() != 3 {
        return None;
    }
    let millis: u64 = millis.parse().ok()?;
    Some(((hours * 3600 + minutes * 60 + seconds) * 1000) + millis)
}

/// One caption cue after ordering and overlap trimming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    /// One-based cue number.
    pub index: usize,
    /// Start offset in milliseconds.
    pub start_ms: u64,
    /// End offset in milliseconds.
    pub end_ms: u64,
    /// Rendered cue text, including a speaker prefix when one is known.
    pub text: String,
}

/// Build the cue list for a revision.
///
/// Cues keep the engine's offsets exactly. The only adjustment is that a cue
/// which would start before the previous cue ended has the *previous* cue's
/// end clamped down to this cue's start, so the file is a valid,
/// non-overlapping caption track without any offset being invented or moved
/// forward. A zero-length cue is given a one-millisecond duration, which is
/// the minimum a caption format can express.
pub fn build_cues(export: &RecordingExport, transcript: &TranscriptFacts) -> ExportResult<Vec<Cue>> {
    eligibility(transcript).map_err(ExportError::CaptionsUnavailable)?;

    let mut cues: Vec<Cue> = Vec::new();
    for segment in &transcript.segments {
        let (Some(start), Some(end)) = (segment.start_ms, segment.end_ms) else {
            // A revision with a timed alignment origin is constrained to carry
            // times on every segment; skip defensively rather than invent one.
            continue;
        };
        let speaker = segment
            .speaker_id
            .as_ref()
            .and_then(|id| export.speaker_display(id));
        let text = match speaker {
            Some(display) => format!("<v {display}>{}", segment.text),
            None => segment.text.clone(),
        };
        cues.push(Cue {
            index: cues.len() + 1,
            start_ms: start,
            end_ms: end.max(start.saturating_add(1)),
            text,
        });
    }

    for position in 1..cues.len() {
        let start = cues[position].start_ms;
        let previous_end = cues[position - 1].end_ms;
        if previous_end > start {
            cues[position - 1].end_ms = start.max(cues[position - 1].start_ms);
        }
    }
    // A clamp can make the previous cue zero-length; give it the minimum
    // expressible duration without pushing it past the next cue.
    for position in 0..cues.len() {
        if cues[position].end_ms <= cues[position].start_ms {
            let next_start = cues.get(position + 1).map_or(u64::MAX, |cue| cue.start_ms);
            cues[position].end_ms = cues[position].start_ms.saturating_add(1).min(next_start);
        }
    }
    Ok(cues)
}

/// Render captions in the requested format.
pub fn render(
    export: &RecordingExport,
    transcript: &TranscriptFacts,
    format: CaptionFormat,
) -> ExportResult<String> {
    let cues = build_cues(export, transcript)?;
    let mut out = String::new();
    if format == CaptionFormat::Vtt {
        let _ = writeln!(out, "WEBVTT");
        let _ = writeln!(
            out,
            "NOTE alignment {} (revision {})",
            transcript.alignment_label, transcript.id
        );
        if transcript.alignment() == AlignmentOrigin::Derived {
            let _ = writeln!(
                out,
                "NOTE These timings come from a separate alignment pass and are approximate."
            );
        }
        let _ = writeln!(out);
    }
    for cue in &cues {
        let _ = writeln!(out, "{}", cue.index);
        let _ = writeln!(
            out,
            "{} --> {}",
            format_cue_time(cue.start_ms, format),
            format_cue_time(cue.end_ms, format)
        );
        if format == CaptionFormat::Srt
            && cue.index == 1
            && transcript.alignment() == AlignmentOrigin::Derived
        {
            // SRT has no comment syntax; the label goes in the first cue so a
            // derived timeline is never presented as engine-native.
            let _ = writeln!(out, "[derived (approximate) alignment]");
        }
        for line in cue.text.split('\n') {
            let _ = writeln!(out, "{line}");
        }
        let _ = writeln!(out);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::{AppStamp, ExportNotices, RecordingFacts, SegmentFacts};
    use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
    use plaudon_domain::time::Timestamp;

    fn transcript(origin: &str, segments: Vec<SegmentFacts>) -> TranscriptFacts {
        TranscriptFacts {
            id: TranscriptRevisionId::new(),
            parent_id: None,
            origin: "machine".into(),
            input_sha256: "0".repeat(64),
            engine: "whisper.cpp".into(),
            model_id: "tiny".into(),
            model_version: None,
            locale: None,
            alignment_label: crate::bundle::alignment_label(origin).to_owned(),
            alignment_origin: origin.to_owned(),
            status: "complete".into(),
            normalization_profile: "default".into(),
            chunking_strategy: "none".into(),
            duration_ms: Some(10_000),
            created_at: Timestamp::from_millis(1),
            engine_backend: None,
            engine_params: None,
            annotations: Vec::new(),
            segments,
        }
    }

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

    fn export_with(transcript: TranscriptFacts) -> RecordingExport {
        RecordingExport {
            schema_version: crate::bundle::EXPORT_SCHEMA_VERSION.to_owned(),
            app: AppStamp::default(),
            exported_at: Timestamp::from_millis(5),
            recording: RecordingFacts {
                id: RecordingId::new(),
                title: "Meeting".into(),
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

    #[test]
    fn a_text_only_revision_refuses_captions_instead_of_inventing_times() {
        let transcript = transcript(
            "none",
            vec![segment(0, None, None, "hello"), segment(1, None, None, "world")],
        );
        let export = export_with(transcript);
        let selected = export.selected_transcript().unwrap();
        let error = render(&export, selected, CaptionFormat::Srt).unwrap_err();
        match error {
            ExportError::CaptionsUnavailable(refusal) => {
                assert_eq!(refusal, CaptionRefusal::NoAlignment);
                assert!(refusal.explanation().contains("no real timestamps"));
            }
            other => panic!("expected a caption refusal, got {other:?}"),
        }
    }

    #[test]
    fn native_alignment_emits_exact_offsets() {
        let transcript = transcript(
            "native",
            vec![
                segment(0, Some(0), Some(1_500), "first"),
                segment(1, Some(1_500), Some(3_061), "second"),
            ],
        );
        let export = export_with(transcript);
        let srt = render(&export, export.selected_transcript().unwrap(), CaptionFormat::Srt).unwrap();
        assert!(srt.starts_with("1\n00:00:00,000 --> 00:00:01,500\nfirst\n"));
        assert!(srt.contains("2\n00:00:01,500 --> 00:00:03,061\nsecond\n"));
    }

    #[test]
    fn derived_alignment_is_labelled_in_both_formats() {
        let transcript = transcript("derived", vec![segment(0, Some(10), Some(20), "text")]);
        let export = export_with(transcript);
        let selected = export.selected_transcript().unwrap();
        let vtt = render(&export, selected, CaptionFormat::Vtt).unwrap();
        assert!(vtt.starts_with("WEBVTT\n"));
        assert!(vtt.contains("derived (approximate)"));
        let srt = render(&export, selected, CaptionFormat::Srt).unwrap();
        assert!(srt.contains("[derived (approximate) alignment]"));
    }

    #[test]
    fn overlapping_segments_produce_a_monotonic_non_overlapping_cue_list() {
        let transcript = transcript(
            "native",
            vec![
                segment(0, Some(0), Some(5_000), "a"),
                segment(1, Some(2_000), Some(6_000), "b"),
                segment(2, Some(6_000), Some(6_000), "c"),
            ],
        );
        let export = export_with(transcript);
        let cues = build_cues(&export, export.selected_transcript().unwrap()).unwrap();
        assert_eq!(cues[0].end_ms, 2_000, "the earlier cue is clamped");
        assert_eq!(cues[1].start_ms, 2_000);
        for pair in cues.windows(2) {
            assert!(pair[0].end_ms <= pair[1].start_ms);
            assert!(pair[0].start_ms <= pair[1].start_ms);
        }
        assert!(cues.last().unwrap().end_ms > cues.last().unwrap().start_ms);
    }

    #[test]
    fn cue_times_round_trip_through_their_text_form() {
        for ms in [0u64, 1, 999, 1_000, 59_999, 3_600_000, 86_399_999] {
            for format in [CaptionFormat::Srt, CaptionFormat::Vtt] {
                let text = format_cue_time(ms, format);
                assert_eq!(parse_cue_time(&text), Some(ms), "{text}");
            }
        }
        assert_eq!(parse_cue_time("00:00:01"), None);
        assert_eq!(parse_cue_time("00:60:01,000"), None);
    }

    #[test]
    fn a_speaker_label_is_rendered_as_a_voice_span() {
        let mut transcript = transcript("native", vec![segment(0, Some(0), Some(10), "hi")]);
        transcript.segments[0].speaker_id = Some("sp-1".into());
        let mut export = export_with(transcript);
        export.speakers.push(crate::bundle::SpeakerFacts {
            id: "sp-1".into(),
            label: "Speaker 1".into(),
            name: None,
            provenance: "inferred".into(),
            user_confirmed: false,
            display: "Speaker 1 (inferred cluster)".into(),
        });
        let vtt = render(&export, export.selected_transcript().unwrap(), CaptionFormat::Vtt).unwrap();
        assert!(vtt.contains("<v Speaker 1 (inferred cluster)>hi"));
    }
}
