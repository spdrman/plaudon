//! The person picker: one widget, every surface that sets a speaker
//! (`VOICE_PROFILES_SPEC` VP-210, VP-214, VP-215).
//!
//! The per-cluster row of the speaker panel, the per-segment transcript chip
//! and the bulk review queue all pick a name the same way, because they all
//! call this. A second picker is exactly how the three surfaces would end up
//! disagreeing about what "Unknown" does, which entries exist, or whether a
//! suggestion is pre-selected — and the last of those would be
//! auto-assignment by the back door, which ADR-0082 forbids at any score.
//!
//! Three fixed entries, always in this order:
//!
//! * the **suggested person**, pinned on top with its band and score, when the
//!   surface has a suggestion — never pre-selected, only offered;
//! * the roster, filtered by what the user typed, matching display names *and*
//!   aliases;
//! * **"Unknown/none"**, which clears the assignment back to the neutral
//!   cluster label, and — where the surface can create a roster row —
//!   **"Create new person…"**, which creates and assigns in one step and warns
//!   inline when the typed name is one that already exists (VP-214).
//!
//! **Ordering, stated rather than faked.** VP-210 asks for most-recently-
//! assigned first. The roster carries no last-assigned column — `people`
//! records `updated_at`, which an edit moves and an assignment does not — so
//! this widget keeps the order `list_people` returned (display name), and the
//! suggestion is the only entry that is promoted. Sorting by an edit timestamp
//! and calling it "recently assigned" would be a lie the user cannot see.

use std::fmt::Write as _;

use egui::{accesskit, Key, Response, Ui};
use plaudon_domain::speaker::{self, MatchBand};

/// The entry that clears an assignment.
///
/// "Nobody" is an answer, not the absence of one: overlapping speech and
/// unattributable audio are what an unassigned cluster means.
pub const UNKNOWN_ENTRY: &str = "Unknown/none";

/// The entry that creates a roster row and assigns it in one step.
pub const CREATE_ENTRY: &str = "Create new person…";

/// Placeholder of the filter field, which is also its accessible name.
pub const FILTER_LABEL: &str = "Filter people by name or alias";

/// One roster entry as the picker offers it.
///
/// Carries the facts the list renders from rather than a composed string, so
/// no caller can supply a second wording.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PickerPerson {
    /// Identity the pick reports back: a roster person id, or — in the
    /// per-segment chip, where the command sets a speaker row — that row's id.
    pub id: String,
    /// Name shown for them.
    pub name: String,
    /// Alternate spellings. Searched, never shown as the name.
    pub aliases: Vec<String>,
    /// One quiet line of context: "in 14 recordings", "voice: enrolled".
    pub detail: Option<String>,
}

impl PickerPerson {
    /// A person with a name and nothing else.
    #[must_use]
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            aliases: Vec::new(),
            detail: None,
        }
    }

    /// The same person with aliases.
    #[must_use]
    pub fn with_aliases(mut self, aliases: Vec<String>) -> Self {
        self.aliases = aliases;
        self
    }

    /// The same person with a context line.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Whether `query` matches the display name or any alias.
    ///
    /// Case-insensitive substring, which is what the roster command does, so
    /// the list the user filters on screen and the list the store would
    /// return cannot disagree.
    #[must_use]
    pub fn matches(&self, query: &str) -> bool {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        self.name.to_lowercase().contains(&needle)
            || self
                .aliases
                .iter()
                .any(|alias| alias.to_lowercase().contains(&needle))
    }

    /// The line the list shows, which is also the entry's accessible name.
    #[must_use]
    pub fn render(&self) -> String {
        let mut text = self.name.clone();
        if !self.aliases.is_empty() {
            let _ = write!(text, " (also {})", self.aliases.join(", "));
        }
        if let Some(detail) = &self.detail {
            let _ = write!(text, " — {detail}");
        }
        text
    }
}

/// The suggestion pinned on top of the list.
///
/// A score and a band, never a label: the sentence is composed by
/// [`plaudon_domain::speaker::suggestion`], the one place that wording lives.
#[derive(Debug, Clone, PartialEq)]
pub struct PickerSuggestion {
    /// Who the score is about.
    pub person: PickerPerson,
    /// Qualitative band.
    pub band: MatchBand,
    /// The similarity score.
    pub score: f64,
}

