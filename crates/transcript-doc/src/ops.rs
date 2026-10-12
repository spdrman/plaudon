//! Editing operations: the only way the document changes.
//!
//! Every operation carries enough information to invert itself without
//! consulting the document, which is what makes undo exact and what makes the
//! log replayable: applying a log to a freshly opened document reproduces the
//! edited document, character for character.

use plaudon_domain::ids::SpeakerId;

use crate::caret::{Position, Selection};
use crate::doc::{DocError, SegmentId, TranscriptDocument};

/// One primitive, invertible change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Insert `text` at `at`.
    Insert {
        /// Where the text goes.
        at: Position,
        /// The inserted text.
        text: String,
    },
    /// Delete the `text` that starts at `at`.
    ///
    /// The text is recorded so the operation can be inverted and so replay can
    /// verify that the log and the document agree.
    Delete {
        /// Start of the deleted run.
        at: Position,
        /// The deleted text.
        text: String,
    },
    /// Reassign a segment's speaker.
    SetSpeaker {
        /// Segment reassigned.
        segment: SegmentId,
        /// Speaker before the change.
        before: Option<SpeakerId>,
        /// Speaker after the change.
        after: Option<SpeakerId>,
    },
}

impl Op {
    /// The operation that undoes this one.
    #[must_use]
    pub fn inverse(&self) -> Self {
        match self {
            Self::Insert { at, text } => Self::Delete {
                at: *at,
                text: text.clone(),
            },
            Self::Delete { at, text } => Self::Insert {
                at: *at,
                text: text.clone(),
            },
            Self::SetSpeaker {
                segment,
                before,
                after,
            } => Self::SetSpeaker {
                segment: *segment,
                before: *after,
                after: *before,
            },
        }
    }

    /// The segment this operation touches.
    #[must_use]
    pub const fn segment(&self) -> SegmentId {
        match self {
            Self::Insert { at, .. } | Self::Delete { at, .. } => at.segment,
            Self::SetSpeaker { segment, .. } => *segment,
        }
    }

    /// Apply the operation to a document.
    pub fn apply(&self, document: &mut TranscriptDocument) -> Result<(), DocError> {
        match self {
            Self::Insert { at, text } => document.insert_text(at.segment, at.offset, text),
            Self::Delete { at, text } => {
                document.delete_text(at.segment, at.offset, text).map(drop)
            }
            Self::SetSpeaker {
                segment, after, ..
            } => document.set_speaker(*segment, *after),
        }
    }

    /// The caret position this operation leaves behind.
    #[must_use]
    pub fn resulting_position(&self) -> Option<Position> {
        match self {
            Self::Insert { at, text } => Some(Position::new(
                at.segment,
                at.offset + text.chars().count(),
            )),
            Self::Delete { at, .. } => Some(*at),
            Self::SetSpeaker { .. } => None,
        }
    }
}

/// What the user did, which decides whether two groups coalesce into one undo
/// step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// Characters typed one at a time.
    Typing,
    /// Backspace.
    DeletingBackward,
    /// Forward delete.
    DeletingForward,
    /// A paste, which is always its own undo step.
    Paste,
    /// A cut.
    Cut,
    /// A replacement, including a replace-all over the whole transcript.
    Replace,
    /// A speaker reassignment.
    SpeakerChange,
    /// A whole-segment text correction.
    Correction,
}

impl EditKind {
    /// Whether two adjacent groups of this kind may merge into one undo step.
    #[must_use]
    pub const fn coalesces(self) -> bool {
        matches!(
            self,
            Self::Typing | Self::DeletingBackward | Self::DeletingForward
        )
    }

    /// A label for the undo affordance and for accessibility announcements.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Typing => "typing",
            Self::DeletingBackward | Self::DeletingForward => "delete",
            Self::Paste => "paste",
            Self::Cut => "cut",
            Self::Replace => "replace",
            Self::SpeakerChange => "speaker change",
            Self::Correction => "correction",
        }
    }
}

/// One undoable step: a run of operations applied together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpGroup {
    /// Operations in application order.
    pub ops: Vec<Op>,
    /// What produced them.
    pub kind: EditKind,
    /// Selection before the step, restored on undo.
    pub selection_before: Selection,
    /// Selection after the step, restored on redo.
    pub selection_after: Selection,
    /// Session time of the step, in milliseconds. Coalescing uses it.
    pub at_ms: u64,
}

impl OpGroup {
    /// A group.
    #[must_use]
    pub const fn new(
        ops: Vec<Op>,
        kind: EditKind,
        selection_before: Selection,
        selection_after: Selection,
        at_ms: u64,
    ) -> Self {
        Self {
            ops,
            kind,
            selection_before,
            selection_after,
            at_ms,
        }
    }

    /// Whether the group changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// Apply every operation of a group, in order.
pub fn apply_group(
    document: &mut TranscriptDocument,
    group: &OpGroup,
) -> Result<(), DocError> {
    for op in &group.ops {
        op.apply(document)?;
    }
    Ok(())
}

/// Undo a group: apply the inverse of each operation, last one first.
pub fn revert_group(
    document: &mut TranscriptDocument,
    group: &OpGroup,
) -> Result<(), DocError> {
    for op in group.ops.iter().rev() {
        op.inverse().apply(document)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::document_with;

    fn caret(segment: SegmentId, offset: usize) -> Selection {
        Selection::caret(Position::new(segment, offset))
    }

    #[test]
    fn an_insert_inverts_to_the_matching_delete() {
        let mut document = document_with(&["we will pay"]);
        let id = document.first_id().expect("segment");
        let op = Op::Insert {
            at: Position::new(id, 8),
            text: "not ".to_owned(),
        };
        op.apply(&mut document).expect("apply");
        assert_eq!(document.segment(id).expect("segment").text(), "we will not pay");
        op.inverse().apply(&mut document).expect("invert");
        assert_eq!(document.segment(id).expect("segment").text(), "we will pay");
    }

    #[test]
    fn a_group_reverts_in_reverse_order() {
        let mut document = document_with(&["abc"]);
        let id = document.first_id().expect("segment");
        let group = OpGroup::new(
            vec![
                Op::Delete {
                    at: Position::new(id, 0),
                    text: "a".to_owned(),
                },
                Op::Insert {
                    at: Position::new(id, 0),
                    text: "XY".to_owned(),
                },
            ],
            EditKind::Replace,
            caret(id, 0),
            caret(id, 2),
            0,
        );
        apply_group(&mut document, &group).expect("apply");
        assert_eq!(document.segment(id).expect("segment").text(), "XYbc");
        revert_group(&mut document, &group).expect("revert");
        assert_eq!(document.segment(id).expect("segment").text(), "abc");
    }

    #[test]
    fn a_speaker_change_inverts_to_the_previous_speaker() {
        let mut document = document_with(&["hello"]);
        let id = document.first_id().expect("segment");
        let speaker = SpeakerId::new();
        let op = Op::SetSpeaker {
            segment: id,
            before: None,
            after: Some(speaker),
        };
        op.apply(&mut document).expect("apply");
        assert_eq!(document.segment(id).expect("segment").speaker_id, Some(speaker));
        op.inverse().apply(&mut document).expect("invert");
        assert_eq!(document.segment(id).expect("segment").speaker_id, None);
    }
}
