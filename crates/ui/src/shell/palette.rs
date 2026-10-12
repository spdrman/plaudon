//! The command palette (parity row S10).
//!
//! The React shell had none, and wordcraft's — the only reference
//! implementation available — is substring matching with no ranking, no
//! keyboard navigation and no way to run the hit
//! (`docs/EGUI_REFERENCE_COMPONENTS.md` §4.3), so the matcher below is
//! Plaudon's own: a ranked subsequence match over the command registry, with
//! arrow-key navigation, Enter to run and the bound chord shown on every row.

use egui::{Key, Modifiers};

use super::commands::{CommandRegistry, CommandSpec};
use super::shortcuts::{display, KeyMap};

/// Most rows the palette shows at once. The list is ranked, so the tail is
/// noise; capping it also bounds the work done per keystroke.
pub const MAX_RESULTS: usize = 20;

/// Longest query the palette accepts.
pub const MAX_QUERY_LEN: usize = 128;

/// One ranked hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Index into the list that was searched.
    pub index: usize,
    /// Higher is better.
    pub score: i32,
    /// Byte offsets in the label that matched, for highlighting.
    pub positions: Vec<usize>,
}

/// Score `query` against `text`, or `None` when it is not a subsequence.
///
/// Scoring, in order of weight: a run of consecutive characters, a character
/// at a word start, a match at the very beginning, and a short gap. It is the
/// usual shape of an editor palette matcher, chosen because it ranks
/// "Command palette" above "Collapse or expand the sidebar" for `cp`.
#[must_use]
pub fn score(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    let haystack: Vec<(usize, char)> = text.char_indices().collect();
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();

    let mut positions = Vec::with_capacity(needle.len());
    let mut score = 0_i32;
    let mut cursor = 0_usize;
    let mut previous_match: Option<usize> = None;

    for wanted in needle {
        let found = haystack[cursor..].iter().position(|(_, character)| {
            character
                .to_lowercase()
                .next()
                .is_some_and(|lowered| lowered == wanted)
        })?;
        let at = cursor + found;
        let (offset, _) = haystack[at];
        positions.push(offset);

        score += 10;
        if at == 0 {
            score += 20;
        } else if is_word_start(&haystack, at) {
            score += 12;
        }
        match previous_match {
            Some(previous) if previous + 1 == at => score += 14,
            Some(previous) => score -= i32::try_from(at - previous - 1).unwrap_or(i32::MAX).min(8),
            None => {}
        }
        previous_match = Some(at);
        cursor = at + 1;
    }
    // Prefer the shorter of two labels that match equally well.
    score -= i32::try_from(text.len() / 8).unwrap_or(0);
    Some((score, positions))
}

fn is_word_start(haystack: &[(usize, char)], at: usize) -> bool {
    at.checked_sub(1).is_some_and(|previous| {
        let before = haystack[previous].1;
        !before.is_alphanumeric() || (before.is_lowercase() && haystack[at].1.is_uppercase())
    })
}

/// Rank `commands` against `query`, best first.
///
/// An empty query keeps the registry's own order, which is the order the
/// shell declares its commands in: navigation first.
#[must_use]
pub fn rank(query: &str, commands: &[CommandSpec]) -> Vec<Match> {
    let query = query.trim();
    if query.is_empty() {
        return commands
            .iter()
            .enumerate()
            .take(MAX_RESULTS)
            .map(|(index, _)| Match {
                index,
                score: 0,
                positions: Vec::new(),
            })
            .collect();
    }
    let mut hits: Vec<Match> = commands
        .iter()
        .enumerate()
        .filter_map(|(index, spec)| {
            let label = score(query, spec.label);
            // The identifier is searched too, so `nav_dev` finds Devices, but
            // it scores lower than a label hit and never supplies highlights.
            let best = match (label, score(query, spec.id)) {
                (Some((points, positions)), _) => (points, positions),
                (None, Some((points, _))) => (points - 15, Vec::new()),
                (None, None) => return None,
            };
            Some(Match {
                index,
                score: best.0,
                positions: best.1,
            })
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| commands[a.index].label.cmp(commands[b.index].label))
    });
    hits.truncate(MAX_RESULTS);
    hits
}

/// The palette's own state.
#[derive(Debug, Default)]
pub struct Palette {
    open: bool,
    query: String,
    selected: usize,
    focus_requested: bool,
}

