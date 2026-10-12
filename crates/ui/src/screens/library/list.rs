//! The cursor-paged `list_recordings` chain, with no egui in it.
//!
//! The rules this encodes are the ones §15.3 and §5.2 make non-negotiable:
//!
//! * the view never awaits — it hands a command to the dispatcher and the
//!   answer arrives later, matched by [`CommandId`];
//! * pagination is the backend's **opaque cursor**, never an offset the view
//!   computed, and a filter or sort change restarts the chain rather than
//!   continuing it with a cursor that described a different ordering;
//! * an event is a hint, not data: a `library.changed` or a dropped-event
//!   resynchronisation restarts the chain from the first page instead of
//!   patching rows out of an event payload;
//! * an answer to a query the user has already moved on from is discarded,
//!   not rendered.

use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{EventKind, RecordingSummary};
use plaudon_domain::error::ErrorEnvelope;

use super::query::{append_page, LibraryQuery, QueryAction, QuerySignature, PAGE_SIZE};

/// How a view hands a command to the dispatcher.
///
/// A function rather than a trait so a test can pass a closure that records
/// what was asked for, and the screen can pass the shell's non-blocking
/// `send`, without either side knowing about the other.
pub type Sender<'a> = &'a mut dyn FnMut(AppCommand) -> CommandId;

/// Committed-state hints that change what the library should show.
pub const LIBRARY_EVENTS: [EventKind; 4] = [
    EventKind::RecordingImported,
    EventKind::LibraryChanged,
    EventKind::RecordingSourceUpdated,
    EventKind::TranscriptCompleted,
];

/// Whether an event means the library listing has to be fetched again.
#[must_use]
pub fn affects_library(kind: EventKind) -> bool {
    LIBRARY_EVENTS.contains(&kind)
}

#[derive(Debug)]
struct PendingPage {
    id: CommandId,
    signature: QuerySignature,
    continues: bool,
}

/// One cursor-paged view of the library.
#[derive(Debug)]
pub struct RecordingList {
    query: LibraryQuery,
    limit: u32,
    rows: Vec<RecordingSummary>,
    total: u64,
    next_cursor: Option<String>,
    pending: Option<PendingPage>,
    error: Option<ErrorEnvelope>,
}

impl RecordingList {
    /// A list over `query` that has not fetched anything yet.
    #[must_use]
    pub fn new(query: LibraryQuery) -> Self {
        Self {
            query,
            limit: PAGE_SIZE,
            rows: Vec::new(),
            total: 0,
            next_cursor: None,
            pending: None,
            error: None,
        }
    }

    /// The query this list is showing.
    #[must_use]
    pub const fn query(&self) -> &LibraryQuery {
        &self.query
    }

    /// Rows loaded so far, in backend order.
    #[must_use]
    pub fn rows(&self) -> &[RecordingSummary] {
        &self.rows
    }

