//! The keyboard-binding schema: chord grammar, overrides and conflicts
//! (matrix row C9).
//!
//! The React shell hard-coded `Mod+1`…`Mod+6` in `app/shortcuts.ts` and had no
//! way to change them. EPIC O turns the command map into data: the shell owns
//! the command registry and the dispatch, Settings owns the editor, and this
//! module owns the one thing both of them have to agree on — what a chord
//! *is*, when two of them collide, and how one is written down.
//!
//! Nothing here names `egui`. A chord is the canonical string
//! `Mod+Ctrl+Alt+Shift+KEY`, where `KEY` is the name `egui::Key::name` prints
//! (`A`, `1`, `Left`, `Space`, `Escape`, `?`). The shell turns a chord into an
//! `egui::KeyboardShortcut`; this module only has to parse, canonicalise,
//! compare and render them, which keeps the rules testable without a window.
//! [`KEY_NAMES`] is the authoritative list, and `plaudon-ui` carries the drift
//! test that proves it still matches `egui::Key`.
//!
//! Three properties are enforced rather than hoped for:
//!
//! * **No silent shadowing.** [`ShortcutBindings::rebind`] refuses a chord
//!   another command already holds and names that command, so a user cannot end
//!   up with two actions on one chord and no way to tell which runs.
//! * **Overrides are sparse.** Only what the user changed is stored, so a
//!   future build that moves a default is picked up instead of being frozen at
//!   install time.
//! * **Some bindings are not negotiable.** A [`ShortcutSpec`] may declare
//!   itself not rebindable (`close_overlay` is Escape, always), and the editor
//!   cannot override it.

use std::collections::BTreeMap;
use std::fmt;

use plaudon_domain::error::{AppError, AppErrorCode};
use serde::{Deserialize, Serialize};

/// Token meaning "the platform's primary modifier": Cmd on macOS, Ctrl
/// elsewhere. Matches `egui::Modifiers::command`.
pub const MOD: &str = "Mod";

/// Longest chord string accepted from storage, which is `Mod+Ctrl+Alt+Shift+`
/// plus the longest key name with room to spare.
const MAX_CHORD_LEN: usize = 48;

/// Every key name a chord may end in, in `egui::Key::name` spelling.
///
/// `Questionmark` is written `?`, which is what a help shortcut is called in
/// every menu that has ever shown one; everything else is the name verbatim.
/// `plaudon-ui` asserts this list against `egui::Key::ALL`, so a key added by an
/// egui upgrade fails a test here instead of silently becoming unbindable.
pub const KEY_NAMES: &[&str] = &[
    "BrowserBack",
    "IntlBackslash",
    "Down",
    "Left",
    "Right",
    "Up",
    "Escape",
    "Tab",
    "Backspace",
    "Enter",
    "Insert",
    "Delete",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Copy",
    "Cut",
    "Paste",
    "Space",
    "Colon",
    "Comma",
    "Minus",
    "Period",
    "Plus",
    "Equals",
    "Semicolon",
    "Backslash",
    "Slash",
    "Pipe",
    "?",
    "Exclamationmark",
    "OpenBracket",
    "CloseBracket",
    "OpenCurlyBracket",
    "CloseCurlyBracket",
    "Backtick",
    "Quote",
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12",
    "F13",
    "F14",
    "F15",
    "F16",
    "F17",
    "F18",
    "F19",
    "F20",
    "F21",
    "F22",
    "F23",
    "F24",
    "F25",
    "F26",
    "F27",
    "F28",
    "F29",
    "F30",
    "F31",
    "F32",
    "F33",
    "F34",
    "F35",
];

/// Keys `egui` reports that a chord may **not** end in.
///
/// These are the physical modifier keys. Binding an action to "Shift" alone
/// would make every chord containing Shift ambiguous, so the editor never
/// offers them; they are listed rather than silently dropped so the
/// `plaudon-ui` drift test can prove the two lists together cover
/// `egui::Key::ALL`.
pub const NON_BINDABLE_KEY_NAMES: &[&str] = &[
    "ShiftLeft",
    "ShiftRight",
    "ControlLeft",
    "ControlRight",
    "AltLeft",
    "AltRight",
    "SuperLeft",
    "SuperRight",
];

