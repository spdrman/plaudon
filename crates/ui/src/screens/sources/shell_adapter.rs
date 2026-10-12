//! How the Sources screen plugs into the shell.
//!
//! [`sources_screen`] renders [`SourcesState`] and returns [`SourceIntent`]s;
//! it reaches no port, opens no dialog, spawns no process and knows no route.
//! This module is the one place that knows both halves: it turns the eight
//! command intents into [`AppCommand`]s, carries out the four host intents,
//! and feeds every answer back with [`SourcesState::applied`].
//!
//! Sources is **not** a destination of its own: it is the
//! `Route::SettingsSources` sub-route, rendered by
//! [`crate::settings::destination::SettingsDestinationScreen`], which forwards
//! every response, event and resync to it. [`Screen::destination`] therefore
//! answers [`Destination::Settings`] — the destination this screen is reached
//! through — and this screen is never registered with the shell directly.
//!
//! Nothing here polls: the connector is probed on the first frame, when a
//! §15.3 source event invalidates the snapshot, on resync, and when the user
//! asks.

use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{DuplicatePolicy, EventKind, ImportFilesRequest, UiEvent};
use plaudon_app::event::ResyncReason;
use plaudon_domain::error::ErrorEnvelope;

use crate::shell::route::{Destination, Route};
use crate::shell::screen::{Screen, ScreenCtx};
use crate::widgets::toast::ToastTone;

use super::{sources_screen, SourceIntent, SourceOp, SourceOutcome, SourcesState};

/// The Sources screen, as the shell sees it.
#[derive(Debug)]
pub struct SourcesShellScreen {
    state: SourcesState,
    /// Which operation each in-flight command belongs to, so an envelope is
    /// reported against the control that caused it.
    in_flight: Vec<(CommandId, SourceOp)>,
    /// `import_files` commands this screen started. Not a [`SourceOp`]: the
    /// manual import has no slot in [`SourcesState`].
    imports: Vec<CommandId>,
    loaded: bool,
}

impl Default for SourcesShellScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl SourcesShellScreen {
    /// A fresh Sources screen.
    #[must_use]
    pub fn new() -> Self {
        Self {
            // `SourcesState::new` is not `Default`: it starts the watched-folder
            // form on the rule the application treats as the safe one.
            state: SourcesState::new(),
            in_flight: Vec::new(),
            imports: Vec::new(),
            loaded: false,
        }
    }

    /// The state underneath, for a test that wants to seed it.
    pub fn state_mut(&mut self) -> &mut SourcesState {
        &mut self.state
    }

    /// Send the command an intent stands for, remembering which operation it
    /// was so its answer can be routed back.
    fn send_intent(&mut self, intent: SourceIntent, cx: &mut ScreenCtx<'_>) {
        let op = intent.op();
        let Some(command) = intent.into_command() else {
            return;
        };
        let id = cx.send(command);
        if let Some(op) = op {
            self.in_flight.push((id, op));
        }
    }

    /// Carry out one intent: a command, a dialog, a navigation, or a link.
    fn dispatch(&mut self, intent: SourceIntent, ui: &egui::Ui, cx: &mut ScreenCtx<'_>) {
        match intent {
            SourceIntent::ChooseFolder => {
                // The dialog blocks on the operating system, so it is opened
                // after the frame the control was clicked in. A cancelled
                // dialog leaves the form as it was; it does not clear a folder
                // the user already chose.
                if let Some(folder) = (cx.hooks().pick_folder)() {
                    self.state.folder_chosen(folder.to_string_lossy());
                }
            }
            SourceIntent::ImportAudioFiles => {
                let paths: Vec<String> = (cx.hooks().pick_audio_files)()
                    .into_iter()
                    .filter_map(|path| path.to_str().map(str::to_owned))
                    .collect();
                if paths.is_empty() {
                    return;
                }
                let id = cx.send(AppCommand::ImportFiles(ImportFilesRequest {
                    paths,
                    // The user chose these files by hand and has said nothing
                    // about duplicates, so the application is asked rather
                    // than told what to do with one.
                    duplicate_policy: DuplicatePolicy::Ask,
                }));
                self.imports.push(id);
            }
            SourceIntent::OpenPrivacySettings => cx.navigate(Route::Settings),
            SourceIntent::OpenUrl(url) => {
                // There is no URL opener in `HostHooks`, and one is not added
                // here: `plaudon-ui` must not spawn a process, and inventing a
                // hook the binary does not implement would be a control that
                // silently does nothing. The link is put on the clipboard
                // instead, which is something this layer can really do, and
                // the toast says so — the vendor page is then opened by the
                // user's own browser.
                ui.ctx().copy_text(url.to_owned());
                cx.toast(
                    ToastTone::Info,
                    "Link copied to the clipboard",
                    Some(url.to_owned()),
                );
            }
            command_intent => self.send_intent(command_intent, cx),
        }
    }

