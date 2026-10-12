//! The playback engine: transport, mixing and the integer clock.
//!
//! The device is behind a trait. The real one is cpal; the one every test in
//! this repository uses is [`FakeAudioOut`], which renders exactly the frames
//! a test asks for and advances its own played-frame counter, so playback is
//! reproducible without audio hardware, without wall-clock sleeps and without
//! a mixing thread.
//!
//! The clock is derived, never accumulated: position is
//! `anchor + (played_frames − anchor_frames) × speed ÷ device_rate`, computed
//! in `i128` from the device's own frame counter. Seeks and speed changes
//! re-anchor. Playing a three-hour recording at 1.25× therefore ends on the
//! exact microsecond the arithmetic says, with no drift to correct.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use plaudon_audio::resample::Resampler;

use crate::ring::{FillCallback, PlaybackStats};
use crate::source::PcmSource;
use crate::stretch::TimeStretcher;
use crate::sync::{CommandInbox, PlaybackSnapshot, PlayerCommand, PlayerHandle, SharedClock};
use crate::time::Micros;

/// Frames produced per mixing pass.
const MIX_BLOCK_FRAMES: usize = 1024;

/// Why playback could not start.
#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    /// No source has been loaded.
    #[error("no audio source is loaded")]
    NoSource,
    /// The output device refused to start.
    #[error("audio output unavailable: {0}")]
    Device(String),
}

/// What the transport is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum TransportState {
    /// Nothing loaded, or stopped and rewound.
    #[default]
    Stopped = 0,
    /// Audio is advancing.
    Playing = 1,
    /// Paused at a position.
    Paused = 2,
    /// Reached the end of the source.
    Ended = 3,
}

impl TransportState {
    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Playing,
            2 => Self::Paused,
            3 => Self::Ended,
            _ => Self::Stopped,
        }
    }
}

/// An exact playback rate.
///
/// The six rates the React player offered are all exact rationals, so the
/// clock stays integer: no `0.1 + 0.2` anywhere near the playhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Speed {
    num: u32,
    den: u32,
}

impl Speed {
    /// 0.5×.
    pub const HALF: Self = Self { num: 1, den: 2 };
    /// 0.75×.
    pub const THREE_QUARTERS: Self = Self { num: 3, den: 4 };
    /// 1×.
    pub const NORMAL: Self = Self { num: 1, den: 1 };
    /// 1.25×.
    pub const ONE_AND_A_QUARTER: Self = Self { num: 5, den: 4 };
    /// 1.5×.
    pub const ONE_AND_A_HALF: Self = Self { num: 3, den: 2 };
    /// 2×.
    pub const DOUBLE: Self = Self { num: 2, den: 1 };

    /// The selectable rates, ascending — the `playbackRate` list the React
    /// player used.
    pub const STEPS: [Self; 6] = [
        Self::HALF,
        Self::THREE_QUARTERS,
        Self::NORMAL,
        Self::ONE_AND_A_QUARTER,
        Self::ONE_AND_A_HALF,
        Self::DOUBLE,
    ];

    /// Build from a rational; a zero part collapses to 1×.
    #[must_use]
    pub const fn from_parts(num: u32, den: u32) -> Self {
        if num == 0 || den == 0 {
            return Self::NORMAL;
        }
        Self { num, den }
    }

    /// Numerator.
    #[must_use]
    pub const fn num(self) -> u32 {
        self.num
    }

    /// Denominator.
    #[must_use]
    pub const fn den(self) -> u32 {
        self.den
    }

    /// The rate as a float, for display and for the resampler.
    #[must_use]
    pub fn as_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// The label the UI shows.
    #[must_use]
    pub fn label(self) -> String {
        let value = self.as_f64();
        if (value - value.round()).abs() < 1e-9 {
            format!("{}×", value.round() as i64)
        } else {
            format!("{value}×")
        }
    }

    /// The next faster selectable rate, or the fastest.
    #[must_use]
    pub fn faster(self) -> Self {
        let index = Self::STEPS
            .iter()
            .position(|step| *step == self)
            .unwrap_or(2);
        Self::STEPS[(index + 1).min(Self::STEPS.len() - 1)]
    }

    /// The next slower selectable rate, or the slowest.
    #[must_use]
    pub fn slower(self) -> Self {
        let index = Self::STEPS
            .iter()
            .position(|step| *step == self)
            .unwrap_or(2);
        Self::STEPS[index.saturating_sub(1)]
    }
}

impl Default for Speed {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// Output gain, in thousandths. Integer so the UI and the mixer agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Volume(u16);

impl Volume {
    /// Silence.
    pub const MUTED: Self = Self(0);
    /// Unity gain.
    pub const FULL: Self = Self(1_000);

    /// Build from thousandths, clamped to unity.
    #[must_use]
    pub const fn from_per_mille(value: u16) -> Self {
        Self(if value > 1_000 { 1_000 } else { value })
    }

    /// Build from a `0.0 ..= 1.0` fraction.
    #[must_use]
    pub fn from_fraction(value: f32) -> Self {
        if !value.is_finite() || value <= 0.0 {
            return Self::MUTED;
        }
        Self::from_per_mille((value.clamp(0.0, 1.0) * 1_000.0).round() as u16)
    }

