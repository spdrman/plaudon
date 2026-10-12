//! The keyboard shortcut editor (matrix row C9).
//!
//! The React shell hard-coded its chords and had no way to change them. Here
//! the command registry is data the shell publishes
//! ([`SettingsData::shortcut_specs`]), the overrides are a sparse map
//! ([`ShortcutBindings`]), and every rule about what a chord is, when two
//! collide and how one is written down lives in `plaudon_app::shortcuts`.
//!
//! This module therefore only has to do three things honestly:
//!
//! * render the chord **in force**, in the host platform's spelling, so a
//!   macOS reader sees `Cmd+1` and a Windows reader sees `Ctrl+1`;
//! * refuse nothing itself — a rebind is tried on a *clone* of the bindings,
//!   and when the schema refuses it the schema's own sentence is shown and the
//!   stored map is left exactly as it was;
//! * say when a command cannot be moved at all, instead of offering a button
//!   that would be rejected.

use egui::{RichText, Ui};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::SetShortcutsRequest;
use plaudon_app::shortcuts::{
    canonical_key, display, KeyChord, ResolvedShortcut, ShortcutBindings, ShortcutSpec,
};
use plaudon_domain::error::{AppError, ErrorEnvelope};

use super::section;
use super::theme::{SettingsTokens, Tone};
use super::{Section, SettingsData, SettingsOutput};

/// Shown when the shell has not published its command map yet.
const REGISTRY_EMPTY: &str =
    "The shell has not published its command registry yet, so there are no shortcuts to list.";

/// Standing explanation above the list.
const PROSE: &str = "Choose a command, press the keys you want, and Plaudon stores only what you \
changed — so a chord this build moves later is picked up instead of frozen.";

/// Rendered for a command with no chord in force.
const UNBOUND: &str = "No shortcut assigned";

/// Rendered for a command the shell will not let the user move.
const FIXED: &str = "Fixed in this build: the shell relies on this key, so it cannot be \
reassigned.";

/// What the capture state is waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Capture {
    /// Escape: leave capture mode, change nothing.
    Cancelled,
    /// A canonical chord string.
    Chord(String),
}

/// Read the next key press while capture mode is on.
///
/// A press of a bare modifier key is not a chord, so it is skipped rather than
/// recorded; [`canonical_key`] returning `None` is exactly that case.
fn capture(ui: &Ui) -> Option<Capture> {
    ui.input(|input| {
        input.events.iter().find_map(|event| match event {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                if *key == egui::Key::Escape {
                    return Some(Capture::Cancelled);
                }
                let key = canonical_key(key.name())?;
                let chord = KeyChord {
                    key: key.to_owned(),
                    // `Modifiers::command` is Cmd on macOS and Ctrl
                    // elsewhere, which is exactly what `Mod` means; a literal
                    // Ctrl is only recorded when it is *not* the platform
                    // modifier, so a chord made on Windows does not come out
                    // as `Mod+Ctrl+…`.
                    command: modifiers.command,
                    ctrl: modifiers.ctrl && !modifiers.command,
                    alt: modifiers.alt,
                    shift: modifiers.shift,
                };
                Some(Capture::Chord(chord.to_chord_string()))
            }
            _ => None,
        })
    })
}

/// The shortcut editor.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ShortcutsSection {
    /// Identifier of the command whose next key press is being captured.
    capturing: Option<String>,
    /// The last refusal, in the schema's or the backend's own words.
    message: Option<String>,
}

impl ShortcutsSection {
    /// Whether the editor is waiting for a key press.
    #[must_use]
    pub fn capturing(&self) -> Option<&str> {
        self.capturing.as_deref()
    }

    /// Raise the command, or show why the schema refused.
    ///
    /// `bindings` is always the *clone* that was edited: on refusal it is
    /// dropped, so the stored map is untouched.
    fn finish(
        &mut self,
        result: Result<(), AppError>,
        bindings: ShortcutBindings,
        specs: &[ShortcutSpec],
        out: &mut SettingsOutput,
    ) {
        match result {
            Ok(()) => {
                self.message = None;
                out.command(AppCommand::SetShortcuts(SetShortcutsRequest {
                    bindings,
                    specs: specs.to_vec(),
                }));
            }
            Err(err) => self.message = Some(err.detail().to_owned()),
        }
    }

