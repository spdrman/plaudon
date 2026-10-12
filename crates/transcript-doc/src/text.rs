//! Per-segment text storage: a piece table over the immutable base-revision
//! text plus an append-only buffer of inserted text.
//!
//! The structure is not merely a performance choice. Each piece records where
//! its characters came from, so the table doubles as the offset map a
//! [`crate::anchors`] remapping needs: a character that still comes from an
//! `Original` piece can be addressed in base-revision coordinates, and a
//! character that does not, cannot. That is exactly the question "is this
//! citation still the text that was cited?".
//!
//! Offsets in this module are **`char` offsets**, matching
//! [`plaudon_domain::anchor::SourceAnchor`], never byte offsets.

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

/// Which buffer a piece reads from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The immutable text of the base revision.
    Original,
    /// Text typed or pasted during this editing session.
    Added,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Piece {
    source: Source,
    byte_start: usize,
    byte_len: usize,
    /// `char` offset of this piece inside its source buffer. For an
    /// `Original` piece this is a base-revision offset, which is what anchor
    /// remapping is expressed in.
    char_start: usize,
    char_len: usize,
}

/// The text of one segment.
///
/// Cloning is cheap in the common case: the base text is shared through an
/// [`Arc`], and an unedited segment holds exactly one piece.
#[derive(Clone, PartialEq, Eq)]
pub struct SegmentText {
    original: Arc<str>,
    original_char_len: usize,
    added: String,
    pieces: Vec<Piece>,
    char_len: usize,
}

impl fmt::Debug for SegmentText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SegmentText")
            .field("text", &self.text())
            .field("edited", &self.is_edited())
            .field("original", &self.original)
            .field("pieces", &self.pieces)
            .field("char_len", &self.char_len)
            .finish_non_exhaustive()
    }
}

/// How a base-revision span survived the edits applied so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanMapping {
    /// Every character of the span is still present, contiguous and
    /// unmodified; the span now occupies `[start, end)` in current
    /// coordinates.
    Mapped {
        /// Inclusive start in current `char` coordinates.
        start: usize,
        /// Exclusive end in current `char` coordinates.
        end: usize,
    },
    /// Some character of the span was deleted, or text was inserted inside
    /// it. The citation no longer quotes what it quoted.
    Broken,
}

impl SegmentText {
    /// Build a table over the base-revision text of a segment.
    #[must_use]
    pub fn new(original: impl Into<Arc<str>>) -> Self {
        let original: Arc<str> = original.into();
        let char_len = original.chars().count();
        let pieces = if original.is_empty() {
            Vec::new()
        } else {
            vec![Piece {
                source: Source::Original,
                byte_start: 0,
                byte_len: original.len(),
                char_start: 0,
                char_len,
            }]
        };
        Self {
            original,
            original_char_len: char_len,
            added: String::new(),
            pieces,
            char_len,
        }
    }

    /// The base-revision text this table started from.
    #[must_use]
    pub fn original(&self) -> &str {
        &self.original
    }

    /// Length in `char`s.
    #[must_use]
    pub const fn char_len(&self) -> usize {
        self.char_len
    }

