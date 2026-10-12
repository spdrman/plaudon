//! Per-recording conversation history bound to one transcript revision
//! (spec 9.7).
//!
//! "We will maintain per-recording chat history linked to the exact transcript
//! revision, not a generic LLM conversation without provenance."
//!
//! That is enforced by construction: a [`RecordingConversation`] cannot exist
//! without a recording and a revision, and a [`ConversationTurn`] cannot exist
//! outside a conversation. There is no constructor that takes free-floating
//! text.

use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
use plaudon_domain::time::Timestamp;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::anchor::SourceAnchor;
use crate::ask::AskOutcomeKind;
use crate::error::ToolError;

/// Who produced a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnRole {
    /// The person using the application.
    User,
    /// The selected text model.
    Assistant,
}

impl TurnRole {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// One persisted turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationTurn {
    /// Turn identity.
    pub id: Uuid,
    /// Conversation this turn belongs to.
    pub conversation_id: Uuid,
    /// Position in the conversation, from zero.
    pub seq_index: u32,
    /// Who produced it.
    pub role: TurnRole,
    /// The text, verbatim.
    pub text: String,
    /// Verified anchors. Empty for user turns and for non-answered outcomes.
    pub anchors: Vec<SourceAnchor>,
    /// Outcome, present only on assistant turns.
    pub outcome: Option<AskOutcomeKind>,
    /// The tool run that produced an assistant turn, when one exists.
    pub tool_run_id: Option<Uuid>,
    /// When the turn was recorded.
    pub created_at: Timestamp,
}

/// Whether a conversation's basis still matches the recording's current
/// transcript (spec 9.6A, I2.4 dependency graph).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BasisState {
    /// The pinned revision is still the recording's head.
    Current,
    /// The transcript has been edited since; prior answers are stale and must
    /// be regenerated rather than silently re-pointed at new text.
    Stale,
}

/// A conversation about one recording, pinned to one revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingConversation {
    /// Conversation identity.
    pub id: Uuid,
    /// The recording it is about.
    pub recording_id: RecordingId,
    /// The exact revision every turn in it was answered against.
    pub transcript_revision_id: TranscriptRevisionId,
    /// When it was started.
    pub created_at: Timestamp,
    turns: Vec<ConversationTurn>,
}

impl RecordingConversation {
    /// Start a conversation against an exact revision.
    #[must_use]
    pub fn start(
        recording_id: RecordingId,
        transcript_revision_id: TranscriptRevisionId,
        created_at: Timestamp,
    ) -> Self {
        Self {
            id: Uuid::now_v7(),
            recording_id,
            transcript_revision_id,
            created_at,
            turns: Vec::new(),
        }
    }

    /// The turns, in order.
    #[must_use]
    pub fn turns(&self) -> &[ConversationTurn] {
        &self.turns
    }

