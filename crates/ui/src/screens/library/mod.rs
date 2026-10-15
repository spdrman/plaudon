//! The library screen (§5.2): a searchable, filterable, sortable, virtualized
//! list of every recording, with import.
//!
//! Recording detail is **opened** from here and rendered by
//! [`crate::screens::recording`]; detail is not a top-level destination
//! (§5.1), which is why this screen reports an intent rather than navigating
//! itself.
//!
//! The screen owns no rules. Paging and event-driven refetching live in
//! [`list`], filters and sorting in [`query`], the selection model in
//! [`selection`], import outcomes in [`import`] — all testable without a
//! window.

pub mod duplicate;
pub mod format;
pub mod import;
pub mod list;
pub mod panels;
pub mod query;
pub mod selection;
pub mod status;
pub mod table;
pub mod testing;
pub mod toolbar;

use std::collections::BTreeSet;

use egui::Ui;
use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{
    DuplicatePolicy, EventKind, StartProcessingRequest, StartProcessingResponse,
};
use plaudon_domain::error::ErrorEnvelope;
use plaudon_domain::ids::RecordingId;

use crate::widgets::error_notice::{error_notice, ErrorNoticeAction};
use duplicate::{duplicate_dialog, DuplicateDecision};
use import::{dropped_paths, hovering_files, ImportIntent, ImportSession};
use list::{RecordingList, Sender};
use query::{LibraryQuery, QueryAction};
use table::{BatchOperation, RecordingTable, TableState};
use toolbar::{toolbar, ToolbarState};

mod screen;

/// The stage `start_processing` is asked for from the library's batch menu.
const TRANSCRIBE_STAGE: &str = "transcribe";

/// Something the screen needs the host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryIntent {
    /// Open this recording's detail view.
    OpenRecording(RecordingId),
    /// Open the cross-recording search view.
    OpenSearch,
    /// Open settings.
    OpenSettings,
    /// Ask the OS for audio files to import. The host owns the dialog; the
    /// resulting paths come back through [`LibraryScreen::import_paths`].
    PickFiles,
    /// A line for the status bar.
    Status(String),
}

/// Outcome counts of a batch `start_processing`.
#[derive(Debug, Default)]
struct BatchProgress {
    pending: BTreeSet<CommandId>,
    queued: usize,
    consent_required: usize,
    blocked: usize,
    failed: usize,
}

impl BatchProgress {
    fn record(&mut self, response: &StartProcessingResponse) {
        match response.dispatch.as_str() {
            "queued" => self.queued += 1,
            "consent_required" => self.consent_required += 1,
            _ => self.blocked += 1,
        }
    }

    /// States every outcome class, never only the happy one.
    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.queued > 0 {
            parts.push(format!("{} queued for transcription", self.queued));
        }
        if self.consent_required > 0 {
            parts.push(format!(
                "{} need cloud consent — open the recording to review it",
                self.consent_required
            ));
        }
        if self.blocked > 0 {
            parts.push(format!("{} blocked", self.blocked));
        }
        if self.failed > 0 {
            parts.push(format!("{} rejected", self.failed));
        }
        if parts.is_empty() {
            "Nothing to transcribe.".to_owned()
        } else {
            format!("{}.", parts.join(", "))
        }
    }
}

/// The library screen.
///
/// It owns the two views that live *inside* Library rather than next to it:
/// cross-recording search and the recording detail container (§5.1 — detail
/// is not a top-level destination).
#[derive(Debug)]
pub struct LibraryScreen {
    list: RecordingList,
    table: TableState,
    toolbar: ToolbarState,
    import: ImportSession,
    processing: BatchProgress,
    started: bool,
    search: crate::screens::search::SearchScreen,
    detail: crate::screens::recording::RecordingDetailScreen,
    panels: Box<dyn crate::screens::recording::DetailPanels>,
}

impl Default for LibraryScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl LibraryScreen {
    /// A screen that has not fetched anything yet.
    ///
    /// The detail view's panels are the ones a host without a player, a
    /// transcript editor or a review panel can supply; use
    /// [`LibraryScreen::with_panels`] to plug the real widgets in.
    #[must_use]
    pub fn new() -> Self {
        Self::with_panels(Box::new(panels::UnbuiltPanels))
    }