    /// Which operation `id` belonged to, removing it from the ledger.
    fn take_in_flight(&mut self, id: CommandId) -> Option<SourceOp> {
        let index = self.in_flight.iter().position(|(known, _)| *known == id)?;
        Some(self.in_flight.swap_remove(index).1)
    }

    /// Whether `id` was one of this screen's manual imports.
    fn take_import(&mut self, id: CommandId) -> bool {
        let Some(index) = self.imports.iter().position(|known| *known == id) else {
            return false;
        };
        self.imports.swap_remove(index);
        true
    }

    /// Ask the application what the connector's state is.
    fn probe(&mut self, cx: &mut ScreenCtx<'_>) {
        self.send_intent(SourcesState::initial_intent(), cx);
    }
}

impl Screen for SourcesShellScreen {
    fn destination(&self) -> Destination {
        // Reached through Settings, at `Route::SettingsSources`; it owns no
        // entry in the navigation rail.
        Destination::Settings
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        if !self.loaded {
            self.loaded = true;
            self.probe(cx);
        }
        for intent in sources_screen(ui, &mut self.state) {
            self.dispatch(intent, ui, cx);
        }
    }

    fn on_response(
        &mut self,
        id: CommandId,
        _command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        cx: &mut ScreenCtx<'_>,
    ) {
        let op = self.take_in_flight(id);
        let was_import = self.take_import(id);
        match result {
            Ok(response) => {
                let outcome = match response {
                    // `plaud_probe` and `plaud_connect` answer with the same
                    // snapshot, which is the whole of what this screen renders
                    // about the connector.
                    AppResponse::PlaudProbe(snapshot) | AppResponse::PlaudConnect(snapshot) => {
                        SourceOutcome::Snapshot(snapshot.clone())
                    }
                    AppResponse::PlaudDisconnect(response) => {
                        SourceOutcome::Disconnected(response.clone())
                    }
                    AppResponse::PlaudList(page) => SourceOutcome::Listed(page.clone()),
                    AppResponse::PlaudImport(imported) => {
                        SourceOutcome::Imported(imported.results.clone())
                    }
                    AppResponse::SourceAddFolder(response) => {
                        SourceOutcome::FolderAdded(response.clone())
                    }
                    AppResponse::SourceSync(response) => SourceOutcome::Synced(response.clone()),
                    AppResponse::SourceReconcile(response) => {
                        SourceOutcome::Reconciled(response.clone())
                    }
                    // Not this screen's answer: `import_files` is reported by
                    // the library it lands in, and the rest belongs to the
                    // other screens that share the shell's fan-out.
                    _ => return,
                };
                self.state.applied(outcome);
            }
            Err(error) => {
                if let Some(op) = op {
                    self.state.applied(SourceOutcome::Failed {
                        op,
                        error: error.clone(),
                    });
                } else if was_import {
                    // The manual import has no slot in `SourcesState`, so the
                    // failure is announced instead — in the application's own
                    // words, with the diagnostic id the §15.4 envelope
                    // carries.
                    cx.toast(
                        ToastTone::Danger,
                        error.message.clone(),
                        Some(format!("Diagnostic id: {}", error.diagnostic_id)),
                    );
                }
            }
        }
    }

    fn on_ui_event(&mut self, event: &UiEvent, cx: &mut ScreenCtx<'_>) {
        // §15.3: the event says the connector's state changed, never what it
        // changed to. The snapshot is read again; nothing is patched from a
        // hint.
        if matches!(
            event.kind,
            EventKind::SourceAuthRequired
                | EventKind::SourceSyncFinished
                | EventKind::RecordingSourceUpdated
        ) {
            let intent = self.state.source_event();
            self.send_intent(intent, cx);
        }
    }

    fn on_resync(&mut self, _reason: ResyncReason, cx: &mut ScreenCtx<'_>) {
        self.probe(cx);
    }
}