/// Spellings accepted on input that are not the canonical name.
///
/// `egui::Key::from_name` is generous in exactly this way; a chord that arrived
/// from a hand-edited file or from the shell's own registry must mean the same
/// thing as one the editor recorded.
const KEY_ALIASES: &[(&str, &str)] = &[
    ("arrowdown", "Down"),
    ("arrowleft", "Left"),
    ("arrowright", "Right"),
    ("arrowup", "Up"),
    ("esc", "Escape"),
    ("return", "Enter"),
    ("numpadenter", "Enter"),
    ("help", "Insert"),
    ("questionmark", "?"),
    ("exclamation", "Exclamationmark"),
    ("num0", "0"),
    ("num1", "1"),
    ("num2", "2"),
    ("num3", "3"),
    ("num4", "4"),
    ("num5", "5"),
    ("num6", "6"),
    ("num7", "7"),
    ("num8", "8"),
    ("num9", "9"),
];

/// One key plus its modifiers, in the structured form the editor records.
///
/// `command` is the platform modifier, stored apart from a literal `ctrl` so a
/// binding made on macOS means the same thing on Windows.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
// The four flags are the four physical modifier keys a keyboard reports; they
// are independent by definition, so folding them into enums would only hide
// the shape the OS hands us.
#[allow(clippy::struct_excessive_bools)]
pub struct KeyChord {
    /// Canonical key name, one of [`KEY_NAMES`].
    pub key: String,
    /// The platform command modifier (Cmd on macOS, Ctrl elsewhere).
    pub command: bool,
    /// Literal Ctrl, on every platform.
    pub ctrl: bool,
    /// Alt / Option.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
}

impl KeyChord {
    /// A chord with no modifiers.
    #[must_use]
    pub fn plain(key: &str) -> Self {
        Self {
            key: key.to_owned(),
            command: false,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// A chord held with the platform command modifier.
    #[must_use]
    pub fn command(key: &str) -> Self {
        Self {
            command: true,
            ..Self::plain(key)
        }
    }

    /// The same chord with Shift added.
    #[must_use]
    pub fn with_shift(mut self) -> Self {
        self.shift = true;
        self
    }

    /// The same chord with Alt added.
    #[must_use]
    pub fn with_alt(mut self) -> Self {
        self.alt = true;
        self
    }

    /// Whether any modifier is held.
    #[must_use]
    pub const fn has_modifier(&self) -> bool {
        self.command || self.ctrl || self.alt || self.shift
    }

    /// Parse a chord string in any accepted spelling.
    ///
    /// Modifier order and letter case do not matter on input; the output is
    /// always canonical.
    #[must_use]
    pub fn parse(chord: &str) -> Option<Self> {
        if chord.is_empty() || chord.len() > MAX_CHORD_LEN {
            return None;
        }
        // `+` is both the separator and a key name, so a chord ending in one
        // is the Plus key: `Mod++` and the bare `+` both mean it.
        let (body, plus_key) = match chord.strip_suffix('+') {
            Some(prefix) => (prefix.strip_suffix('+').unwrap_or(prefix), true),
            None => (chord, false),
        };
        let mut parsed = Self::plain("");
        let mut key: Option<&str> = if plus_key { Some("Plus") } else { None };
        for token in body.split('+') {
            let token = token.trim();
            if token.is_empty() {
                if plus_key && body.is_empty() {
                    continue;
                }
                return None;
            }
            match token.to_ascii_lowercase().as_str() {
                "mod" | "cmd" | "command" | "super" | "meta" | "win" => {
                    if parsed.command {
                        return None;
                    }
                    parsed.command = true;
                }
                "ctrl" | "control" => {
                    if parsed.ctrl {
                        return None;
                    }
                    parsed.ctrl = true;
                }
                "alt" | "option" | "opt" => {
                    if parsed.alt {
                        return None;
                    }
                    parsed.alt = true;
                }
                "shift" => {
                    if parsed.shift {
                        return None;
                    }
                    parsed.shift = true;
                }
                _ => {
                    if key.is_some() {
                        return None;
                    }
                    key = Some(token);
                }
            }
        }
        canonical_key(key?)?.clone_into(&mut parsed.key);
        Some(parsed)
    }

    /// The canonical chord string: `Mod+Ctrl+Alt+Shift+KEY`.
    #[must_use]
    pub fn to_chord_string(&self) -> String {
        let mut out = String::with_capacity(MAX_CHORD_LEN);
        if self.command {
            out.push_str(MOD);
            out.push('+');
        }
        if self.ctrl {
            out.push_str("Ctrl+");
        }
        if self.alt {
            out.push_str("Alt+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        out.push_str(&self.key);
        out
    }

    /// Rendered for a reader: `Cmd+Shift+Z` on macOS, `Ctrl+Shift+Z` elsewhere.
    ///
    /// `mac` is passed in rather than read from `cfg!`, so a test renders the
    /// same string wherever it runs.
    #[must_use]
    pub fn display(&self, mac: bool) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(5);
        if self.command {
            parts.push(if mac { "Cmd" } else { "Ctrl" });
        }
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push(if mac { "Option" } else { "Alt" });
        }
        if self.shift {
            parts.push("Shift");
        }
        parts.push(&self.key);
        parts.join("+")
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_chord_string())
    }
}

/// The canonical spelling of a key name, or `None` when no key is spelled
/// that way.
#[must_use]
pub fn canonical_key(name: &str) -> Option<&'static str> {
    let lowered = name.to_ascii_lowercase();
    if let Some((_, canonical)) = KEY_ALIASES.iter().find(|(alias, _)| *alias == lowered) {
        return Some(canonical);
    }
    KEY_NAMES
        .iter()
        .copied()
        .find(|candidate| candidate.to_ascii_lowercase() == lowered)
}

