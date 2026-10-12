//! A chunk-windowed decode pipeline, and a deliberately unbounded control.
//!
//! Spec 17.5 requires "bounded memory growth proportional to active chunk
//! window rather than full decoded PCM". [`decode_windowed`] is the shape the
//! product must have: decoded frames live in a bounded ring of chunks and are
//! consumed as they arrive. [`decode_unbounded`] is the shape it must not
//! have: every decoded frame is retained.
//!
//! Both produce the same checksum over the same audio, which is what makes the
//! comparison meaningful — the windowed version is not cheating by doing less
//! work, it is doing the same work with a bounded working set.

use std::collections::VecDeque;

use serde::Serialize;

/// Bytes per sample in the decoded representation (`f32`).
pub const BYTES_PER_SAMPLE: u64 = 4;

/// The active chunk window of a decode pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ChunkWindow {
    /// Frames produced by one decode step.
    pub chunk_frames: usize,
    /// Chunks held live at once.
    pub window_chunks: usize,
    /// Interleaved channels.
    pub channels: usize,
}

impl ChunkWindow {
    /// The window the harness measures against: 32,768 frames per chunk, four
    /// chunks live, mono — 512 KiB of decoded audio in flight.
    pub const DEFAULT: Self = Self {
        chunk_frames: 32_768,
        window_chunks: 4,
        channels: 1,
    };

    /// Bytes of decoded PCM the window may hold at once.
    #[must_use]
    pub const fn window_bytes(&self) -> u64 {
        self.chunk_frames as u64
            * self.window_chunks as u64
            * self.channels as u64
            * BYTES_PER_SAMPLE
    }

    /// Bytes a fully decoded buffer of `total_frames` would occupy.
    #[must_use]
    pub const fn total_pcm_bytes(&self, total_frames: u64) -> u64 {
        total_frames * self.channels as u64 * BYTES_PER_SAMPLE
    }
}

/// Result of one decode run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecodeOutcome {
    /// Frames decoded.
    pub frames: u64,
    /// Checksum over every decoded sample, so the work cannot be elided.
    pub checksum: u64,
    /// Largest absolute sample value seen.
    pub peak_level: f32,
}

/// Fill `chunk` with the synthetic signal for frames starting at `first_frame`.
///
/// The signal is a cheap integer-driven triangle wave: deterministic, free of
/// floating point transcendentals, and different at every offset, so a buggy
/// windowed reader that re-reads the same chunk produces a different checksum.
fn decode_into(chunk: &mut [f32], first_frame: u64, channels: usize) {
    for (offset, sample) in chunk.iter_mut().enumerate() {
        let frame = first_frame + (offset / channels.max(1)) as u64;
        let phase = frame % 2_048;
        #[allow(clippy::cast_precision_loss)]
        let value = (phase as f32 / 1_024.0) - 1.0;
        *sample = value;
    }
}

fn fold(checksum: u64, chunk: &[f32]) -> (u64, f32) {
    let mut accumulator = checksum;
    let mut peak = 0.0f32;
    for sample in chunk {
        accumulator = accumulator
            .rotate_left(7)
            .wrapping_add(u64::from(sample.to_bits()));
        peak = peak.max(sample.abs());
    }
    (accumulator, peak)
}

