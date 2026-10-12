//! Undo and redo over the operation log, with typing coalescing.
//!
//! The React editor coalesced one editing *session* into one undo step. A
//! caret editor has no sessions, so the rule here is the conventional one:
//! consecutive single-character typing in the same segment merges while it
//! stays contiguous and no more than [`COALESCE_WINDOW_MS`] passes between
//! keystrokes. Every other kind of edit — paste, cut, replace, speaker change,
//! correction — is its own step, so a replace-all can be undone in one action.

use crate::ops::{Op, OpGroup};

/// Longest gap between two keystrokes that still merges them into one undo
/// step.
pub const COALESCE_WINDOW_MS: u64 = 600;

/// Most undo steps retained. A 3-hour transcript edit session is long; the
/// cap bounds memory without being reachable in practice by hand.
pub const DEFAULT_CAPACITY: usize = 2_000;

/// The undo and redo stacks.
#[derive(Debug, Clone)]
pub struct History {
    undo: Vec<OpGroup>,
    redo: Vec<OpGroup>,
    capacity: usize,
    coalesce_window_ms: u64,
    /// Set after an undo, a redo or a selection change that breaks a typing
    /// run, so the next keystroke starts a new step.
    barrier: bool,
}

impl Default for History {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY, COALESCE_WINDOW_MS)
    }
}

impl History {
    /// A history with an explicit capacity and coalescing window.
    #[must_use]
    pub const fn new(capacity: usize, coalesce_window_ms: u64) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            capacity,
            coalesce_window_ms,
            barrier: true,
        }
    }

    /// Whether there is anything to undo.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether there is anything to redo.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Number of undo steps retained.
    #[must_use]
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    /// Number of redo steps retained.
    #[must_use]
    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    /// Label of the step the next undo would reverse.
    #[must_use]
    pub fn undo_label(&self) -> Option<&'static str> {
        self.undo.last().map(|group| group.kind.label())
    }

    /// Label of the step the next redo would reapply.
    #[must_use]
    pub fn redo_label(&self) -> Option<&'static str> {
        self.redo.last().map(|group| group.kind.label())
    }

    /// End the current coalescing run, so the next edit starts a new step.
    pub fn break_coalescing(&mut self) {
        self.barrier = true;
    }

    /// Record an applied group, merging it into the previous step when the two
    /// are a continuation of the same typing or deleting run.
    ///
    /// Returns `true` when the group was merged rather than pushed.
    pub fn record(&mut self, group: OpGroup) -> bool {
        if group.is_empty() {
            return false;
        }
        self.redo.clear();
        let merged = if self.barrier {
            false
        } else {
            self.try_merge(&group)
        };
        self.barrier = false;
        if !merged {
            self.undo.push(group);
            if self.undo.len() > self.capacity {
                self.undo.remove(0);
            }
        }
        merged
    }

    /// Take the next step to undo. The caller applies its inverse.
    pub fn pop_undo(&mut self) -> Option<OpGroup> {
        self.barrier = true;
        let group = self.undo.pop()?;
        self.redo.push(group.clone());
        Some(group)
    }

    /// Take the next step to redo. The caller re-applies it.
    pub fn pop_redo(&mut self) -> Option<OpGroup> {
        self.barrier = true;
        let group = self.redo.pop()?;
        self.undo.push(group.clone());
        Some(group)
    }

    /// Every recorded step, oldest first. Replaying these over a freshly
    /// opened document reproduces the current one.
    #[must_use]
    pub fn undo_stack(&self) -> &[OpGroup] {
        &self.undo
    }

    fn try_merge(&mut self, group: &OpGroup) -> bool {
        let Some(last) = self.undo.last_mut() else {
            return false;
        };
        if last.kind != group.kind
            || !group.kind.coalesces()
            || group.at_ms.saturating_sub(last.at_ms) > self.coalesce_window_ms
        {
            return false;
        }
        let (Some(previous), Some(next)) = (last.ops.last().cloned(), group.ops.first()) else {
            return false;
        };
        if group.ops.len() != 1 {
            return false;
        }
        match (previous, next) {
            (
                Op::Insert {
                    at: previous_at,
                    text: previous_text,
                },
                Op::Insert { at, text },
            ) if previous_at.segment == at.segment
                && previous_at.offset + previous_text.chars().count() == at.offset
                && !text.contains(char::is_whitespace) =>
            {
                let combined = format!("{previous_text}{text}");
                let index = last.ops.len() - 1;
                last.ops[index] = Op::Insert {
                    at: previous_at,
                    text: combined,
                };
            }
            (
                Op::Delete {
                    at: previous_at,
                    text: previous_text,
                },
                Op::Delete { at, text },
            ) if previous_at.segment == at.segment
                && at.offset + text.chars().count() == previous_at.offset =>
            {
                // Backspace: the newer deletion sits immediately before the
                // older one, so the merged run starts at the newer offset.
                let combined = format!("{text}{previous_text}");
                let index = last.ops.len() - 1;
                last.ops[index] = Op::Delete {
                    at: *at,
                    text: combined,
                };
            }
            (
                Op::Delete {
                    at: previous_at,
                    text: previous_text,
                },
                Op::Delete { at, text },
            ) if previous_at.segment == at.segment && previous_at.offset == at.offset => {
                // Forward delete: successive deletions start at the same
                // offset, so the merged run is the concatenation in order.
                let combined = format!("{previous_text}{text}");
                let index = last.ops.len() - 1;
                last.ops[index] = Op::Delete {
                    at: previous_at,
                    text: combined,
                };
            }
            _ => return false,
        }
        last.selection_after = group.selection_after;
        last.at_ms = group.at_ms;
        true
    }
}