/// Canonicalise a chord string, or `None` when it is not a chord.
///
/// This is the single grammar: the shell's key map, the settings editor and
/// anything read back from disk all go through it, so two spellings of one
/// chord can never be stored as two bindings.
#[must_use]
pub fn normalize(chord: &str) -> Option<String> {
    KeyChord::parse(chord).map(|parsed| parsed.to_chord_string())
}

/// Render a chord string the way a menu would.
///
/// An unparseable string is returned unchanged rather than hidden: the user is
/// shown what is actually stored.
#[must_use]
pub fn display(chord: &str, mac: bool) -> String {
    KeyChord::parse(chord).map_or_else(|| chord.to_owned(), |parsed| parsed.display(mac))
}

/// One entry of the command registry, as the shortcut editor needs it.
///
/// The shell builds these from its command map; nothing in this crate decides
/// what commands exist. Keeping it data is what lets the player and the
/// transcript editor register their own entries at runtime without this module
/// or the settings screen knowing their names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutSpec {
    /// Stable command identifier, for example `nav_library`.
    pub id: String,
    /// Label shown in the editor and the help dialog.
    pub label: String,
    /// Heading the entry is listed under, for example `Navigation`.
    pub group: String,
    /// Chord this build ships with, canonical form. `None` means the command
    /// has no default and is reached another way.
    pub default_chord: Option<String>,
    /// Whether the user may move it. `false` is for bindings the shell relies
    /// on, such as Escape closing an overlay.
    pub rebindable: bool,
}

impl ShortcutSpec {
    /// A rebindable entry with a default chord.
    #[must_use]
    pub fn new(id: &str, label: &str, group: &str, default_chord: &str) -> Self {
        Self {
            id: id.to_owned(),
            label: label.to_owned(),
            group: group.to_owned(),
            default_chord: Some(default_chord.to_owned()),
            rebindable: true,
        }
    }

    /// The same entry with no shipped chord.
    #[must_use]
    pub fn without_default(mut self) -> Self {
        self.default_chord = None;
        self
    }

    /// The same entry marked as not rebindable.
    #[must_use]
    pub fn fixed(mut self) -> Self {
        self.rebindable = false;
        self
    }
}

/// An attempted binding that another command already holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutConflict {
    /// Identifier of the command that already holds the chord.
    pub held_by: String,
    /// Label of that command, for the message the user reads.
    pub held_by_label: String,
    /// The chord in question, canonical form.
    pub chord: String,
}