    /// Thousandths.
    #[must_use]
    pub const fn per_mille(self) -> u16 {
        self.0
    }

    /// Linear gain in `0.0 ..= 1.0`.
    #[must_use]
    pub fn gain(self) -> f32 {
        f32::from(self.0) / 1_000.0
    }
}

impl Default for Volume {
    fn default() -> Self {
        Self::FULL
    }
}

/// An audio output device.
pub trait AudioOut: Send + std::fmt::Debug {
    /// Device sample rate.
    fn sample_rate(&self) -> u32;
    /// Device channel count.
    fn channels(&self) -> usize;
    /// Start the stream, driven by `fill(buffer, channels)`.
    fn start(&mut self, fill: FillCallback) -> Result<(), PlaybackError>;
    /// Stop the stream. The played-frame counter keeps its value.
    fn stop(&mut self);
    /// Frames the device has consumed since the device was created. Counts
    /// across stream restarts, so a seek does not rewind the clock source.
    fn played_frames(&self) -> u64;
    /// Whether a stream is running.
    fn is_running(&self) -> bool;
}

/// The deterministic device used by every test in this repository.
///
/// Nothing happens until [`FakeDevice::render`] is called, and it renders
/// exactly the frames asked for, so a test can state "play 0.5 s" and get
/// precisely 0.5 s of clock movement.
#[derive(Debug, Clone)]
pub struct FakeDevice {
    inner: Arc<FakeInner>,
}

struct FakeInner {
    sample_rate: u32,
    channels: usize,
    played: AtomicU64,
    running: AtomicBool,
    fill: Mutex<Option<FillCallback>>,
    rendered: Mutex<Vec<f32>>,
    capture: AtomicBool,
}

impl std::fmt::Debug for FakeInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeInner")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("played", &self.played)
            .field("running", &self.running)
            .finish_non_exhaustive()
    }
}

impl FakeDevice {
    /// Render `frames` through the installed callback and advance the clock.
    /// Returns the interleaved samples produced.
    pub fn render(&self, frames: usize) -> Vec<f32> {
        let mut buffer = vec![0.0f32; frames * self.inner.channels];
        let mut guard = self.inner.fill.lock().expect("fake device");
        if let Some(fill) = guard.as_mut() {
            fill(&mut buffer, self.inner.channels);
            self.inner
                .played
                .fetch_add(frames as u64, Ordering::Relaxed);
        }
        drop(guard);
        if self.inner.capture.load(Ordering::Relaxed) {
            self.inner
                .rendered
                .lock()
                .expect("fake device")
                .extend_from_slice(&buffer);
        }
        buffer
    }

    /// Render `micros` worth of frames, rounded down.
    pub fn render_micros(&self, micros: Micros) -> Vec<f32> {
        let frames = micros.to_frames(self.inner.sample_rate).max(0) as usize;
        self.render(frames)
    }

    /// Keep every rendered sample, for assertions about the audio itself.
    pub fn capture(&self, enabled: bool) {
        self.inner.capture.store(enabled, Ordering::Relaxed);
    }

    /// Everything rendered while capture was on.
    #[must_use]
    pub fn captured(&self) -> Vec<f32> {
        self.inner.rendered.lock().expect("fake device").clone()
    }

    /// Discard captured samples.
    pub fn clear_captured(&self) {
        self.inner.rendered.lock().expect("fake device").clear();
    }

    /// Frames rendered so far.
    #[must_use]
    pub fn played_frames(&self) -> u64 {
        self.inner.played.load(Ordering::Relaxed)
    }

    /// Whether a stream is installed.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::Relaxed)
    }
}

/// The [`AudioOut`] half of a [`FakeDevice`].
#[derive(Debug)]
pub struct FakeAudioOut {
    inner: Arc<FakeInner>,
}

impl FakeAudioOut {
    /// A fake device at `sample_rate` with `channels` channels.
    #[must_use]
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        Self {
            inner: Arc::new(FakeInner {
                sample_rate: sample_rate.max(1),
                channels: channels.max(1),
                played: AtomicU64::new(0),
                running: AtomicBool::new(false),
                fill: Mutex::new(None),
                rendered: Mutex::new(Vec::new()),
                capture: AtomicBool::new(false),
            }),
        }
    }

    /// A handle a test keeps after the player takes ownership of the device.
    #[must_use]
    pub fn device(&self) -> FakeDevice {
        FakeDevice {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl AudioOut for FakeAudioOut {
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate
    }

    fn channels(&self) -> usize {
        self.inner.channels
    }

    fn start(&mut self, fill: FillCallback) -> Result<(), PlaybackError> {
        *self.inner.fill.lock().expect("fake device") = Some(fill);
        self.inner.running.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn stop(&mut self) {
        *self.inner.fill.lock().expect("fake device") = None;
        self.inner.running.store(false, Ordering::Relaxed);
    }

    fn played_frames(&self) -> u64 {
        self.inner.played.load(Ordering::Relaxed)
    }

    fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::Relaxed)
    }
}

