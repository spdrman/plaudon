//! The memory-bound harness behind the spec 17.5 chunk-window claim.
//!
//! One observation is: decode `n` minutes of audio through a bounded window
//! while sampling this process's resident size at every chunk boundary, and
//! report the high-water growth over the baseline taken immediately before the
//! run. Sampling during the run (rather than reading the process high-water
//! mark afterwards) makes an observation independent of whatever the test
//! binary allocated earlier, so observations can be taken in any order and the
//! deliberately unbounded control cannot blind the ones after it.

use crate::pcm::{
    decode_unbounded_observed, decode_windowed_observed, frames_for_minutes, ChunkWindow,
};
use crate::results::MemoryObservation;
use crate::rss;
use crate::thresholds::MemoryBudget;
use crate::SAMPLE_RATE_HZ;

/// Peak growth a bounded run is allowed.
#[must_use]
pub fn allowed_growth_bytes(budget: MemoryBudget, window: ChunkWindow) -> u64 {
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation
    )]
    let scaled = (window.window_bytes() as f64 * budget.window_multiple) as u64;
    scaled.saturating_add(budget.slack_bytes)
}

/// Decode `minutes` of audio through `window`, reporting peak RSS growth.
///
/// Returns `None` on a host with no resident-size source; the caller must skip
/// with [`rss::unsupported_reason`] rather than publish a number.
#[must_use]
pub fn observe_windowed(minutes: u64, window: ChunkWindow) -> Option<MemoryObservation> {
    let baseline = rss::sample()?.current_bytes;
    let frames = frames_for_minutes(minutes, SAMPLE_RATE_HZ);
    let mut high_water = baseline;
    let outcome = decode_windowed_observed(frames, window, |_| {
        if let Some(reading) = rss::sample() {
            high_water = high_water.max(reading.current_bytes);
        }
    });
    assert_eq!(outcome.frames, frames, "the decoder must consume the audio");
    Some(MemoryObservation {
        audio_minutes: minutes,
        total_pcm_bytes: window.total_pcm_bytes(frames),
        peak_growth_bytes: high_water.saturating_sub(baseline),
    })
}

/// The control: the same decode retaining every frame.
///
/// Used only to show the harness can detect unbounded growth.
#[must_use]
pub fn observe_unbounded(minutes: u64, window: ChunkWindow) -> Option<MemoryObservation> {
    let baseline = rss::sample()?.current_bytes;
    let frames = frames_for_minutes(minutes, SAMPLE_RATE_HZ);
    let mut high_water = baseline;
    let outcome = decode_unbounded_observed(frames, window, |_| {
        if let Some(reading) = rss::sample() {
            high_water = high_water.max(reading.current_bytes);
        }
    });
    assert_eq!(outcome.frames, frames, "the decoder must consume the audio");
    Some(MemoryObservation {
        audio_minutes: minutes,
        total_pcm_bytes: window.total_pcm_bytes(frames),
        peak_growth_bytes: high_water.saturating_sub(baseline),
    })
}

/// Why an observation failed its budget, or `None` when it passed.
#[must_use]
pub fn breach(
    observation: &MemoryObservation,
    allowed_bytes: u64,
    window: ChunkWindow,
) -> Option<String> {
    if observation.peak_growth_bytes <= allowed_bytes {
        return None;
    }
    Some(format!(
        "decoding {} min of audio grew the working set by {} bytes, over the {} byte budget for a \
         {} byte chunk window (a full decode would have held {} bytes). Memory is tracking total \
         PCM, not the window.",
        observation.audio_minutes,
        observation.peak_growth_bytes,
        allowed_bytes,
        window.window_bytes(),
        observation.total_pcm_bytes,
    ))
}