impl Palette {
    /// A closed palette.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the palette is showing.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Open it, empty and with the first row selected.
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.focus_requested = false;
    }

    /// Close it.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// The current query, for tests.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The row the keyboard is on, for tests.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// Draw the palette and return the command the user ran, if any.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        registry: &CommandRegistry,
        keymap: &KeyMap,
    ) -> Option<String> {
        if !self.open {
            return None;
        }
        let os = ctx.os();
        let hits = rank(&self.query, registry.all());
        if self.selected >= hits.len() {
            self.selected = hits.len().saturating_sub(1);
        }

        // Navigation is read before the text field sees the frame's events,
        // so the arrows move the list instead of the caret.
        let mut chosen: Option<String> = None;
        ctx.input_mut(|input| {
            if input.consume_key(Modifiers::NONE, Key::ArrowDown) && !hits.is_empty() {
                self.selected = (self.selected + 1) % hits.len();
            }
            if input.consume_key(Modifiers::NONE, Key::ArrowUp) && !hits.is_empty() {
                self.selected = (self.selected + hits.len() - 1) % hits.len();
            }
            if input.consume_key(Modifiers::NONE, Key::Home) {
                self.selected = 0;
            }
            if input.consume_key(Modifiers::NONE, Key::End) {
                self.selected = hits.len().saturating_sub(1);
            }
            if input.consume_key(Modifiers::NONE, Key::Enter) {
                chosen = hits
                    .get(self.selected)
                    .map(|hit| registry.all()[hit.index].id.to_owned());
            }
            if input.consume_key(Modifiers::NONE, Key::Escape) {
                self.open = false;
            }
        });
        if !self.open {
            return None;
        }

        let modal = egui::Modal::new(egui::Id::new("plaudon-command-palette")).show(ctx, |ui| {
            ui.set_width(520.0);
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Type a command")
                    .desired_width(f32::INFINITY),
            );
            if !self.focus_requested {
                field.request_focus();
                self.focus_requested = true;
            }
            if field.changed() {
                self.query.truncate(
                    self.query
                        .char_indices()
                        .nth(MAX_QUERY_LEN)
                        .map_or(self.query.len(), |(offset, _)| offset),
                );
                self.selected = 0;
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    if hits.is_empty() {
                        ui.label("No matching command");
                        return;
                    }
                    for (row, hit) in hits.iter().enumerate() {
                        let spec = &registry.all()[hit.index];
                        let selected = row == self.selected;
                        let response = ui.selectable_label(
                            selected,
                            command_row_text(spec, keymap.primary_chord(spec).as_deref(), os),
                        );
                        if selected {
                            response.scroll_to_me(None);
                        }
                        if response.clicked() {
                            chosen = Some(spec.id.to_owned());
                        }
                    }
                });
        });
        if modal.should_close() {
            self.open = false;
        }
        if chosen.is_some() {
            self.open = false;
        }
        chosen
    }
}

/// `Label  ·  Group  ·  Chord`, as one row reads to a screen reader.
fn command_row_text(
    spec: &CommandSpec,
    chord: Option<&str>,
    os: egui::os::OperatingSystem,
) -> String {
    match chord {
        Some(chord) => format!(
            "{}  ·  {}  ·  {}",
            spec.label,
            spec.group.label(),
            display(chord, os)
        ),
        None => format!("{}  ·  {}", spec.label, spec.group.label()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::commands::{CommandGroup, CommandRegistry, ShellAction};

    #[test]
    fn a_subsequence_matches_and_a_non_subsequence_does_not() {
        assert!(score("cp", "Command palette").is_some());
        assert!(score("cmdpal", "Command palette").is_some());
        assert!(score("zzz", "Command palette").is_none());
        assert!(score("", "anything").is_some());
    }

    #[test]
    fn ranking_puts_the_word_start_match_first() {
        let registry = CommandRegistry::with_builtins();
        let hits = rank("cp", registry.all());
        let best = &registry.all()[hits[0].index];
        assert_eq!(best.id, "command_palette");
    }

    #[test]
    fn a_prefix_beats_a_scattered_match() {
        let registry = CommandRegistry::with_builtins();
        let hits = rank("lib", registry.all());
        assert_eq!(registry.all()[hits[0].index].id, "nav_library");
    }

    #[test]
    fn the_identifier_is_searchable_but_ranks_below_a_label() {
        let commands = vec![
            CommandSpec::new(
                "nav_devices",
                "Devices",
                CommandGroup::Navigation,
                ShellAction::Screen,
            ),
            CommandSpec::new(
                "other",
                "Nav development helper",
                CommandGroup::General,
                ShellAction::Screen,
            ),
        ];
        let hits = rank("navdev", &commands);
        assert_eq!(hits.len(), 2);
        assert_eq!(commands[hits[0].index].id, "other");
    }

    #[test]
    fn results_are_capped_so_a_blank_query_cannot_paint_the_world() {
        let registry = CommandRegistry::with_builtins();
        assert!(rank("", registry.all()).len() <= MAX_RESULTS);
    }

    #[test]
    fn highlight_positions_are_byte_offsets_into_the_label() {
        let (_, positions) = score("cp", "Command palette").expect("matches");
        assert_eq!(positions, vec![0, 8]);
    }
}