    /// A screen whose detail view hosts `panels`.
    #[must_use]
    pub fn with_panels(panels: Box<dyn crate::screens::recording::DetailPanels>) -> Self {
        Self {
            list: RecordingList::new(LibraryQuery::default()),
            table: TableState::default(),
            toolbar: ToolbarState::default(),
            import: ImportSession::default(),
            processing: BatchProgress::default(),
            started: false,
            search: crate::screens::search::SearchScreen::new(),
            detail: crate::screens::recording::RecordingDetailScreen::new(),
            panels,
        }
    }

    /// The paged list, for tests and for the screens that embed it.
    #[must_use]
    pub const fn list(&self) -> &RecordingList {
        &self.list
    }

    /// The current selection.
    #[must_use]
    pub const fn selection(&self) -> &selection::Selection {
        &self.table.selection
    }

    /// The import batch in progress, if any.
    #[must_use]
    pub const fn import_session(&self) -> &ImportSession {
        &self.import
    }

    /// Ask for the next page, as the grid does when it reaches the bottom.
    pub fn load_more(&mut self, send: Sender<'_>) {
        self.list.load_more(send);
    }

    /// Import paths the host obtained from the OS picker.
    pub fn import_paths(&mut self, paths: &[String], send: Sender<'_>) {
        self.import.start(paths, DuplicatePolicy::Ask, send);
    }

    /// Route a dispatcher answer.
    pub fn on_response(
        &mut self,
        id: CommandId,
        result: &Result<AppResponse, ErrorEnvelope>,
        send: Sender<'_>,
    ) -> Vec<LibraryIntent> {
        let mut intents = Vec::new();
        if self.list.on_response(id, result) {
            return intents;
        }
        if self.import.on_response(id, result) {
            // A finished import changes the library; the backend also emits
            // `library.changed`, but refetching here keeps the list correct
            // even if that event is coalesced away.
            if !self.import.is_pending() {
                self.list.refresh(send);
                intents.push(LibraryIntent::Status(self.import.announcement().to_owned()));
            }
            return intents;
        }
        if self.processing.pending.remove(&id) {
            match result {
                Ok(AppResponse::StartProcessing(response)) => self.processing.record(response),
                Ok(_) => {}
                Err(_) => self.processing.failed += 1,
            }
            if self.processing.pending.is_empty() {
                intents.push(LibraryIntent::Status(self.processing.describe()));
                self.processing = BatchProgress::default();
            }
        }
        intents
    }

    /// Route a §15.3 event.
    pub fn on_ui_event(&mut self, kind: EventKind, send: Sender<'_>) {
        self.list.on_ui_event(kind, send);
    }

    /// Route a resynchronisation.
    pub fn on_resync(&mut self, send: Sender<'_>) {
        self.list.on_resync(send);
    }

    /// Draw the screen.
    pub fn ui(&mut self, ui: &mut Ui, send: Sender<'_>) -> Vec<LibraryIntent> {
        let mut intents = Vec::new();
        if !self.started {
            self.started = true;
            self.list.refresh(send);
        }

        self.handle_drops(ui, send);

        let toolbar_output = toolbar(ui, &mut self.toolbar, self.list.query(), self.list.rows());
        if toolbar_output.open_search {
            intents.push(LibraryIntent::OpenSearch);
        }
        for action in toolbar_output.actions {
            if matches!(action, QueryAction::ClearFilters) {
                self.toolbar.set_text("");
            }
            self.list.apply(action, send);
        }

        ui.separator();

        if let Some(envelope) = self.list.error().cloned() {
            match error_notice(ui, &envelope, "Recordings could not be loaded.") {
                Some(ErrorNoticeAction::Retry) => self.list.refresh(send),
                Some(
                    ErrorNoticeAction::OpenSettings
                    | ErrorNoticeAction::InstallSpeechModel
                    | ErrorNoticeAction::ImportModelFile,
                ) => {
                    intents.push(LibraryIntent::OpenSettings);
                }
                None => {}
            }
        } else if self.list.is_loading() {
            ui.label("Loading recordings…");
        } else if self.list.rows().is_empty() {
            intents.extend(self.empty_state(ui, send));
        } else {
            intents.extend(self.table(ui, send));
        }

        if hovering_files(ui.ctx()) {
            import::drop_overlay(ui);
        }
        intents.extend(self.import_panel(ui, send));
        intents
    }