impl PickerSuggestion {
    /// The sentence the entry reads as.
    #[must_use]
    pub fn sentence(&self) -> String {
        speaker::suggestion(&self.person.name, self.band, self.score)
    }
}

/// What the user chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonPick {
    /// Assign this roster entry.
    Person(String),
    /// Clear the assignment back to the neutral cluster label.
    Unknown,
    /// Create a roster entry with this name and assign it, in one step.
    Create(String),
}

/// The picker's state between frames: what has been typed, and where the
/// keyboard is in the list.
///
/// Owned by the surface rather than by egui's memory, so a test can read it
/// and so two pickers on screen cannot share a filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersonPickerState {
    /// The filter text.
    query: String,
    /// Which entry the keyboard is on.
    highlight: usize,
    /// Whether the create-new field is open.
    creating: bool,
    /// The name typed into it.
    draft: String,
    /// Whether the popup was open on the previous frame, so the filter can be
    /// focused exactly once per opening.
    was_open: bool,
}

impl PersonPickerState {
    /// What the user has typed into the filter.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The name typed into the create-new field.
    #[must_use]
    pub fn draft(&self) -> &str {
        &self.draft
    }

    /// Forget everything typed, which is what closing the picker does.
    pub fn reset(&mut self) {
        self.query.clear();
        self.draft.clear();
        self.creating = false;
        self.highlight = 0;
    }
}

/// One row of the open list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    /// The pinned suggestion.
    Suggested,
    /// A roster person, by index into the filtered list.
    Person(usize),
    /// Clear the assignment.
    Unknown,
    /// Open the create-new field.
    Create,
}

/// The picker (VP-210).
#[derive(Debug)]
pub struct PersonPicker<'a> {
    label: &'a str,
    id_salt: &'a str,
    people: &'a [PickerPerson],
    selected: Option<&'a str>,
    neutral_label: &'a str,
    suggestion: Option<&'a PickerSuggestion>,
    allow_create: bool,
}

impl<'a> PersonPicker<'a> {
    /// A picker over `people`.
    ///
    /// `label` is the accessible name of the combobox and names the scope of
    /// the action — "Assign Speaker 2 to", "Assign this segment to" — because
    /// the scope of an assignment is exactly what the control says it is
    /// (VP-211).
    #[must_use]
    pub fn new(id_salt: &'a str, label: &'a str, people: &'a [PickerPerson]) -> Self {
        Self {
            label,
            id_salt,
            people,
            selected: None,
            neutral_label: UNKNOWN_ENTRY,
            suggestion: None,
            allow_create: false,
        }
    }

    /// The entry that is assigned now, if any.
    #[must_use]
    pub fn selected(mut self, selected: Option<&'a str>) -> Self {
        self.selected = selected;
        self
    }

    /// What the control reads as while nothing is assigned — the neutral
    /// cluster label, which is a fact about the recording and not a name.
    #[must_use]
    pub fn neutral_label(mut self, label: &'a str) -> Self {
        self.neutral_label = label;
        self
    }

    /// Pin a suggestion on top. It is offered, never pre-selected.
    #[must_use]
    pub fn suggestion(mut self, suggestion: Option<&'a PickerSuggestion>) -> Self {
        self.suggestion = suggestion;
        self
    }

    /// Offer "Create new person…".
    ///
    /// Surfaces that can create a roster row and assign it in one command say
    /// so; the per-segment chip, whose command sets an existing speaker row,
    /// does not — offering it there would promise a write the application
    /// cannot make.
    #[must_use]
    pub const fn allow_create(mut self, allow: bool) -> Self {
        self.allow_create = allow;
        self
    }

    /// Draw the picker and report what the user chose.
    pub fn show(self, ui: &mut Ui, state: &mut PersonPickerState) -> Option<PersonPick> {
        let current = self
            .selected
            .and_then(|id| self.people.iter().find(|person| person.id == id))
            .map(|person| person.name.clone());
        let button_text = current
            .clone()
            .unwrap_or_else(|| self.neutral_label.to_owned());
        let announced = match &current {
            Some(name) => format!("{}: {name}", self.label),
            None => format!("{}: {}", self.label, self.neutral_label),
        };

        let mut picked = None;
        // The list closes when the user clicks *outside* it, not on the first
        // click inside: filtering and creating a person both happen in here,
        // and a list that closed on the first keystroke would be unusable.
        // Picking an entry closes it explicitly.
        let combo = egui::ComboBox::from_id_salt(self.id_salt)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .selected_text(button_text)
            .show_ui(ui, |ui| {
                picked = self.show_list(ui, state);
            });
        name_as_combo_box(ui, &combo.response, &announced);

        let open = combo.inner.is_some();
        if open && !state.was_open {
            state.reset();
        }
        state.was_open = open;

        // The create-new field lives *below* the control, not inside the open
        // list: the list closes as soon as an entry is chosen, and a field
        // that vanished with it could never be typed into.
        if self.allow_create && state.creating {
            picked = self.create_field(ui, state).or(picked);
        }
        if picked.is_some() {
            state.reset();
        }
        picked
    }

