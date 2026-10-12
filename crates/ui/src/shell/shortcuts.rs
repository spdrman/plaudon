//! Chords, the keymap and the keyboard reference (parity rows S9 and C9).
//!
//! A chord is a portable string — `Mod+Shift+F`, `?`, `Escape` — written in
//! the canonical order `Mod+Ctrl+Alt+Shift+KEY`, where `Mod` is Command on
//! macOS and Control everywhere else and `KEY` is an [`egui::Key`] name. The
//! shape is wordcraft's (`docs/EGUI_REFERENCE_COMPONENTS.md` §4.3); the
//! implementation is Plaudon's, because the shell has to resolve chords
//! against its own command registry and has to suppress them inside text
//! entry.
//!
//! Only two things are platform-specific and neither is a behaviour: how a
//! chord is *displayed* (`Cmd` versus `Ctrl`) and which physical key `Mod`
//! is. Both are decided by the operating system egui reports, so a test that
//! fixes the reported OS fixes the rendering too.

use std::collections::{BTreeMap, BTreeSet};

use egui::{Key, Modifiers};

use super::commands::{CommandRegistry, CommandSpec};

/// Longest chord string that will be accepted, in bytes. A longer one is not
/// a keystroke; it is a paste into a settings field.
pub const MAX_CHORD_LEN: usize = 48;

/// Most bindings a user may override. Far beyond any real keyboard, and it
/// bounds what a corrupt file can allocate.
pub const MAX_OVERRIDES: usize = 512;

/// Keys that cannot be typed without Shift on a common layout, so a chord
/// naming them does not also have to name Shift.
const SHIFTED_SYMBOLS: [Key; 7] = [
    Key::Questionmark,
    Key::Exclamationmark,
    Key::Colon,
    Key::Plus,
    Key::Pipe,
    Key::OpenCurlyBracket,
    Key::CloseCurlyBracket,
];

/// A parsed chord.
///
/// Four independent modifier flags, which is what a chord is;
/// `clippy::struct_excessive_bools` would have this be a bitfield, and a
/// bitfield is harder to read in a test than `Chord { command: true, .. }`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    /// `Mod`: Command on macOS, Control elsewhere.
    pub command: bool,
    /// A literal Control, for the rare chord that wants it on macOS too.
    pub ctrl: bool,
    /// Alt / Option.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
    /// The key itself.
    pub key: Key,
}

impl Chord {
    /// Parse a chord, accepting any spelling and casing egui's
    /// [`Key::from_name`] accepts.
    ///
    /// `None` when the string names no key, names two, or is longer than
    /// [`MAX_CHORD_LEN`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if text.is_empty() || text.len() > MAX_CHORD_LEN {
            return None;
        }
        let mut chord = Self {
            command: false,
            ctrl: false,
            alt: false,
            shift: false,
            key: Key::Space,
        };
        let mut key = None;
        for token in split_tokens(text) {
            match token.to_ascii_lowercase().as_str() {
                "mod" | "cmd" | "command" | "super" | "meta" => chord.command = true,
                "ctrl" | "control" => chord.ctrl = true,
                "alt" | "option" | "opt" => chord.alt = true,
                "shift" => chord.shift = true,
                _ => {
                    if key.is_some() {
                        return None;
                    }
                    key = Some(parse_key(token)?);
                }
            }
        }
        chord.key = key?;
        Some(chord)
    }

    /// The canonical string form.
    #[must_use]
    pub fn to_chord_string(self) -> String {
        let mut text = String::with_capacity(MAX_CHORD_LEN);
        if self.command {
            text.push_str("Mod+");
        }
        if self.ctrl {
            text.push_str("Ctrl+");
        }
        if self.alt {
            text.push_str("Alt+");
        }
        if self.shift {
            text.push_str("Shift+");
        }
        text.push_str(key_token(self.key));
        text
    }

    /// Whether a key press with `modifiers` is this chord.
    ///
    /// `Mod` matches egui's own [`Modifiers::command`], which is Command on
    /// macOS and Control elsewhere; a chord that names no modifier therefore
    /// refuses a press that holds one, so `?` does not fire on `Mod+?`.
    #[must_use]
    pub fn matches(self, modifiers: Modifiers) -> bool {
        if self.alt != modifiers.alt {
            return false;
        }
        // A symbol that needs Shift to type is matched with or without it.
        if !(self.shift || SHIFTED_SYMBOLS.contains(&self.key)) && modifiers.shift {
            return false;
        }
        if self.shift && !modifiers.shift {
            return false;
        }
        if self.command {
            if !modifiers.command {
                return false;
            }
        } else if self.ctrl {
            if !modifiers.ctrl {
                return false;
            }
        } else if modifiers.command || modifiers.ctrl {
            return false;
        }
        if self.ctrl && !modifiers.ctrl {
            return false;
        }
        true
    }
}