    /// Append a user question.
    pub fn ask(&mut self, text: impl Into<String>, at: Timestamp) -> Result<&ConversationTurn, ToolError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(ToolError::InvalidArgument("question must not be empty"));
        }
        if matches!(self.turns.last().map(|t| t.role), Some(TurnRole::User)) {
            return Err(ToolError::InvalidArgument(
                "a user turn must be answered before another is asked",
            ));
        }
        self.push(TurnRole::User, text, Vec::new(), None, None, at);
        Ok(self.turns.last().expect("just pushed"))
    }

    /// Append an assistant answer.
    ///
    /// An `Answered` outcome without a verified anchor is refused: spec 9.8
    /// forbids returning an unanchored answer as confident prose.
    pub fn answer(
        &mut self,
        text: impl Into<String>,
        outcome: AskOutcomeKind,
        anchors: Vec<SourceAnchor>,
        tool_run_id: Option<Uuid>,
        at: Timestamp,
    ) -> Result<&ConversationTurn, ToolError> {
        if !matches!(self.turns.last().map(|t| t.role), Some(TurnRole::User)) {
            return Err(ToolError::InvalidArgument(
                "an assistant turn must answer a user turn",
            ));
        }
        if outcome == AskOutcomeKind::Answered && anchors.is_empty() {
            return Err(ToolError::UngroundedOutput(0));
        }
        if anchors
            .iter()
            .any(|a| a.revision_id != self.transcript_revision_id)
        {
            return Err(ToolError::InvalidArgument(
                "an anchor must cite the conversation's pinned revision",
            ));
        }
        self.push(
            TurnRole::Assistant,
            text.into(),
            anchors,
            Some(outcome),
            tool_run_id,
            at,
        );
        Ok(self.turns.last().expect("just pushed"))
    }

    fn push(
        &mut self,
        role: TurnRole,
        text: String,
        anchors: Vec<SourceAnchor>,
        outcome: Option<AskOutcomeKind>,
        tool_run_id: Option<Uuid>,
        created_at: Timestamp,
    ) {
        let seq_index = u32::try_from(self.turns.len()).unwrap_or(u32::MAX);
        self.turns.push(ConversationTurn {
            id: Uuid::now_v7(),
            conversation_id: self.id,
            seq_index,
            role,
            text,
            anchors,
            outcome,
            tool_run_id,
            created_at,
        });
    }

    /// Whether the pinned revision is still the recording's head revision.
    #[must_use]
    pub fn basis_state(&self, head_revision_id: TranscriptRevisionId) -> BasisState {
        if head_revision_id == self.transcript_revision_id {
            BasisState::Current
        } else {
            BasisState::Stale
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation() -> RecordingConversation {
        RecordingConversation::start(
            RecordingId::new(),
            TranscriptRevisionId::new(),
            Timestamp::from_millis(0),
        )
    }

    fn anchor(revision: TranscriptRevisionId) -> SourceAnchor {
        SourceAnchor::new(revision, 0, 0, 2, "We")
    }

    #[test]
    fn a_turn_always_carries_its_recording_and_revision() {
        let mut convo = conversation();
        convo.ask("what happened?", Timestamp::from_millis(1)).unwrap();
        let turn = convo
            .answer(
                "This happened.",
                AskOutcomeKind::Answered,
                vec![anchor(convo.transcript_revision_id)],
                None,
                Timestamp::from_millis(2),
            )
            .unwrap()
            .clone();
        assert_eq!(turn.conversation_id, convo.id);
        assert_eq!(turn.anchors[0].revision_id, convo.transcript_revision_id);
    }

    #[test]
    fn an_answered_turn_without_anchors_is_refused() {
        let mut convo = conversation();
        convo.ask("q", Timestamp::from_millis(1)).unwrap();
        assert_eq!(
            convo.answer(
                "confident prose",
                AskOutcomeKind::Answered,
                Vec::new(),
                None,
                Timestamp::from_millis(2)
            ),
            Err(ToolError::UngroundedOutput(0))
        );
        assert_eq!(convo.turns().len(), 1);
    }

    #[test]
    fn an_insufficient_evidence_turn_may_have_no_anchors() {
        let mut convo = conversation();
        convo.ask("q", Timestamp::from_millis(1)).unwrap();
        convo
            .answer(
                "The recording does not say.",
                AskOutcomeKind::InsufficientEvidence,
                Vec::new(),
                None,
                Timestamp::from_millis(2),
            )
            .unwrap();
        assert_eq!(convo.turns().len(), 2);
    }

    #[test]
    fn an_anchor_into_another_revision_cannot_be_recorded() {
        let mut convo = conversation();
        convo.ask("q", Timestamp::from_millis(1)).unwrap();
        let foreign = TranscriptRevisionId::new();
        assert!(convo
            .answer(
                "answer",
                AskOutcomeKind::Answered,
                vec![anchor(foreign)],
                None,
                Timestamp::from_millis(2)
            )
            .is_err());
    }

    #[test]
    fn turns_alternate_and_indices_are_contiguous() {
        let mut convo = conversation();
        convo.ask("one", Timestamp::from_millis(1)).unwrap();
        assert!(convo.ask("two", Timestamp::from_millis(2)).is_err());
        convo
            .answer(
                "a",
                AskOutcomeKind::NotSupportedByRecording,
                Vec::new(),
                None,
                Timestamp::from_millis(3),
            )
            .unwrap();
        convo.ask("two", Timestamp::from_millis(4)).unwrap();
        let indices: Vec<u32> = convo.turns().iter().map(|t| t.seq_index).collect();
        assert_eq!(indices, [0, 1, 2]);
    }

    #[test]
    fn an_assistant_turn_cannot_open_a_conversation() {
        let mut convo = conversation();
        assert!(convo
            .answer(
                "unprompted",
                AskOutcomeKind::InsufficientEvidence,
                Vec::new(),
                None,
                Timestamp::from_millis(1)
            )
            .is_err());
    }

    #[test]
    fn an_empty_question_is_refused() {
        let mut convo = conversation();
        assert!(convo.ask("   ", Timestamp::from_millis(1)).is_err());
    }

    #[test]
    fn a_new_head_revision_marks_the_conversation_basis_stale() {
        let convo = conversation();
        assert_eq!(
            convo.basis_state(convo.transcript_revision_id),
            BasisState::Current
        );
        assert_eq!(
            convo.basis_state(TranscriptRevisionId::new()),
            BasisState::Stale
        );
    }
}