impl fmt::Display for ShortcutConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} is already assigned to {}",
            display(&self.chord, cfg!(target_os = "macos")),
            self.held_by_label
        )
    }
}

impl From<ShortcutConflict> for AppError {
    fn from(conflict: ShortcutConflict) -> Self {
        Self::new(AppErrorCode::InvalidArgument, conflict.to_string())
    }
}

/// How one command is currently bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedShortcut {
    /// The registry entry.
    pub spec: ShortcutSpec,
    /// The chord in force, canonical form.
    pub chord: Option<String>,
    /// Whether the user moved it off its shipped chord.
    pub customised: bool,
}

/// The chords the user chose, keyed by command identifier.
///
/// Sparse by construction: a command the user never touched is absent. An
/// explicit `None` is how a user clears a binding without inheriting the
/// default back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutBindings {
    /// Overrides, command id to chord. `None` means "deliberately unbound".
    #[serde(default)]
    overrides: BTreeMap<String, Option<String>>,
}

impl ShortcutBindings {
    /// No overrides: every command on its shipped chord.
    #[must_use]
    pub fn defaults() -> Self {
        Self::default()
    }

    /// The raw override map, for the shell's key map.
    #[must_use]
    pub const fn overrides(&self) -> &BTreeMap<String, Option<String>> {
        &self.overrides
    }

    /// Whether `id` has been moved off its shipped chord.
    #[must_use]
    pub fn is_customised(&self, id: &str) -> bool {
        self.overrides.contains_key(id)
    }

    /// The chord in force for `spec`.
    #[must_use]
    pub fn chord(&self, spec: &ShortcutSpec) -> Option<String> {
        match self.overrides.get(&spec.id) {
            Some(chosen) => chosen.clone(),
            None => spec.default_chord.clone(),
        }
    }

    /// Every entry of `specs` with the chord in force, in registry order.
    #[must_use]
    pub fn resolve(&self, specs: &[ShortcutSpec]) -> Vec<ResolvedShortcut> {
        specs
            .iter()
            .map(|spec| ResolvedShortcut {
                chord: self.chord(spec),
                customised: self.is_customised(&spec.id),
                spec: spec.clone(),
            })
            .collect()
    }

    /// The command holding `chord`, ignoring `except`.
    #[must_use]
    pub fn holder_of(
        &self,
        specs: &[ShortcutSpec],
        chord: &str,
        except: &str,
    ) -> Option<ShortcutSpec> {
        specs
            .iter()
            .filter(|spec| spec.id != except)
            .find(|spec| self.chord(spec).as_deref() == Some(chord))
            .cloned()
    }

    /// Move `id` onto `chord`.
    ///
    /// Refused when the command is not rebindable, when the chord is not a
    /// chord, or when another command already holds it — in which case the
    /// editor shows which one and nothing changes.
    pub fn rebind(
        &mut self,
        specs: &[ShortcutSpec],
        id: &str,
        chord: &str,
    ) -> Result<(), AppError> {
        let spec = Self::spec_for(specs, id)?;
        if !spec.rebindable {
            return Err(AppError::new(
                AppErrorCode::InvalidArgument,
                format!("{} cannot be reassigned", spec.label),
            ));
        }
        let canonical = normalize(chord).ok_or_else(|| {
            AppError::new(
                AppErrorCode::InvalidArgument,
                "that key combination is not one this build can bind",
            )
        })?;
        if let Some(held_by) = self.holder_of(specs, &canonical, id) {
            return Err(ShortcutConflict {
                held_by: held_by.id,
                held_by_label: held_by.label,
                chord: canonical,
            }
            .into());
        }
        if spec.default_chord.as_deref() == Some(canonical.as_str()) {
            self.overrides.remove(id);
        } else {
            self.overrides.insert(id.to_owned(), Some(canonical));
        }
        Ok(())
    }

    /// Leave `id` with no chord at all.
    pub fn unbind(&mut self, specs: &[ShortcutSpec], id: &str) -> Result<(), AppError> {
        let spec = Self::spec_for(specs, id)?;
        if !spec.rebindable {
            return Err(AppError::new(
                AppErrorCode::InvalidArgument,
                format!("{} cannot be unbound", spec.label),
            ));
        }
        if spec.default_chord.is_none() {
            self.overrides.remove(id);
        } else {
            self.overrides.insert(id.to_owned(), None);
        }
        Ok(())
    }