    /// Whether the text is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.char_len == 0
    }

    /// Whether the current text differs from the base-revision text.
    ///
    /// This is a structural test, not a string comparison: text that was
    /// deleted and retyped identically counts as edited, because the anchor
    /// evidence for it was destroyed in between.
    #[must_use]
    pub fn is_edited(&self) -> bool {
        match self.pieces.as_slice() {
            [] => self.original_char_len != 0,
            [only] => {
                only.source != Source::Original
                    || only.char_start != 0
                    || only.char_len != self.original_char_len
            }
            _ => true,
        }
    }

    /// The current text.
    ///
    /// Borrowed for an unedited or single-piece segment — the overwhelmingly
    /// common case — and materialised only for a segment that has actually
    /// been edited.
    #[must_use]
    pub fn text(&self) -> Cow<'_, str> {
        match self.pieces.as_slice() {
            [] => Cow::Borrowed(""),
            [only] => Cow::Borrowed(self.piece_str(only)),
            many => {
                let mut out = String::with_capacity(many.iter().map(|p| p.byte_len).sum());
                for piece in many {
                    out.push_str(self.piece_str(piece));
                }
                Cow::Owned(out)
            }
        }
    }

    /// The pieces of the current text, in order, each with the base-revision
    /// `char` offset of its first character when it is still original text.
    ///
    /// Search scans these slices directly, so an unedited transcript is
    /// searched without allocating anything.
    pub fn runs(&self) -> impl Iterator<Item = TextRun<'_>> + '_ {
        let mut char_offset = 0usize;
        self.pieces.iter().map(move |piece| {
            let start = char_offset;
            char_offset += piece.char_len;
            TextRun {
                text: self.piece_str(piece),
                char_start: start,
                original_char_start: match piece.source {
                    Source::Original => Some(piece.char_start),
                    Source::Added => None,
                },
            }
        })
    }

    /// The substring `[start, end)` in `char` offsets, clamped to the text.
    #[must_use]
    pub fn slice(&self, start: usize, end: usize) -> String {
        let start = start.min(self.char_len);
        let end = end.min(self.char_len).max(start);
        self.text()
            .chars()
            .skip(start)
            .take(end - start)
            .collect::<String>()
    }

    /// Insert `text` at `at` (a `char` offset, clamped to the end).
    pub fn insert(&mut self, at: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        let at = at.min(self.char_len);
        let byte_start = self.added.len();
        self.added.push_str(text);
        let inserted = Piece {
            source: Source::Added,
            byte_start,
            byte_len: text.len(),
            char_start: self.added[..byte_start].chars().count(),
            char_len: text.chars().count(),
        };

        let (index, offset) = self.locate(at);
        if offset == 0 {
            self.pieces.insert(index, inserted);
        } else if index < self.pieces.len() {
            let (left, right) = self.split_piece(self.pieces[index], offset);
            self.pieces[index] = left;
            self.pieces.insert(index + 1, inserted);
            self.pieces.insert(index + 2, right);
        } else {
            self.pieces.push(inserted);
        }
        self.char_len += inserted.char_len;
    }

    /// Remove `[start, end)` in `char` offsets and return the removed text.
    pub fn remove(&mut self, start: usize, end: usize) -> String {
        let start = start.min(self.char_len);
        let end = end.min(self.char_len).max(start);
        if start == end {
            return String::new();
        }
        let removed = self.slice(start, end);

        // Split at both boundaries, then drop the pieces in between.
        self.split_at(end);
        self.split_at(start);
        let (from, _) = self.locate(start);
        let mut dropped = 0usize;
        let mut cut = from;
        while cut < self.pieces.len() && dropped < end - start {
            dropped += self.pieces[cut].char_len;
            cut += 1;
        }
        self.pieces.drain(from..cut);
        self.char_len -= end - start;
        removed
    }

    /// Where a base-revision `char` offset sits in the current text, if the
    /// character at that offset is still present and unmodified.
    #[must_use]
    pub fn map_original_offset(&self, original_offset: usize) -> Option<usize> {
        let mut current = 0usize;
        for piece in &self.pieces {
            if piece.source == Source::Original {
                let piece_end = piece.char_start + piece.char_len;
                if (piece.char_start..piece_end).contains(&original_offset) {
                    return Some(current + (original_offset - piece.char_start));
                }
            }
            current += piece.char_len;
        }
        None
    }

    /// Where a base-revision span sits in the current text.
    ///
    /// `Mapped` is returned only when the whole span is still contiguous
    /// original text: deleting one character of it, or typing inside it,
    /// yields `Broken`, because the quoted evidence no longer exists.
    #[must_use]
    pub fn map_original_span(&self, start: usize, end: usize) -> SpanMapping {
        if end <= start {
            return SpanMapping::Broken;
        }
        let mut current = 0usize;
        let mut entry: Option<(usize, usize)> = None; // (piece index, current offset)
        for (index, piece) in self.pieces.iter().enumerate() {
            if piece.source == Source::Original
                && (piece.char_start..piece.char_start + piece.char_len).contains(&start)
            {
                entry = Some((index, current));
                break;
            }
            current += piece.char_len;
        }
        let Some((mut index, piece_current)) = entry else {
            return SpanMapping::Broken;
        };
        let mapped_start = piece_current + (start - self.pieces[index].char_start);

        // Walk forward; every further character of the span must come from the
        // next piece in order, still original and still contiguous.
        let mut covered = start;
        loop {
            let piece = self.pieces[index];
            if piece.source != Source::Original || piece.char_start > covered {
                return SpanMapping::Broken;
            }
            covered = (piece.char_start + piece.char_len).min(end);
            if covered == end {
                return SpanMapping::Mapped {
                    start: mapped_start,
                    end: mapped_start + (end - start),
                };
            }
            index += 1;
            if index >= self.pieces.len() {
                return SpanMapping::Broken;
            }
        }
    }

    /// Collapse the table so the current text becomes the new base text.
    ///
    /// Called once a revision carrying the edits has been persisted: from
    /// then on the saved text *is* the original, and anchors are expressed
    /// against the saved revision.
    pub fn rebase(&mut self) {
        let text = self.text().into_owned();
        *self = Self::new(text);
    }

    fn piece_str(&self, piece: &Piece) -> &str {
        let buffer: &str = match piece.source {
            Source::Original => &self.original,
            Source::Added => &self.added,
        };
        &buffer[piece.byte_start..piece.byte_start + piece.byte_len]
    }

    /// Piece index and `char` offset inside that piece for a document offset.
    ///
    /// An offset at the very end returns `(pieces.len(), 0)`.
    fn locate(&self, at: usize) -> (usize, usize) {
        let mut remaining = at;
        for (index, piece) in self.pieces.iter().enumerate() {
            if remaining < piece.char_len {
                return (index, remaining);
            }
            remaining -= piece.char_len;
        }
        (self.pieces.len(), remaining)
    }

    fn split_piece(&self, piece: Piece, offset: usize) -> (Piece, Piece) {
        let text = self.piece_str(&piece);
        let byte_offset = text
            .char_indices()
            .nth(offset)
            .map_or(text.len(), |(index, _)| index);
        (
            Piece {
                source: piece.source,
                byte_start: piece.byte_start,
                byte_len: byte_offset,
                char_start: piece.char_start,
                char_len: offset,
            },
            Piece {
                source: piece.source,
                byte_start: piece.byte_start + byte_offset,
                byte_len: piece.byte_len - byte_offset,
                char_start: piece.char_start + offset,
                char_len: piece.char_len - offset,
            },
        )
    }

    fn split_at(&mut self, at: usize) {
        let (index, offset) = self.locate(at);
        if offset == 0 || index >= self.pieces.len() {
            return;
        }
        let (left, right) = self.split_piece(self.pieces[index], offset);
        self.pieces[index] = left;
        self.pieces.insert(index + 1, right);
    }
}