/// A key by any spelling egui accepts, in any case.
///
/// [`Key::from_name`] is case-sensitive, and a user typing `escape` or a
/// persisted `mod+k` must still resolve.
fn parse_key(token: &str) -> Option<Key> {
    Key::from_name(token).or_else(|| {
        Key::ALL.iter().copied().find(|key| {
            key.name().eq_ignore_ascii_case(token)
                || key.symbol_or_name().eq_ignore_ascii_case(token)
        })
    })
}

fn split_tokens(text: &str) -> Vec<&str> {
    // `+` is both the separator and a key name, so a trailing or doubled `+`
    // is the Plus key rather than an empty token.
    let mut tokens = Vec::new();
    let mut rest = text.trim();
    while let Some(position) = rest.find('+') {
        if position == 0 {
            tokens.push("+");
            rest = &rest[1..];
            continue;
        }
        tokens.push(rest[..position].trim());
        rest = rest[position + 1..].trim_start();
        if rest.is_empty() {
            tokens.push("+");
            return tokens;
        }
    }
    if !rest.is_empty() {
        tokens.push(rest);
    }
    tokens
}

/// The canonical token for a key.
///
/// [`Key::name`] everywhere except `?`, which every keyboard reference —
/// including the one this shell replaces — writes as the symbol.
fn key_token(key: Key) -> &'static str {
    if key == Key::Questionmark {
        "?"
    } else {
        key.name()
    }
}

/// Rewrite a chord into the canonical form, or reject it.
#[must_use]
pub fn normalize(text: &str) -> Option<String> {
    Chord::parse(text).map(Chord::to_chord_string)
}

/// A chord as it should be shown to the user on `os`.
///
/// `Mod` becomes `Cmd` on macOS and `Ctrl` elsewhere, which is exactly what
/// `formatShortcut` did in the shell being replaced.
#[must_use]
pub fn display(text: &str, os: egui::os::OperatingSystem) -> String {
    let mac = os == egui::os::OperatingSystem::Mac;
    let canonical = normalize(text).unwrap_or_else(|| text.to_owned());
    canonical.replace("Mod+", if mac { "Cmd+" } else { "Ctrl+" })
}

/// Why a binding was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    /// The string does not name a key.
    #[error("`{0}` is not a keyboard chord")]
    NotAChord(String),
    /// No such command.
    #[error("no command with id `{0}`")]
    UnknownCommand(String),
    /// The command refuses to be rebound.
    #[error("`{0}` cannot be rebound")]
    NotRebindable(String),
    /// Another command already answers to this chord.
    #[error("`{chord}` is already bound to `{command}`")]
    Conflict {
        /// The chord that is taken.
        chord: String,
        /// The command that has it.
        command: String,
    },
    /// Too many overrides.
    #[error("too many changed bindings")]
    TooMany,
}

/// Which chord runs which command.
///
/// Holds **overrides only**: a command with no entry keeps the default chord
/// from its [`CommandSpec`], and a command with an empty entry has
/// deliberately been left unbound. That distinction is what lets a changed
/// binding survive a change to the defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyMap {
    overrides: BTreeMap<String, Vec<String>>,
}

impl KeyMap {
    /// An empty map: every command uses its default chord.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from persisted overrides, dropping entries that no longer parse.
    ///
    /// A binding file written by an older build, or edited by hand, must not
    /// stop the shell from starting.
    #[must_use]
    pub fn from_overrides(overrides: &BTreeMap<String, Vec<String>>) -> Self {
        let mut map = Self::new();
        for (id, chords) in overrides.iter().take(MAX_OVERRIDES) {
            let canonical: Vec<String> = chords.iter().filter_map(|c| normalize(c)).collect();
            if canonical.len() == chords.len() {
                map.overrides.insert(id.clone(), canonical);
            } else if canonical.is_empty() && !chords.is_empty() {
                // Every chord in the entry was unreadable: fall back to the
                // default rather than silently unbinding the command.
                tracing::warn!(command = %id, "dropping unreadable keyboard bindings");
            } else {
                map.overrides.insert(id.clone(), canonical);
            }
        }
        map
    }

    /// The overrides, for persistence.
    #[must_use]
    pub fn overrides(&self) -> &BTreeMap<String, Vec<String>> {
        &self.overrides
    }

