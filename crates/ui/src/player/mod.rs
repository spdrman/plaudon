//! The audio player view: ruler, waveform, transport.
//!
//! EPIC O task O1.5 (#203). The widgets are usable one at a time — the
//! recording screen composes them itself — and [`PlayerPanel`] is the
//! assembled default, holding the timeline and the peak source and turning
//! the widgets' commands into requests on a [`PlayerHandle`].

pub mod peaks;
pub mod position;
pub mod ruler;
pub mod style;
pub mod transport;
pub mod waveform;

use egui::Ui;
use plaudon_audio_playback::regions::SpeakerRegions;
use plaudon_audio_playback::sync::{PlaybackSnapshot, PlayerCommand, PlayerHandle};
use plaudon_audio_playback::time::Micros;
use plaudon_audio_playback::timeline::Timeline;

pub use peaks::{PeakSource, PeakStatus};
pub use position::SnapshotPosition;
pub use ruler::{RulerOutcome, TimeRuler};
pub use style::PlayerStyle;
pub use transport::{keyboard_commands, speed_shortcut, TransportBar, TransportOutcome, SKIP};
pub use waveform::{WaveformOutcome, WaveformView, ARROW_STEP, PAGE_STEP};

/// Points of slack kept around the playhead when auto-following.
const FOLLOW_MARGIN_POINTS: f32 = 48.0;

/// Ruler, waveform and transport, wired to one player.
#[derive(Debug)]
pub struct PlayerPanel {
    /// Zoom and scroll of the waveform.
    pub timeline: Timeline,
    /// Peaks, and the worker building them.
    pub peaks: PeakSource,
    /// Speaker regions drawn under the waveform.
    pub regions: SpeakerRegions,
    /// Colours and sizes.
    pub style: PlayerStyle,
    /// Keep the playhead on screen while playing.
    pub follow_playhead: bool,
    /// Suspended while the user is dragging, so a scrub does not fight the
    /// auto-follow for the scroll offset.
    scrubbing: bool,
}

impl PlayerPanel {
    /// A panel over an empty timeline.
    #[must_use]
    pub fn new(peaks: PeakSource, style: PlayerStyle) -> Self {
        Self {
            timeline: Timeline::new(Micros::ZERO, 800),
            peaks,
            regions: SpeakerRegions::default(),
            style,
            follow_playhead: true,
            scrubbing: false,
        }
    }

    /// Point the panel at a recording of `duration`, resetting the zoom.
    pub fn set_duration(&mut self, duration: Micros) {
        if self.timeline.duration() == duration {
            return;
        }
        let viewport = self.timeline.viewport_points();
        self.timeline = Timeline::new(duration, viewport);
    }

    /// Whether the user is dragging the playhead.
    #[must_use]
    pub const fn is_scrubbing(&self) -> bool {
        self.scrubbing
    }

    /// Draw everything and send the resulting commands to `handle`.
    pub fn show(&mut self, ui: &mut Ui, snapshot: PlaybackSnapshot, handle: &PlayerHandle) {
        for command in self.frame(ui, snapshot) {
            handle.send(command);
        }
    }

    /// Draw everything and return the commands, for callers that route them
    /// themselves — and for tests, which assert on them.
    pub fn frame(&mut self, ui: &mut Ui, snapshot: PlaybackSnapshot) -> Vec<PlayerCommand> {
        self.peaks.poll();
        if let Some(duration) = snapshot.duration {
            self.set_duration(duration);
        }

        let mut commands = Vec::new();
        commands.extend(keyboard_commands(ui.ctx()));
        commands.extend(speed_shortcut(ui.ctx(), snapshot.speed));

        let ruler = TimeRuler::new(&self.timeline, snapshot.position, &self.style).show(ui);
        commands.extend(ruler.commands);

        let pyramid = self.peaks.pyramid();
        let regions = (!self.regions.is_empty()).then_some(&self.regions);
        let waveform = WaveformView::new(&mut self.timeline, snapshot.position, &self.style)
            .peaks(pyramid)
            .regions(regions)
            .show(ui);
        self.scrubbing = waveform.scrubbing;
        commands.extend(waveform.commands);

        if self.follow_playhead && snapshot.is_playing() && !self.scrubbing {
            self.timeline
                .scroll_to_reveal(snapshot.position, FOLLOW_MARGIN_POINTS);
        }

        commands.extend(TransportBar::new(snapshot).show(ui).commands);
        commands
    }
}
