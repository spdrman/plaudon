//! The Inbox (§5.1): recordings that just arrived or still have work pending.
//!
//! The condition is "import not finished **or** transcription not started or
//! running", which `RecordingFilter` cannot express as one request. Two
//! cursor-paged queries therefore run side by side and are unioned by
//! recording identity, then ordered client-side by the same rules the backend
//! would have used — which is why [`query::sort_rows`] exists and why an
//! unknown value sorts last rather than first.

use egui::Ui;
use plaudon_app::command::{AppResponse, CommandId};
use plaudon_app::dto::{EventKind, RecordingSummary};
use plaudon_domain::error::ErrorEnvelope;
use plaudon_domain::ids::RecordingId;

use crate::screens::library::list::{RecordingList, Sender};
use crate::screens::library::query::{
    self, inbox_import_query, inbox_work_query, merge_inbox_rows, QueryAction,
};
use crate::screens::library::table::{BatchOperation, RecordingTable, TableState};
use crate::widgets::error_notice::{error_notice, ErrorNoticeAction};

/// Something the inbox needs the host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxIntent {
    /// Open this recording's detail view.
    OpenRecording(RecordingId),
    /// Go to the full library.
    OpenLibrary,
    /// Ask the OS for audio files to import.
    PickFiles,
}

/// The inbox screen.
#[derive(Debug)]
pub struct InboxScreen {
    imports: RecordingList,
    work: RecordingList,
    table: TableState,
    rows: Vec<RecordingSummary>,
    started: bool,
}

impl Default for InboxScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl InboxScreen {
    /// An inbox that has not fetched anything yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            imports: RecordingList::new(inbox_import_query()),
            work: RecordingList::new(inbox_work_query()),
            table: TableState::default(),
            rows: Vec::new(),
            started: false,
        }
    }

    /// The merged rows currently shown.
    #[must_use]
    pub fn rows(&self) -> &[RecordingSummary] {
        &self.rows
    }

    /// Route a dispatcher answer to whichever branch asked for it.
    pub fn on_response(
        &mut self,
        id: CommandId,
        result: &Result<AppResponse, ErrorEnvelope>,
    ) -> bool {
        let handled = self.imports.on_response(id, result) | self.work.on_response(id, result);
        if handled {
            self.recompute();
        }
        handled
    }

    /// Route a §15.3 event to both branches.
    pub fn on_ui_event(&mut self, kind: EventKind, send: Sender<'_>) {
        self.imports.on_ui_event(kind, send);
        self.work.on_ui_event(kind, send);
    }

    /// Route a resynchronisation to both branches.
    pub fn on_resync(&mut self, send: Sender<'_>) {
        self.imports.on_resync(send);
        self.work.on_resync(send);
    }

    fn recompute(&mut self) {
        self.rows = merge_inbox_rows(&[self.imports.rows(), self.work.rows()]);
        query::sort_rows(&mut self.rows, self.imports.query().sort());
    }

    /// Draw the inbox.
    pub fn ui(&mut self, ui: &mut Ui, send: Sender<'_>) -> Vec<InboxIntent> {
        let mut intents = Vec::new();
        if !self.started {
            self.started = true;
            self.imports.refresh(send);
            self.work.refresh(send);
        }

        let error = self.imports.error().or_else(|| self.work.error()).cloned();
        if let Some(envelope) = error {
            if let Some(ErrorNoticeAction::Retry) =
                error_notice(ui, &envelope, "The inbox could not be loaded.")
            {
                self.imports.refresh(send);
                self.work.refresh(send);
            }
            return intents;
        }

        if (self.imports.is_loading() || self.work.is_loading()) && self.rows.is_empty() {
            ui.label("Loading the inbox…");
            return intents;
        }

        if self.rows.is_empty() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.heading("Nothing waiting");
                ui.label(
                    "Recordings appear here while they are importing or waiting to be \
                     transcribed. Finished recordings live in Library.",
                );
                ui.horizontal(|ui| {
                    if ui.button("Import an audio file").clicked() {
                        intents.push(InboxIntent::PickFiles);
                    }
                    if ui.button("Go to Library").clicked() {
                        intents.push(InboxIntent::OpenLibrary);
                    }
                });
            });
            return intents;
        }

        let output = RecordingTable {
            label: "Inbox recordings",
            rows: &self.rows,
            // The union's size is the only count that is true of what is
            // displayed; neither branch's total describes it.
            row_count: self.rows.len() as u64,
            sort_field: self.imports.query().sort_field,
            sort_direction: self.imports.query().sort_direction,
            has_more: self.imports.has_more() || self.work.has_more(),
            loading_more: self.imports.is_loading_more() || self.work.is_loading_more(),
        }
        .show(ui, &mut self.table);

        if let Some(field) = output.sort {
            self.imports.apply(QueryAction::Sort(field), send);
            self.work.apply(QueryAction::Sort(field), send);
            self.rows.clear();
        }
        if let Some(recording_id) = output.open {
            intents.push(InboxIntent::OpenRecording(recording_id));
        }
        if output.load_more {
            self.imports.load_more(send);
            self.work.load_more(send);
        }
        if let Some(BatchOperation::CopyIdentities(ids)) = output.batch {
            ui.ctx().copy_text(
                ids.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        intents
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::library::testing::{page, row_with};
    use plaudon_app::command::AppCommand;
    use plaudon_app::dto::{ImportStatus, TranscriptionStatus};

    #[derive(Debug, Default)]
    struct Recorder {
        next: u64,
    }

    impl Recorder {
        fn send(&mut self) -> impl FnMut(AppCommand) -> CommandId + '_ {
            |_command| {
                self.next += 1;
                CommandId::from_raw(self.next)
            }
        }
    }

    #[test]
    fn a_recording_in_both_branches_appears_once() {
        let mut inbox = InboxScreen::new();
        let mut recorder = Recorder::default();
        inbox.imports.refresh(&mut recorder.send());
        inbox.work.refresh(&mut recorder.send());

        let shared = row_with("Shared", |row| {
            row.import_status = ImportStatus::Importing;
            row.transcription_status = TranscriptionStatus::None;
        });
        let work_only = row_with("Queued", |row| {
            row.transcription_status = TranscriptionStatus::Queued;
        });

        inbox.on_response(
            CommandId::from_raw(1),
            &Ok(AppResponse::ListRecordings(page(
                vec![shared.clone()],
                None,
                1,
            ))),
        );
        inbox.on_response(
            CommandId::from_raw(2),
            &Ok(AppResponse::ListRecordings(page(
                vec![shared.clone(), work_only.clone()],
                None,
                2,
            ))),
        );

        assert_eq!(inbox.rows().len(), 2);
        let ids: Vec<_> = inbox.rows().iter().map(|row| row.recording_id).collect();
        assert!(ids.contains(&shared.recording_id));
        assert!(ids.contains(&work_only.recording_id));
    }
}