/// How the player drives the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerConfig {
    /// Mix on a background thread into a lead buffer. Required for a real
    /// device; off for tests, where the device callback *is* the mixer and
    /// everything stays on one thread.
    pub play_ahead: bool,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self { play_ahead: true }
    }
}

impl PlayerConfig {
    /// Single-threaded, no lead buffer: the shape tests use.
    #[must_use]
    pub const fn deterministic() -> Self {
        Self { play_ahead: false }
    }
}

/// The transport surface every view and every test talks to.
pub trait PlayerEngine {
    /// Replace the source. Stops playback and rewinds.
    fn load(&mut self, source: Box<dyn PcmSource>);
    /// Drop the source.
    fn unload(&mut self);
    /// Start or resume.
    fn play(&mut self) -> Result<(), PlaybackError>;
    /// Pause where it is.
    fn pause(&mut self);
    /// Stop and rewind.
    fn stop(&mut self);
    /// Jump to an absolute position, clamped to the source.
    fn seek(&mut self, position: Micros);
    /// Jump by a relative amount.
    fn skip(&mut self, delta: Micros);
    /// Set the playback rate, keeping the position.
    fn set_speed(&mut self, speed: Speed);
    /// Set the output gain.
    fn set_volume(&mut self, volume: Volume);
    /// Current position.
    fn position(&self) -> Micros;
    /// Duration, when the source states one.
    fn duration(&self) -> Option<Micros>;
    /// Transport state.
    fn state(&self) -> TransportState;
    /// Current rate.
    fn speed(&self) -> Speed;
    /// Current gain.
    fn volume(&self) -> Volume;
    /// Apply queued commands and advance the clock from the device.
    /// Called once per rendered frame.
    fn pump(&mut self);
    /// A cloneable handle for views.
    fn handle(&self) -> PlayerHandle;
    /// Everything at once.
    fn snapshot(&self) -> PlaybackSnapshot;
}

/// Mixer state, owned by the mixing thread (or by the device callback in
/// deterministic mode) and reached only through its mutex.
struct MixerState {
    source: Box<dyn PcmSource>,
    stretcher: TimeStretcher,
    resampler: Option<Resampler>,
    device_rate: u32,
    device_channels: usize,
    source_channels: usize,
    /// Interleaved device-rate frames mixed but not yet handed to the device.
    ready: Vec<f32>,
    ready_read: usize,
    /// Device frame the next mixed sample belongs to.
    expected_cursor: i64,
    source_drained: bool,
    /// Scratch buffers, kept so the mixing pass allocates nothing.
    stretch_buffer: Vec<f32>,
    mapped_buffer: Vec<f32>,
    input_buffer: Vec<f32>,
    resample_in: Vec<f64>,
}

impl std::fmt::Debug for MixerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MixerState")
            .field("device_rate", &self.device_rate)
            .field("device_channels", &self.device_channels)
            .field("source_channels", &self.source_channels)
            .field("source_drained", &self.source_drained)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct MixerShared {
    state: Mutex<MixerState>,
    volume_per_mille: AtomicU32,
    /// Position the source ran out at, or `-1` while it has more to give.
    end_position_us: AtomicI64,
}

/// The player.
#[derive(Debug)]
pub struct Player {
    out: Box<dyn AudioOut>,
    config: PlayerConfig,
    clock: Arc<SharedClock>,
    handle: PlayerHandle,
    inbox: CommandInbox,
    mixer: Option<Arc<MixerShared>>,
    stats: Arc<PlaybackStats>,
    anchor_frames: u64,
    anchor_position: Micros,
    state: TransportState,
    speed: Speed,
    volume: Volume,
    duration: Option<Micros>,
}

impl Player {
    /// A player over `out`, mixing on a background thread.
    #[must_use]
    pub fn new(out: Box<dyn AudioOut>) -> Self {
        Self::with_config(out, PlayerConfig::default())
    }

    /// A player over `out` with explicit configuration.
    #[must_use]
    pub fn with_config(out: Box<dyn AudioOut>, config: PlayerConfig) -> Self {
        let clock = Arc::new(SharedClock::default());
        let (sender, receiver) = std::sync::mpsc::channel();
        let handle = PlayerHandle::new(Arc::clone(&clock), sender);
        Self {
            out,
            config,
            clock,
            handle,
            inbox: CommandInbox::new(receiver),
            mixer: None,
            stats: Arc::new(PlaybackStats::default()),
            anchor_frames: 0,
            anchor_position: Micros::ZERO,
            state: TransportState::Stopped,
            speed: Speed::NORMAL,
            volume: Volume::FULL,
            duration: None,
        }
    }

    /// Underrun accounting for the current run.
    #[must_use]
    pub fn stats(&self) -> &Arc<PlaybackStats> {
        &self.stats
    }

    fn apply(&mut self, command: PlayerCommand) {
        match command {
            PlayerCommand::Play => {
                let _ = self.play();
            }
            PlayerCommand::Pause => self.pause(),
            PlayerCommand::TogglePlay => {
                if self.state == TransportState::Playing {
                    self.pause();
                } else {
                    let _ = self.play();
                }
            }
            PlayerCommand::Stop => self.stop(),
            PlayerCommand::Seek(position) => self.seek(position),
            PlayerCommand::Skip(delta) => self.skip(delta),
            PlayerCommand::SetSpeed(speed) => self.set_speed(speed),
            PlayerCommand::SetVolume(volume) => self.set_volume(volume),
        }
    }

