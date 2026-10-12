//! Debounced autosave of transcript edits.
//!
//! This module decides *when* to save and *what* to send. It does not save:
//! persistence goes through the existing `save_transcript_revision` port,
//! which creates a child revision against a `base_revision_id` and answers
//! `Conflict` when the base is no longer the head. The UI never touches
//! SQLite, and the state machine here is pure so its timing is testable
//! without a clock.
//!
//! Two rules come straight from the behaviour the React pane already had:
//!
//! * a conflict never discards the user's text — autosave stops and waits for
//!   the user to choose (reload the newest revision, or keep their copy);
//! * a failure is retried with a backoff, not silently dropped.

use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};

use crate::doc::TranscriptDocument;

/// Quiet period after the last keystroke before a save is attempted.
pub const DEFAULT_DEBOUNCE_MS: u64 = 1_200;

/// Longest a continuously typing user goes unsaved.
pub const DEFAULT_MAX_DELAY_MS: u64 = 15_000;

/// Delay before retrying after a failed save.
pub const DEFAULT_RETRY_BACKOFF_MS: u64 = 5_000;

/// The payload of one `save_transcript_revision` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveRequest {
    /// Recording being edited.
    pub recording_id: RecordingId,
    /// Revision the edits are expressed against.
    pub base_revision_id: TranscriptRevisionId,
    /// Changed segments only, as `(seq_index, text)`, ascending.
    pub edits: Vec<(u32, String)>,
}

/// What autosave is doing, for the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutosaveState {
    /// Nothing to save.
    Idle,
    /// Edits are waiting for the debounce to elapse.
    Pending,
    /// A save is in flight.
    Saving,
    /// A save failed and will be retried.
    Retrying,
    /// The base revision is no longer the head. Autosave is stopped until the
    /// user resolves the conflict; their text is still here.
    Conflict,
}

impl AutosaveState {
    /// A short status label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "All changes saved",
            Self::Pending => "Saving shortly",
            Self::Saving => "Saving",
            Self::Retrying => "Save failed, retrying",
            Self::Conflict => "Someone else saved a newer revision",
        }
    }
}

/// The debounce state machine.
#[derive(Debug, Clone)]
pub struct Autosave {
    debounce_ms: u64,
    max_delay_ms: u64,
    retry_backoff_ms: u64,
    dirty_since_ms: Option<u64>,
    last_edit_ms: u64,
    not_before_ms: u64,
    in_flight: bool,
    conflict: bool,
    enabled: bool,
}

impl Default for Autosave {
    fn default() -> Self {
        Self::new(
            DEFAULT_DEBOUNCE_MS,
            DEFAULT_MAX_DELAY_MS,
            DEFAULT_RETRY_BACKOFF_MS,
        )
    }
}

impl Autosave {
    /// An autosave with explicit timings.
    #[must_use]
    pub const fn new(debounce_ms: u64, max_delay_ms: u64, retry_backoff_ms: u64) -> Self {
        Self {
            debounce_ms,
            max_delay_ms,
            retry_backoff_ms,
            dirty_since_ms: None,
            last_edit_ms: 0,
            not_before_ms: 0,
            in_flight: false,
            conflict: false,
            enabled: true,
        }
    }

    /// Turn autosave off; explicit saves still work.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Whether autosave is on.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Current state.
    #[must_use]
    pub const fn state(&self) -> AutosaveState {
        if self.conflict {
            AutosaveState::Conflict
        } else if self.in_flight {
            AutosaveState::Saving
        } else if self.dirty_since_ms.is_none() {
            AutosaveState::Idle
        } else if self.not_before_ms > 0 {
            AutosaveState::Retrying
        } else {
            AutosaveState::Pending
        }
    }

    /// Record that the document changed at `now_ms`.
    pub fn note_edit(&mut self, now_ms: u64) {
        self.last_edit_ms = now_ms;
        if self.dirty_since_ms.is_none() {
            self.dirty_since_ms = Some(now_ms);
        }
    }

    /// The save to issue now, if any.
    ///
    /// Returns `None` while the user is still typing, while a save is in
    /// flight, after a conflict, and when there is nothing to save.
    pub fn take_request(
        &mut self,
        now_ms: u64,
        document: &TranscriptDocument,
    ) -> Option<SaveRequest> {
        if !self.enabled || self.in_flight || self.conflict || !document.is_editable() {
            return None;
        }
        let dirty_since = self.dirty_since_ms?;
        if now_ms < self.not_before_ms {
            return None;
        }
        let quiet = now_ms.saturating_sub(self.last_edit_ms) >= self.debounce_ms;
        let overdue = now_ms.saturating_sub(dirty_since) >= self.max_delay_ms;
        if !quiet && !overdue {
            return None;
        }
        self.request_now(document)
    }

    /// The save to issue regardless of the debounce, for an explicit save or
    /// for a window-close flush.
    pub fn request_now(&mut self, document: &TranscriptDocument) -> Option<SaveRequest> {
        if self.in_flight || !document.is_editable() {
            return None;
        }
        let edits = document.text_edits();
        if edits.is_empty() {
            self.dirty_since_ms = None;
            self.not_before_ms = 0;
            return None;
        }
        self.in_flight = true;
        self.not_before_ms = 0;
        Some(SaveRequest {
            recording_id: document.recording_id(),
            base_revision_id: document.base_revision_id(),
            edits,
        })
    }