    /// Rows matching the query at the time the last page was answered. A
    /// snapshot, which is why the footer says so.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.total
    }

    /// Whether another page exists.
    #[must_use]
    pub const fn has_more(&self) -> bool {
        self.next_cursor.is_some()
    }

    /// Whether the first page is in flight and there is nothing to show yet.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.pending.is_some() && self.rows.is_empty() && self.error.is_none()
    }

    /// Whether a continuation page is in flight.
    #[must_use]
    pub fn is_loading_more(&self) -> bool {
        self.pending.as_ref().is_some_and(|pending| pending.continues)
    }

    /// Whether anything has been fetched since the last restart.
    #[must_use]
    pub fn has_fetched(&self) -> bool {
        self.pending.is_none() && (self.error.is_some() || !self.rows.is_empty())
    }

    /// The last error the chain reported, if it has not been superseded.
    #[must_use]
    pub const fn error(&self) -> Option<&ErrorEnvelope> {
        self.error.as_ref()
    }

    /// Restart the chain from the first page, discarding what is loaded.
    pub fn refresh(&mut self, send: Sender<'_>) {
        self.rows.clear();
        self.total = 0;
        self.next_cursor = None;
        self.error = None;
        self.dispatch(None, send);
    }

    /// Ask for the next page. Does nothing when there is no next page or a
    /// request is already in flight — a scroll to the bottom must not queue
    /// the same page twice.
    pub fn load_more(&mut self, send: Sender<'_>) {
        if self.pending.is_some() {
            return;
        }
        let Some(cursor) = self.next_cursor.clone() else {
            return;
        };
        self.dispatch(Some(cursor), send);
    }

    /// Change the query. A change that invalidates pagination restarts the
    /// chain; anything else leaves the loaded pages alone.
    pub fn apply(&mut self, action: QueryAction, send: Sender<'_>) {
        let before = self.query.signature();
        self.query.apply(action);
        if before != self.query.signature() {
            self.refresh(send);
        }
    }

    /// Route a dispatcher answer. Returns whether it was for this list.
    pub fn on_response(
        &mut self,
        id: CommandId,
        result: &Result<AppResponse, ErrorEnvelope>,
    ) -> bool {
        let Some(pending) = self.pending.as_ref() else {
            return false;
        };
        if pending.id != id {
            return false;
        }
        let stale = pending.signature != self.query.signature();
        let continues = pending.continues;
        self.pending = None;
        if stale {
            // The user changed the filter or the sort while this page was in
            // flight. Rendering it would mix two queries.
            return true;
        }
        match result {
            Ok(AppResponse::ListRecordings(page)) => {
                self.error = None;
                self.total = page.total;
                self.next_cursor.clone_from(&page.next_cursor);
                if continues {
                    append_page(&mut self.rows, page.items.clone());
                } else {
                    self.rows.clone_from(&page.items);
                }
            }
            Ok(_) => return false,
            Err(envelope) => self.error = Some(envelope.clone()),
        }
        true
    }

    /// Route a §15.3 event. Returns whether it restarted the chain.
    pub fn on_ui_event(&mut self, kind: EventKind, send: Sender<'_>) -> bool {
        if !affects_library(kind) {
            return false;
        }
        self.refresh(send);
        true
    }

    /// A dropped or missed event means the cached rows may be wrong; the
    /// contract is the same as for an event we did see.
    pub fn on_resync(&mut self, send: Sender<'_>) {
        self.refresh(send);
    }

    fn dispatch(&mut self, cursor: Option<String>, send: Sender<'_>) {
        let continues = cursor.is_some();
        let request = self.query.list_request(cursor, self.limit);
        let id = send(AppCommand::ListRecordings(request));
        self.pending = Some(PendingPage {
            id,
            signature: self.query.signature(),
            continues,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::library::testing::{page, rows};
    use plaudon_app::dto::RecordingSortField;
    use plaudon_domain::error::{AppError, AppErrorCode, DiagnosticLog};
    use plaudon_domain::time::Timestamp;

    /// A sender that records what was dispatched and hands out ids.
    #[derive(Debug, Default)]
    struct Recorder {
        sent: Vec<AppCommand>,
        next: u64,
    }

    impl Recorder {
        fn send(&mut self) -> impl FnMut(AppCommand) -> CommandId + '_ {
            |command| {
                self.next += 1;
                self.sent.push(command);
                CommandId::from_raw(self.next)
            }
        }

        fn last_request(&self) -> &plaudon_app::dto::ListRecordingsRequest {
            match self.sent.last().expect("a command was dispatched") {
                AppCommand::ListRecordings(request) => request,
                other => panic!("expected list_recordings, got {}", other.name()),
            }
        }
    }

    fn envelope() -> ErrorEnvelope {
        let mut log = DiagnosticLog::new();
        AppError::new(AppErrorCode::InternalError, "detail").to_envelope(&mut log, Timestamp::now())
    }

    fn list() -> (RecordingList, Recorder) {
        (RecordingList::new(LibraryQuery::default()), Recorder::default())
    }

    #[test]
    fn the_first_page_is_requested_without_a_cursor() {
        let (mut list, mut recorder) = list();
        list.refresh(&mut recorder.send());
        assert_eq!(recorder.sent.len(), 1);
        assert_eq!(recorder.last_request().cursor, None);
        assert_eq!(recorder.last_request().limit, PAGE_SIZE);
        assert!(list.is_loading());
    }

    #[test]
    fn a_page_answer_fills_the_rows_and_the_snapshot_total() {
        let (mut list, mut recorder) = list();
        list.refresh(&mut recorder.send());
        let handled = list.on_response(
            CommandId::from_raw(1),
            &Ok(AppResponse::ListRecordings(page(rows(3), Some("01"), 9))),
        );
        assert!(handled);
        assert_eq!(list.rows().len(), 3);
        assert_eq!(list.total(), 9);
        assert!(list.has_more());
        assert!(!list.is_loading());
    }

    #[test]
    fn the_next_page_continues_the_backends_cursor_and_never_an_offset() {
        let (mut list, mut recorder) = list();
        list.refresh(&mut recorder.send());
        list.on_response(
            CommandId::from_raw(1),
            &Ok(AppResponse::ListRecordings(page(rows(3), Some("ff01"), 6))),
        );
        list.load_more(&mut recorder.send());
        assert_eq!(recorder.last_request().cursor.as_deref(), Some("ff01"));
        assert!(list.is_loading_more());
    }

    #[test]
    fn a_second_load_more_while_one_is_in_flight_asks_for_nothing() {
        let (mut list, mut recorder) = list();
        list.refresh(&mut recorder.send());
        list.on_response(
            CommandId::from_raw(1),
            &Ok(AppResponse::ListRecordings(page(rows(3), Some("ff01"), 6))),
        );
        list.load_more(&mut recorder.send());
        list.load_more(&mut recorder.send());
        assert_eq!(recorder.sent.len(), 2, "the same page must not be queued twice");
    }

    #[test]
    fn pages_are_appended_and_an_overlapping_row_renders_once() {
        let (mut list, mut recorder) = list();
        list.refresh(&mut recorder.send());
        let first = rows(3);
        list.on_response(
            CommandId::from_raw(1),
            &Ok(AppResponse::ListRecordings(page(
                first.clone(),
                Some("02"),
                5,
            ))),
        );
        list.load_more(&mut recorder.send());
        let mut second = vec![first[2].clone()];
        second.extend(rows(5).into_iter().skip(3));
        list.on_response(
            CommandId::from_raw(2),
            &Ok(AppResponse::ListRecordings(page(second, None, 5))),
        );
        assert_eq!(list.rows().len(), 5);
        assert!(!list.has_more());
    }

    #[test]
    fn a_filter_change_restarts_the_chain_and_drops_the_loaded_rows() {
        let (mut list, mut recorder) = list();
        list.refresh(&mut recorder.send());
        list.on_response(
            CommandId::from_raw(1),
            &Ok(AppResponse::ListRecordings(page(rows(3), Some("02"), 5))),
        );
        list.apply(
            QueryAction::Query("board".to_owned()),
            &mut recorder.send(),
        );
        assert!(list.rows().is_empty());
        assert_eq!(recorder.last_request().cursor, None);
        assert_eq!(recorder.last_request().filter.query.as_deref(), Some("board"));
    }

    #[test]
    fn a_query_change_that_changes_nothing_does_not_refetch() {
        let (mut list, mut recorder) = list();
        list.refresh(&mut recorder.send());
        list.on_response(
            CommandId::from_raw(1),
            &Ok(AppResponse::ListRecordings(page(rows(3), None, 3))),
        );
        list.apply(QueryAction::Query("   ".to_owned()), &mut recorder.send());
        assert_eq!(recorder.sent.len(), 1);
        assert_eq!(list.rows().len(), 3);
    }

    #[test]
    fn an_answer_to_a_query_the_user_moved_on_from_is_discarded() {
        let (mut list, mut recorder) = list();
        list.refresh(&mut recorder.send());
        list.apply(
            QueryAction::Sort(RecordingSortField::Title),
            &mut recorder.send(),
        );
        // The first page finally arrives, for the ordering nobody is on.
        list.on_response(
            CommandId::from_raw(1),
            &Ok(AppResponse::ListRecordings(page(rows(3), None, 3))),
        );
        assert!(list.rows().is_empty(), "a stale page must not be rendered");
    }

    #[test]
    fn an_answer_to_another_screens_command_is_not_ours() {
        let (mut list, mut recorder) = list();
        list.refresh(&mut recorder.send());
        assert!(!list.on_response(
            CommandId::from_raw(99),
            &Ok(AppResponse::ListRecordings(page(rows(1), None, 1)))
        ));
        assert!(list.is_loading());
    }

    #[test]
    fn an_error_is_kept_as_an_envelope_and_retrying_clears_it() {
        let (mut list, mut recorder) = list();
        list.refresh(&mut recorder.send());
        list.on_response(CommandId::from_raw(1), &Err(envelope()));
        assert!(list.error().is_some());
        assert!(!list.is_loading());
        list.refresh(&mut recorder.send());
        assert!(list.error().is_none());
    }

    #[test]
    fn only_the_four_library_events_refetch() {
        let (mut list, mut recorder) = list();
        assert!(list.on_ui_event(EventKind::LibraryChanged, &mut recorder.send()));
        assert!(list.on_ui_event(EventKind::RecordingImported, &mut recorder.send()));
        assert!(list.on_ui_event(EventKind::TranscriptCompleted, &mut recorder.send()));
        assert!(list.on_ui_event(EventKind::RecordingSourceUpdated, &mut recorder.send()));
        assert!(!list.on_ui_event(EventKind::DeviceDiscovered, &mut recorder.send()));
        assert_eq!(recorder.sent.len(), 4);
    }

    #[test]
    fn a_resynchronisation_restarts_the_chain_from_the_first_page() {
        let (mut list, mut recorder) = list();
        list.refresh(&mut recorder.send());
        list.on_response(
            CommandId::from_raw(1),
            &Ok(AppResponse::ListRecordings(page(rows(3), Some("02"), 5))),
        );
        list.on_resync(&mut recorder.send());
        assert!(list.rows().is_empty());
        assert_eq!(recorder.last_request().cursor, None);
    }
}
