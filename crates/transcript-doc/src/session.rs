//! The editing session: the document, the caret, the operation log, search and
//! autosave, behind one command surface.
//!
//! The widget in `plaudon-ui` calls these methods and renders the result. It
//! holds no editing logic of its own, which is why the behaviour below is
//! testable without a window, a GPU or a database.
//!
//! Time is supplied, never read: [`EditSession::set_now`] takes the session
//! clock in milliseconds, so coalescing and autosave are deterministic in
//! tests and driven by `egui`'s frame time at run time.

use plaudon_domain::ids::{SpeakerId, TranscriptRevisionId};

use crate::autosave::{Autosave, AutosaveState, SaveRequest};
use crate::caret::{move_position, Motion, Position, Selection};
use crate::clipboard;
use crate::doc::{DocError, SegmentId, TranscriptDocument};
use crate::history::History;
use crate::ops::{apply_group, revert_group, EditKind, Op, OpGroup};
use crate::search::{SearchMatch, SearchQuery, SearchResults};

/// An open transcript, with everything the editor needs to change it.
#[derive(Debug)]
pub struct EditSession {
    document: TranscriptDocument,
    selection: Selection,
    history: History,
    autosave: Autosave,
    search: SearchResults,
    now_ms: u64,
}

impl EditSession {
    /// Open a session over a document, with the caret at the start.
    #[must_use]
    pub fn new(document: TranscriptDocument) -> Self {
        let selection = document.first_id().map_or_else(
            || Selection::caret(Position::start_of(SegmentId::NONE)),
            |id| Selection::caret(Position::start_of(id)),
        );
        Self {
            document,
            selection,
            history: History::default(),
            autosave: Autosave::default(),
            search: SearchResults::default(),
            now_ms: 0,
        }
    }

    /// The document.
    #[must_use]
    pub const fn document(&self) -> &TranscriptDocument {
        &self.document
    }

    /// The current selection.
    #[must_use]
    pub const fn selection(&self) -> Selection {
        self.selection
    }

    /// Undo/redo state, for the toolbar and for announcements.
    #[must_use]
    pub const fn history(&self) -> &History {
        &self.history
    }

    /// Autosave state, for the status line.
    #[must_use]
    pub const fn autosave(&self) -> &Autosave {
        &self.autosave
    }

    /// Mutable autosave, to enable or disable it.
    pub const fn autosave_mut(&mut self) -> &mut Autosave {
        &mut self.autosave
    }

    /// The current search results.
    #[must_use]
    pub const fn search(&self) -> &SearchResults {
        &self.search
    }

    /// Advance the session clock. Call once per frame.
    pub fn set_now(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
    }

    /// The session clock.
    #[must_use]
    pub const fn now_ms(&self) -> u64 {
        self.now_ms
    }

    // ------------------------------------------------------------ selection

    /// Place the caret, collapsing any selection.
    pub fn set_caret(&mut self, position: Position) {
        self.set_selection(Selection::caret(position));
    }

    /// Replace the selection.
    pub fn set_selection(&mut self, selection: Selection) {
        let next = selection.clamped(&self.document);
        if next != self.selection {
            self.history.break_coalescing();
        }
        self.selection = next;
    }

    /// Move the caret; `extend` keeps the anchor, making a selection.
    pub fn move_caret(&mut self, motion: Motion, extend: bool) {
        let focus = move_position(&self.document, self.selection.focus, motion);
        let anchor = if extend { self.selection.anchor } else { focus };
        self.set_selection(Selection::new(anchor, focus));
    }

    /// Select the whole of one segment.
    pub fn select_segment(&mut self, id: SegmentId) {
        let Some(segment) = self.document.segment(id) else {
            return;
        };
        let len = segment.char_len();
        self.set_selection(Selection::new(
            Position::start_of(id),
            Position::new(id, len),
        ));
    }