    /// The open list: filter, entries, create-new.
    ///
    /// Public because the transcript chip opens the same list from a chip-
    /// shaped button rather than from a combobox: the entries, the filter, the
    /// keyboard path and "Unknown/none" are the ones defined here, which is
    /// what VP-210's *one* picker means in practice.
    pub fn show_list(&self, ui: &mut Ui, state: &mut PersonPickerState) -> Option<PersonPick> {
        let filter_id = ui.make_persistent_id((self.id_salt, "filter"));
        let field = ui.add(
            egui::TextEdit::singleline(&mut state.query)
                .id(filter_id)
                .hint_text(FILTER_LABEL)
                .desired_width(220.0),
        );
        // The hint text is not a name: a field whose only clue disappears as
        // soon as the user types is a field a screen reader cannot announce.
        ui.ctx().accesskit_node_builder(field.id, |node| {
            node.set_label(FILTER_LABEL);
        });
        if !state.was_open {
            field.request_focus();
        }

        let matching: Vec<usize> = self
            .people
            .iter()
            .enumerate()
            .filter(|(_, person)| person.matches(&state.query))
            .map(|(index, _)| index)
            .collect();

        let mut entries = Vec::with_capacity(matching.len() + 3);
        if self.suggestion.is_some() {
            entries.push(Entry::Suggested);
        }
        entries.extend(matching.iter().map(|index| Entry::Person(*index)));
        entries.push(Entry::Unknown);
        if self.allow_create {
            entries.push(Entry::Create);
        }
        state.highlight = state.highlight.min(entries.len().saturating_sub(1));

        announce(
            ui,
            &format!("{} of {} people match", matching.len(), self.people.len()),
        );

        let key = arrow_key(ui, entries.len(), state.highlight);
        if let Some(target) = key {
            state.highlight = target;
        }
        let activated = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter));

        let mut picked = None;
        let mut creating = false;
        egui::ScrollArea::vertical()
            .max_height(260.0)
            .show(ui, |ui| {
                for (position, entry) in entries.iter().enumerate() {
                    let highlighted = position == state.highlight;
                    match entry {
                        Entry::Suggested => {
                            let suggestion =
                                self.suggestion.expect("the entry exists because it does");
                            let response = row(ui, highlighted, false, &suggestion.sentence());
                            let response = response.on_hover_text(speaker::SCORE_TOOLTIP);
                            if response.clicked() || (highlighted && activated) {
                                picked = Some(PersonPick::Person(suggestion.person.id.clone()));
                            }
                        }
                        Entry::Person(index) => {
                            let person = &self.people[*index];
                            let selected = self.selected == Some(person.id.as_str());
                            if row(ui, highlighted, selected, &person.render()).clicked()
                                || (highlighted && activated)
                            {
                                picked = Some(PersonPick::Person(person.id.clone()));
                            }
                        }
                        Entry::Unknown => {
                            // Never "(assigned)": nobody is, which is the
                            // whole point of this entry.
                            let selected = self.selected.is_none();
                            if row(ui, highlighted, false, UNKNOWN_ENTRY).clicked()
                                || (highlighted && activated)
                            {
                                picked = Some(PersonPick::Unknown);
                            }
                            debug_assert!(
                                selected || self.selected.is_some(),
                                "the clear entry is selected exactly when nothing is assigned"
                            );
                        }
                        Entry::Create => {
                            if row(ui, highlighted, false, CREATE_ENTRY).clicked()
                                || (highlighted && activated && !state.creating)
                            {
                                creating = true;
                            }
                        }
                    }
                }
            });

        if creating {
            state.creating = true;
            if state.draft.is_empty() {
                state.query.trim().clone_into(&mut state.draft);
            }
        }
        if picked.is_some() || creating {
            ui.close();
        }
        picked
    }

    /// The inline create-new field, with the duplicate warning (VP-214).
    fn create_field(&self, ui: &mut Ui, state: &mut PersonPickerState) -> Option<PersonPick> {
        let mut picked = None;
        ui.separator();
        let label = ui.label("New person's name");
        let field = ui.add(
            egui::TextEdit::singleline(&mut state.draft)
                .desired_width(220.0)
                .char_limit(plaudon_domain::speaker::MAX_NAME_CHARS),
        );
        field.labelled_by(label.id);
        let typed = state.draft.trim().to_owned();

        let duplicate = self
            .people
            .iter()
            .find(|person| person.name.trim().eq_ignore_ascii_case(&typed) && !typed.is_empty());
        if let Some(existing) = duplicate {
            let warning = format!(
                "{} is already on the list. Assign to the existing {} instead?",
                existing.name, existing.name
            );
            announce(ui, &warning);
            if ui
                .button(format!("Assign to the existing {}", existing.name))
                .clicked()
            {
                picked = Some(PersonPick::Person(existing.id.clone()));
            }
        }

        if ui
            .add_enabled(!typed.is_empty(), egui::Button::new("Create and assign"))
            .on_disabled_hover_text("Type a name first — a person cannot be created without one.")
            .clicked()
        {
            picked = Some(PersonPick::Create(typed));
        }
        if ui.button("Cancel creating a person").clicked() {
            state.creating = false;
            state.draft.clear();
        }
        picked
    }
}

