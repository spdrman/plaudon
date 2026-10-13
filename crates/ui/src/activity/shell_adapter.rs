//! How the activity history plugs into the shell.
//!
//! [`ActivityLog`] renders from a borrowed [`ActivityPage`] and returns
//! [`ActivityOutput`]; this module holds the page, fetches it and turns the
//! output into shell calls.
//!
//! The activity history is **not** a destination of its own: it is the
//! `Route::SettingsActivity` sub-route, rendered by
//! [`crate::settings::destination::SettingsDestinationScreen`], which forwards
//! every response, event and resync to it. [`Screen::destination`] therefore
//! answers [`Destination::Settings`] — the destination this screen is reached
//! through — and this screen is never registered with the shell directly.
//!
//! Nothing here polls: the page is read on the first frame, on
//! `activity.added`, on resync, and when the user asks.

use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{ActivityPage, EventKind, UiEvent};
use plaudon_app::event::ResyncReason;
use plaudon_domain::error::ErrorEnvelope;

use crate::shell::intents::apply_ai_output;
use crate::shell::route::{Destination, Route};
use crate::shell::screen::{Screen, ScreenCtx};

use super::{ActivityLink, ActivityLog};

/// The activity history, as the shell sees it.
#[derive(Debug, Default)]
pub struct ActivityShellScreen {
    log: ActivityLog,
    page: Option<ActivityPage>,
    loaded: bool,
}

impl ActivityShellScreen {
    /// A fresh activity history.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The last `list_activity` answer, for a host that counts unread entries
    /// in a header bell rather than counting them a second time itself.
    #[must_use]
    pub const fn page(&self) -> Option<&ActivityPage> {
        self.page.as_ref()
    }

    /// Read the first page again.
    ///
    /// This is the screen's whole first-frame load, so it latches `loaded`:
    /// the subscription resynchronisation the shell fans out before the first
    /// frame asks for exactly this read, and reading again from [`Screen::ui`]
    /// would put two `list_activity` answers in flight for one startup — the
    /// later of which overwrites the page the earlier one drew.
    fn refresh(&mut self, cx: &mut ScreenCtx<'_>) {
        self.loaded = true;
        cx.send(AppCommand::ListActivity(self.log.refresh_request()));
    }

    /// Read the page on screen again, keeping the user's cursor.
    fn refetch(&mut self, cx: &mut ScreenCtx<'_>) {
        cx.send(AppCommand::ListActivity(self.log.resync_request()));
    }
}

impl Screen for ActivityShellScreen {
    fn destination(&self) -> Destination {
        // Reached through Settings, at `Route::SettingsActivity`; it owns no
        // entry in the navigation rail.
        Destination::Settings
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        if !self.loaded {
            self.refresh(cx);
        }

        let output = self.log.ui(ui, self.page.as_ref());

        // A deep link names a job or a recording; the shell addresses both by
        // route, and the id travels verbatim.
        for link in output.links {
            match link {
                ActivityLink::Job(job_id) => cx.navigate(Route::JobsJob { job_id }),
                ActivityLink::Recording(recording_id) => {
                    cx.navigate(Route::LibraryRecording { recording_id });
                }
            }
        }

        // Everything else — the refresh buttons and the "open settings" link
        // an entry targeting the settings screen raises — goes through the
        // shell's single, total intent translation.
        let seeks = apply_ai_output(output.ai, cx);
        if !seeks.is_empty() {
            // An activity entry has no transcript anchors, so it cannot cite a
            // position; this is written out rather than dropped silently. No
            // payload is logged (§15.4).
            tracing::warn!(
                count = seeks.len(),
                "the activity history produced a seek, but it has no player"
            );
        }
    }

    fn on_response(
        &mut self,
        _id: CommandId,
        _command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        _cx: &mut ScreenCtx<'_>,
    ) {
        if let Ok(AppResponse::ListActivity(page)) = result {
            self.page = Some(page.clone());
        }
    }

    fn on_ui_event(&mut self, event: &UiEvent, cx: &mut ScreenCtx<'_>) {
        // §15.3: the event carries no entry, only the fact that one exists.
        // The page on screen is read again; nothing is appended from a hint.
        if event.kind == EventKind::ActivityAdded {
            self.refetch(cx);
        }
    }

    fn on_resync(&mut self, _reason: ResyncReason, cx: &mut ScreenCtx<'_>) {
        self.refresh(cx);
    }
}
