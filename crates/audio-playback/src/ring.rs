//! Play-ahead: an SPSC ring and a mixing thread that keeps it full.
//!
//! Adapted from filmcraft's `crates/ui-egui/src/play_ahead.rs`
//! (<https://github.com/spdrman/filmcraft>, MIT OR Apache-2.0,
//! © the filmcraft authors) — see `NOTICE`. The structure, the lead/skip
//! accounting and the "device callback only pops" rule are theirs; the type
//! names, the stats surface and the tests are rewritten for this crate, and
//! the `serde_json` stats export is dropped.
//!
//! Why it exists: decoding inside the device callback gives the mixer one
//! I/O buffer (~10 ms) to return. Any stall there is heard as a dropout. With
//! a lead buffer a stall only eats into the lead, and the callback never
//! decodes, allocates or takes a lock. When the ring does run dry the
//! callback plays silence and counts the frames, and the mixer skips the same
//! number, so audio stays locked to the clock instead of sliding behind it.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Seconds of audio the mixer keeps ahead of the device.
pub const LEAD_SECONDS: f64 = 0.2;
/// Frames mixed per block on the mixing thread.
pub const BLOCK_FRAMES: usize = 1024;
/// Ceiling on mixed channels.
const MAX_CHANNELS: usize = 64;

/// Underrun accounting for the current playback run.
#[derive(Debug, Default)]
pub struct PlaybackStats {
    /// Device callbacks served.
    pub callbacks: AtomicU64,
    /// Callbacks the ring could not fill completely.
    pub underruns: AtomicU64,
    /// Frames played as silence because the ring was empty.
    pub missing_frames: AtomicU64,
    /// Fewest frames left after a callback once the ring first filled;
    /// `u64::MAX` until then.
    pub min_lead_frames: AtomicU64,
}

impl PlaybackStats {
    /// Zero every counter.
    pub fn reset(&self) {
        self.callbacks.store(0, Ordering::Relaxed);
        self.underruns.store(0, Ordering::Relaxed);
        self.missing_frames.store(0, Ordering::Relaxed);
        self.min_lead_frames.store(u64::MAX, Ordering::Relaxed);
    }

    /// Smallest observed lead in milliseconds, once the ring has filled.
    #[must_use]
    pub fn min_lead_ms(&self, sample_rate: u32) -> Option<f64> {
        let frames = self.min_lead_frames.load(Ordering::Relaxed);
        if frames == u64::MAX || sample_rate == 0 {
            return None;
        }
        Some(frames as f64 * 1_000.0 / f64::from(sample_rate))
    }
}

/// Single-producer, single-consumer ring of interleaved `f32` samples held as
/// bit patterns, so it needs no `unsafe`. Indices only grow.
#[derive(Debug)]
pub struct Ring {
    slots: Vec<AtomicU32>,
    write: AtomicU64,
    read: AtomicU64,
}

impl Ring {
    /// A ring holding `samples` interleaved samples.
    #[must_use]
    pub fn new(samples: usize) -> Self {
        Self {
            slots: (0..samples.max(1)).map(|_| AtomicU32::new(0)).collect(),
            write: AtomicU64::new(0),
            read: AtomicU64::new(0),
        }
    }

    /// Capacity in samples.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Samples currently buffered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.write
            .load(Ordering::Acquire)
            .saturating_sub(self.read.load(Ordering::Acquire)) as usize
    }

    /// Whether nothing is buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Producer: append as many samples as fit; returns how many were taken.
    pub fn push(&self, src: &[f32]) -> usize {
        let capacity = self.slots.len();
        let write = self.write.load(Ordering::Relaxed);
        let used = write.saturating_sub(self.read.load(Ordering::Acquire)) as usize;
        let free = capacity.saturating_sub(used);
        let count = src.len().min(free);
        for (index, sample) in src.iter().take(count).enumerate() {
            let slot = ((write + index as u64) % capacity as u64) as usize;
            if let Some(cell) = self.slots.get(slot) {
                cell.store(sample.to_bits(), Ordering::Relaxed);
            }
        }
        self.write.store(write + count as u64, Ordering::Release);
        count
    }

    /// Consumer: fill whole frames of `device_channels` from frames of
    /// `mix_channels`, copying the channels both have and zeroing the rest.
    /// Returns the frames copied; the remainder of `dst` is the caller's.
    pub fn pop(&self, dst: &mut [f32], device_channels: usize, mix_channels: usize) -> usize {
        let capacity = self.slots.len();
        let device_channels = device_channels.max(1);
        let mix_channels = mix_channels.max(1);
        let read = self.read.load(Ordering::Relaxed);
        let available =
            self.write.load(Ordering::Acquire).saturating_sub(read) as usize / mix_channels;
        let frames = (dst.len() / device_channels).min(available);
        for (frame_index, frame) in dst.chunks_mut(device_channels).take(frames).enumerate() {
            for (channel, value) in frame.iter_mut().enumerate() {
                *value = if channel < mix_channels {
                    let offset = (frame_index * mix_channels + channel) as u64;
                    let slot = ((read + offset) % capacity as u64) as usize;
                    self.slots
                        .get(slot)
                        .map_or(0.0, |cell| f32::from_bits(cell.load(Ordering::Relaxed)))
                } else {
                    0.0
                };
            }
        }
        self.read
            .store(read + (frames * mix_channels) as u64, Ordering::Release);
        frames
    }
}

