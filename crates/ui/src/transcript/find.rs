//! The find-and-replace bar.
//!
//! Search is literal: no regular expressions, matching the behaviour the React
//! pane had and keeping a keystroke over a 30,000-segment transcript linear.
//! Replace is new in this editor (parity matrix T10); replace-all is a single
//! undo step, which is the only way "replace all" is safe to offer at this
//! scale.

use egui::{Align, Layout, Ui};
use plaudon_transcript_doc::search::SearchQuery;
use plaudon_transcript_doc::session::EditSession;

/// State of the find bar, owned by the caller so it survives frames.
///
/// The flags are independent user choices (shown, replace row shown, match
/// case, whole word, focus pending), not a state machine: collapsing them into
/// enums would only hide what the find bar actually is.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default)]
pub struct FindBarState {
    /// Whether the bar is shown.
    pub open: bool,
    /// Whether the replace controls are shown.
    pub replace_open: bool,
    /// Text to find.
    pub needle: String,
    /// Replacement text.
    pub replacement: String,
    /// Whether case matters.
    pub case_sensitive: bool,
    /// Whether matches must be whole words.
    pub whole_word: bool,
    /// Set when the bar should take keyboard focus on the next frame.
    pub focus_requested: bool,
}

impl FindBarState {
    /// Open the bar and ask for focus.
    pub fn open(&mut self) {
        self.open = true;
        self.focus_requested = true;
    }

    /// Open the bar with the replace row shown.
    pub fn open_replace(&mut self) {
        self.open();
        self.replace_open = true;
    }

    /// Close the bar and drop the search.
    pub fn close(&mut self, session: &mut EditSession) {
        self.open = false;
        self.focus_requested = false;
        session.clear_search();
    }

    fn query(&self) -> SearchQuery {
        SearchQuery {
            needle: self.needle.clone(),
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
        }
    }
}

/// What the find bar did this frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FindOutcome {
    /// The user closed the bar.
    pub closed: bool,
    /// The document changed (a replace happened).
    pub replaced: usize,
    /// The caret moved to a match.
    pub moved_to_match: bool,
}

/// Draw the bar and apply its actions to the session.
pub fn show(ui: &mut Ui, state: &mut FindBarState, session: &mut EditSession) -> FindOutcome {
    let mut outcome = FindOutcome::default();
    if !state.open {
        return outcome;
    }
    let editable = session.document().is_editable();

    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            let caption = ui.label("Find");
            let field = ui
                .add(
                    egui::TextEdit::singleline(&mut state.needle)
                        .hint_text("Find in transcript")
                        .desired_width(180.0),
                )
                .labelled_by(caption.id);
            field.widget_info(|| egui::WidgetInfo {
                label: Some("Find in transcript".to_owned()),
                ..egui::WidgetInfo::text_edit(true, "", &state.needle, "Find in transcript")
            });
            if state.focus_requested {
                field.request_focus();
                state.focus_requested = false;
            }
            if field.changed() {
                session.set_search_query(state.query());
            }

            let case = ui.checkbox(&mut state.case_sensitive, "Match case");
            let word = ui.checkbox(&mut state.whole_word, "Whole word");
            if case.changed() || word.changed() {
                session.set_search_query(state.query());
            }

            if ui
                .button("Previous")
                .on_hover_text("Previous match")
                .clicked()
            {
                session.focus_previous_match();
                outcome.moved_to_match = true;
            }
            if ui.button("Next").on_hover_text("Next match").clicked() {
                session.focus_next_match();
                outcome.moved_to_match = true;
            }

            let status = match session.search().status().as_str() {
                "" => "Type to search".to_owned(),
                other => other.to_owned(),
            };
            let label = ui.label(&status);
            label.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &status));

            ui.toggle_value(&mut state.replace_open, "Replace");
            if ui.button("Close").clicked() {
                outcome.closed = true;
            }
        });

        if state.replace_open {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let caption = ui.label("Replace with");
                let field = ui
                    .add(
                        egui::TextEdit::singleline(&mut state.replacement)
                            .hint_text("Replace with")
                            .desired_width(180.0),
                    )
                    .labelled_by(caption.id);
                field.widget_info(|| egui::WidgetInfo {
                    label: Some("Replace with".to_owned()),
                    ..egui::WidgetInfo::text_edit(editable, "", &state.replacement, "Replace with")
                });
                let has_matches = !session.search().is_empty();
                let replace_one = ui
                    .add_enabled(editable && has_matches, egui::Button::new("Replace"))
                    .clicked();
                if replace_one
                    && session
                        .replace_current_match(&state.replacement)
                        .unwrap_or(false)
                {
                    outcome.replaced += 1;
                }
                if ui
                    .add_enabled(editable && has_matches, egui::Button::new("Replace all"))
                    .on_hover_text("Replaces every match as one undoable change")
                    .clicked()
                {
                    outcome.replaced += session.replace_all(&state.replacement).unwrap_or(0);
                }
            });
        }
    });

    if outcome.closed {
        state.close(session);
    }
    outcome
}