    /// Render one command's row.
    fn row(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        mac: bool,
        item: &ResolvedShortcut,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        let label = item.spec.label.clone();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(&label).strong().color(tokens.text()));
            let chord = item
                .chord
                .as_deref()
                .map_or_else(|| UNBOUND.to_owned(), |chord| display(chord, mac));
            ui.label(RichText::new(chord).color(tokens.text()));
            if item.customised {
                section::status_badge(ui, tokens, Tone::Neutral, "✎", "Customised");
            }
        });

        if !item.spec.rebindable {
            section::note(ui, tokens, FIXED);
            return;
        }

        ui.horizontal_wrapped(|ui| {
            if self.capturing.as_deref() == Some(item.spec.id.as_str()) {
                if ui
                    .button(format!("Stop changing the shortcut for {label}"))
                    .clicked()
                {
                    self.capturing = None;
                }
            } else if ui
                .button(format!("Change the shortcut for {label}"))
                .clicked()
            {
                self.capturing = Some(item.spec.id.clone());
                self.message = None;
            }

            if item.chord.is_some()
                && ui
                    .button(format!("Clear the shortcut for {label}"))
                    .clicked()
            {
                let mut bindings = data.shortcuts.clone();
                let result = bindings.unbind(&data.shortcut_specs, &item.spec.id);
                self.finish(result, bindings, &data.shortcut_specs, out);
            }

            if item.customised
                && ui
                    .button(format!("Reset the shortcut for {label} to its default"))
                    .clicked()
            {
                let mut bindings = data.shortcuts.clone();
                let result = bindings.reset(&data.shortcut_specs, &item.spec.id);
                self.finish(result, bindings, &data.shortcut_specs, out);
            }
        });

        if self.capturing.as_deref() == Some(item.spec.id.as_str()) {
            section::note(
                ui,
                tokens,
                &format!("Press the new shortcut for {label}, or Escape to cancel."),
            );
        }
    }
}

impl Section for ShortcutsSection {
    fn title(&self) -> &'static str {
        "Keyboard shortcuts"
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        if data.shortcut_specs.is_empty() {
            section::prose(ui, tokens, REGISTRY_EMPTY);
            return;
        }
        let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;

        if let Some(id) = self.capturing.clone() {
            match capture(ui) {
                Some(Capture::Cancelled) => self.capturing = None,
                Some(Capture::Chord(chord)) => {
                    self.capturing = None;
                    let mut bindings = data.shortcuts.clone();
                    let result = bindings.rebind(&data.shortcut_specs, &id, &chord);
                    self.finish(result, bindings, &data.shortcut_specs, out);
                }
                None => {}
            }
        }

        section::prose(ui, tokens, PROSE);
        ui.add_space(6.0);

        let resolved = data.shortcuts.resolve(&data.shortcut_specs);
        let mut groups: Vec<String> = Vec::new();
        for item in &resolved {
            if !groups.iter().any(|group| group == &item.spec.group) {
                groups.push(item.spec.group.clone());
            }
        }

        for group in &groups {
            ui.add_space(6.0);
            ui.label(RichText::new(group).strong().color(tokens.muted_text()));
            for item in resolved.iter().filter(|item| &item.spec.group == group) {
                self.row(ui, tokens, mac, item, data, out);
            }
        }

        ui.add_space(8.0);
        if ui.button("Reset all shortcuts").clicked() {
            let mut bindings = data.shortcuts.clone();
            bindings.reset_all();
            self.finish(Ok(()), bindings, &data.shortcut_specs, out);
        }

        if let Some(message) = self.message.clone() {
            section::status_badge(ui, tokens, Tone::Danger, "✕", &message);
        }
    }

    fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        _out: &mut SettingsOutput,
    ) {
        if command != "set_shortcuts" {
            return;
        }
        match result {
            Ok(_) => self.message = None,
            // The envelope's message is the backend's; it is shown verbatim.
            Err(envelope) => self.message = Some(envelope.message.clone()),
        }
    }
}