/// Replay a log over a document.
///
/// This is how the op-log/state equivalence property is checked, and it is the
/// contract that lets a crash-recovery path rebuild unsaved edits from the log
/// instead of from a half-written document.
pub fn replay(
    document: &mut crate::doc::TranscriptDocument,
    groups: &[OpGroup],
) -> Result<(), crate::doc::DocError> {
    for group in groups {
        crate::ops::apply_group(document, group)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caret::{Position, Selection};
    use crate::ops::EditKind;
    use crate::doc::SegmentId;

    fn typing(segment: SegmentId, offset: usize, text: &str, at_ms: u64) -> OpGroup {
        OpGroup::new(
            vec![Op::Insert {
                at: Position::new(segment, offset),
                text: text.to_owned(),
            }],
            EditKind::Typing,
            Selection::caret(Position::new(segment, offset)),
            Selection::caret(Position::new(segment, offset + text.chars().count())),
            at_ms,
        )
    }

    fn segment_id(document: &crate::doc::TranscriptDocument) -> SegmentId {
        document.first_id().expect("segment")
    }

    #[test]
    fn contiguous_typing_merges_into_one_step() {
        let document = crate::testing::document_with(&["hi"]);
        let id = segment_id(&document);
        let mut history = History::default();
        history.record(typing(id, 2, "t", 0));
        assert!(history.record(typing(id, 3, "h", 100)));
        assert!(history.record(typing(id, 4, "x", 200)));
        assert_eq!(history.undo_depth(), 1);
        let group = history.undo_stack()[0].clone();
        assert_eq!(
            group.ops,
            vec![Op::Insert {
                at: Position::new(id, 2),
                text: "thx".to_owned()
            }]
        );
    }

    #[test]
    fn a_pause_breaks_the_typing_run() {
        let document = crate::testing::document_with(&["hi"]);
        let id = segment_id(&document);
        let mut history = History::default();
        history.record(typing(id, 2, "a", 0));
        assert!(!history.record(typing(id, 3, "b", COALESCE_WINDOW_MS + 1)));
        assert_eq!(history.undo_depth(), 2);
    }

    #[test]
    fn whitespace_ends_a_typing_run_so_undo_steps_follow_words() {
        let document = crate::testing::document_with(&["hi"]);
        let id = segment_id(&document);
        let mut history = History::default();
        history.record(typing(id, 2, "a", 0));
        assert!(!history.record(typing(id, 3, " ", 10)));
        assert_eq!(history.undo_depth(), 2);
    }

    #[test]
    fn typing_in_another_segment_starts_a_new_step() {
        let document = crate::testing::document_with(&["hi", "there"]);
        let ids: Vec<SegmentId> = document.segments().iter().map(|s| s.id).collect();
        let mut history = History::default();
        history.record(typing(ids[0], 2, "a", 0));
        assert!(!history.record(typing(ids[1], 5, "b", 10)));
        assert_eq!(history.undo_depth(), 2);
    }

    #[test]
    fn a_replace_is_never_merged() {
        let document = crate::testing::document_with(&["hi"]);
        let id = segment_id(&document);
        let mut history = History::default();
        let mut group = typing(id, 2, "a", 0);
        group.kind = EditKind::Replace;
        history.record(group.clone());
        assert!(!history.record(group));
        assert_eq!(history.undo_depth(), 2);
    }

    #[test]
    fn undo_then_a_new_edit_clears_the_redo_stack() {
        let document = crate::testing::document_with(&["hi"]);
        let id = segment_id(&document);
        let mut history = History::default();
        history.record(typing(id, 2, "a", 0));
        assert!(history.pop_undo().is_some());
        assert!(history.can_redo());
        history.record(typing(id, 2, "b", 10));
        assert!(!history.can_redo());
    }

    #[test]
    fn the_capacity_bounds_the_stack() {
        let document = crate::testing::document_with(&["hi"]);
        let id = segment_id(&document);
        let mut history = History::new(2, COALESCE_WINDOW_MS);
        for step in 0..5u64 {
            history.break_coalescing();
            history.record(typing(id, 2, "a", step * 10_000));
        }
        assert_eq!(history.undo_depth(), 2);
    }
}