    /// Position implied by the device's frame counter.
    fn clock_position(&self) -> Micros {
        let rate = self.out.sample_rate();
        if rate == 0 {
            return self.anchor_position;
        }
        let delta = self.out.played_frames().saturating_sub(self.anchor_frames);
        let elapsed = i128::from(delta) * 1_000_000 * i128::from(self.speed.num())
            / (i128::from(rate) * i128::from(self.speed.den()));
        let micros = i128::from(self.anchor_position.get()) + elapsed;
        Micros::new(micros.clamp(0, i128::from(i64::MAX)) as i64)
    }

    fn reanchor(&mut self, position: Micros) {
        self.anchor_frames = self.out.played_frames();
        self.anchor_position = position;
        self.clock.set_position(position);
    }

    fn restart_stream_if_running(&mut self) {
        if self.out.is_running() {
            self.out.stop();
            let _ = self.start_stream();
        }
    }

    fn start_stream(&mut self) -> Result<(), PlaybackError> {
        let Some(mixer) = self.mixer.as_ref().map(Arc::clone) else {
            return Err(PlaybackError::NoSource);
        };
        {
            let mut state = mixer.state.lock().expect("mixer");
            state.expected_cursor = i64::MIN;
        }
        let channels = self.out.channels();
        let rate = self.out.sample_rate();
        let fill: FillCallback = if self.config.play_ahead {
            let mix_mixer = Arc::clone(&mixer);
            crate::ring::spawn(
                move |cursor, buffer, channels| mix_into(&mix_mixer, cursor, buffer, channels),
                0,
                rate,
                channels,
                Arc::clone(&self.stats),
            )
        } else {
            // Deterministic mode: the device callback mixes directly, so a
            // test's `render(n)` produces exactly those n frames of audio.
            let cursor = Arc::new(AtomicI64::new(0));
            let mix_mixer = Arc::clone(&mixer);
            Box::new(move |buffer: &mut [f32], device_channels: usize| {
                let device_channels = device_channels.max(1);
                let frames = (buffer.len() / device_channels) as i64;
                let at = cursor.fetch_add(frames, Ordering::Relaxed);
                mix_into(&mix_mixer, at, buffer, device_channels);
            })
        };
        self.out.start(fill)
    }
}

impl PlayerEngine for Player {
    fn load(&mut self, source: Box<dyn PcmSource>) {
        self.out.stop();
        let device_rate = self.out.sample_rate();
        let device_channels = self.out.channels();
        let source_rate = source.sample_rate();
        let source_channels = usize::from(source.channels()).max(1);
        self.duration = source.duration();
        let resampler = (source_rate != device_rate)
            .then(|| Resampler::new(source_rate, device_rate, device_channels as u16));
        let state = MixerState {
            stretcher: TimeStretcher::new(source_channels, source_rate),
            source,
            resampler,
            device_rate,
            device_channels,
            source_channels,
            ready: Vec::new(),
            ready_read: 0,
            expected_cursor: i64::MIN,
            source_drained: false,
            stretch_buffer: vec![0.0; MIX_BLOCK_FRAMES * source_channels],
            mapped_buffer: vec![0.0; MIX_BLOCK_FRAMES * device_channels],
            input_buffer: Vec::new(),
            resample_in: Vec::new(),
        };
        self.mixer = Some(Arc::new(MixerShared {
            state: Mutex::new(state),
            volume_per_mille: AtomicU32::new(u32::from(self.volume.per_mille())),
            end_position_us: AtomicI64::new(-1),
        }));
        self.state = TransportState::Stopped;
        self.clock.set_duration(self.duration);
        self.clock.set_state(self.state);
        self.reanchor(Micros::ZERO);
    }

    fn unload(&mut self) {
        self.out.stop();
        self.mixer = None;
        self.duration = None;
        self.state = TransportState::Stopped;
        self.clock.set_duration(None);
        self.clock.set_state(self.state);
        self.reanchor(Micros::ZERO);
    }

    fn play(&mut self) -> Result<(), PlaybackError> {
        if self.mixer.is_none() {
            return Err(PlaybackError::NoSource);
        }
        if self.state == TransportState::Playing {
            return Ok(());
        }
        if self.state == TransportState::Ended {
            self.seek(Micros::ZERO);
        }
        let position = self.anchor_position;
        self.start_stream()?;
        self.reanchor(position);
        self.state = TransportState::Playing;
        self.clock.set_state(self.state);
        Ok(())
    }

    fn pause(&mut self) {
        if self.state != TransportState::Playing {
            return;
        }
        let position = self.clock_position();
        self.out.stop();
        self.reanchor(position);
        self.state = TransportState::Paused;
        self.clock.set_state(self.state);
    }

    fn stop(&mut self) {
        self.out.stop();
        self.seek(Micros::ZERO);
        self.state = TransportState::Stopped;
        self.clock.set_state(self.state);
    }

