//! The seam between the player and everything that watches it.
//!
//! The transcript editor needs two things from playback and must not be
//! coupled to either the device or the decoder to get them: the current
//! position, read once per frame with no lock, and a way to ask for a seek
//! when the user clicks a timestamp. Both live here.
//!
//! The clock is a handful of atomics, so a view can read it from the render
//! thread at any time; commands go the other way over a channel the player
//! drains in [`crate::PlayerEngine::pump`]. Nothing in this module blocks.

use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use crate::engine::{Speed, TransportState, Volume};
use crate::time::Micros;

/// Everything a view needs to render the transport, taken atomically enough
/// that the parts never disagree about which playback run they describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackSnapshot {
    /// Current position.
    pub position: Micros,
    /// Total duration, when the source states one.
    pub duration: Option<Micros>,
    /// Transport state.
    pub state: TransportState,
    /// Playback speed.
    pub speed: Speed,
    /// Output volume.
    pub volume: Volume,
    /// Monotonic counter bumped on every observable change.
    pub revision: u64,
}

impl PlaybackSnapshot {
    /// Whether audio is advancing.
    #[must_use]
    pub fn is_playing(self) -> bool {
        self.state == TransportState::Playing
    }

    /// Position as a fraction of the duration, when both are known.
    #[must_use]
    pub fn fraction(self) -> Option<f32> {
        let duration = self.duration?;
        if duration.get() <= 0 {
            return None;
        }
        Some((self.position.get() as f64 / duration.get() as f64).clamp(0.0, 1.0) as f32)
    }
}

/// A request from a view to the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerCommand {
    /// Start or resume.
    Play,
    /// Pause, keeping the position.
    Pause,
    /// Pause if playing, play if not.
    TogglePlay,
    /// Stop and rewind to the start.
    Stop,
    /// Jump to an absolute position.
    Seek(Micros),
    /// Jump by a relative amount.
    Skip(Micros),
    /// Change speed.
    SetSpeed(Speed),
    /// Change volume.
    SetVolume(Volume),
}

/// Lock-free playback state shared by the player and its handles.
#[derive(Debug)]
pub struct SharedClock {
    position_us: AtomicI64,
    /// Duration in microseconds, or `-1` when unknown.
    duration_us: AtomicI64,
    state: AtomicU8,
    speed_num: AtomicU32,
    speed_den: AtomicU32,
    volume_per_mille: AtomicU32,
    revision: AtomicU64,
}

impl Default for SharedClock {
    fn default() -> Self {
        Self {
            position_us: AtomicI64::new(0),
            duration_us: AtomicI64::new(-1),
            state: AtomicU8::new(TransportState::Stopped as u8),
            speed_num: AtomicU32::new(Speed::NORMAL.num()),
            speed_den: AtomicU32::new(Speed::NORMAL.den()),
            volume_per_mille: AtomicU32::new(u32::from(Volume::FULL.per_mille())),
            revision: AtomicU64::new(0),
        }
    }
}

impl SharedClock {
    /// Current position.
    #[must_use]
    pub fn position(&self) -> Micros {
        Micros::new(self.position_us.load(Ordering::Relaxed))
    }

    /// Duration, when known.
    #[must_use]
    pub fn duration(&self) -> Option<Micros> {
        let value = self.duration_us.load(Ordering::Relaxed);
        (value >= 0).then(|| Micros::new(value))
    }

    /// Transport state.
    #[must_use]
    pub fn state(&self) -> TransportState {
        TransportState::from_u8(self.state.load(Ordering::Relaxed))
    }

    /// Playback speed.
    #[must_use]
    pub fn speed(&self) -> Speed {
        Speed::from_parts(
            self.speed_num.load(Ordering::Relaxed),
            self.speed_den.load(Ordering::Relaxed),
        )
    }

    /// Output volume.
    #[must_use]
    pub fn volume(&self) -> Volume {
        Volume::from_per_mille(self.volume_per_mille.load(Ordering::Relaxed) as u16)
    }

    /// Revision counter: bumped whenever anything above changes.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// Everything at once.
    #[must_use]
    pub fn snapshot(&self) -> PlaybackSnapshot {
        PlaybackSnapshot {
            position: self.position(),
            duration: self.duration(),
            state: self.state(),
            speed: self.speed(),
            volume: self.volume(),
            revision: self.revision(),
        }
    }

    pub(crate) fn set_position(&self, position: Micros) {
        if self.position_us.swap(position.get(), Ordering::Relaxed) != position.get() {
            self.bump();
        }
    }

    pub(crate) fn set_duration(&self, duration: Option<Micros>) {
        let value = duration.map_or(-1, Micros::get);
        if self.duration_us.swap(value, Ordering::Relaxed) != value {
            self.bump();
        }
    }

    pub(crate) fn set_state(&self, state: TransportState) {
        if self.state.swap(state as u8, Ordering::Relaxed) != state as u8 {
            self.bump();
        }
    }

    pub(crate) fn set_speed(&self, speed: Speed) {
        self.speed_num.store(speed.num(), Ordering::Relaxed);
        self.speed_den.store(speed.den(), Ordering::Relaxed);
        self.bump();
    }

    pub(crate) fn set_volume(&self, volume: Volume) {
        self.volume_per_mille
            .store(u32::from(volume.per_mille()), Ordering::Relaxed);
        self.bump();
    }

