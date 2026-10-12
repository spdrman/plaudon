//! The audio playhead, as the transcript sees it.
//!
//! The editor highlights the segment being spoken. It does not own, decode or
//! drive audio: it reads one number from whatever is playing. Keeping that as
//! a one-method trait in this crate means the player engine and the editor
//! never depend on each other, and a kittest harness can drive the highlight
//! with a plain counter.

use crate::doc::{SegmentId, TranscriptDocument};

/// A source of the current playback position.
pub trait PlaybackPosition {
    /// Position of the playhead in source-time milliseconds, or `None` when
    /// nothing is loaded or playing.
    fn position_ms(&self) -> Option<u64>;

    /// Whether audio is advancing. The editor uses it to decide whether to
    /// follow the playhead with the viewport.
    fn is_playing(&self) -> bool {
        false
    }
}

/// A fixed position, for tests and for a paused player.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FixedPosition {
    /// The position to report.
    pub position_ms: Option<u64>,
    /// Whether to report playing.
    pub playing: bool,
}

impl FixedPosition {
    /// A paused playhead at `position_ms`.
    #[must_use]
    pub const fn paused_at(position_ms: u64) -> Self {
        Self {
            position_ms: Some(position_ms),
            playing: false,
        }
    }

    /// A playing playhead at `position_ms`.
    #[must_use]
    pub const fn playing_at(position_ms: u64) -> Self {
        Self {
            position_ms: Some(position_ms),
            playing: true,
        }
    }
}

impl PlaybackPosition for FixedPosition {
    fn position_ms(&self) -> Option<u64> {
        self.position_ms
    }

    fn is_playing(&self) -> bool {
        self.playing
    }
}

impl<T: PlaybackPosition + ?Sized> PlaybackPosition for &T {
    fn position_ms(&self) -> Option<u64> {
        (*self).position_ms()
    }

    fn is_playing(&self) -> bool {
        (*self).is_playing()
    }
}

/// Which segment the playhead is in, and whether seeking is honest here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackHighlight {
    /// Segment to highlight.
    pub segment: SegmentId,
    /// Whether the highlight came from real engine-reported alignment
    /// (`Native`) rather than a labelled alignment pass (`Derived`).
    pub approximate: bool,
}

/// Resolve the highlight for a playback source.
///
/// `None` when nothing is playing, or when the revision's alignment origin
/// forbids times — in which case there is no honest highlight to show.
#[must_use]
pub fn highlight(
    document: &TranscriptDocument,
    playback: &impl PlaybackPosition,
) -> Option<PlaybackHighlight> {
    let position_ms = playback.position_ms()?;
    let segment = document.segment_at_ms(position_ms)?;
    Some(PlaybackHighlight {
        segment,
        approximate: matches!(
            document.alignment_origin(),
            plaudon_domain::transcript::AlignmentOrigin::Derived
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{document_with, timed_document};

    #[test]
    fn the_highlight_follows_the_playhead() {
        let document = timed_document(&[(0, 1_000, "first"), (1_000, 2_000, "second")]);
        let ids: Vec<_> = document.segments().iter().map(|s| s.id).collect();
        let found = highlight(&document, &FixedPosition::playing_at(1_200)).expect("highlight");
        assert_eq!(found.segment, ids[1]);
        assert!(!found.approximate);
    }

    #[test]
    fn an_unaligned_transcript_has_no_highlight() {
        let document = document_with(&["one", "two"]);
        assert_eq!(highlight(&document, &FixedPosition::playing_at(500)), None);
    }

    #[test]
    fn a_stopped_player_highlights_nothing() {
        let document = timed_document(&[(0, 1_000, "first")]);
        assert_eq!(highlight(&document, &FixedPosition::default()), None);
    }
}