    fn seek(&mut self, position: Micros) {
        let clamped = match self.duration {
            Some(duration) => position.clamp_to(Micros::ZERO, duration),
            None => Micros::new(position.get().max(0)),
        };
        if let Some(mixer) = self.mixer.as_ref() {
            mixer.end_position_us.store(-1, Ordering::Relaxed);
            let mut state = mixer.state.lock().expect("mixer");
            state.seek_to(clamped);
        }
        self.reanchor(clamped);
        if self.state == TransportState::Ended {
            self.state = TransportState::Paused;
            self.clock.set_state(self.state);
        }
        // The lead buffer holds audio from before the seek; the only way to
        // not hear it is to rebuild the stream.
        self.restart_stream_if_running();
        self.reanchor(clamped);
    }

    fn skip(&mut self, delta: Micros) {
        let target = if self.state == TransportState::Playing {
            self.clock_position().saturating_add(delta)
        } else {
            self.anchor_position.saturating_add(delta)
        };
        self.seek(target);
    }

    fn set_speed(&mut self, speed: Speed) {
        if speed == self.speed {
            return;
        }
        let position = if self.state == TransportState::Playing {
            self.clock_position()
        } else {
            self.anchor_position
        };
        self.speed = speed;
        self.clock.set_speed(speed);
        if let Some(mixer) = self.mixer.as_ref() {
            let mut state = mixer.state.lock().expect("mixer");
            state.stretcher.set_speed(speed);
        }
        self.reanchor(position);
        self.restart_stream_if_running();
        self.reanchor(position);
    }

    fn set_volume(&mut self, volume: Volume) {
        self.volume = volume;
        self.clock.set_volume(volume);
        if let Some(mixer) = self.mixer.as_ref() {
            mixer
                .volume_per_mille
                .store(u32::from(volume.per_mille()), Ordering::Relaxed);
        }
    }

    fn position(&self) -> Micros {
        if self.state == TransportState::Playing {
            self.clock_position()
        } else {
            self.anchor_position
        }
    }

    fn duration(&self) -> Option<Micros> {
        self.duration
    }

    fn state(&self) -> TransportState {
        self.state
    }

    fn speed(&self) -> Speed {
        self.speed
    }

    fn volume(&self) -> Volume {
        self.volume
    }

    fn pump(&mut self) {
        for command in self.inbox.drain() {
            self.apply(command);
        }
        if self.state != TransportState::Playing {
            return;
        }
        let position = self.clock_position();
        let end = self
            .mixer
            .as_ref()
            .map_or(-1, |mixer| mixer.end_position_us.load(Ordering::Relaxed));
        let reached_end = match (self.duration, end) {
            (Some(duration), _) if position >= duration => Some(duration),
            (_, end) if end >= 0 && position.get() >= end => Some(Micros::new(end)),
            _ => None,
        };
        if let Some(stop_at) = reached_end {
            self.out.stop();
            self.reanchor(stop_at);
            self.state = TransportState::Ended;
            self.clock.set_state(self.state);
            return;
        }
        self.clock.set_position(position);
    }

    fn handle(&self) -> PlayerHandle {
        self.handle.clone()
    }

    fn snapshot(&self) -> PlaybackSnapshot {
        PlaybackSnapshot {
            position: self.position(),
            duration: self.duration,
            state: self.state,
            speed: self.speed,
            volume: self.volume,
            revision: self.clock.revision(),
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.out.stop();
    }
}

impl MixerState {
    fn seek_to(&mut self, position: Micros) {
        let frame = position.to_frames(self.source.sample_rate()).max(0) as u64;
        self.stretcher.reset(frame);
        if let Some(resampler) = self.resampler.as_mut() {
            let _ = resampler.finish();
        }
        self.ready.clear();
        self.ready_read = 0;
        self.source_drained = false;
        self.expected_cursor = i64::MIN;
    }

    fn ready_frames(&self) -> usize {
        (self.ready.len() / self.device_channels).saturating_sub(self.ready_read)
    }

    /// Mix one block into `ready`. Returns false once the source is drained.
    fn produce(&mut self) -> bool {
        // 1. Stretched source-rate frames.
        let mut produced = 0usize;
        while produced == 0 {
            produced = self.stretcher.pull(&mut self.stretch_buffer);
            if produced > 0 {
                break;
            }
            if self.source_drained {
                break;
            }
            let (start, count) = self.stretcher.wanted();
            self.input_buffer.resize(count * self.source_channels, 0.0);
            let got = self.source.read(start, &mut self.input_buffer);
            if got == 0 {
                self.stretcher.mark_drained();
                self.source_drained = true;
                // One more pull flushes whatever the window still holds.
                produced = self.stretcher.pull(&mut self.stretch_buffer);
                break;
            }
            self.stretcher
                .feed(start, &self.input_buffer[..got * self.source_channels]);
        }
        if produced == 0 {
            return false;
        }

        // 2. Channel map to the device layout.
        let mapped_len = produced * self.device_channels;
        if self.mapped_buffer.len() < mapped_len {
            self.mapped_buffer.resize(mapped_len, 0.0);
        }
        map_channels(
            &self.stretch_buffer[..produced * self.source_channels],
            self.source_channels,
            &mut self.mapped_buffer[..mapped_len],
            self.device_channels,
        );

        // 3. Resample to the device rate.
        match self.resampler.as_mut() {
            Some(resampler) => {
                self.resample_in.clear();
                self.resample_in.extend(
                    self.mapped_buffer[..mapped_len]
                        .iter()
                        .map(|v| f64::from(*v)),
                );
                let out = resampler.process(&self.resample_in);
                self.ready.extend(out.into_iter().map(|v| v as f32));
            }
            None => self
                .ready
                .extend_from_slice(&self.mapped_buffer[..mapped_len]),
        }
        true
    }