    /// Select the whole transcript.
    pub fn select_all(&mut self) {
        let (Some(first), Some(last)) = (self.document.first_id(), self.document.last_id()) else {
            return;
        };
        let end = self
            .document
            .segment(last)
            .map_or(0, crate::doc::Segment::char_len);
        self.set_selection(Selection::new(
            Position::start_of(first),
            Position::new(last, end),
        ));
    }

    /// The selected text, with no timestamps.
    #[must_use]
    pub fn selected_text(&self) -> String {
        clipboard::selected_text(&self.document, self.selection)
    }

    /// The selection formatted as `[m:ss] text` lines, for "copy passage".
    #[must_use]
    pub fn copy_passage(&self) -> String {
        clipboard::format_passage(&self.document, self.selection)
    }

    // --------------------------------------------------------------- edits

    /// Insert text at the caret, replacing the selection.
    ///
    /// A single character with no selection coalesces with the previous
    /// keystroke; anything longer is its own undo step.
    pub fn insert_text(&mut self, text: &str) -> Result<(), DocError> {
        if text.is_empty() {
            return Ok(());
        }
        let kind = if text.chars().count() == 1 && self.selection.is_empty() {
            EditKind::Typing
        } else {
            EditKind::Paste
        };
        let before = self.selection;
        let mut ops = self.selection_removal_ops();
        let at = self.insertion_point();
        ops.push(Op::Insert {
            at,
            text: text.to_owned(),
        });
        let after = Selection::caret(clipboard::position_after_paste(at, text));
        self.commit(OpGroup::new(ops, kind, before, after, self.now_ms))
    }

    /// Delete the selection, or the character before the caret.
    pub fn delete_backward(&mut self) -> Result<(), DocError> {
        if !self.selection.is_empty() {
            return self.delete_selection();
        }
        let focus = self.selection.focus;
        if focus.offset == 0 {
            // Segments are never merged, so backspace at the start of a
            // segment moves to the end of the previous one instead of
            // destroying a boundary that anchors and speaker turns depend on.
            self.move_caret(Motion::CharLeft, false);
            return Ok(());
        }
        let Some(segment) = self.document.segment(focus.segment) else {
            return Err(DocError::UnknownSegment(focus.segment));
        };
        let at = Position::new(focus.segment, focus.offset - 1);
        let text = segment.slice(at.offset, focus.offset);
        let before = self.selection;
        self.commit(OpGroup::new(
            vec![Op::Delete {
                at,
                text,
            }],
            EditKind::DeletingBackward,
            before,
            Selection::caret(at),
            self.now_ms,
        ))
    }

    /// Delete the selection, or the character after the caret.
    pub fn delete_forward(&mut self) -> Result<(), DocError> {
        if !self.selection.is_empty() {
            return self.delete_selection();
        }
        let focus = self.selection.focus;
        let Some(segment) = self.document.segment(focus.segment) else {
            return Err(DocError::UnknownSegment(focus.segment));
        };
        if focus.offset >= segment.char_len() {
            self.move_caret(Motion::CharRight, false);
            return Ok(());
        }
        let text = segment.slice(focus.offset, focus.offset + 1);
        let before = self.selection;
        self.commit(OpGroup::new(
            vec![Op::Delete { at: focus, text }],
            EditKind::DeletingForward,
            before,
            Selection::caret(focus),
            self.now_ms,
        ))
    }

    /// Delete the current selection.
    ///
    /// A selection spanning several segments empties the covered text of each
    /// of them; it never merges or removes segments.
    pub fn delete_selection(&mut self) -> Result<(), DocError> {
        if self.selection.is_empty() {
            return Ok(());
        }
        let before = self.selection;
        let ops = self.selection_removal_ops();
        let after = Selection::caret(self.insertion_point());
        self.commit(OpGroup::new(
            ops,
            EditKind::Cut,
            before,
            after,
            self.now_ms,
        ))
    }

    /// Cut: the removed text is returned for the host to put on the clipboard.
    pub fn cut(&mut self) -> Result<String, DocError> {
        let text = self.selected_text();
        self.delete_selection()?;
        Ok(text)
    }