    /// Put `id` back on its shipped chord.
    ///
    /// Refused, and nothing changes, when the shipped chord is currently held
    /// by a command the user moved there.
    pub fn reset(&mut self, specs: &[ShortcutSpec], id: &str) -> Result<(), AppError> {
        let spec = Self::spec_for(specs, id)?;
        if let Some(default) = spec.default_chord.as_deref() {
            if let Some(held_by) = self.holder_of(specs, default, id) {
                return Err(ShortcutConflict {
                    held_by: held_by.id,
                    held_by_label: held_by.label,
                    chord: default.to_owned(),
                }
                .into());
            }
        }
        self.overrides.remove(id);
        Ok(())
    }

    /// Put every command back on its shipped chord.
    pub fn reset_all(&mut self) {
        self.overrides.clear();
    }

    /// Validate a map that arrived from storage or from a caller.
    ///
    /// Canonicalises every chord, drops overrides for commands this build does
    /// not have, and rejects a map in which two commands end up on one chord.
    pub fn check(&mut self, specs: &[ShortcutSpec]) -> Result<(), AppError> {
        let mut cleaned: BTreeMap<String, Option<String>> = BTreeMap::new();
        for (id, chord) in &self.overrides {
            let Some(spec) = specs.iter().find(|spec| &spec.id == id) else {
                // The command no longer exists; its binding is not an error,
                // it is just gone.
                continue;
            };
            if !spec.rebindable {
                return Err(AppError::new(
                    AppErrorCode::InvalidArgument,
                    format!("{} cannot be reassigned", spec.label),
                ));
            }
            match chord {
                None => {
                    cleaned.insert(id.clone(), None);
                }
                Some(raw) => {
                    let canonical = normalize(raw).ok_or_else(|| {
                        AppError::new(
                            AppErrorCode::InvalidArgument,
                            "a stored keyboard shortcut is not a key combination",
                        )
                    })?;
                    cleaned.insert(id.clone(), Some(canonical));
                }
            }
        }
        self.overrides = cleaned;

        let mut seen: BTreeMap<String, ShortcutSpec> = BTreeMap::new();
        for spec in specs {
            let Some(chord) = self.chord(spec) else {
                continue;
            };
            if let Some(held_by) = seen.get(&chord) {
                return Err(ShortcutConflict {
                    held_by: held_by.id.clone(),
                    held_by_label: held_by.label.clone(),
                    chord,
                }
                .into());
            }
            seen.insert(chord, spec.clone());
        }
        Ok(())
    }