    /// Source position of the next frame to be mixed.
    fn source_position(&self) -> Micros {
        Micros::from_frames(self.stretcher.position() as i64, self.source.sample_rate())
    }

    fn take(&mut self, out: &mut [f32], device_channels: usize) -> usize {
        let channels = device_channels.max(1);
        let mut written = 0usize;
        while written * channels < out.len() {
            if self.ready_frames() == 0 {
                // Keep `ready` from growing without bound over a long play.
                if self.ready_read > 0 {
                    self.ready.drain(..self.ready_read * self.device_channels);
                    self.ready_read = 0;
                }
                if !self.produce() {
                    break;
                }
            }
            let available = self.ready_frames();
            if available == 0 {
                break;
            }
            let frames = ((out.len() - written * channels) / channels).min(available);
            if frames == 0 {
                break;
            }
            let from = self.ready_read * self.device_channels;
            for frame in 0..frames {
                for channel in 0..channels {
                    let source_index = from + frame * self.device_channels + channel;
                    let value = if channel < self.device_channels {
                        self.ready.get(source_index).copied().unwrap_or(0.0)
                    } else {
                        0.0
                    };
                    out[(written + frame) * channels + channel] = value;
                }
            }
            self.ready_read += frames;
            written += frames;
        }
        written
    }
}

/// Map `source_channels` interleaved frames onto `device_channels`.
fn map_channels(input: &[f32], source_channels: usize, out: &mut [f32], device_channels: usize) {
    let source_channels = source_channels.max(1);
    let device_channels = device_channels.max(1);
    let frames = input.len() / source_channels;
    for frame in 0..frames {
        let base = frame * source_channels;
        if source_channels == device_channels {
            out[frame * device_channels..(frame + 1) * device_channels]
                .copy_from_slice(&input[base..base + source_channels]);
        } else if source_channels == 1 {
            // Mono plays from every speaker, not just the left one.
            let value = input[base];
            for channel in 0..device_channels {
                out[frame * device_channels + channel] = value;
            }
        } else if device_channels == 1 {
            let sum: f32 = input[base..base + source_channels].iter().sum();
            out[frame] = sum / source_channels as f32;
        } else {
            for channel in 0..device_channels {
                out[frame * device_channels + channel] = input[base + channel % source_channels];
            }
        }
    }
}

/// The mixing entry point shared by the lead-buffer thread and the
/// deterministic callback.
fn mix_into(mixer: &Arc<MixerShared>, cursor: i64, buffer: &mut [f32], device_channels: usize) {
    let Ok(mut state) = mixer.state.try_lock() else {
        // The UI holds the mixer for a seek: silence for one block is
        // correct, and far better than blocking the device callback.
        buffer.fill(0.0);
        return;
    };
    let channels = device_channels.max(1);
    if state.expected_cursor != i64::MIN && cursor > state.expected_cursor {
        // The device played silence while we were behind; skip the same
        // amount of source audio so sound stays locked to the clock.
        let skipped = cursor - state.expected_cursor;
        let position = state.source_position();
        let rate = state.device_rate.max(1);
        let speed_scaled = Micros::from_frames(skipped, rate)
            .scaled(state.stretcher.speed().num(), state.stretcher.speed().den());
        state.seek_to(position.saturating_add(speed_scaled));
    }

    let frames = state.take(buffer, channels);
    if let Some(rest) = buffer.get_mut(frames * channels..) {
        rest.fill(0.0);
    }
    if frames * channels < buffer.len() && state.source_drained {
        let end = state.source_position();
        let _ = mixer.end_position_us.compare_exchange(
            -1,
            end.get(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
    state.expected_cursor = cursor + (buffer.len() / channels) as i64;

    let gain = mixer.volume_per_mille.load(Ordering::Relaxed) as f32 / 1_000.0;
    if (gain - 1.0).abs() > f32::EPSILON {
        for sample in buffer.iter_mut() {
            *sample *= gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemoryPcmSource;

    fn player_with(frames: usize, rate: u32) -> (Player, FakeDevice) {
        let out = FakeAudioOut::new(rate, 1);
        let device = out.device();
        let mut player = Player::with_config(Box::new(out), PlayerConfig::deterministic());
        player.load(Box::new(MemoryPcmSource::sine(rate, frames, 220.0, 0.5)));
        (player, device)
    }

    #[test]
    fn a_loaded_source_reports_its_duration_and_starts_stopped() {
        let (player, _device) = player_with(16_000, 16_000);
        assert_eq!(player.duration(), Some(Micros::from_secs(1)));
        assert_eq!(player.state(), TransportState::Stopped);
        assert_eq!(player.position(), Micros::ZERO);
    }

    #[test]
    fn playing_advances_the_clock_by_exactly_the_frames_rendered() {
        let (mut player, device) = player_with(16_000 * 10, 16_000);
        player.play().expect("play");
        device.render(8_000);
        player.pump();
        assert_eq!(player.position(), Micros::from_millis(500));
        device.render(8_000);
        player.pump();
        assert_eq!(player.position(), Micros::from_secs(1));
    }

    #[test]
    fn pausing_freezes_the_clock_and_resuming_continues_from_there() {
        let (mut player, device) = player_with(16_000 * 10, 16_000);
        player.play().expect("play");
        device.render(4_000);
        player.pump();
        player.pause();
        let paused_at = player.position();
        assert_eq!(paused_at, Micros::from_millis(250));
        // Rendering while paused does nothing: the stream is stopped.
        device.render(16_000);
        player.pump();
        assert_eq!(player.position(), paused_at);
        player.play().expect("play");
        device.render(4_000);
        player.pump();
        assert_eq!(player.position(), Micros::from_millis(500));
    }

    #[test]
    fn seeking_moves_the_clock_and_the_audio_together() {
        let (mut player, device) = player_with(16_000 * 10, 16_000);
        device.capture(true);
        player.seek(Micros::from_secs(5));
        assert_eq!(player.position(), Micros::from_secs(5));
        player.play().expect("play");
        device.clear_captured();
        device.render(1_000);
        player.pump();
        assert_eq!(player.position(), Micros::new(5_062_500));

        // The audio really came from five seconds in: compare with the
        // source at that frame.
        let expected = MemoryPcmSource::sine(16_000, 16_000 * 10, 220.0, 0.5);
        let captured = device.captured();
        let offset = 5 * 16_000;
        for (index, sample) in captured.iter().take(64).enumerate() {
            let want = expected.samples()[offset + index];
            assert!(
                (sample - want).abs() < 1e-5,
                "sample {index}: {sample} vs {want}"
            );
        }
    }

    #[test]
    fn seeking_is_clamped_to_the_source() {
        let (mut player, _device) = player_with(16_000, 16_000);
        player.seek(Micros::from_secs(100));
        assert_eq!(player.position(), Micros::from_secs(1));
        player.seek(Micros::from_secs(-5));
        assert_eq!(player.position(), Micros::ZERO);
    }

    #[test]
    fn skipping_matches_the_react_player_ten_second_jumps() {
        let (mut player, device) = player_with(16_000 * 60, 16_000);
        player.play().expect("play");
        device.render(16_000);
        player.pump();
        player.skip(Micros::from_secs(10));
        assert_eq!(player.position(), Micros::from_secs(11));
        player.skip(Micros::from_secs(-10));
        assert_eq!(player.position(), Micros::from_secs(1));
    }

    #[test]
    fn speed_scales_the_clock_exactly() {
        let (mut player, device) = player_with(16_000 * 60, 16_000);
        player.set_speed(Speed::ONE_AND_A_HALF);
        player.play().expect("play");
        device.render(16_000);
        player.pump();
        assert_eq!(player.position(), Micros::from_millis(1_500));
        player.set_speed(Speed::HALF);
        device.render(16_000);
        player.pump();
        assert_eq!(player.position(), Micros::from_millis(2_000));
    }

    #[test]
    fn a_long_playback_at_a_fractional_speed_does_not_drift() {
        let (mut player, device) = player_with(16_000 * 3, 16_000);
        player.set_speed(Speed::ONE_AND_A_QUARTER);
        player.play().expect("play");
        // Three hours of device time in 1024-frame callbacks.
        let callbacks = 16_000 * 3 * 3_600 / 1_024;
        for _ in 0..callbacks {
            device.render(1_024);
        }
        let expected = Micros::from_frames(callbacks * 1_024, 16_000).scaled(5, 4);
        // The source is three seconds long, so the player has ended; the
        // arithmetic is what is under test, so read it before `pump` clamps.
        let position = player.position();
        assert_eq!(position, expected);
    }

    #[test]
    fn reaching_the_end_stops_and_reports_ended() {
        let (mut player, device) = player_with(16_000, 16_000);
        player.play().expect("play");
        device.render(16_000);
        player.pump();
        assert_eq!(player.state(), TransportState::Ended);
        assert_eq!(player.position(), Micros::from_secs(1));
        // Playing again restarts from the beginning.
        player.play().expect("play");
        assert_eq!(player.position(), Micros::ZERO);
        assert_eq!(player.state(), TransportState::Playing);
    }

    #[test]
    fn volume_scales_the_rendered_samples() {
        let (mut player, device) = player_with(16_000, 16_000);
        device.capture(true);
        player.set_volume(Volume::from_fraction(0.5));
        player.play().expect("play");
        let rendered = device.render(256);
        let expected = MemoryPcmSource::sine(16_000, 16_000, 220.0, 0.5);
        for (index, sample) in rendered.iter().take(256).enumerate() {
            let want = expected.samples()[index] * 0.5;
            assert!(
                (sample - want).abs() < 1e-5,
                "sample {index}: {sample} vs {want}"
            );
        }
        player.set_volume(Volume::MUTED);
        let muted = device.render(256);
        assert!(muted.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn commands_from_a_handle_are_applied_on_pump() {
        let (mut player, device) = player_with(16_000 * 10, 16_000);
        let handle = player.handle();
        handle.send(PlayerCommand::Play);
        player.pump();
        assert_eq!(player.state(), TransportState::Playing);
        device.render(1_600);
        player.pump();
        handle.request_seek(Micros::from_secs(3));
        player.pump();
        assert_eq!(player.position(), Micros::from_secs(3));
        handle.toggle_play();
        player.pump();
        assert_eq!(player.state(), TransportState::Paused);
    }

    #[test]
    fn the_handle_clock_follows_the_player() {
        let (mut player, device) = player_with(16_000 * 10, 16_000);
        let handle = player.handle();
        let mut subscriber = handle.subscribe();
        player.play().expect("play");
        device.render(8_000);
        player.pump();
        let change = subscriber.changed().expect("a change");
        assert_eq!(change.position, Micros::from_millis(500));
        assert!(change.is_playing());
        assert_eq!(change.fraction(), Some(0.05));
    }

    #[test]
    fn playing_without_a_source_is_an_error_not_a_panic() {
        let out = FakeAudioOut::new(48_000, 2);
        let mut player = Player::with_config(Box::new(out), PlayerConfig::deterministic());
        assert!(matches!(player.play(), Err(PlaybackError::NoSource)));
        player.pump();
        assert_eq!(player.state(), TransportState::Stopped);
    }

    #[test]
    fn a_mono_source_on_a_stereo_device_plays_from_both_channels() {
        let out = FakeAudioOut::new(16_000, 2);
        let device = out.device();
        let mut player = Player::with_config(Box::new(out), PlayerConfig::deterministic());
        player.load(Box::new(MemoryPcmSource::sine(16_000, 16_000, 220.0, 0.5)));
        player.play().expect("play");
        let rendered = device.render(64);
        for frame in rendered.chunks(2) {
            assert_eq!(frame[0], frame[1]);
        }
        assert!(rendered.iter().any(|sample| sample.abs() > 0.01));
    }

    #[test]
    fn a_source_at_a_different_rate_is_resampled_to_the_device() {
        let out = FakeAudioOut::new(48_000, 1);
        let device = out.device();
        let mut player = Player::with_config(Box::new(out), PlayerConfig::deterministic());
        player.load(Box::new(MemoryPcmSource::sine(16_000, 16_000, 220.0, 0.5)));
        assert_eq!(player.duration(), Some(Micros::from_secs(1)));
        player.play().expect("play");
        let rendered = device.render(4_800);
        player.pump();
        assert!(rendered.iter().any(|sample| sample.abs() > 0.05));
        assert_eq!(player.position(), Micros::from_millis(100));
    }

    #[test]
    fn unloading_clears_the_transport() {
        let (mut player, device) = player_with(16_000, 16_000);
        player.play().expect("play");
        device.render(100);
        player.unload();
        assert_eq!(player.state(), TransportState::Stopped);
        assert_eq!(player.duration(), None);
        assert!(matches!(player.play(), Err(PlaybackError::NoSource)));
    }

    #[test]
    fn the_default_lead_buffer_configuration_produces_audio() {
        // The deterministic configuration is what every other test uses, so
        // this one exercises the production path: a mixing thread filling
        // the ring ahead of the device. Timing-tolerant by necessity.
        let out = FakeAudioOut::new(16_000, 1);
        let device = out.device();
        let mut player = Player::new(Box::new(out));
        player.load(Box::new(MemoryPcmSource::sine(
            16_000,
            16_000 * 5,
            220.0,
            0.6,
        )));
        device.capture(true);
        player.play().expect("play");

        let mut heard = false;
        for _ in 0..40 {
            let block = device.render(512);
            if block.iter().any(|sample| sample.abs() > 0.05) {
                heard = true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        player.pump();
        assert!(heard, "the lead buffer never delivered audio");
        assert!(player.position() > Micros::ZERO);
        assert!(player.stats().callbacks.load(Ordering::Relaxed) >= 40);
        player.pause();
    }

    #[test]
    fn speed_labels_read_the_way_the_react_select_did() {
        assert_eq!(Speed::HALF.label(), "0.5×");
        assert_eq!(Speed::NORMAL.label(), "1×");
        assert_eq!(Speed::ONE_AND_A_QUARTER.label(), "1.25×");
        assert_eq!(Speed::DOUBLE.label(), "2×");
        assert_eq!(Speed::NORMAL.faster(), Speed::ONE_AND_A_QUARTER);
        assert_eq!(Speed::NORMAL.slower(), Speed::THREE_QUARTERS);
        assert_eq!(Speed::DOUBLE.faster(), Speed::DOUBLE);
        assert_eq!(Speed::HALF.slower(), Speed::HALF);
    }
}