    /// Every chord that runs `spec`, override or default.
    #[must_use]
    pub fn chords_for(&self, spec: &CommandSpec) -> Vec<String> {
        if let Some(chords) = self.overrides.get(spec.id) {
            return chords.clone();
        }
        spec.default_chord.and_then(normalize).into_iter().collect()
    }

    /// The first chord that runs `spec`, which is what a menu or the palette
    /// shows next to its label.
    #[must_use]
    pub fn primary_chord(&self, spec: &CommandSpec) -> Option<String> {
        self.chords_for(spec).into_iter().next()
    }

    /// Bind `chord` to `id`, replacing whatever that command had.
    ///
    /// # Errors
    ///
    /// Fails when the chord is unreadable, the command is unknown or fixed,
    /// another command already uses the chord, or the override table is full.
    pub fn assign(
        &mut self,
        registry: &CommandRegistry,
        id: &str,
        chord: &str,
    ) -> Result<(), BindingError> {
        let canonical =
            normalize(chord).ok_or_else(|| BindingError::NotAChord(chord.to_owned()))?;
        let spec = registry
            .get(id)
            .ok_or_else(|| BindingError::UnknownCommand(id.to_owned()))?;
        if !spec.rebindable {
            return Err(BindingError::NotRebindable(id.to_owned()));
        }
        if let Some(existing) = self.command_for_chord(registry, &canonical) {
            if existing.id != id {
                return Err(BindingError::Conflict {
                    chord: canonical,
                    command: existing.id.to_owned(),
                });
            }
        }
        if !self.overrides.contains_key(id) && self.overrides.len() >= MAX_OVERRIDES {
            return Err(BindingError::TooMany);
        }
        self.overrides.insert(id.to_owned(), vec![canonical]);
        Ok(())
    }

    /// Leave `id` with no binding at all.
    pub fn unbind(&mut self, id: &str) {
        self.overrides.insert(id.to_owned(), Vec::new());
    }

    /// Give `id` its default chord back.
    pub fn reset(&mut self, id: &str) {
        self.overrides.remove(id);
    }

    /// Give every command its default chord back.
    pub fn reset_all(&mut self) {
        self.overrides.clear();
    }

    /// Replace every override at once, reporting the ones that were refused.
    ///
    /// Used by the settings editor: it hands over the whole table and gets
    /// back the entries that could not be applied, instead of failing the
    /// lot because one chord collided.
    pub fn apply(
        &mut self,
        registry: &CommandRegistry,
        overrides: &BTreeMap<String, Vec<String>>,
    ) -> Vec<BindingError> {
        let mut refused = Vec::new();
        self.overrides.clear();
        for (id, chords) in overrides.iter().take(MAX_OVERRIDES) {
            if chords.is_empty() {
                self.unbind(id);
                continue;
            }
            for chord in chords {
                if let Err(error) = self.assign(registry, id, chord) {
                    refused.push(error);
                }
            }
        }
        refused
    }

    /// The command a chord runs, if any.
    #[must_use]
    pub fn command_for_chord<'a>(
        &self,
        registry: &'a CommandRegistry,
        chord: &str,
    ) -> Option<&'a CommandSpec> {
        let canonical = normalize(chord)?;
        let overridden: BTreeSet<&str> = self.overrides.keys().map(String::as_str).collect();
        // An override wins over a default, so that rebinding `Mod+K` away
        // from the palette really frees the chord.
        for (id, chords) in &self.overrides {
            if chords.contains(&canonical) {
                if let Some(spec) = registry.get(id) {
                    return Some(spec);
                }
            }
        }
        registry.all().iter().find(|spec| {
            !overridden.contains(spec.id)
                && spec.default_chord.and_then(normalize).as_deref() == Some(canonical.as_str())
        })
    }

    /// The command a key press runs, if any.
    #[must_use]
    pub fn command_for_press<'a>(
        &self,
        registry: &'a CommandRegistry,
        key: Key,
        modifiers: Modifiers,
    ) -> Option<&'a CommandSpec> {
        let overridden: BTreeSet<&str> = self.overrides.keys().map(String::as_str).collect();
        for (id, chords) in &self.overrides {
            for chord in chords {
                if Chord::parse(chord)
                    .is_some_and(|parsed| parsed.key == key && parsed.matches(modifiers))
                {
                    if let Some(spec) = registry.get(id) {
                        return Some(spec);
                    }
                }
            }
        }
        registry.all().iter().find(|spec| {
            if overridden.contains(spec.id) {
                return false;
            }
            spec.default_chord
                .and_then(Chord::parse)
                .is_some_and(|parsed| parsed.key == key && parsed.matches(modifiers))
        })
    }
}