    fn spec_for(specs: &[ShortcutSpec], id: &str) -> Result<ShortcutSpec, AppError> {
        specs
            .iter()
            .find(|spec| spec.id == id)
            .cloned()
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCode::InvalidArgument,
                    "that command is not in this build's command map",
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs() -> Vec<ShortcutSpec> {
        vec![
            ShortcutSpec::new("nav_library", "Go to Library", "Navigation", "Mod+1"),
            ShortcutSpec::new("nav_settings", "Go to Settings", "Navigation", "Mod+5"),
            ShortcutSpec::new("search_library", "Search", "Application", "Mod+Shift+F"),
            ShortcutSpec::new("keyboard_shortcuts", "Shortcut help", "Application", "?"),
            ShortcutSpec::new("focus_main_region", "Focus content", "Application", "Mod+J")
                .without_default(),
            ShortcutSpec::new("close_overlay", "Close overlay", "Application", "Escape").fixed(),
        ]
    }

    #[test]
    fn modifier_order_and_case_do_not_change_a_chord() {
        assert_eq!(normalize("shift+MOD+f").as_deref(), Some("Mod+Shift+F"));
        assert_eq!(normalize("Mod+Shift+F").as_deref(), Some("Mod+Shift+F"));
        assert_eq!(normalize("cmd+1").as_deref(), Some("Mod+1"));
        assert_eq!(
            normalize("ctrl+alt+Delete").as_deref(),
            Some("Ctrl+Alt+Delete")
        );
    }

    #[test]
    fn the_three_spellings_the_shell_and_the_editor_must_agree_on() {
        assert_eq!(normalize("Mod+1").as_deref(), Some("Mod+1"));
        assert_eq!(normalize("Mod+Num1").as_deref(), Some("Mod+1"));
        assert_eq!(normalize("?").as_deref(), Some("?"));
        assert_eq!(normalize("Questionmark").as_deref(), Some("?"));
        assert_eq!(normalize("Mod+K").as_deref(), Some("Mod+K"));
    }

    #[test]
    fn arrow_and_escape_aliases_resolve_to_one_name() {
        assert_eq!(normalize("ArrowLeft").as_deref(), Some("Left"));
        assert_eq!(normalize("Left").as_deref(), Some("Left"));
        assert_eq!(normalize("Esc").as_deref(), Some("Escape"));
        assert_eq!(normalize("Return").as_deref(), Some("Enter"));
    }

    #[test]
    fn a_trailing_plus_is_the_plus_key() {
        assert_eq!(normalize("Mod++").as_deref(), Some("Mod+Plus"));
        assert_eq!(normalize("+").as_deref(), Some("Plus"));
        assert_eq!(normalize("Mod+Plus").as_deref(), Some("Mod+Plus"));
    }

    #[test]
    fn nonsense_is_not_a_chord() {
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("Mod"), None, "a modifier alone is not a chord");
        assert_eq!(normalize("Mod+NotAKey"), None);
        assert_eq!(normalize("Mod+A+B"), None, "two keys is not a chord");
        assert_eq!(
            normalize("Mod+Mod+A"),
            None,
            "a repeated modifier is malformed"
        );
        assert_eq!(normalize(&"A".repeat(MAX_CHORD_LEN + 1)), None);
    }

    #[test]
    fn chords_render_with_the_hosts_modifier_names() {
        assert_eq!(display("Mod+Shift+Z", true), "Cmd+Shift+Z");
        assert_eq!(display("Mod+Shift+Z", false), "Ctrl+Shift+Z");
        assert_eq!(display("Alt+Left", true), "Option+Left");
        assert_eq!(display("Space", true), "Space");
    }

    #[test]
    fn an_unparseable_stored_chord_is_shown_as_stored_not_hidden() {
        assert_eq!(display("Hyper+Q", true), "Hyper+Q");
    }

    #[test]
    fn an_untouched_command_resolves_to_its_shipped_chord() {
        let specs = specs();
        let bindings = ShortcutBindings::defaults();
        assert_eq!(bindings.chord(&specs[0]).as_deref(), Some("Mod+1"));
        assert!(!bindings.is_customised("nav_library"));
        assert_eq!(bindings.chord(&specs[4]), None, "no default means no chord");
    }

    #[test]
    fn rebinding_onto_another_commands_chord_is_refused_and_names_it() {
        let specs = specs();
        let mut bindings = ShortcutBindings::defaults();
        let error = bindings
            .rebind(&specs, "search_library", "Mod+1")
            .expect_err("Mod+1 belongs to the library");
        assert_eq!(error.code(), AppErrorCode::InvalidArgument);
        assert!(error.detail().contains("Go to Library"), "{error:?}");
        assert_eq!(
            bindings.chord(&specs[2]).as_deref(),
            Some("Mod+Shift+F"),
            "a refused rebinding changes nothing"
        );
    }

    #[test]
    fn rebinding_back_onto_the_default_drops_the_override() {
        let specs = specs();
        let mut bindings = ShortcutBindings::defaults();
        bindings.rebind(&specs, "search_library", "Mod+E").unwrap();
        assert!(bindings.is_customised("search_library"));
        bindings
            .rebind(&specs, "search_library", "shift+mod+f")
            .unwrap();
        assert!(!bindings.is_customised("search_library"));
    }

    #[test]
    fn a_fixed_binding_cannot_be_moved_or_unbound() {
        let specs = specs();
        let mut bindings = ShortcutBindings::defaults();
        assert!(bindings.rebind(&specs, "close_overlay", "Mod+W").is_err());
        assert!(bindings.unbind(&specs, "close_overlay").is_err());
        assert_eq!(bindings.chord(&specs[5]).as_deref(), Some("Escape"));
    }