    fn bump(&self) {
        self.revision.fetch_add(1, Ordering::Release);
    }
}

/// A cheap, cloneable view of the player: read the clock, request changes.
#[derive(Debug, Clone)]
pub struct PlayerHandle {
    clock: Arc<SharedClock>,
    commands: Sender<PlayerCommand>,
}

impl PlayerHandle {
    pub(crate) fn new(clock: Arc<SharedClock>, commands: Sender<PlayerCommand>) -> Self {
        Self { clock, commands }
    }

    /// The shared clock.
    #[must_use]
    pub fn clock(&self) -> &Arc<SharedClock> {
        &self.clock
    }

    /// Current state in one read.
    #[must_use]
    pub fn snapshot(&self) -> PlaybackSnapshot {
        self.clock.snapshot()
    }

    /// Current position.
    #[must_use]
    pub fn position(&self) -> Micros {
        self.clock.position()
    }

    /// Queue a command. Silently ignored once the player is gone, which is
    /// the correct behaviour for a view outliving a closed recording.
    pub fn send(&self, command: PlayerCommand) {
        let _ = self.commands.send(command);
    }

    /// Ask the player to seek. The clock does not move until the player
    /// applies it, so views must render the clock, never their own guess.
    pub fn request_seek(&self, position: Micros) {
        self.send(PlayerCommand::Seek(position));
    }

    /// Ask the player to skip by a relative amount.
    pub fn request_skip(&self, delta: Micros) {
        self.send(PlayerCommand::Skip(delta));
    }

    /// Toggle play/pause.
    pub fn toggle_play(&self) {
        self.send(PlayerCommand::TogglePlay);
    }

    /// Watch the clock for changes.
    #[must_use]
    pub fn subscribe(&self) -> PositionSubscriber {
        PositionSubscriber {
            clock: Arc::clone(&self.clock),
            last_revision: self.clock.revision(),
        }
    }
}

/// A change-detecting view of the clock, for the transcript editor's
/// auto-follow: ask once per frame, repaint only when something moved.
#[derive(Debug, Clone)]
pub struct PositionSubscriber {
    clock: Arc<SharedClock>,
    last_revision: u64,
}

impl PositionSubscriber {
    /// The snapshot if anything changed since the last call, else `None`.
    pub fn changed(&mut self) -> Option<PlaybackSnapshot> {
        let snapshot = self.clock.snapshot();
        if snapshot.revision == self.last_revision {
            return None;
        }
        self.last_revision = snapshot.revision;
        Some(snapshot)
    }

    /// The current snapshot, without affecting change detection.
    #[must_use]
    pub fn peek(&self) -> PlaybackSnapshot {
        self.clock.snapshot()
    }
}

/// The player's end of the command channel.
#[derive(Debug)]
pub(crate) struct CommandInbox {
    receiver: Receiver<PlayerCommand>,
}

impl CommandInbox {
    pub(crate) const fn new(receiver: Receiver<PlayerCommand>) -> Self {
        Self { receiver }
    }

    /// Every command queued since the last drain.
    pub(crate) fn drain(&self) -> Vec<PlayerCommand> {
        self.receiver.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_reports_a_fraction_only_when_the_duration_is_known() {
        let clock = SharedClock::default();
        clock.set_position(Micros::from_secs(30));
        assert_eq!(clock.snapshot().fraction(), None);
        clock.set_duration(Some(Micros::from_secs(60)));
        assert_eq!(clock.snapshot().fraction(), Some(0.5));
        clock.set_duration(Some(Micros::ZERO));
        assert_eq!(clock.snapshot().fraction(), None);
    }

    #[test]
    fn the_revision_only_moves_when_something_changes() {
        let clock = SharedClock::default();
        let before = clock.revision();
        clock.set_position(Micros::ZERO);
        assert_eq!(clock.revision(), before);
        clock.set_position(Micros::from_secs(1));
        assert!(clock.revision() > before);
    }

    #[test]
    fn a_subscriber_reports_each_change_once() {
        let clock = Arc::new(SharedClock::default());
        let (sender, _receiver) = std::sync::mpsc::channel();
        let handle = PlayerHandle::new(Arc::clone(&clock), sender);
        let mut subscriber = handle.subscribe();
        assert!(subscriber.changed().is_none());
        clock.set_position(Micros::from_secs(2));
        assert_eq!(
            subscriber.changed().map(|snapshot| snapshot.position),
            Some(Micros::from_secs(2))
        );
        assert!(subscriber.changed().is_none());
    }

    #[test]
    fn commands_queue_and_drain_in_order() {
        let clock = Arc::new(SharedClock::default());
        let (sender, receiver) = std::sync::mpsc::channel();
        let handle = PlayerHandle::new(clock, sender);
        let inbox = CommandInbox::new(receiver);
        handle.request_seek(Micros::from_secs(4));
        handle.toggle_play();
        assert_eq!(
            inbox.drain(),
            vec![
                PlayerCommand::Seek(Micros::from_secs(4)),
                PlayerCommand::TogglePlay
            ]
        );
        assert_eq!(inbox.drain().len(), 0);
    }

    #[test]
    fn sending_after_the_player_is_gone_is_a_no_op() {
        let clock = Arc::new(SharedClock::default());
        let (sender, receiver) = std::sync::mpsc::channel();
        let handle = PlayerHandle::new(clock, sender);
        drop(receiver);
        handle.request_skip(Micros::from_secs(10));
    }
}
