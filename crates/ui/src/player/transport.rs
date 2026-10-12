//! The transport bar and its keyboard map.
//!
//! Parity with the React player is deliberate and literal: the same ±10 s
//! skips, the same six speeds, the same `Space / J / K / L / , / .` window
//! shortcuts suppressed while typing, and the same "Duration unknown" with
//! the skips disabled when the source never stated a length.

use egui::{Key, Ui};
use plaudon_audio_playback::engine::{Speed, TransportState, Volume};
use plaudon_audio_playback::ruler::{format_clock, format_position};
use plaudon_audio_playback::sync::{PlaybackSnapshot, PlayerCommand};
use plaudon_audio_playback::time::Micros;

/// Skip distance for the back/forward buttons and for `J`/`L`.
pub const SKIP: Micros = Micros::new(10_000_000);

/// What one frame of the transport bar produced.
#[derive(Debug, Default)]
pub struct TransportOutcome {
    /// Transport commands the interaction asks for, in order.
    pub commands: Vec<PlayerCommand>,
}

/// Play/pause, skips, speed, volume and the position readout.
#[derive(Debug)]
pub struct TransportBar {
    snapshot: PlaybackSnapshot,
    enabled: bool,
}

impl TransportBar {
    /// A bar rendering `snapshot`.
    #[must_use]
    pub const fn new(snapshot: PlaybackSnapshot) -> Self {
        Self {
            snapshot,
            enabled: true,
        }
    }

    /// Disable every control, for a recording with no playable media.
    #[must_use]
    pub const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Lay out and handle clicks.
    pub fn show(self, ui: &mut Ui) -> TransportOutcome {
        let mut commands = Vec::new();
        let snapshot = self.snapshot;
        // Without a duration there is nothing to skip towards: the React
        // player disabled these too rather than guessing.
        let can_skip = self.enabled && snapshot.duration.is_some();

        ui.horizontal(|ui| {
            ui.add_enabled_ui(self.enabled, |ui| {
                let playing = snapshot.state == TransportState::Playing;
                let label = if playing { "Pause" } else { "Play" };
                if ui.button(label).clicked() {
                    commands.push(PlayerCommand::TogglePlay);
                }
            });

            ui.add_enabled_ui(can_skip, |ui| {
                if ui.button("Back 10 s").clicked() {
                    commands.push(PlayerCommand::Skip(-SKIP));
                }
                if ui.button("Forward 10 s").clicked() {
                    commands.push(PlayerCommand::Skip(SKIP));
                }
            });

            ui.label(format_position(snapshot.position, snapshot.duration));

            ui.add_enabled_ui(self.enabled, |ui| {
                speed_controls(ui, snapshot.speed, &mut commands);
                volume_control(ui, snapshot.volume, &mut commands);
            });
        });

        TransportOutcome { commands }
    }
}

fn speed_controls(ui: &mut Ui, speed: Speed, commands: &mut Vec<PlayerCommand>) {
    ui.label("Speed");
    for step in Speed::STEPS {
        let selected = step == speed;
        if ui.selectable_label(selected, step.label()).clicked() && !selected {
            commands.push(PlayerCommand::SetSpeed(step));
        }
    }
}

fn volume_control(ui: &mut Ui, volume: Volume, commands: &mut Vec<PlayerCommand>) {
    let mut value = f32::from(volume.per_mille()) / 1_000.0;
    let slider = egui::Slider::new(&mut value, 0.0..=1.0)
        .show_value(false)
        .text("Volume");
    if ui.add(slider).changed() {
        commands.push(PlayerCommand::SetVolume(Volume::from_fraction(value)));
    }
}

/// The window-level shortcuts, read once per frame.
///
/// Returns nothing while a text field has the keyboard, which is how the
/// React player kept `Space` from toggling playback mid-word.
#[must_use]
pub fn keyboard_commands(ctx: &egui::Context) -> Vec<PlayerCommand> {
    if ctx.egui_wants_keyboard_input() {
        return Vec::new();
    }
    let pressed: Vec<Key> = ctx.input(|input| {
        input
            .events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if modifiers.is_none() => Some(*key),
                _ => None,
            })
            .collect()
    });
    let mut commands = Vec::new();
    for key in pressed {
        match key {
            Key::Space => commands.push(PlayerCommand::TogglePlay),
            Key::J => commands.push(PlayerCommand::Skip(-SKIP)),
            Key::L => commands.push(PlayerCommand::Skip(SKIP)),
            Key::K => commands.push(PlayerCommand::Pause),
            _ => {}
        }
    }
    commands
}

/// The speed shortcuts, which need the current speed to step from.
#[must_use]
pub fn speed_shortcut(ctx: &egui::Context, speed: Speed) -> Option<PlayerCommand> {
    if ctx.egui_wants_keyboard_input() {
        return None;
    }
    let (slower, faster) = ctx.input(|input| {
        (
            input.key_pressed(Key::Comma),
            input.key_pressed(Key::Period),
        )
    });
    if slower {
        return Some(PlayerCommand::SetSpeed(speed.slower()));
    }
    if faster {
        return Some(PlayerCommand::SetSpeed(speed.faster()));
    }
    None
}

/// `h:mm:ss` of a position, for callers that want the same formatting the
/// transport uses.
#[must_use]
pub fn position_label(position: Micros) -> String {
    format_clock(position)
}