    #[test]
    fn unbinding_leaves_the_command_with_no_chord() {
        let specs = specs();
        let mut bindings = ShortcutBindings::defaults();
        bindings.unbind(&specs, "nav_settings").unwrap();
        assert_eq!(bindings.chord(&specs[1]), None);
        assert!(bindings.is_customised("nav_settings"));
    }

    #[test]
    fn resetting_onto_a_chord_somebody_else_took_is_refused() {
        let specs = specs();
        let mut bindings = ShortcutBindings::defaults();
        bindings.rebind(&specs, "search_library", "Mod+E").unwrap();
        bindings
            .rebind(&specs, "nav_settings", "Mod+Shift+F")
            .unwrap();
        let error = bindings
            .reset(&specs, "search_library")
            .expect_err("Mod+Shift+F is now held by Settings");
        assert!(error.detail().contains("Go to Settings"), "{error:?}");
        assert_eq!(bindings.chord(&specs[2]).as_deref(), Some("Mod+E"));
    }

    #[test]
    fn reset_all_returns_every_command_to_its_default() {
        let specs = specs();
        let mut bindings = ShortcutBindings::defaults();
        bindings.rebind(&specs, "nav_settings", "Mod+Y").unwrap();
        bindings.reset_all();
        assert_eq!(bindings, ShortcutBindings::defaults());
        assert_eq!(bindings.chord(&specs[1]).as_deref(), Some("Mod+5"));
    }

    #[test]
    fn check_canonicalises_what_storage_held() {
        let mut bindings: ShortcutBindings =
            serde_json::from_str(r#"{"overrides":{"nav_settings":"ctrl+alt+num9"}}"#).unwrap();
        bindings.check(&specs()).unwrap();
        assert_eq!(
            bindings.overrides().get("nav_settings"),
            Some(&Some("Ctrl+Alt+9".to_owned()))
        );
    }

    #[test]
    fn check_drops_a_binding_for_a_command_this_build_removed() {
        let mut bindings: ShortcutBindings =
            serde_json::from_str(r#"{"overrides":{"nav_moon":"Mod+M"}}"#).unwrap();
        bindings.check(&specs()).unwrap();
        assert!(
            bindings.overrides().is_empty(),
            "expected bindings.overrides() to be empty"
        );
    }

    #[test]
    fn check_rejects_a_stored_collision() {
        let mut bindings: ShortcutBindings =
            serde_json::from_str(r#"{"overrides":{"search_library":"Mod+1"}}"#).unwrap();
        let error = bindings
            .check(&specs())
            .expect_err("Mod+1 is the library's");
        assert_eq!(error.code(), AppErrorCode::InvalidArgument);
    }

    #[test]
    fn check_rejects_a_stored_chord_that_is_not_a_chord() {
        let mut bindings: ShortcutBindings =
            serde_json::from_str(r#"{"overrides":{"nav_settings":"Mod+Sneeze"}}"#).unwrap();
        assert!(bindings.check(&specs()).is_err());
    }

    #[test]
    fn two_unbound_commands_do_not_count_as_a_collision() {
        let specs = specs();
        let mut bindings = ShortcutBindings::defaults();
        bindings.unbind(&specs, "nav_library").unwrap();
        bindings.unbind(&specs, "nav_settings").unwrap();
        bindings.check(&specs).expect("no chord cannot collide");
    }

    #[test]
    fn the_structured_form_round_trips_through_the_string_form() {
        let chord = KeyChord::command("F").with_shift();
        assert_eq!(chord.to_chord_string(), "Mod+Shift+F");
        assert_eq!(KeyChord::parse("Mod+Shift+F"), Some(chord));
        assert!(KeyChord::plain("Space").has_modifier().eq(&false));
        assert!(KeyChord::plain("Z").with_alt().has_modifier());
    }

    #[test]
    fn every_key_name_is_distinct() {
        let mut sorted: Vec<&str> = KEY_NAMES.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "a key name is listed twice");
    }

    #[test]
    fn every_alias_points_at_a_real_key_name() {
        for (alias, canonical) in KEY_ALIASES {
            assert!(
                KEY_NAMES.contains(canonical),
                "{alias} points at {canonical}, which is not a key name"
            );
        }
    }
}