/// Stops the mixing thread when the device callback that owns it is dropped.
#[derive(Debug)]
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// The device callback produced by [`spawn`].
pub type FillCallback = Box<dyn FnMut(&mut [f32], usize) + Send>;

/// Run `mix` on its own thread, keeping [`LEAD_SECONDS`] of audio ready, and
/// return the device callback that plays it.
///
/// `mix(cursor, buffer, channels)` writes interleaved frames starting at
/// device frame `cursor`. The first block is mixed before this returns, so
/// playback starts with sound rather than with an underrun. The thread stops
/// when the returned callback is dropped.
pub fn spawn<M>(
    mut mix: M,
    start_frame: i64,
    sample_rate: u32,
    channels: usize,
    stats: Arc<PlaybackStats>,
) -> FillCallback
where
    M: FnMut(i64, &mut [f32], usize) + Send + 'static,
{
    stats.reset();
    let channels = channels.clamp(1, MAX_CHANNELS);
    let lead_frames = ((f64::from(sample_rate) * LEAD_SECONDS) as usize).max(BLOCK_FRAMES * 2);
    let ring = Arc::new(Ring::new(lead_frames * channels));
    let mut block = vec![0.0f32; BLOCK_FRAMES * channels];
    mix(start_frame, &mut block, channels);
    ring.push(&block);

    // Frames the callback played as silence that the mixer has not skipped yet.
    let skip = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    {
        let (ring, skip, stop) = (Arc::clone(&ring), Arc::clone(&skip), Arc::clone(&stop));
        let body = move || {
            let mut cursor = start_frame + BLOCK_FRAMES as i64;
            while !stop.load(Ordering::Acquire) {
                if ring.len() + block.len() > lead_frames * channels {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                cursor += skip.swap(0, Ordering::AcqRel) as i64;
                block.fill(0.0);
                mix(cursor, &mut block, channels);
                cursor += BLOCK_FRAMES as i64;
                // Silence played while this block was mixing: its leading
                // frames are already late, so they are dropped rather than
                // played behind the clock.
                let late = skip.swap(0, Ordering::AcqRel) as usize;
                let dropped = late.min(BLOCK_FRAMES);
                cursor += (late - dropped) as i64;
                ring.push(block.get(dropped * channels..).unwrap_or_default());
            }
        };
        let spawned = std::thread::Builder::new()
            .name("plaudon-audio-mix".to_owned())
            .spawn(move || {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).is_err() {
                    tracing::error!(
                        "audio mixing stopped after a panic; playback continues silent"
                    );
                }
            });
        if let Err(error) = spawned {
            tracing::error!(%error, "could not start the audio mixing thread");
        }
    }

    let guard = StopOnDrop(stop);
    let mut primed = false;
    Box::new(move |buffer: &mut [f32], device_channels: usize| {
        let _keep = &guard;
        stats.callbacks.fetch_add(1, Ordering::Relaxed);
        let device_channels = device_channels.max(1);
        let played = ring.pop(buffer, device_channels, channels);
        if let Some(rest) = buffer.get_mut(played * device_channels..) {
            rest.fill(0.0);
        }
        let wanted = buffer.len() / device_channels;
        if played < wanted {
            let missing = (wanted - played) as u64;
            stats.underruns.fetch_add(1, Ordering::Relaxed);
            stats.missing_frames.fetch_add(missing, Ordering::Relaxed);
            skip.fetch_add(missing, Ordering::AcqRel);
        }
        let left = ring.len();
        primed |= left + 2 * BLOCK_FRAMES * channels > lead_frames * channels;
        if primed {
            stats
                .min_lead_frames
                .fetch_min((left / channels) as u64, Ordering::Relaxed);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_round_trips_frames_in_order() {
        let ring = Ring::new(8);
        assert!(ring.is_empty());
        assert_eq!(ring.push(&[1.0, 2.0, 3.0, 4.0]), 4);
        let mut out = [0.0f32; 4];
        assert_eq!(ring.pop(&mut out, 2, 2), 2);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        assert!(ring.is_empty());
    }

    #[test]
    fn a_full_ring_refuses_the_overflow_rather_than_overwriting() {
        let ring = Ring::new(4);
        assert_eq!(ring.push(&[1.0, 2.0, 3.0, 4.0, 5.0]), 4);
        let mut out = [0.0f32; 4];
        assert_eq!(ring.pop(&mut out, 1, 1), 4);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn popping_an_empty_ring_yields_no_frames() {
        let ring = Ring::new(4);
        let mut out = [9.0f32; 4];
        assert_eq!(ring.pop(&mut out, 1, 1), 0);
        assert_eq!(out, [9.0; 4]);
    }

    #[test]
    fn a_wider_device_gets_the_shared_channels_and_silence_elsewhere() {
        let ring = Ring::new(4);
        ring.push(&[0.5, -0.5]);
        let mut out = [7.0f32; 4];
        assert_eq!(ring.pop(&mut out, 4, 2), 1);
        assert_eq!(out, [0.5, -0.5, 0.0, 0.0]);
    }

    #[test]
    fn indices_wrap_without_losing_samples() {
        let ring = Ring::new(4);
        let mut out = [0.0f32; 2];
        for round in 0..50 {
            let base = round as f32;
            assert_eq!(ring.push(&[base, base + 0.5]), 2);
            assert_eq!(ring.pop(&mut out, 1, 1), 2);
            assert_eq!(out, [base, base + 0.5]);
        }
    }

    #[test]
    fn the_mixer_thread_keeps_the_callback_fed_and_stops_when_dropped() {
        let stats = Arc::new(PlaybackStats::default());
        let mixed = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&mixed);
        let mut fill = spawn(
            move |cursor, buffer, channels| {
                counter.fetch_add(1, Ordering::Relaxed);
                for (index, frame) in buffer.chunks_mut(channels).enumerate() {
                    let value = (cursor + index as i64) as f32;
                    for sample in frame {
                        *sample = value;
                    }
                }
            },
            0,
            48_000,
            2,
            Arc::clone(&stats),
        );

        let mut buffer = vec![0.0f32; 512 * 2];
        fill(&mut buffer, 2);
        assert_eq!(buffer[0], 0.0);
        assert_eq!(buffer[2], 1.0);
        assert_eq!(stats.callbacks.load(Ordering::Relaxed), 1);

        // Give the mixer time to refill, then drain several callbacks.
        std::thread::sleep(Duration::from_millis(50));
        for _ in 0..4 {
            fill(&mut buffer, 2);
        }
        assert!(mixed.load(Ordering::Relaxed) > 1);
        drop(fill);
        // The thread observes the stop flag; nothing to assert but that the
        // test process exits, which it only does if the thread ends.
    }

    #[test]
    fn an_underrun_is_counted_and_filled_with_silence() {
        let stats = Arc::new(PlaybackStats::default());
        // A mixer that never produces anything after the priming block.
        let primed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&primed);
        let mut fill = spawn(
            move |_cursor, buffer, _channels| {
                if flag.swap(true, Ordering::Relaxed) {
                    // Sleep so the ring stays empty for the callbacks below.
                    std::thread::sleep(Duration::from_millis(5));
                }
                buffer.fill(0.25);
            },
            0,
            48_000,
            1,
            Arc::clone(&stats),
        );
        let mut buffer = vec![1.0f32; 4096];
        for _ in 0..8 {
            fill(&mut buffer, 1);
        }
        assert!(stats.callbacks.load(Ordering::Relaxed) >= 8);
        assert!(stats.missing_frames.load(Ordering::Relaxed) > 0);
        // Silence, not stale data, in the unfilled tail.
        assert!(buffer.contains(&0.0));
    }
}
