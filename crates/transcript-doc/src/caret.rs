//! Caret and selection over `(segment, char offset)` coordinates.
//!
//! Offsets are `char` offsets, so a selection can never split a multi-byte
//! encoding, and the same numbers address text here, in
//! [`plaudon_domain::anchor::SourceAnchor`] and in the save command.

use crate::doc::{SegmentId, TranscriptDocument};

/// A caret position: a segment and a `char` offset inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    /// Segment the caret sits in.
    pub segment: SegmentId,
    /// `char` offset inside that segment.
    pub offset: usize,
}

impl Position {
    /// A position.
    #[must_use]
    pub const fn new(segment: SegmentId, offset: usize) -> Self {
        Self { segment, offset }
    }

    /// The start of a segment.
    #[must_use]
    pub const fn start_of(segment: SegmentId) -> Self {
        Self { segment, offset: 0 }
    }
}

/// A selection, as an anchor (where the drag started) and a focus (where the
/// caret is now). A collapsed selection is a plain caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Fixed end of the selection.
    pub anchor: Position,
    /// Moving end; this is where the caret is drawn.
    pub focus: Position,
}

impl Selection {
    /// A collapsed selection at `position`.
    #[must_use]
    pub const fn caret(position: Position) -> Self {
        Self {
            anchor: position,
            focus: position,
        }
    }

    /// A selection between two positions.
    #[must_use]
    pub const fn new(anchor: Position, focus: Position) -> Self {
        Self { anchor, focus }
    }

    /// Whether nothing is selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }

    /// The selection ordered in document order, as `(start, end)`.
    ///
    /// Requires the document to resolve segment order; a selection whose
    /// endpoints are not both in the document is returned unordered.
    #[must_use]
    pub fn ordered(&self, document: &TranscriptDocument) -> (Position, Position) {
        let anchor_position = document.position(self.anchor.segment);
        let focus_position = document.position(self.focus.segment);
        match (anchor_position, focus_position) {
            (Some(a), Some(f)) if (f, self.focus.offset) < (a, self.anchor.offset) => {
                (self.focus, self.anchor)
            }
            _ => (self.anchor, self.focus),
        }
    }

    /// Whether the selection covers more than one segment.
    #[must_use]
    pub fn spans_segments(&self) -> bool {
        self.anchor.segment != self.focus.segment
    }

    /// Collapse to the focus.
    #[must_use]
    pub const fn collapsed(&self) -> Self {
        Self::caret(self.focus)
    }

    /// Clamp both ends to positions that exist in `document`.
    #[must_use]
    pub fn clamped(&self, document: &TranscriptDocument) -> Self {
        Self {
            anchor: clamp_position(document, self.anchor),
            focus: clamp_position(document, self.focus),
        }
    }
}

fn clamp_position(document: &TranscriptDocument, position: Position) -> Position {
    let segment = document
        .segment(position.segment)
        .or_else(|| document.segment_at(0));
    segment.map_or(position, |segment| Position {
        segment: segment.id,
        offset: position.offset.min(segment.char_len()),
    })
}

/// A caret movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// One character left, crossing into the previous segment at a boundary.
    CharLeft,
    /// One character right, crossing into the next segment at a boundary.
    CharRight,
    /// To the start of the current or previous word.
    WordLeft,
    /// To the end of the current or next word.
    WordRight,
    /// To the start of the current segment.
    SegmentStart,
    /// To the end of the current segment.
    SegmentEnd,
    /// To the same offset in the previous segment.
    SegmentUp,
    /// To the same offset in the next segment.
    SegmentDown,
    /// To the start of the transcript.
    DocumentStart,
    /// To the end of the transcript.
    DocumentEnd,
}

