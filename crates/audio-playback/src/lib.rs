//! The audio player's engine room: integer time, waveform peaks and playback,
//! with no user-interface dependency of any kind.
//!
//! Plaudon's React player was a `<audio>` element and a canvas: the browser
//! decoded, clocked and drew. Replacing it in Rust means owning all three.
//! This crate owns the first two and a half — everything except the pixels —
//! so the egui widgets in `plaudon-ui` stay thin and every rule that matters
//! (where the playhead is, which samples a column covers, when a decode is
//! cancelled) is testable without a window or a sound card.
//!
//! Spec references: §8.1 (canonical PCM), §17.5 (bounded memory), EPIC O
//! task O1.5 (#203).
//!
//! ```text
//! media file ─▶ worker pool ─▶ plaudon_audio decode ─▶ PeakBuilder ─▶ PeakPyramid
//!                   │                                                     │
//!                   └── PeakCache (content hash + decoder version)        │
//!                                                                         ▼
//!  Timeline (integer zoom/scroll) ──────────────────────────────▶ columns for N pixels
//!
//!  PcmSource ─▶ TimeStretcher ─▶ Resampler ─▶ ring buffer ─▶ AudioOut ─▶ device
//!                                                  │
//!                                       PlayerHandle (integer clock, seek requests)
//! ```

pub mod cache;
pub mod engine;
pub mod generate;
pub mod peaks;
pub mod regions;
pub mod ring;
pub mod ruler;
pub mod source;
pub mod stretch;
pub mod sync;
pub mod time;
pub mod timeline;
pub mod worker;

#[cfg(feature = "cpal-output")]
pub mod cpal_out;

pub use cache::{PeakCache, PeakCacheKey};
pub use engine::{FakeAudioOut, FakeDevice, Player, PlayerEngine, Speed, TransportState, Volume};
pub use generate::{build_peaks, BuiltPeaks, PeakBuildError};
pub use peaks::{PeakBucket, PeakBuilder, PeakLevel, PeakParams, PeakPyramid};
pub use regions::{SpeakerRegion, SpeakerRegions};
pub use ruler::{choose_spacing, format_clock, format_position, ticks, Tick, TickSpacing};
pub use source::{MemoryPcmSource, PcmSource};
pub use sync::{PlaybackSnapshot, PlayerCommand, PlayerHandle, PositionSubscriber};
pub use time::{Micros, TimeSpan};
pub use timeline::{Ratio, Scale, Timeline};
pub use worker::{
    PeakCompletion, PeakOutcome, PeakRequest, PeakRequestId, PeakStore, PeakWorkerPool, Waker,
    WorkerConfig,
};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-audio-playback";

/// Version of the inter-module contract exposed by this crate (spec §6.6).
pub const CONTRACT_VERSION: u32 = 1;