    /// Paste text at the caret, replacing the selection.
    ///
    /// Line breaks are flattened to spaces: a paste must not invent segments.
    pub fn paste(&mut self, text: &str) -> Result<(), DocError> {
        let sanitized = clipboard::sanitize_paste(text);
        if sanitized.is_empty() {
            return Ok(());
        }
        self.insert_text(&sanitized)
    }

    /// Replace a whole segment's text, as one undo step.
    ///
    /// Only the characters that actually changed are touched, so a correction
    /// at the end of a segment leaves anchors into its opening words intact.
    pub fn set_segment_text(&mut self, id: SegmentId, text: &str) -> Result<(), DocError> {
        let Some(segment) = self.document.segment(id) else {
            return Err(DocError::UnknownSegment(id));
        };
        let current: Vec<char> = segment.text().chars().collect();
        let next: Vec<char> = text.chars().collect();
        let prefix = current
            .iter()
            .zip(next.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = current[prefix..]
            .iter()
            .rev()
            .zip(next[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let removed: String = current[prefix..current.len() - suffix].iter().collect();
        let inserted: String = next[prefix..next.len() - suffix].iter().collect();
        if removed.is_empty() && inserted.is_empty() {
            return Ok(());
        }
        let at = Position::new(id, prefix);
        let mut ops = Vec::with_capacity(2);
        if !removed.is_empty() {
            ops.push(Op::Delete {
                at,
                text: removed,
            });
        }
        if !inserted.is_empty() {
            ops.push(Op::Insert {
                at,
                text: inserted.clone(),
            });
        }
        let before = self.selection;
        let after = Selection::caret(Position::new(id, prefix + inserted.chars().count()));
        self.commit(OpGroup::new(
            ops,
            EditKind::Correction,
            before,
            after,
            self.now_ms,
        ))
    }

    /// Reassign a segment's speaker, as its own undo step.
    pub fn set_speaker(
        &mut self,
        id: SegmentId,
        speaker: Option<SpeakerId>,
    ) -> Result<(), DocError> {
        let Some(segment) = self.document.segment(id) else {
            return Err(DocError::UnknownSegment(id));
        };
        if segment.speaker_id == speaker {
            return Ok(());
        }
        let op = Op::SetSpeaker {
            segment: id,
            before: segment.speaker_id,
            after: speaker,
        };
        let selection = self.selection;
        self.commit(OpGroup::new(
            vec![op],
            EditKind::SpeakerChange,
            selection,
            selection,
            self.now_ms,
        ))
    }

    /// Reassign every segment currently attributed to `from`.
    ///
    /// One undo step, because "that was Dana, not Speaker 2" is one decision.
    pub fn reassign_speaker_everywhere(
        &mut self,
        from: Option<SpeakerId>,
        to: Option<SpeakerId>,
    ) -> Result<usize, DocError> {
        if from == to {
            return Ok(0);
        }
        let ops: Vec<Op> = self
            .document
            .segments()
            .iter()
            .filter(|segment| segment.speaker_id == from)
            .map(|segment| Op::SetSpeaker {
                segment: segment.id,
                before: from,
                after: to,
            })
            .collect();
        let count = ops.len();
        if count == 0 {
            return Ok(0);
        }
        let selection = self.selection;
        self.commit(OpGroup::new(
            ops,
            EditKind::SpeakerChange,
            selection,
            selection,
            self.now_ms,
        ))?;
        Ok(count)
    }

    // ------------------------------------------------------------ undo/redo

    /// Undo the last step.
    pub fn undo(&mut self) -> Result<bool, DocError> {
        let Some(group) = self.history.pop_undo() else {
            return Ok(false);
        };
        revert_group(&mut self.document, &group)?;
        self.selection = group.selection_before.clamped(&self.document);
        self.after_mutation();
        Ok(true)
    }

    /// Redo the last undone step.
    pub fn redo(&mut self) -> Result<bool, DocError> {
        let Some(group) = self.history.pop_redo() else {
            return Ok(false);
        };
        apply_group(&mut self.document, &group)?;
        self.selection = group.selection_after.clamped(&self.document);
        self.after_mutation();
        Ok(true)
    }

    // -------------------------------------------------------------- search

    /// Run a query over the document.
    pub fn set_search_query(&mut self, query: SearchQuery) {
        self.search = SearchResults::run(&self.document, query);
        let focus = self.selection.focus;
        self.search
            .focus_nearest(&self.document, focus.segment, focus.offset);
    }

    /// Clear the search.
    pub fn clear_search(&mut self) {
        self.search = SearchResults::default();
    }

    /// Move to the next match and select it.
    pub fn focus_next_match(&mut self) -> Option<SearchMatch> {
        self.search.refresh(&self.document);
        let hit = self.search.next_match()?;
        self.select_match(hit);
        Some(hit)
    }

    /// Move to the previous match and select it.
    pub fn focus_previous_match(&mut self) -> Option<SearchMatch> {
        self.search.refresh(&self.document);
        let hit = self.search.previous_match()?;
        self.select_match(hit);
        Some(hit)
    }

    /// Select the current match without moving.
    pub fn focus_current_match(&mut self) -> Option<SearchMatch> {
        self.search.refresh(&self.document);
        let hit = self.search.current()?;
        self.select_match(hit);
        Some(hit)
    }

    fn select_match(&mut self, hit: SearchMatch) {
        self.set_selection(Selection::new(
            Position::new(hit.segment, hit.start),
            Position::new(hit.segment, hit.end),
        ));
    }

    /// Replace the current match, then move to the next one.
    pub fn replace_current_match(&mut self, replacement: &str) -> Result<bool, DocError> {
        self.search.refresh(&self.document);
        let Some(hit) = self.search.current() else {
            return Ok(false);
        };
        let Some(segment) = self.document.segment(hit.segment) else {
            return Ok(false);
        };
        let removed = segment.slice(hit.start, hit.end);
        let at = Position::new(hit.segment, hit.start);
        let mut ops = vec![Op::Delete {
            at,
            text: removed,
        }];
        if !replacement.is_empty() {
            ops.push(Op::Insert {
                at,
                text: replacement.to_owned(),
            });
        }
        let before = self.selection;
        let after = Selection::caret(Position::new(
            hit.segment,
            hit.start + replacement.chars().count(),
        ));
        self.commit(OpGroup::new(
            ops,
            EditKind::Replace,
            before,
            after,
            self.now_ms,
        ))?;
        self.search.refresh(&self.document);
        self.focus_current_match();
        Ok(true)
    }

    /// Replace every match in the transcript as **one** undoable step.
    ///
    /// Returns the number of matches replaced.
    pub fn replace_all(&mut self, replacement: &str) -> Result<usize, DocError> {
        self.search.refresh(&self.document);
        let matches = self.search.matches().to_vec();
        if matches.is_empty() {
            return Ok(0);
        }
        // Apply from the end backwards: every operation is recorded with the
        // offsets it was applied at, so replaying the group reproduces the
        // same document.
        let mut ops = Vec::with_capacity(matches.len() * 2);
        for hit in matches.iter().rev() {
            let Some(segment) = self.document.segment(hit.segment) else {
                continue;
            };
            let at = Position::new(hit.segment, hit.start);
            ops.push(Op::Delete {
                at,
                text: segment.slice(hit.start, hit.end),
            });
            if !replacement.is_empty() {
                ops.push(Op::Insert {
                    at,
                    text: replacement.to_owned(),
                });
            }
        }
        let count = matches.len();
        let before = self.selection;
        let after = Selection::caret(before.focus);
        self.commit(OpGroup::new(
            ops,
            EditKind::Replace,
            before,
            after,
            self.now_ms,
        ))?;
        self.search.refresh(&self.document);
        Ok(count)
    }

    // ------------------------------------------------------------ autosave

    /// The save to issue now, if the debounce says so.
    pub fn poll_autosave(&mut self) -> Option<SaveRequest> {
        let now = self.now_ms;
        self.autosave.take_request(now, &self.document)
    }

    /// Force a save of the pending edits, for an explicit save or a shutdown
    /// flush.
    pub fn save_now(&mut self) -> Option<SaveRequest> {
        self.autosave.request_now(&self.document)
    }

    /// A save succeeded and produced `revision_id`.
    ///
    /// Anchors must be mapped with [`crate::anchors::map_anchor`] *before*
    /// this call: it makes the saved text the new base.
    pub fn note_saved(&mut self, revision_id: TranscriptRevisionId) {
        self.document.adopt_saved_revision(revision_id);
        self.autosave.note_saved();
    }

    /// A save failed transiently.
    pub fn note_save_failed(&mut self) {
        let now = self.now_ms;
        self.autosave.note_failed(now);
    }

    /// A save hit a conflict: the base revision is no longer the head.
    pub fn note_save_conflict(&mut self) {
        self.autosave.note_conflict();
    }

    /// Autosave state for the status line.
    #[must_use]
    pub const fn autosave_state(&self) -> AutosaveState {
        self.autosave.state()
    }

    // ------------------------------------------------------------- private

    fn insertion_point(&self) -> Position {
        self.selection.ordered(&self.document).0
    }

    /// Operations that remove the current selection, in application order.
    fn selection_removal_ops(&self) -> Vec<Op> {
        if self.selection.is_empty() {
            return Vec::new();
        }
        let (start, end) = self.selection.ordered(&self.document);
        let (Some(first), Some(last)) = (
            self.document.position(start.segment),
            self.document.position(end.segment),
        ) else {
            return Vec::new();
        };
        let mut ops = Vec::new();
        // Later segments first, so earlier offsets stay valid as the
        // operations are applied in order.
        for position in (first..=last).rev() {
            let Some(segment) = self.document.segment_at(position) else {
                continue;
            };
            let from = if position == first { start.offset } else { 0 };
            let to = if position == last {
                end.offset
            } else {
                segment.char_len()
            };
            if to <= from {
                continue;
            }
            ops.push(Op::Delete {
                at: Position::new(segment.id, from),
                text: segment.slice(from, to),
            });
        }
        ops
    }

    fn commit(&mut self, group: OpGroup) -> Result<(), DocError> {
        if group.is_empty() {
            return Ok(());
        }
        apply_group(&mut self.document, &group)?;
        self.selection = group.selection_after.clamped(&self.document);
        self.history.record(group);
        self.after_mutation();
        Ok(())
    }

    fn after_mutation(&mut self) {
        let now = self.now_ms;
        self.autosave.note_edit(now);
        self.search.refresh(&self.document);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{document_with, timed_document};

    fn session(texts: &[&str]) -> EditSession {
        EditSession::new(document_with(texts))
    }

    fn ids(session: &EditSession) -> Vec<SegmentId> {
        session.document().segments().iter().map(|s| s.id).collect()
    }

    fn text_of(session: &EditSession, index: usize) -> String {
        session
            .document()
            .segment_at(index)
            .expect("segment")
            .text()
            .into_owned()
    }

    #[test]
    fn typing_inserts_at_the_caret_and_coalesces_into_one_undo_step() {
        let mut session = session(&["we will pay"]);
        let id = ids(&session)[0];
        session.set_caret(Position::new(id, 8));
        for (step, letter) in "not ".chars().enumerate() {
            session.set_now(step as u64 * 50);
            session.insert_text(&letter.to_string()).expect("type");
        }
        assert_eq!(text_of(&session, 0), "we will not pay");
        // "not" coalesces; the trailing space ends the run.
        assert_eq!(session.history().undo_depth(), 2);
        session.undo().expect("undo");
        session.undo().expect("undo");
        assert_eq!(text_of(&session, 0), "we will pay");
        session.redo().expect("redo");
        session.redo().expect("redo");
        assert_eq!(text_of(&session, 0), "we will not pay");
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let mut session = session(&["we will not pay"]);
        let id = ids(&session)[0];
        session.set_selection(Selection::new(
            Position::new(id, 8),
            Position::new(id, 11),
        ));
        session.insert_text("never").expect("insert");
        assert_eq!(text_of(&session, 0), "we will never pay");
        session.undo().expect("undo");
        assert_eq!(text_of(&session, 0), "we will not pay");
    }

    #[test]
    fn backspace_at_a_segment_start_moves_instead_of_merging_segments() {
        let mut session = session(&["one", "two"]);
        let segment_ids = ids(&session);
        session.set_caret(Position::start_of(segment_ids[1]));
        session.delete_backward().expect("backspace");
        assert_eq!(session.document().len(), 2);
        assert_eq!(text_of(&session, 0), "one");
        assert_eq!(
            session.selection().focus,
            Position::new(segment_ids[0], 3)
        );
    }

    #[test]
    fn a_cross_segment_selection_is_deleted_without_losing_segments() {
        let mut session = session(&["alpha", "beta", "gamma"]);
        let segment_ids = ids(&session);
        session.set_selection(Selection::new(
            Position::new(segment_ids[0], 2),
            Position::new(segment_ids[2], 3),
        ));
        let cut = session.cut().expect("cut");
        assert_eq!(cut, "pha\nbeta\ngam");
        assert_eq!(session.document().len(), 3);
        assert_eq!(text_of(&session, 0), "al");
        assert_eq!(text_of(&session, 1), "");
        assert_eq!(text_of(&session, 2), "ma");
        session.undo().expect("undo");
        assert_eq!(text_of(&session, 1), "beta");
    }

    #[test]
    fn pasting_multiple_lines_does_not_create_segments() {
        let mut session = session(&["one"]);
        let id = ids(&session)[0];
        session.set_caret(Position::new(id, 3));
        session.paste(" two\nthree").expect("paste");
        assert_eq!(session.document().len(), 1);
        assert_eq!(text_of(&session, 0), "one two three");
    }

    #[test]
    fn a_correction_touches_only_the_changed_characters() {
        let mut session = session(&["we will not pay the invoice"]);
        let id = ids(&session)[0];
        session
            .set_segment_text(id, "we will not pay the invoices")
            .expect("correct");
        assert_eq!(text_of(&session, 0), "we will not pay the invoices");
        let group = &session.history().undo_stack()[0];
        assert_eq!(
            group.ops,
            vec![Op::Insert {
                at: Position::new(id, 27),
                text: "s".to_owned()
            }]
        );
    }

    #[test]
    fn speaker_reassignment_is_one_undo_step_and_leaves_text_alone() {
        let mut session = session(&["one", "two"]);
        let segment_ids = ids(&session);
        let speaker = SpeakerId::new();
        session.set_speaker(segment_ids[0], Some(speaker)).expect("assign");
        assert_eq!(
            session.document().segment(segment_ids[0]).expect("segment").speaker_id,
            Some(speaker)
        );
        assert_eq!(text_of(&session, 0), "one");
        assert_eq!(
            session.document().speaker_reassignments(),
            vec![(0, Some(speaker))]
        );
        session.undo().expect("undo");
        assert_eq!(
            session.document().segment(segment_ids[0]).expect("segment").speaker_id,
            None
        );
    }

    #[test]
    fn reassigning_a_whole_speaker_is_one_step() {
        let mut session = session(&["one", "two", "three"]);
        let speaker = SpeakerId::new();
        let other = SpeakerId::new();
        let segment_ids = ids(&session);
        session.set_speaker(segment_ids[0], Some(speaker)).expect("assign");
        session.set_speaker(segment_ids[1], Some(speaker)).expect("assign");
        let changed = session
            .reassign_speaker_everywhere(Some(speaker), Some(other))
            .expect("reassign");
        assert_eq!(changed, 2);
        session.undo().expect("undo");
        assert_eq!(
            session.document().segment(segment_ids[1]).expect("segment").speaker_id,
            Some(speaker)
        );
    }

    #[test]
    fn replace_all_is_one_undo_step() {
        let mut session = session(&["pay the invoice", "pay again", "nothing here"]);
        session.set_search_query(SearchQuery::literal("pay"));
        assert_eq!(session.search().len(), 2);
        let replaced = session.replace_all("settle").expect("replace");
        assert_eq!(replaced, 2);
        assert_eq!(text_of(&session, 0), "settle the invoice");
        assert_eq!(text_of(&session, 1), "settle again");
        session.undo().expect("undo");
        assert_eq!(text_of(&session, 0), "pay the invoice");
        assert_eq!(text_of(&session, 1), "pay again");
    }

    #[test]
    fn replace_all_handles_several_matches_in_one_segment() {
        let mut session = session(&["pay pay pay"]);
        session.set_search_query(SearchQuery::literal("pay"));
        assert_eq!(session.replace_all("owe").expect("replace"), 3);
        assert_eq!(text_of(&session, 0), "owe owe owe");
        session.undo().expect("undo");
        assert_eq!(text_of(&session, 0), "pay pay pay");
    }

    #[test]
    fn replacing_the_current_match_advances_to_the_next() {
        let mut session = session(&["pay now", "pay later"]);
        session.set_search_query(SearchQuery::literal("pay"));
        assert!(session.replace_current_match("settle").expect("replace"));
        assert_eq!(text_of(&session, 0), "settle now");
        assert_eq!(session.search().len(), 1);
        let hit = session.search().current().expect("match");
        assert_eq!(
            session.document().position(hit.segment),
            Some(1),
            "the surviving match is in the second segment"
        );
    }

    #[test]
    fn search_navigation_selects_the_match() {
        let mut session = session(&["one two", "two three"]);
        session.set_search_query(SearchQuery::literal("two"));
        let hit = session.focus_current_match().expect("match");
        assert_eq!(
            session.selection(),
            Selection::new(
                Position::new(hit.segment, hit.start),
                Position::new(hit.segment, hit.end)
            )
        );
        let next = session.focus_next_match().expect("next");
        assert_ne!(next.segment, hit.segment);
    }

    #[test]
    fn copying_a_passage_uses_timestamps_when_the_alignment_allows_it() {
        let mut session = EditSession::new(timed_document(&[
            (0, 1_000, "we keep it local"),
            (42_000, 44_000, "always"),
        ]));
        session.select_all();
        assert_eq!(
            session.copy_passage(),
            "[0:00] we keep it local\n[0:42] always"
        );
    }

    #[test]
    fn autosave_sends_only_changed_segments_after_the_debounce() {
        let mut session = session(&["one", "two"]);
        let id = ids(&session)[1];
        session.set_now(1_000);
        session.set_caret(Position::new(id, 3));
        session.insert_text("!").expect("type");
        assert!(session.poll_autosave().is_none());
        session.set_now(1_000 + crate::autosave::DEFAULT_DEBOUNCE_MS);
        let request = session.poll_autosave().expect("request");
        assert_eq!(request.edits, vec![(1, "two!".to_owned())]);
        let saved = TranscriptRevisionId::new();
        session.note_saved(saved);
        assert_eq!(session.document().base_revision_id(), saved);
        assert_eq!(session.autosave_state(), AutosaveState::Idle);
    }

    #[test]
    fn undo_after_a_save_makes_the_document_dirty_again() {
        let mut session = session(&["one"]);
        let id = ids(&session)[0];
        session.set_caret(Position::new(id, 3));
        session.insert_text("!").expect("type");
        session.set_now(10_000);
        session.poll_autosave().expect("request");
        session.note_saved(TranscriptRevisionId::new());
        assert!(!session.document().is_dirty());
        session.undo().expect("undo");
        assert_eq!(text_of(&session, 0), "one");
        assert!(session.document().is_dirty());
    }

    #[test]
    fn a_read_only_document_refuses_edits_without_panicking() {
        let mut session = EditSession::new(crate::testing::read_only_document(&["one"]));
        let id = ids(&session)[0];
        session.set_caret(Position::new(id, 3));
        assert_eq!(session.insert_text("!"), Err(DocError::ReadOnly));
        assert_eq!(text_of(&session, 0), "one");
    }
}
