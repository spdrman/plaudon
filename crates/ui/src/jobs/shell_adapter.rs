//! How the Jobs destination plugs into the shell.
//!
//! [`JobsScreen`] knows nothing about `plaudon_ui::shell`: it renders from a
//! borrowed [`JobPage`] and returns [`crate::ai::AiIntent`]s, which is what
//! lets a kittest test drive it with no dispatcher and no routing. This module
//! is the one place that knows both halves — it holds the last page, sends the
//! commands that fetch it, and drains the intents into shell calls.
//!
//! Nothing here polls. The page is read on the first frame, when a §15.3 event
//! invalidates it, when the stream resynchronises, and when the user asks.

use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{Capabilities, EventKind, JobPage, UiEvent};
use plaudon_app::event::ResyncReason;
use plaudon_domain::error::ErrorEnvelope;

use crate::shell::intents::{apply_ai_output, wall_clock_ms};
use crate::shell::route::{Destination, Route};
use crate::shell::screen::{Screen, ScreenCtx};

use super::JobsScreen;

/// The Jobs destination, as the shell sees it.
#[derive(Debug, Default)]
pub struct JobsShellScreen {
    screen: JobsScreen,
    page: Option<JobPage>,
    capabilities: Option<Capabilities>,
    loaded: bool,
    /// The job id the route last deep-linked to, so the same link is honoured
    /// once rather than on every frame.
    focused: Option<String>,
}

impl JobsShellScreen {
    /// A fresh Jobs screen.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The screen underneath, for a test that wants to seed it.
    pub fn inner_mut(&mut self) -> &mut JobsScreen {
        &mut self.screen
    }

    /// The last `get_capabilities` answer, for a host that mirrors the
    /// storage facts elsewhere. `None` until one arrives — this process never
    /// guesses what the machine can do (§8.6A).
    #[must_use]
    pub const fn capabilities(&self) -> Option<&Capabilities> {
        self.capabilities.as_ref()
    }

    /// Read the first page under the current filters, and the storage facts
    /// the §J7 capacity comparison needs.
    fn load(&mut self, cx: &mut ScreenCtx<'_>) {
        cx.send(AppCommand::ListJobs(self.screen.first_page_request()));
        cx.send(AppCommand::GetCapabilities);
    }

    /// Read the page on screen again, keeping the user's cursor and filters.
    fn refetch(&mut self, cx: &mut ScreenCtx<'_>) {
        cx.send(AppCommand::ListJobs(self.screen.resync_request()));
    }

    /// Honour `Route::JobsJob { job_id }` once per distinct job id.
    fn follow_route(&mut self, cx: &ScreenCtx<'_>) {
        let job_id = match cx.route() {
            Route::JobsJob { job_id } => Some(job_id.clone()),
            _ => None,
        };
        if job_id == self.focused {
            return;
        }
        if let Some(job_id) = job_id.as_deref() {
            self.screen.focus_job(job_id);
        }
        self.focused = job_id;
    }
}

impl Screen for JobsShellScreen {
    fn destination(&self) -> Destination {
        Destination::Jobs
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        if !self.loaded {
            self.loaded = true;
            self.load(cx);
        }
        self.follow_route(cx);

        // One clock for the whole shell: `wall_clock_ms` is the single place
        // the real time of day is read, so a row's elapsed label and every
        // other elapsed label in the window are measured against the same
        // instant. The screen still takes it as an argument, which is what
        // keeps it testable without a clock at all.
        let output = self.screen.ui(ui, self.page.as_ref(), wall_clock_ms());

        // `apply_ai_output` is the shell's single, total translation of an
        // `AiIntent`: a command is dispatched, a `ControlIntent` navigates
        // where §5.4 says it should, and the two `Pick…` variants open the
        // host's dialog and dispatch the export they stand for. Writing a
        // second mapping here is how two screens end up sending the user to
        // two different places for the same button.
        let seeks = apply_ai_output(output, cx);
        if !seeks.is_empty() {
            // A job row has no transcript and no anchors, so it cannot cite a
            // position: this branch is unreachable today. It is written out
            // rather than dropped silently so that a future panel embedded
            // here is not quietly deprived of playback. No payload is logged
            // (§15.4); the count is enough to find it.
            tracing::warn!(
                count = seeks.len(),
                "the jobs screen produced a seek, but this destination has no player"
            );
        }
    }

    fn on_response(
        &mut self,
        _id: CommandId,
        _command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        cx: &mut ScreenCtx<'_>,
    ) {
        let Ok(response) = result else {
            return;
        };
        match response {
            AppResponse::ListJobs(page) => self.page = Some(page.clone()),
            AppResponse::GetCapabilities(capabilities) => {
                self.screen.set_storage(Some(capabilities.storage.clone()));
                self.capabilities = Some(capabilities.clone());
            }
            // A cancellation or a retry answers with the one job it changed.
            // The row is not patched from it: the page is the backend's, and
            // one summary is not a page. The page on screen is read again, so
            // the per-state counts beside it stay the counts of what is shown.
            AppResponse::CancelJob(_) | AppResponse::RetryJob(_) => self.refetch(cx),
            _ => {}
        }
    }

    fn on_ui_event(&mut self, event: &UiEvent, cx: &mut ScreenCtx<'_>) {
        // Matrix row J9: events never carry state. A job transition invalidates
        // the page on screen, so the page on screen is read again — the event's
        // payload is a hint and is never rendered.
        if matches!(event.kind, EventKind::JobUpdated | EventKind::JobFailed) {
            self.refetch(cx);
        }
    }

    fn on_resync(&mut self, _reason: ResyncReason, cx: &mut ScreenCtx<'_>) {
        self.load(cx);
    }
}