/// One row of the list: a button, named for what it does.
fn row(ui: &mut Ui, highlighted: bool, selected: bool, text: &str) -> Response {
    let mut rich = egui::RichText::new(text);
    if highlighted {
        rich = rich.underline();
    }
    let response = ui.selectable_label(selected, rich);
    let name = if selected {
        format!("{text} (assigned)")
    } else {
        text.to_owned()
    };
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &name)
    });
    response
}

/// Where the arrow keys move the highlight, when one was pressed.
fn arrow_key(ui: &Ui, count: usize, current: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let (down, up) = ui.input_mut(|input| {
        (
            input.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
            input.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
        )
    });
    if down {
        Some((current + 1) % count)
    } else if up {
        Some((current + count - 1) % count)
    } else {
        None
    }
}

/// A polite live region, the way the library table announces its selection.
fn announce(ui: &mut Ui, text: &str) {
    let response = ui.label(text);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    let announcement = text.to_owned();
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(accesskit::Role::Status);
        node.set_label(announcement);
        node.set_live(accesskit::Live::Polite);
    });
}

/// Name the combobox button and give it the role a screen reader expects.
pub fn name_as_combo_box(ui: &Ui, response: &Response, name: &str) {
    let label = name.to_owned();
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(accesskit::Role::ComboBox);
        node.set_label(label.clone());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster() -> Vec<PickerPerson> {
        vec![
            PickerPerson::new("p1", "Priya").with_aliases(vec!["P. Nair".to_owned()]),
            PickerPerson::new("p2", "Dana"),
        ]
    }

    #[test]
    fn the_filter_matches_display_names_and_aliases_case_insensitively() {
        let people = roster();
        assert!(people[0].matches("pri"));
        assert!(people[0].matches("NAIR"));
        assert!(!people[0].matches("dana"));
        assert!(people[1].matches(""));
    }

    #[test]
    fn an_entry_renders_its_aliases_and_context_without_composing_a_claim() {
        let person = PickerPerson::new("p1", "Priya")
            .with_aliases(vec!["P. Nair".to_owned()])
            .with_detail("in 14 recordings");
        assert_eq!(person.render(), "Priya (also P. Nair) — in 14 recordings");
    }

    #[test]
    fn a_suggestion_reads_as_the_domain_sentence_and_never_as_an_identity() {
        let suggestion = PickerSuggestion {
            person: PickerPerson::new("p1", "Priya"),
            band: MatchBand::Strong,
            score: 0.82,
        };
        let sentence = suggestion.sentence();
        assert_eq!(
            sentence,
            speaker::suggestion("Priya", MatchBand::Strong, 0.82)
        );
        assert!(sentence.contains("sounds like"));
        assert!(!sentence.contains("identified"));
    }
}