    fn handle_drops(&mut self, ui: &Ui, send: Sender<'_>) {
        let dropped = dropped_paths(ui.ctx());
        if dropped.is_empty() {
            return;
        }
        if !dropped.unrepresentable.is_empty() {
            self.import.note_unrepresentable(dropped.unrepresentable);
        }
        if !dropped.paths.is_empty() {
            self.import
                .start(&dropped.paths, DuplicatePolicy::Ask, send);
        }
    }

    fn empty_state(&mut self, ui: &mut Ui, send: Sender<'_>) -> Vec<LibraryIntent> {
        let mut intents = Vec::new();
        let filtered = self.list.query().has_active_filters();
        egui::Frame::group(ui.style()).show(ui, |ui| {
            if filtered {
                ui.heading("No recordings match these filters");
                ui.label(
                    "Nothing in the library matches the current search, status, source, tag or \
                     date range.",
                );
                if ui.button("Clear filters").clicked() {
                    self.toolbar.set_text("");
                    self.list.apply(QueryAction::ClearFilters, send);
                }
            } else {
                ui.heading("No recordings");
                ui.label(
                    "Import audio from this computer to get started. Direct PLAUD device import \
                     is not part of this build; Settings lists the sources that are supported.",
                );
                ui.horizontal(|ui| {
                    if ui.button("Import an audio file").clicked() {
                        intents.push(LibraryIntent::PickFiles);
                    }
                    if ui.button("Connect PLAUD").clicked() {
                        intents.push(LibraryIntent::OpenSettings);
                    }
                });
            }
        });
        intents
    }

    fn table(&mut self, ui: &mut Ui, send: Sender<'_>) -> Vec<LibraryIntent> {
        let mut intents = Vec::new();
        let output = RecordingTable {
            label: "Recordings",
            rows: self.list.rows(),
            row_count: self.list.total(),
            sort_field: self.list.query().sort_field,
            sort_direction: self.list.query().sort_direction,
            has_more: self.list.has_more(),
            loading_more: self.list.is_loading_more(),
        }
        .show(ui, &mut self.table);

        if let Some(field) = output.sort {
            self.list.apply(QueryAction::Sort(field), send);
        }
        if let Some(recording_id) = output.open {
            intents.push(LibraryIntent::OpenRecording(recording_id));
        }
        if output.load_more {
            self.list.load_more(send);
        }
        match output.batch {
            Some(BatchOperation::Transcribe(ids)) => {
                for recording_id in ids {
                    let id = send(AppCommand::StartProcessing(StartProcessingRequest {
                        recording_id: recording_id.to_string(),
                        stage: TRANSCRIBE_STAGE.to_owned(),
                        model_override: None,
                        // The library list cannot show a price, so it never
                        // acknowledges one: a run that would cost money comes
                        // back as `consent_required` and is reported as such.
                        cost_acknowledged: false,
                        rerun: false,
                    }));
                    self.processing.pending.insert(id);
                }
                intents.push(LibraryIntent::Status(
                    "Asked to transcribe the selected recordings.".to_owned(),
                ));
            }
            Some(BatchOperation::CopyIdentities(ids)) => {
                let text = ids
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n");
                ui.ctx().copy_text(text);
                intents.push(LibraryIntent::Status(format!(
                    "Copied {} recording {}.",
                    ids.len(),
                    if ids.len() == 1 {
                        "identity"
                    } else {
                        "identities"
                    }
                )));
            }
            None => {}
        }
        intents
    }

    fn import_panel(&mut self, ui: &mut Ui, send: Sender<'_>) -> Vec<LibraryIntent> {
        let mut intents = Vec::new();
        match duplicate_dialog(ui, &self.import) {
            Some(DuplicateDecision::DecideLater) => self.import.decide_later(),
            Some(DuplicateDecision::Chose { path, choice }) => {
                if let Some(recording_id) = self.import.resolve_duplicate(&path, choice, send) {
                    intents.push(LibraryIntent::OpenRecording(recording_id));
                }
            }
            None => {}
        }
        match import::import_results_panel(ui, &mut self.import) {
            Some(ImportIntent::PickFiles) => intents.push(LibraryIntent::PickFiles),
            Some(ImportIntent::RetryPath(path)) => {
                self.import.start(&[path], DuplicatePolicy::Ask, send);
            }
            Some(ImportIntent::OpenRecording(recording_id)) => {
                intents.push(LibraryIntent::OpenRecording(recording_id));
            }
            Some(ImportIntent::OpenSettings) => intents.push(LibraryIntent::OpenSettings),
            None => {}
        }
        intents
    }
}