/// Decode `total_frames` while holding at most `window.window_chunks` chunks.
///
/// This is the bounded shape: the oldest chunk is dropped as soon as the
/// window advances, so peak working set is a function of the window, not of
/// the length of the audio.
#[must_use]
pub fn decode_windowed(total_frames: u64, window: ChunkWindow) -> DecodeOutcome {
    let samples_per_chunk = window.chunk_frames * window.channels.max(1);
    let mut live: VecDeque<Vec<f32>> = VecDeque::with_capacity(window.window_chunks);
    let mut checksum = 0u64;
    let mut peak = 0.0f32;
    let mut decoded = 0u64;

    while decoded < total_frames {
        let frames = window
            .chunk_frames
            .min(usize::try_from(total_frames - decoded).unwrap_or(window.chunk_frames));
        let mut chunk = vec![0.0f32; frames * window.channels.max(1)];
        decode_into(&mut chunk, decoded, window.channels);
        debug_assert!(chunk.len() <= samples_per_chunk);
        live.push_back(chunk);
        if live.len() > window.window_chunks {
            // The consumer has finished with the oldest chunk; its memory is
            // released here, which is the whole point of the window.
            let retired = live.pop_front().expect("the window is not empty");
            let (next, chunk_peak) = fold(checksum, &retired);
            checksum = next;
            peak = peak.max(chunk_peak);
        }
        decoded += frames as u64;
    }
    for remaining in live.drain(..) {
        let (next, chunk_peak) = fold(checksum, &remaining);
        checksum = next;
        peak = peak.max(chunk_peak);
    }

    DecodeOutcome {
        frames: decoded,
        checksum,
        peak_level: peak,
    }
}

/// Decode `total_frames` retaining every chunk.
///
/// This is the control the harness uses to prove it can detect the failure it
/// claims to guard against. It is never a product code path.
#[must_use]
pub fn decode_unbounded(total_frames: u64, window: ChunkWindow) -> DecodeOutcome {
    let mut retained: Vec<Vec<f32>> = Vec::new();
    let mut decoded = 0u64;
    while decoded < total_frames {
        let frames = window
            .chunk_frames
            .min(usize::try_from(total_frames - decoded).unwrap_or(window.chunk_frames));
        let mut chunk = vec![0.0f32; frames * window.channels.max(1)];
        decode_into(&mut chunk, decoded, window.channels);
        retained.push(chunk);
        decoded += frames as u64;
    }
    let mut checksum = 0u64;
    let mut peak = 0.0f32;
    for chunk in &retained {
        let (next, chunk_peak) = fold(checksum, chunk);
        checksum = next;
        peak = peak.max(chunk_peak);
    }
    // Keep the retained PCM alive until after the caller has sampled RSS by
    // returning a value derived from it; the caller drops it afterwards.
    debug_assert_eq!(
        retained.iter().map(Vec::len).sum::<usize>() as u64,
        decoded * window.channels.max(1) as u64
    );
    DecodeOutcome {
        frames: decoded,
        checksum,
        peak_level: peak,
    }
}

/// Frames in `minutes` of audio at the harness sample rate.
#[must_use]
pub const fn frames_for_minutes(minutes: u64, sample_rate_hz: u32) -> u64 {
    minutes * 60 * sample_rate_hz as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SAMPLE_RATE_HZ;

    #[test]
    fn the_window_and_the_control_decode_identical_audio() {
        let window = ChunkWindow {
            chunk_frames: 1_024,
            window_chunks: 3,
            channels: 1,
        };
        let frames = 10_000;
        let windowed = decode_windowed(frames, window);
        let unbounded = decode_unbounded(frames, window);
        assert_eq!(windowed.frames, frames);
        assert_eq!(
            windowed.checksum, unbounded.checksum,
            "the bounded pipeline must do the same work, not less"
        );
        assert!((windowed.peak_level - unbounded.peak_level).abs() < f32::EPSILON);
    }

    #[test]
    fn decoding_is_deterministic_and_position_dependent() {
        let window = ChunkWindow::DEFAULT;
        assert_eq!(
            decode_windowed(50_000, window).checksum,
            decode_windowed(50_000, window).checksum
        );
        assert_ne!(
            decode_windowed(50_000, window).checksum,
            decode_windowed(50_001, window).checksum
        );
    }

    #[test]
    fn window_accounting_matches_the_declared_shape() {
        let window = ChunkWindow::DEFAULT;
        assert_eq!(window.window_bytes(), 32_768 * 4 * 4);
        let hour = frames_for_minutes(60, SAMPLE_RATE_HZ);
        assert_eq!(hour, 57_600_000);
        assert_eq!(window.total_pcm_bytes(hour), 230_400_000);
        assert!(window.total_pcm_bytes(hour) > window.window_bytes() * 1_000);
    }
}