    /// Whether a save is in flight.
    #[must_use]
    pub const fn is_in_flight(&self) -> bool {
        self.in_flight
    }

    /// The save succeeded.
    ///
    /// The caller must also call
    /// [`TranscriptDocument::adopt_saved_revision`] (after mapping any
    /// anchors) so later edits are expressed against the saved revision.
    pub fn note_saved(&mut self) {
        self.in_flight = false;
        self.dirty_since_ms = None;
        self.not_before_ms = 0;
    }

    /// The save failed for a transient reason; retry after the backoff.
    pub fn note_failed(&mut self, now_ms: u64) {
        self.in_flight = false;
        self.not_before_ms = now_ms + self.retry_backoff_ms;
        if self.dirty_since_ms.is_none() {
            self.dirty_since_ms = Some(now_ms);
        }
    }

    /// The base revision is no longer the head.
    ///
    /// Autosave stops. The document keeps every edit, so the user can still
    /// copy their text or reload the newest revision.
    pub fn note_conflict(&mut self) {
        self.in_flight = false;
        self.conflict = true;
    }

    /// Whether a conflict is outstanding.
    #[must_use]
    pub const fn has_conflict(&self) -> bool {
        self.conflict
    }

    /// The user resolved the conflict; resume autosaving.
    pub fn clear_conflict(&mut self, now_ms: u64) {
        self.conflict = false;
        self.in_flight = false;
        self.last_edit_ms = now_ms;
        self.not_before_ms = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::document_with;

    fn dirty_document() -> TranscriptDocument {
        let mut document = document_with(&["one", "two"]);
        let id = document.first_id().expect("segment");
        document.insert_text(id, 3, "!").expect("insert");
        document
    }

    #[test]
    fn nothing_is_saved_while_the_user_is_still_typing() {
        let document = dirty_document();
        let mut autosave = Autosave::default();
        autosave.note_edit(1_000);
        assert_eq!(autosave.take_request(1_500, &document), None);
        assert_eq!(autosave.state(), AutosaveState::Pending);
    }

    #[test]
    fn the_quiet_period_produces_exactly_the_changed_segments() {
        let document = dirty_document();
        let mut autosave = Autosave::default();
        autosave.note_edit(1_000);
        let request = autosave
            .take_request(1_000 + DEFAULT_DEBOUNCE_MS, &document)
            .expect("request");
        assert_eq!(request.edits, vec![(0, "one!".to_owned())]);
        assert_eq!(request.base_revision_id, document.base_revision_id());
        assert_eq!(autosave.state(), AutosaveState::Saving);
        // Nothing else is issued while the first save is in flight.
        assert_eq!(autosave.take_request(99_999, &document), None);
    }

    #[test]
    fn a_continuously_typing_user_is_still_saved() {
        let document = dirty_document();
        let mut autosave = Autosave::default();
        autosave.note_edit(0);
        for tick in 1..=30u64 {
            let now = tick * 500;
            autosave.note_edit(now);
            if let Some(request) = autosave.take_request(now, &document) {
                assert!(now <= DEFAULT_MAX_DELAY_MS + 500, "saved far too late");
                assert_eq!(request.edits.len(), 1);
                return;
            }
        }
        panic!("a continuously typing user was never saved");
    }

    #[test]
    fn a_clean_document_produces_no_request() {
        let document = document_with(&["one"]);
        let mut autosave = Autosave::default();
        autosave.note_edit(0);
        assert_eq!(autosave.take_request(10_000, &document), None);
        assert_eq!(autosave.state(), AutosaveState::Idle);
    }

    #[test]
    fn a_conflict_stops_autosave_and_keeps_the_edits() {
        let document = dirty_document();
        let mut autosave = Autosave::default();
        autosave.note_edit(0);
        autosave
            .take_request(DEFAULT_DEBOUNCE_MS, &document)
            .expect("request");
        autosave.note_conflict();
        assert_eq!(autosave.state(), AutosaveState::Conflict);
        assert_eq!(autosave.take_request(1_000_000, &document), None);
        assert!(
            document.is_dirty(),
            "the user's text must survive a conflict"
        );
        autosave.clear_conflict(1_000_000);
        assert!(autosave
            .take_request(1_000_000 + DEFAULT_DEBOUNCE_MS, &document)
            .is_some());
    }

    #[test]
    fn a_failure_is_retried_after_the_backoff() {
        let document = dirty_document();
        let mut autosave = Autosave::default();
        autosave.note_edit(0);
        autosave
            .take_request(DEFAULT_DEBOUNCE_MS, &document)
            .expect("request");
        autosave.note_failed(DEFAULT_DEBOUNCE_MS);
        assert_eq!(autosave.state(), AutosaveState::Retrying);
        assert_eq!(
            autosave.take_request(DEFAULT_DEBOUNCE_MS + 1, &document),
            None
        );
        assert!(autosave
            .take_request(DEFAULT_DEBOUNCE_MS + DEFAULT_RETRY_BACKOFF_MS, &document)
            .is_some());
    }

    #[test]
    fn a_read_only_document_is_never_autosaved() {
        let document = crate::testing::read_only_document(&["one"]);
        let mut autosave = Autosave::default();
        autosave.note_edit(0);
        assert_eq!(autosave.take_request(100_000, &document), None);
    }
}