/// Apply a motion to a position.
#[must_use]
pub fn move_position(
    document: &TranscriptDocument,
    position: Position,
    motion: Motion,
) -> Position {
    let Some(segment) = document.segment(position.segment) else {
        return position;
    };
    let offset = position.offset.min(segment.char_len());
    match motion {
        Motion::CharLeft => {
            if offset > 0 {
                Position::new(segment.id, offset - 1)
            } else {
                document
                    .previous_id(segment.id)
                    .and_then(|previous| document.segment(previous))
                    .map_or(position, |previous| {
                        Position::new(previous.id, previous.char_len())
                    })
            }
        }
        Motion::CharRight => {
            if offset < segment.char_len() {
                Position::new(segment.id, offset + 1)
            } else {
                document
                    .next_id(segment.id)
                    .map_or(position, Position::start_of)
            }
        }
        Motion::WordLeft => {
            if offset == 0 {
                return move_position(document, position, Motion::CharLeft);
            }
            let text: Vec<char> = segment.text().chars().collect();
            Position::new(segment.id, word_start(&text, offset))
        }
        Motion::WordRight => {
            if offset >= segment.char_len() {
                return move_position(document, position, Motion::CharRight);
            }
            let text: Vec<char> = segment.text().chars().collect();
            Position::new(segment.id, word_end(&text, offset))
        }
        Motion::SegmentStart => Position::start_of(segment.id),
        Motion::SegmentEnd => Position::new(segment.id, segment.char_len()),
        Motion::SegmentUp => document
            .previous_id(segment.id)
            .and_then(|previous| document.segment(previous))
            .map_or(Position::start_of(segment.id), |previous| {
                Position::new(previous.id, offset.min(previous.char_len()))
            }),
        Motion::SegmentDown => document
            .next_id(segment.id)
            .and_then(|next| document.segment(next))
            .map_or(Position::new(segment.id, segment.char_len()), |next| {
                Position::new(next.id, offset.min(next.char_len()))
            }),
        Motion::DocumentStart => document.first_id().map_or(position, Position::start_of),
        Motion::DocumentEnd => document
            .last_id()
            .and_then(|last| document.segment(last))
            .map_or(position, |last| Position::new(last.id, last.char_len())),
    }
}

/// Whether a character belongs to a word, for word-wise motion and whole-word
/// search.
#[must_use]
pub fn is_word_char(value: char) -> bool {
    value.is_alphanumeric() || value == '_' || value == '\''
}

fn word_start(text: &[char], from: usize) -> usize {
    let mut index = from.min(text.len());
    while index > 0 && !is_word_char(text[index - 1]) {
        index -= 1;
    }
    while index > 0 && is_word_char(text[index - 1]) {
        index -= 1;
    }
    index
}

fn word_end(text: &[char], from: usize) -> usize {
    let mut index = from;
    while index < text.len() && !is_word_char(text[index]) {
        index += 1;
    }
    while index < text.len() && is_word_char(text[index]) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::document_with;

    #[test]
    fn horizontal_motion_crosses_segment_boundaries() {
        let document = document_with(&["one", "two"]);
        let ids: Vec<SegmentId> = document.segments().iter().map(|s| s.id).collect();
        let end_of_first = Position::new(ids[0], 3);
        assert_eq!(
            move_position(&document, end_of_first, Motion::CharRight),
            Position::new(ids[1], 0)
        );
        assert_eq!(
            move_position(&document, Position::new(ids[1], 0), Motion::CharLeft),
            end_of_first
        );
    }

    #[test]
    fn word_motion_stops_at_word_boundaries() {
        let document = document_with(&["we will not pay"]);
        let id = document.first_id().expect("segment");
        let start = Position::new(id, 0);
        let after_we = move_position(&document, start, Motion::WordRight);
        assert_eq!(after_we.offset, 2);
        let after_will = move_position(&document, after_we, Motion::WordRight);
        assert_eq!(after_will.offset, 7);
        assert_eq!(
            move_position(&document, after_will, Motion::WordLeft).offset,
            3
        );
    }

    #[test]
    fn a_selection_orders_itself_in_document_order() {
        let document = document_with(&["one", "two"]);
        let ids: Vec<SegmentId> = document.segments().iter().map(|s| s.id).collect();
        let backwards = Selection::new(Position::new(ids[1], 1), Position::new(ids[0], 2));
        let (start, end) = backwards.ordered(&document);
        assert_eq!(start, Position::new(ids[0], 2));
        assert_eq!(end, Position::new(ids[1], 1));
        assert!(backwards.spans_segments());
        assert!(!backwards.is_empty());
    }

    #[test]
    fn a_position_past_the_end_of_a_segment_is_clamped() {
        let document = document_with(&["one"]);
        let id = document.first_id().expect("segment");
        let clamped = Selection::caret(Position::new(id, 99)).clamped(&document);
        assert_eq!(clamped.focus, Position::new(id, 3));
    }
}
