//! How Library and Inbox plug into the shell.
//!
//! The screens themselves know nothing about the shell: they take a sender
//! and return intents. This module is the only place that turns an intent
//! into a route, a toast or a host-hook call, which keeps every rule above it
//! testable without a `ScreenCtx`.

use plaudon_app::command::{AppResponse, CommandId};
use plaudon_app::dto::UiEvent;
use plaudon_app::event::ResyncReason;
use plaudon_domain::error::ErrorEnvelope;

use crate::screens::inbox::{InboxIntent, InboxScreen};
use crate::screens::recording::{DetailContext, DetailIntent};
use crate::screens::search::SearchIntent;
use crate::shell::route::{Destination, Route};
use crate::shell::screen::{Screen, ScreenCtx};

use super::{LibraryIntent, LibraryScreen};

/// Run `body` with a sender backed by `cx`.
macro_rules! with_sender {
    ($cx:expr, |$send:ident| $body:expr) => {{
        let mut $send = |command| $cx.send(command);
        $body
    }};
}

impl LibraryScreen {
    fn route_intents(&mut self, intents: Vec<LibraryIntent>, cx: &mut ScreenCtx<'_>) {
        for intent in intents {
            match intent {
                LibraryIntent::OpenRecording(recording_id) => {
                    cx.navigate(Route::LibraryRecording {
                        recording_id: recording_id.to_string(),
                    });
                }
                LibraryIntent::OpenSearch => cx.navigate(Route::LibrarySearch),
                LibraryIntent::OpenSettings => cx.navigate(Route::Settings),
                LibraryIntent::PickFiles => {
                    let paths: Vec<String> = (cx.hooks().pick_audio_files)()
                        .into_iter()
                        .filter_map(|path| path.to_str().map(str::to_owned))
                        .collect();
                    if !paths.is_empty() {
                        with_sender!(cx, |send| self.import_paths(paths, &mut send));
                    }
                }
                LibraryIntent::Status(text) => cx.set_status(text),
            }
        }
    }
}

impl Screen for LibraryScreen {
    fn destination(&self) -> Destination {
        Destination::Library
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        match cx.route().clone() {
            Route::LibrarySearch => {
                let intents = with_sender!(cx, |send| self.search.ui(ui, &mut send));
                for intent in intents {
                    match intent {
                        SearchIntent::OpenSegment { recording_id, .. }
                        | SearchIntent::Seek { recording_id, .. } => {
                            cx.navigate(Route::LibraryRecording {
                                recording_id: recording_id.to_string(),
                            });
                        }
                        SearchIntent::OpenSettings => cx.navigate(Route::Settings),
                    }
                }
            }
            Route::LibraryRecording { recording_id } => {
                let Ok(parsed) = recording_id.parse() else {
                    ui.label("That recording identifier is not valid.");
                    return;
                };
                with_sender!(cx, |send| self.detail.open(parsed, &mut send));
                let context = DetailContext {
                    local_only: true,
                };
                let panels = self.panels.as_mut();
                let intents =
                    with_sender!(cx, |send| self.detail.ui(ui, panels, context, &mut send));
                for intent in intents {
                    match intent {
                        DetailIntent::Back => cx.navigate(Route::Library),
                        DetailIntent::OpenSettings => cx.navigate(Route::Settings),
                    }
                }
            }
            _ => {
                let intents = with_sender!(cx, |send| LibraryScreen::ui(self, ui, &mut send));
                self.route_intents(intents, cx);
            }
        }
    }

    fn on_response(
        &mut self,
        id: CommandId,
        _command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        cx: &mut ScreenCtx<'_>,
    ) {
        if self.search.on_response(id, result) || self.detail.on_response(id, result) {
            return;
        }
        let intents = with_sender!(cx, |send| LibraryScreen::on_response(
            self, id, result, &mut send
        ));
        self.route_intents(intents, cx);
    }

    fn on_ui_event(&mut self, event: &UiEvent, cx: &mut ScreenCtx<'_>) {
        let kind = event.kind;
        with_sender!(cx, |send| {
            LibraryScreen::on_ui_event(self, kind, &mut send);
            self.detail.on_ui_event(kind, &mut send);
        });
    }

    fn on_resync(&mut self, _reason: ResyncReason, cx: &mut ScreenCtx<'_>) {
        with_sender!(cx, |send| {
            LibraryScreen::on_resync(self, &mut send);
            self.detail.on_resync(&mut send);
        });
    }
}

impl Screen for InboxScreen {
    fn destination(&self) -> Destination {
        Destination::Inbox
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        let intents = with_sender!(cx, |send| InboxScreen::ui(self, ui, &mut send));
        for intent in intents {
            match intent {
                InboxIntent::OpenRecording(recording_id) => cx.navigate(Route::LibraryRecording {
                    recording_id: recording_id.to_string(),
                }),
                InboxIntent::OpenLibrary => cx.navigate(Route::Library),
                InboxIntent::PickFiles => {
                    // The inbox's import button goes through the same host
                    // hook as the library's; the paths are imported from the
                    // library screen, which owns the batch.
                    cx.navigate(Route::Library);
                }
            }
        }
    }

    fn on_response(
        &mut self,
        id: CommandId,
        _command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        _cx: &mut ScreenCtx<'_>,
    ) {
        InboxScreen::on_response(self, id, result);
    }

    fn on_ui_event(&mut self, event: &UiEvent, cx: &mut ScreenCtx<'_>) {
        let kind = event.kind;
        with_sender!(cx, |send| InboxScreen::on_ui_event(self, kind, &mut send));
    }

    fn on_resync(&mut self, _reason: ResyncReason, cx: &mut ScreenCtx<'_>) {
        with_sender!(cx, |send| InboxScreen::on_resync(self, &mut send));
    }
}