/// One contiguous run of the current text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRun<'a> {
    /// The run's text.
    pub text: &'a str,
    /// `char` offset of the run inside the current segment text.
    pub char_start: usize,
    /// `char` offset of the run inside the base-revision text, when the run is
    /// still original text.
    pub original_char_start: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_table_borrows_the_original_text() {
        let text = SegmentText::new("we will not pay");
        assert_eq!(text.text(), "we will not pay");
        assert_eq!(text.char_len(), 15);
        assert!(!text.is_edited());
        assert!(matches!(text.text(), Cow::Borrowed(_)));
    }

    #[test]
    fn insert_and_remove_compose() {
        let mut text = SegmentText::new("we will pay");
        text.insert(8, "not ");
        assert_eq!(text.text(), "we will not pay");
        assert!(text.is_edited());
        let removed = text.remove(8, 12);
        assert_eq!(removed, "not ");
        assert_eq!(text.text(), "we will pay");
    }

    #[test]
    fn offsets_are_char_offsets_not_byte_offsets() {
        let mut text = SegmentText::new("naïve café");
        assert_eq!(text.char_len(), 10);
        text.insert(10, "!");
        assert_eq!(text.text(), "naïve café!");
        assert_eq!(text.slice(6, 10), "café");
    }

    #[test]
    fn an_untouched_span_maps_through_an_edit_elsewhere() {
        let mut text = SegmentText::new("we will not pay the invoice");
        text.insert(0, "Frankly, ");
        assert_eq!(
            text.map_original_span(0, 15),
            SpanMapping::Mapped { start: 9, end: 24 }
        );
    }

    #[test]
    fn an_edited_span_breaks() {
        let mut text = SegmentText::new("we will not pay");
        text.remove(8, 12);
        assert_eq!(text.map_original_span(0, 15), SpanMapping::Broken);
        assert_eq!(text.map_original_offset(9), None);
    }

    #[test]
    fn typing_inside_a_span_breaks_it() {
        let mut text = SegmentText::new("we will not pay");
        text.insert(10, "X");
        assert_eq!(text.map_original_span(8, 11), SpanMapping::Broken);
        assert_eq!(
            text.map_original_span(0, 7),
            SpanMapping::Mapped { start: 0, end: 7 }
        );
    }

    #[test]
    fn rebasing_makes_the_edited_text_the_new_original() {
        let mut text = SegmentText::new("we will pay");
        text.insert(8, "not ");
        text.rebase();
        assert!(!text.is_edited());
        assert_eq!(text.original(), "we will not pay");
        assert_eq!(
            text.map_original_span(8, 11),
            SpanMapping::Mapped { start: 8, end: 11 }
        );
    }

    #[test]
    fn runs_expose_original_coordinates() {
        let mut text = SegmentText::new("abcdef");
        text.insert(3, "XY");
        let runs: Vec<_> = text.runs().collect();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].text, "abc");
        assert_eq!(runs[0].original_char_start, Some(0));
        assert_eq!(runs[1].text, "XY");
        assert_eq!(runs[1].original_char_start, None);
        assert_eq!(runs[2].text, "def");
        assert_eq!(runs[2].original_char_start, Some(3));
        assert_eq!(runs[2].char_start, 5);
    }

    #[test]
    fn removing_across_several_pieces_keeps_the_rest() {
        let mut text = SegmentText::new("hello world");
        text.insert(5, ",");
        text.insert(0, ">> ");
        assert_eq!(text.text(), ">> hello, world");
        assert_eq!(text.remove(3, 9), "hello,");
        assert_eq!(text.text(), ">>  world");
        assert_eq!(text.char_len(), 9);
    }
}