/// The commands a frame's key presses asked for, in order.
///
/// Presses are consumed, so a chord the shell handles does not also reach a
/// widget underneath. Typing wins over shortcuts: while a text field has
/// keyboard focus only `Escape` is read, which is the rule
/// `isTextEntryTarget` enforced in the React shell and the one wordcraft
/// applies with `egui_wants_keyboard_input`.
#[must_use]
pub fn pressed_commands(
    ctx: &egui::Context,
    registry: &CommandRegistry,
    keymap: &KeyMap,
) -> Vec<String> {
    let typing = ctx.egui_wants_keyboard_input();
    let mut wanted: Vec<(Key, Modifiers, String)> = Vec::new();
    ctx.input(|input| {
        for event in &input.events {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                repeat: false,
                ..
            } = event
            else {
                continue;
            };
            if typing && *key != Key::Escape {
                continue;
            }
            if let Some(spec) = keymap.command_for_press(registry, *key, *modifiers) {
                wanted.push((*key, *modifiers, spec.id.to_owned()));
            }
        }
    });
    let mut ids = Vec::with_capacity(wanted.len());
    for (key, modifiers, id) in wanted {
        ctx.input_mut(|input| input.consume_key(modifiers, key));
        ids.push(id);
    }
    ids
}

/// The keyboard reference dialog, generated from the command registry.
///
/// Replaces `app/ShortcutsDialog.tsx`: the same grouping, the same
/// platform-adjusted rendering, but the rows are the live command map rather
/// than a hand-maintained list that could drift from it.
pub fn shortcuts_dialog(
    ctx: &egui::Context,
    registry: &CommandRegistry,
    keymap: &KeyMap,
    open: &mut bool,
) {
    if !*open {
        return;
    }
    let os = ctx.os();
    let response = egui::Modal::new(egui::Id::new("plaudon-shortcuts")).show(ctx, |ui| {
        ui.set_width(520.0);
        ui.heading("Keyboard shortcuts");
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .max_height(420.0)
            .show(ui, |ui| {
                for group in super::commands::CommandGroup::ALL {
                    let rows: Vec<(&CommandSpec, Vec<String>)> = registry
                        .all()
                        .iter()
                        .filter(|spec| spec.group == group)
                        .map(|spec| (spec, keymap.chords_for(spec)))
                        .filter(|(_, chords)| !chords.is_empty())
                        .collect();
                    if rows.is_empty() {
                        continue;
                    }
                    ui.label(egui::RichText::new(group.label()).strong());
                    for (spec, chords) in rows {
                        ui.horizontal(|ui| {
                            let rendered: Vec<String> =
                                chords.iter().map(|c| display(c, os)).collect();
                            ui.add_sized(
                                [140.0, 20.0],
                                egui::Label::new(
                                    egui::RichText::new(rendered.join(" / ")).monospace(),
                                )
                                .selectable(false),
                            );
                            ui.label(spec.label);
                        });
                    }
                    ui.add_space(8.0);
                }
            });
        ui.separator();
        if ui.button("Close").clicked() {
            *open = false;
        }
    });
    if response.should_close() {
        *open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::commands::CommandRegistry;

    #[test]
    fn chords_normalise_into_one_canonical_order() {
        assert_eq!(normalize("shift+mod+f").as_deref(), Some("Mod+Shift+F"));
        assert_eq!(normalize("CMD+K").as_deref(), Some("Mod+K"));
        assert_eq!(normalize("Mod + 1").as_deref(), Some("Mod+1"));
        assert_eq!(normalize("?").as_deref(), Some("?"));
        assert_eq!(normalize("escape").as_deref(), Some("Escape"));
        assert_eq!(normalize("Alt+Left").as_deref(), Some("Alt+Left"));
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("Mod+Shift"), None);
        assert_eq!(normalize("Mod+A+B"), None);
        assert_eq!(normalize(&"x".repeat(MAX_CHORD_LEN + 1)), None);
    }

    #[test]
    fn every_key_round_trips_through_its_canonical_token() {
        for key in Key::ALL {
            let chord = Chord {
                command: false,
                ctrl: false,
                alt: false,
                shift: false,
                key: *key,
            };
            let text = chord.to_chord_string();
            assert_eq!(
                Chord::parse(&text).map(|parsed| parsed.key),
                Some(*key),
                "{text} does not round trip"
            );
        }
    }

    #[test]
    fn display_follows_the_platform_and_nothing_else() {
        use egui::os::OperatingSystem;
        assert_eq!(display("Mod+1", OperatingSystem::Mac), "Cmd+1");
        assert_eq!(display("Mod+1", OperatingSystem::Windows), "Ctrl+1");
        assert_eq!(display("Mod+1", OperatingSystem::Nix), "Ctrl+1");
        assert_eq!(display("?", OperatingSystem::Mac), "?");
    }

    #[test]
    fn a_chord_refuses_a_press_that_holds_an_extra_modifier() {
        let chord = Chord::parse("Mod+1").expect("parses");
        assert!(chord.matches(Modifiers::COMMAND));
        assert!(!chord.matches(Modifiers::COMMAND | Modifiers::SHIFT));
        assert!(!chord.matches(Modifiers::NONE));

        let plain = Chord::parse("?").expect("parses");
        // `?` needs Shift on most layouts, so Shift is tolerated, but a
        // command modifier is not.
        assert!(plain.matches(Modifiers::SHIFT));
        assert!(plain.matches(Modifiers::NONE));
        assert!(!plain.matches(Modifiers::COMMAND));
    }

    #[test]
    fn defaults_resolve_until_the_user_rebinds_them() {
        let registry = CommandRegistry::with_builtins();
        let mut keymap = KeyMap::new();
        assert_eq!(
            keymap
                .command_for_chord(&registry, "Mod+K")
                .map(|spec| spec.id),
            Some("command_palette")
        );
        keymap
            .assign(&registry, "command_palette", "Mod+Shift+P")
            .expect("rebinds");
        assert_eq!(
            keymap
                .command_for_chord(&registry, "Mod+Shift+P")
                .map(|spec| spec.id),
            Some("command_palette")
        );
        assert_eq!(keymap.command_for_chord(&registry, "Mod+K"), None);
    }

    #[test]
    fn a_conflicting_or_fixed_binding_is_refused_with_a_reason() {
        let registry = CommandRegistry::with_builtins();
        let mut keymap = KeyMap::new();
        assert_eq!(
            keymap.assign(&registry, "import_audio", "Mod+K"),
            Err(BindingError::Conflict {
                chord: "Mod+K".to_owned(),
                command: "command_palette".to_owned(),
            })
        );
        assert_eq!(
            keymap.assign(&registry, "close_overlay", "Mod+J"),
            Err(BindingError::NotRebindable("close_overlay".to_owned()))
        );
        assert_eq!(
            keymap.assign(&registry, "nav_library", "Mod+Shift"),
            Err(BindingError::NotAChord("Mod+Shift".to_owned()))
        );
        assert!(matches!(
            keymap.assign(&registry, "no_such_command", "Mod+J"),
            Err(BindingError::UnknownCommand(_))
        ));
    }

    #[test]
    fn overrides_survive_a_round_trip_through_persistence() {
        let registry = CommandRegistry::with_builtins();
        let mut keymap = KeyMap::new();
        keymap
            .assign(&registry, "import_audio", "mod+shift+i")
            .expect("rebinds");
        keymap.unbind("toggle_aside");
        let persisted = keymap.overrides().clone();

        let restored = KeyMap::from_overrides(&persisted);
        assert_eq!(restored, keymap);
        assert_eq!(
            restored
                .command_for_chord(&registry, "Mod+Shift+I")
                .map(|spec| spec.id),
            Some("import_audio")
        );
        let aside = registry.get("toggle_aside").expect("exists");
        assert_eq!(restored.chords_for(aside), Vec::<String>::new());
    }

    #[test]
    fn an_unreadable_persisted_chord_does_not_stop_the_shell() {
        let mut persisted = BTreeMap::new();
        persisted.insert("import_audio".to_owned(), vec!["Mod+Shift".to_owned()]);
        let keymap = KeyMap::from_overrides(&persisted);
        let registry = CommandRegistry::with_builtins();
        let import = registry.get("import_audio").expect("exists");
        // Falls back to the default rather than leaving the command unbound.
        assert_eq!(keymap.chords_for(import), vec!["Mod+O".to_owned()]);
    }

    #[test]
    fn a_press_resolves_to_the_command_the_platform_expects() {
        let registry = CommandRegistry::with_builtins();
        let keymap = KeyMap::new();
        assert_eq!(
            keymap
                .command_for_press(&registry, Key::Num3, Modifiers::COMMAND)
                .map(|spec| spec.id),
            Some("nav_jobs")
        );
        assert_eq!(
            keymap
                .command_for_press(&registry, Key::Questionmark, Modifiers::SHIFT)
                .map(|spec| spec.id),
            Some("keyboard_shortcuts")
        );
        assert_eq!(
            keymap.command_for_press(&registry, Key::Num3, Modifiers::NONE),
            None
        );
    }
}
