//! The bridge between the player's clock and the transcript's highlight.
//!
//! `plaudon-transcript-doc` decides which segment is playing from a
//! [`PlaybackPosition`]; `plaudon-audio-playback` publishes a
//! [`PlaybackSnapshot`]. Neither knows about the other, which is why the
//! editor can be tested with a fixed position and the engine with no editor.
//! This is the ten lines that join them.

use plaudon_audio_playback::sync::PlaybackSnapshot;
use plaudon_transcript_doc::playback::PlaybackPosition;

/// One frame's playback snapshot, seen as a transcript position.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotPosition(PlaybackSnapshot);

impl SnapshotPosition {
    /// Wrap the snapshot the player published this frame.
    #[must_use]
    pub const fn new(snapshot: PlaybackSnapshot) -> Self {
        Self(snapshot)
    }
}

impl PlaybackPosition for SnapshotPosition {
    fn position_ms(&self) -> Option<u64> {
        // A negative position is not a time: the clock is monotonic from
        // zero, and a conversion that failed is an absence, never a zero
        // that would highlight the first segment by accident.
        u64::try_from(self.0.position.as_millis()).ok()
    }

    fn is_playing(&self) -> bool {
        self.0.is_playing()
    }
}
