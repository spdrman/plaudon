//! Deterministic TXT, Markdown and JSON renderings (spec 4.6, 9.2, 22(6)).
//!
//! Three properties are deliberate:
//!
//! * **Verbatim transcript text.** Segment text is copied byte for byte. We
//!   do not escape, reflow or "fix" it, so a rendered export can be compared
//!   with the revision it came from.
//! * **No fabrication.** Missing owners and dates render as the literal value
//!   the extraction recorded (`not_specified`, `unknown`, `unverified`); we
//!   never substitute a plausible value, and we never drop an item because a
//!   field is missing.
//! * **Stable order.** Segments follow `seq_index`; outputs follow their
//!   stored order; evidence follows `(item_path, segment_id)`. Re-exporting an
//!   unchanged recording produces identical bytes.

use std::fmt::Write as _;

use crate::bundle::{OutputFacts, RecordingExport, SegmentFacts, TranscriptFacts};
use crate::error::ExportResult;

/// Prefix the Markdown transcript section uses before verbatim segment text.
pub const MD_SEGMENT_SEPARATOR: &str = " | ";

/// Format a millisecond offset as `HH:MM:SS.mmm`.
#[must_use]
pub fn format_offset(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let millis = ms % 1000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

/// Strip ASCII control characters that would corrupt a rendered document.
///
/// Transcript *text* is never passed through this: spec requires verbatim
/// transcript bytes. It applies to titles, labels and model-produced strings
/// that sit inside document structure.
#[must_use]
pub fn strip_controls(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

fn segment_prefix(
    export: &RecordingExport,
    transcript: &TranscriptFacts,
    segment: &SegmentFacts,
) -> String {
    let mut parts = Vec::new();
    if let Some(start) = segment.start_ms {
        if transcript.alignment().permits_times() {
            parts.push(format!(
                "[{} {}]",
                format_offset(start),
                transcript.alignment_label
            ));
        }
    }
    if let Some(speaker_id) = &segment.speaker_id {
        if let Some(display) = export.speaker_display(speaker_id) {
            parts.push(strip_controls(display));
        }
    }
    parts.join(" ")
}

/// Render the plain-text export.
#[must_use]
pub fn to_text(export: &RecordingExport) -> String {
    let mut out = String::new();
    let recording = &export.recording;
    let _ = writeln!(out, "{}", strip_controls(&recording.title));
    let _ = writeln!(
        out,
        "{}",
        "=".repeat(recording.title.chars().count().clamp(3, 80))
    );
    let _ = writeln!(out, "Recording id: {}", recording.id);
    if let Some(captured) = recording.captured_at {
        let _ = writeln!(out, "Captured: {captured}");
    }
    if let Some(duration) = recording.duration_ms {
        let _ = writeln!(out, "Duration: {}", format_offset(duration));
    }
    if !recording.tags.is_empty() {
        let _ = writeln!(out, "Tags: {}", recording.tags.join(", "));
    }
    for media in &export.media {
        let _ = writeln!(
            out,
            "Audio: {} ({} bytes, sha256 {})",
            media.quality_label, media.byte_length, media.sha256
        );
    }
    for source in &export.sources {
        let _ = writeln!(
            out,
            "Source: {} [{}] {}",
            strip_controls(&source.display_name),
            source.source_kind,
            source.external_id.as_deref().unwrap_or("no external id")
        );
    }
    let _ = writeln!(out, "Exported: {}", export.exported_at);
    let _ = writeln!(out, "\n{}", export.notices.not_a_backup);

    if let Some(annotation) = &recording.annotation {
        let _ = writeln!(out, "\nNotes\n-----\n{}", strip_controls(annotation));
    }

    if let Some(transcript) = export.selected_transcript() {
        let _ = writeln!(out, "\nTranscript\n----------");
        let _ = writeln!(
            out,
            "Revision {} | engine {} | model {} | alignment {} | status {}",
            transcript.id,
            transcript.engine,
            transcript.model_id,
            transcript.alignment_label,
            transcript.status
        );
        let _ = writeln!(out);
        for segment in &transcript.segments {
            let prefix = segment_prefix(export, transcript, segment);
            if prefix.is_empty() {
                let _ = writeln!(out, "{}", segment.text);
            } else {
                let _ = writeln!(out, "{prefix}{MD_SEGMENT_SEPARATOR}{}", segment.text);
            }
        }

        let outputs = export.outputs_for(transcript.id);
        if !outputs.is_empty() {
            let _ = writeln!(out, "\nGenerated outputs\n-----------------");
            let _ = writeln!(out, "{}\n", export.notices.anchors);
            for output in outputs {
                render_output_text(&mut out, output);
            }
        }
    } else {
        let _ = writeln!(
            out,
            "\nTranscript\n----------\nNo transcript revision exists yet."
        );
    }
    out
}

fn render_output_text(out: &mut String, output: &OutputFacts) {
    let _ = writeln!(
        out,
        "## {} ({}, template {}, model {}, status {})",
        output.output_type, output.id, output.template_version, output.model_id, output.status
    );
    let body = serde_json::to_string_pretty(&output.body).unwrap_or_else(|_| "{}".to_owned());
    let _ = writeln!(out, "{}", strip_controls(&body));
    for evidence in &output.evidence {
        let time = evidence
            .start_ms
            .map_or_else(|| "no timestamp".to_owned(), format_offset);
        let _ = writeln!(
            out,
            "  anchor {} -> segment {} [{}] ({}): {}",
            evidence.item_path,
            evidence
                .segment_index
                .map_or_else(|| "unresolved".to_owned(), |i| i.to_string()),
            time,
            evidence.verification_status,
            evidence.quote
        );
    }
    let _ = writeln!(out);
}

/// Render the Markdown export.
#[must_use]
pub fn to_markdown(export: &RecordingExport) -> String {
    let mut out = String::new();
    let recording = &export.recording;
    let _ = writeln!(out, "# {}", strip_controls(&recording.title));
    let _ = writeln!(out);
    let _ = writeln!(out, "| Field | Value |");
    let _ = writeln!(out, "| --- | --- |");
    let _ = writeln!(out, "| Recording id | `{}` |", recording.id);
    if let Some(captured) = recording.captured_at {
        let _ = writeln!(out, "| Captured | {captured} |");
    }
    if let Some(duration) = recording.duration_ms {
        let _ = writeln!(out, "| Duration | {} |", format_offset(duration));
    }
    if !recording.tags.is_empty() {
        let _ = writeln!(out, "| Tags | {} |", recording.tags.join(", "));
    }
    let _ = writeln!(out, "| Exported | {} |", export.exported_at);
    let _ = writeln!(out);

    if !export.media.is_empty() {
        let _ = writeln!(out, "## Audio\n");
        for media in &export.media {
            let _ = writeln!(
                out,
                "- {} — {} bytes — `sha256:{}` — verification: {}",
                media.quality_label, media.byte_length, media.sha256, media.verification_state
            );
        }
        let _ = writeln!(out);
    }

    if !export.sources.is_empty() {
        let _ = writeln!(out, "## Provenance\n");
        for source in &export.sources {
            let _ = writeln!(
                out,
                "- {} (`{}`) — external id: {} — remote state: {}",
                strip_controls(&source.display_name),
                source.source_kind,
                source.external_id.as_deref().unwrap_or("none"),
                source.remote_state
            );
        }
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "> {}\n", export.notices.not_a_backup);

    if let Some(annotation) = &recording.annotation {
        let _ = writeln!(out, "## Notes\n\n{}\n", strip_controls(annotation));
    }

    if let Some(transcript) = export.selected_transcript() {
        let outputs = export.outputs_for(transcript.id);
        if !outputs.is_empty() {
            let _ = writeln!(out, "## Summary and extracted items\n");
            let _ = writeln!(out, "> {}\n", export.notices.anchors);
            for output in outputs {
                render_output_markdown(&mut out, output);
            }
        }

        let _ = writeln!(out, "## Transcript\n");
        let _ = writeln!(
            out,
            "Revision `{}` · engine `{}` · model `{}` · alignment **{}** · status **{}**\n",
            transcript.id,
            transcript.engine,
            transcript.model_id,
            transcript.alignment_label,
            transcript.status
        );
        if !transcript.alignment().permits_times() {
            let _ = writeln!(
                out,
                "_This revision has no timestamps. No seek positions are shown because none exist._\n"
            );
        }
        for segment in &transcript.segments {
            let prefix = segment_prefix(export, transcript, segment);
            if prefix.is_empty() {
                let _ = writeln!(out, "{}", segment.text);
            } else {
                let _ = writeln!(out, "{prefix}{MD_SEGMENT_SEPARATOR}{}", segment.text);
            }
            let _ = writeln!(out);
        }
    } else {
        let _ = writeln!(out, "## Transcript\n\nNo transcript revision exists yet.\n");
    }
    out
}

fn render_output_markdown(out: &mut String, output: &OutputFacts) {
    let _ = writeln!(out, "### {}\n", output.output_type);
    let _ = writeln!(
        out,
        "Template `{}` · model `{}` · prompt `{}` · schema `{}` · status **{}** · from revision `{}`\n",
        output.template_version,
        output.model_id,
        output.prompt_version,
        output.schema_version,
        output.status,
        output.transcript_revision_id
    );
    let body = serde_json::to_string_pretty(&output.body).unwrap_or_else(|_| "{}".to_owned());
    let _ = writeln!(out, "```json\n{}\n```\n", strip_controls(&body));
    if output.evidence.is_empty() {
        let _ = writeln!(out, "_No source anchors were recorded for this output._\n");
        return;
    }
    let _ = writeln!(out, "| Item | Segment | Time | Status | Passage |");
    let _ = writeln!(out, "| --- | --- | --- | --- | --- |");
    for evidence in &output.evidence {
        let time = evidence
            .start_ms
            .map_or_else(|| "unavailable".to_owned(), format_offset);
        let quote = strip_controls(&evidence.quote).replace('|', "\\|");
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} |",
            evidence.item_path,
            evidence
                .segment_index
                .map_or_else(|| "unresolved".to_owned(), |i| i.to_string()),
            time,
            evidence.verification_status,
            quote
        );
    }
    let _ = writeln!(out);
}

/// Render the JSON export document.
pub fn to_json(export: &RecordingExport) -> ExportResult<String> {
    let mut text = serde_json::to_string_pretty(export)?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_render_with_millisecond_precision() {
        assert_eq!(format_offset(0), "00:00:00.000");
        assert_eq!(format_offset(1_000), "00:00:01.000");
        assert_eq!(format_offset(3_661_042), "01:01:01.042");
        assert_eq!(format_offset(86_399_999), "23:59:59.999");
        assert_eq!(format_offset(90_061_123), "25:01:01.123");
    }

    #[test]
    fn control_characters_are_stripped_from_structure_fields_only() {
        assert_eq!(strip_controls("a\u{0}b\u{7}c"), "abc");
        assert_eq!(strip_controls("line\nbreak\tkept"), "line\nbreak\tkept");
    }
}
