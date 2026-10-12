//! The time ruler above the waveform.
//!
//! Tick spacing and label text come from `plaudon_audio_playback::ruler`, so
//! the ruler and the waveform cannot disagree about where a second is; this
//! file only draws and forwards clicks.

use egui::{pos2, vec2, Align2, CornerRadius, FontId, Rect, Response, Sense, Stroke, Ui};
use plaudon_audio_playback::ruler::{choose_spacing, format_tick, ticks};
use plaudon_audio_playback::sync::PlayerCommand;
use plaudon_audio_playback::time::Micros;
use plaudon_audio_playback::timeline::Timeline;

use super::style::PlayerStyle;

/// Minimum spacing between unlabelled ticks, in points.
const MIN_MINOR_POINTS: f32 = 12.0;
/// Minimum spacing between labels, in points. Wide enough for `1:00:00`.
const MIN_MAJOR_POINTS: f32 = 110.0;

/// What one frame of the ruler produced.
#[derive(Debug)]
pub struct RulerOutcome {
    /// The allocated response.
    pub response: Response,
    /// Transport commands the interaction asks for.
    pub commands: Vec<PlayerCommand>,
}

/// A ruler showing the visible span of a [`Timeline`].
#[derive(Debug)]
pub struct TimeRuler<'a> {
    timeline: &'a Timeline,
    style: &'a PlayerStyle,
    position: Micros,
    height: Option<f32>,
    interactive: bool,
}

impl<'a> TimeRuler<'a> {
    /// A ruler over `timeline`.
    #[must_use]
    pub fn new(timeline: &'a Timeline, position: Micros, style: &'a PlayerStyle) -> Self {
        Self {
            timeline,
            style,
            position,
            height: None,
            interactive: true,
        }
    }

    /// Override the height.
    #[must_use]
    pub fn height(mut self, height: f32) -> Self {
        self.height = Some(height);
        self
    }

    /// Turn click-to-seek off.
    #[must_use]
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    /// Lay out, handle clicks and paint.
    pub fn show(self, ui: &mut Ui) -> RulerOutcome {
        let height = self.height.unwrap_or(self.style.ruler_height);
        let sense = if self.interactive {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), sense);

        let mut commands = Vec::new();
        if self.interactive && response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let time = self.timeline.time_at_point(pos.x - rect.left());
                commands.push(PlayerCommand::Seek(self.timeline.clamp_time(time)));
            }
        }

        self.paint(ui, rect);

        RulerOutcome { response, commands }
    }

    fn paint(&self, ui: &Ui, rect: Rect) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::ZERO, self.style.background);

        let spacing = choose_spacing(*self.timeline, MIN_MINOR_POINTS, MIN_MAJOR_POINTS);
        let stroke = Stroke::new(1.0, self.style.tick);
        for tick in ticks(self.timeline.visible(), spacing) {
            let x = rect.left() + self.timeline.point_of_time(tick.time);
            if x < rect.left() - 1.0 || x > rect.right() + 1.0 {
                continue;
            }
            let length = if tick.major { 10.0 } else { 4.0 };
            painter.vline(x, (rect.bottom() - length)..=rect.bottom(), stroke);
            if tick.major {
                painter.text(
                    pos2(x + 3.0, rect.top()),
                    Align2::LEFT_TOP,
                    format_tick(tick.time, spacing.major),
                    FontId::monospace(10.0),
                    self.style.label,
                );
            }
        }

        let playhead_x = rect.left() + self.timeline.point_of_time(self.position);
        if (rect.left()..=rect.right()).contains(&playhead_x) {
            painter.vline(
                playhead_x,
                rect.top()..=rect.bottom(),
                Stroke::new(2.0, self.style.playhead),
            );
        }
    }
}
