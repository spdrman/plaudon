//! Clipboard text: what a copy of a transcript selection actually says.
//!
//! The host owns the clipboard. This module only produces strings, so the
//! rules are testable without a window system and identical in the editor, in
//! export and in a kittest harness.

use crate::caret::{Position, Selection};
use crate::doc::TranscriptDocument;

/// `h:mm:ss` past an hour, `m:ss` below it. Matches the clock the React pane
/// used, so copied passages are unchanged by the migration.
#[must_use]
pub fn format_clock(milliseconds: u64) -> String {
    let total_seconds = milliseconds / 1_000;
    let seconds = total_seconds % 60;
    let minutes = (total_seconds / 60) % 60;
    let hours = total_seconds / 3_600;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// The selected text, segment per line, with no timestamps.
#[must_use]
pub fn selected_text(document: &TranscriptDocument, selection: Selection) -> String {
    join_selection(document, selection, false)
}

/// The selected text as `[m:ss] text`, one line per segment.
///
/// A segment with no timeline — an unaligned revision, or a segment without a
/// start — is copied as plain text. We never invent a seek time (spec 8.6A).
#[must_use]
pub fn format_passage(document: &TranscriptDocument, selection: Selection) -> String {
    join_selection(document, selection, true)
}

fn join_selection(
    document: &TranscriptDocument,
    selection: Selection,
    with_timestamps: bool,
) -> String {
    let (start, end) = selection.ordered(document);
    let (Some(first), Some(last)) = (
        document.position(start.segment),
        document.position(end.segment),
    ) else {
        return String::new();
    };
    let mut lines: Vec<String> = Vec::with_capacity(last - first + 1);
    for position in first..=last {
        let Some(segment) = document.segment_at(position) else {
            continue;
        };
        let from = if position == first { start.offset } else { 0 };
        let to = if position == last {
            end.offset
        } else {
            segment.char_len()
        };
        if to <= from && first != last {
            continue;
        }
        let text = segment.slice(from, to);
        let stamped = match segment.start {
            Some(at) if with_timestamps && document.permits_seek() => {
                format!("[{}] {text}", format_clock(at.get()))
            }
            _ => text,
        };
        lines.push(stamped);
    }
    lines.join("\n")
}

/// Split pasted text into the runs that may be inserted into a segment.
///
/// Segments are never split by the editor, so a multi-line paste cannot
/// create segments. The lines are joined with a single space, which keeps the
/// pasted words in the transcript instead of silently dropping them.
#[must_use]
pub fn sanitize_paste(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_end_matches('\r');
        if index > 0 {
            pending_space = true;
        }
        if trimmed.is_empty() {
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        pending_space = false;
        out.push_str(trimmed);
    }
    out
}

/// The position a caret should take after pasting `text` at `at`.
#[must_use]
pub fn position_after_paste(at: Position, text: &str) -> Position {
    Position::new(at.segment, at.offset + text.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{document_with, timed_document};

    #[test]
    fn the_clock_matches_the_react_format() {
        assert_eq!(format_clock(0), "0:00");
        assert_eq!(format_clock(42_000), "0:42");
        assert_eq!(format_clock(90_000), "1:30");
        assert_eq!(format_clock(3_723_000), "1:02:03");
    }

    #[test]
    fn a_timed_passage_carries_a_timestamp_per_segment() {
        let document = timed_document(&[(0, 1_000, "we keep it local"), (42_000, 44_000, "always")]);
        let ids: Vec<_> = document.segments().iter().map(|s| s.id).collect();
        let selection = Selection::new(
            Position::new(ids[0], 0),
            Position::new(ids[1], 6),
        );
        assert_eq!(
            format_passage(&document, selection),
            "[0:00] we keep it local\n[0:42] always"
        );
    }

    #[test]
    fn an_unaligned_transcript_is_copied_as_plain_text() {
        let document = document_with(&["no timeline here"]);
        let id = document.first_id().expect("segment");
        let selection = Selection::new(Position::new(id, 0), Position::new(id, 16));
        assert_eq!(format_passage(&document, selection), "no timeline here");
    }

    #[test]
    fn a_partial_selection_copies_only_what_is_selected() {
        let document = document_with(&["we will not pay"]);
        let id = document.first_id().expect("segment");
        let selection = Selection::new(Position::new(id, 8), Position::new(id, 15));
        assert_eq!(selected_text(&document, selection), "not pay");
    }

    #[test]
    fn a_multi_line_paste_becomes_one_line() {
        assert_eq!(sanitize_paste("one\r\ntwo\n\nthree"), "one two three");
        assert_eq!(sanitize_paste("single"), "single");
    }
}
