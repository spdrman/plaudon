//! The filter bar (§5.2 L4): free text with a debounce, four single-choice
//! filters, a date range and a way to clear them all.
//!
//! The debounce is the reason this has state at all: typing must not issue one
//! `list_recordings` per keystroke. The field keeps what the user typed and
//! emits a [`QueryAction::Query`] once they stop, and asks for exactly one
//! delayed repaint while a change is pending rather than repainting every
//! frame.

use std::time::Duration;

use egui::Ui;
use plaudon_app::dto::{ImportStatus, RecordingSummary, TranscriptionStatus};

use super::query::{
    tag_options, LibraryQuery, QueryAction, IMPORT_STATUS_OPTIONS, SEARCH_DEBOUNCE,
    SOURCE_OPTIONS, TRANSCRIPTION_STATUS_OPTIONS,
};
use super::status::{import_status, transcription_status};

/// Frame-to-frame state of the filter bar.
#[derive(Debug, Default)]
pub struct ToolbarState {
    text: String,
    text_initialised: bool,
    pending_since: Option<f64>,
}

impl ToolbarState {
    /// What is currently typed in the search field.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replace the field's contents, for example after Clear filters.
    pub fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.pending_since = None;
        self.text_initialised = true;
    }
}

/// What the toolbar asked for this frame.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ToolbarOutput {
    /// Query changes to apply, in the order they were made.
    pub actions: Vec<QueryAction>,
    /// The user asked for cross-recording search.
    pub open_search: bool,
}

/// Draw the filter bar.
pub fn toolbar(
    ui: &mut Ui,
    state: &mut ToolbarState,
    query: &LibraryQuery,
    rows: &[RecordingSummary],
) -> ToolbarOutput {
    let mut output = ToolbarOutput::default();
    if !state.text_initialised {
        state.set_text(&query.query);
    }

    ui.horizontal_wrapped(|ui| {
        let label = ui.label("Search recordings");
        let field = ui.add(
            egui::TextEdit::singleline(&mut state.text)
                .hint_text("Title contains…")
                .desired_width(200.0),
        );
        let field = field.labelled_by(label.id);
        if field.changed() {
            state.pending_since = Some(ui.input(|input| input.time));
        }

        output
            .actions
            .extend(choice_filter(
                ui,
                "Import status",
                "All import states",
                query.import_status.first().copied(),
                &IMPORT_STATUS_OPTIONS,
                |status| import_status(status).label.to_owned(),
                QueryAction::ImportStatus,
            ));

        output
            .actions
            .extend(choice_filter(
                ui,
                "Transcription status",
                "All transcription states",
                query.transcription_status.first().copied(),
                &TRANSCRIPTION_STATUS_OPTIONS,
                |status| transcription_status(status).label.to_owned(),
                QueryAction::TranscriptionStatus,
            ));

        let sources: Vec<String> = SOURCE_OPTIONS
            .iter()
            .map(|(value, _)| (*value).to_owned())
            .collect();
        output.actions.extend(choice_filter(
            ui,
            "Source",
            "All sources",
            query.sources.first().cloned(),
            &sources,
            |value: String| {
                SOURCE_OPTIONS
                    .iter()
                    .find(|(candidate, _)| *candidate == value)
                    .map_or(value.clone(), |(_, label)| (*label).to_owned())
            },
            QueryAction::Source,
        ));

        let tags = tag_options(rows, &query.tags);
        output.actions.extend(choice_filter(
            ui,
            "Tag",
            "All tags",
            query.tags.first().cloned(),
            &tags,
            |tag: String| tag,
            QueryAction::Tag,
        ));
    });

    ui.horizontal_wrapped(|ui| {
        if let Some(action) = day_field(ui, "From date (YYYY-MM-DD)", query.date_from.as_deref()) {
            output.actions.push(QueryAction::DateFrom(action));
        }
        if let Some(action) = day_field(ui, "To date (YYYY-MM-DD)", query.date_to.as_deref()) {
            output.actions.push(QueryAction::DateTo(action));
        }
        if query.has_active_filters() && ui.button("Clear filters").clicked() {
            state.set_text("");
            output.actions.push(QueryAction::ClearFilters);
        }
        if ui.button("Search across all recordings").clicked() {
            output.open_search = true;
        }
    });

    if let Some(since) = state.pending_since {
        let now = ui.input(|input| input.time);
        let elapsed = Duration::from_secs_f64((now - since).max(0.0));
        if elapsed >= SEARCH_DEBOUNCE {
            state.pending_since = None;
            if state.text != query.query {
                output.actions.push(QueryAction::Query(state.text.clone()));
            }
        } else {
            // One scheduled wake-up, not a repaint every frame.
            ui.ctx().request_repaint_after(SEARCH_DEBOUNCE - elapsed);
        }
    }

    output
}

/// A single-choice filter rendered as a combo box with an explicit "all"
/// entry, because an empty combo box does not say that nothing is filtered.
fn choice_filter<T, F, A>(
    ui: &mut Ui,
    label: &str,
    all_label: &str,
    selected: Option<T>,
    options: &[T],
    describe: F,
    action: A,
) -> Option<QueryAction>
where
    T: Clone + PartialEq,
    F: Fn(T) -> String,
    A: Fn(Option<T>) -> QueryAction,
{
    let mut chosen: Option<Option<T>> = None;
    let selected_text = selected
        .clone()
        .map_or_else(|| all_label.to_owned(), &describe);
    egui::ComboBox::from_label(label)
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            if ui.selectable_label(selected.is_none(), all_label).clicked() {
                chosen = Some(None);
            }
            for option in options {
                let is_selected = selected.as_ref() == Some(option);
                if ui
                    .selectable_label(is_selected, describe(option.clone()))
                    .clicked()
                {
                    chosen = Some(Some(option.clone()));
                }
            }
        });
    chosen.map(action)
}

/// A calendar-day field. An incomplete or malformed day is simply not a
/// bound — [`super::query::date_bound_from`] drops it — so the user can type
/// freely without the list jumping on every character.
fn day_field(ui: &mut Ui, label: &str, value: Option<&str>) -> Option<Option<String>> {
    let mut text = value.unwrap_or_default().to_owned();
    let field_label = ui.label(label);
    let field = ui.add(
        egui::TextEdit::singleline(&mut text)
            .hint_text("2026-03-12")
            .desired_width(110.0),
    );
    let field = field.labelled_by(field_label.id);
    if field.changed() {
        Some((!text.is_empty()).then_some(text))
    } else {
        None
    }
}

/// Re-exported so a screen can offer the same statuses in its own menus.
#[must_use]
pub const fn import_status_options() -> [ImportStatus; 4] {
    IMPORT_STATUS_OPTIONS
}

/// Re-exported so a screen can offer the same statuses in its own menus.
#[must_use]
pub const fn transcription_status_options() -> [TranscriptionStatus; 6] {
    TRANSCRIPTION_STATUS_OPTIONS
}
