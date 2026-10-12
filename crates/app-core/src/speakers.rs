//! Speaker labelling, renaming and confirmation (spec 8.7, 17.6).
//!
//! The product rule this module exists to enforce is narrow and important:
//! **a cluster label is not an identity**. Diarization produces clusters;
//! `Speaker 1` names a cluster. A human name only ever appears because a user
//! typed it, and it is only presented as identity after that user explicitly
//! confirmed it. Nothing here infers identity from voice.
//!
//! Renaming is display-time resolution. The stored machine transcript text is
//! never rewritten: a segment keeps pointing at a speaker row, and the row
//! carries the label. That is what makes a rename reversible — [`SpeakerRegistry::reset`]
//! restores the original machine label because the original was never lost.

use std::collections::BTreeMap;

use plaudon_ai_diarization::contract::ClusterKey;
use plaudon_domain::ids::{PersonId, RecordingId, SpeakerId};
use plaudon_domain::time::Timestamp;
use serde::{Deserialize, Serialize};

use crate::diarization::{Reconciliation, SegmentAssignment};

/// Where a speaker row's identity came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerProvenance {
    /// An inferred cluster. The label is machine-assigned.
    Inferred,
    /// A user typed a name and confirmed it.
    UserConfirmed,
}

impl SpeakerProvenance {
    /// Stable wire string, matching the `speakers.provenance` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inferred => "inferred",
            Self::UserConfirmed => "user_confirmed",
        }
    }
}

/// One speaker row, scoped to a recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Speaker {
    /// Row identity.
    pub id: SpeakerId,
    /// Recording the row belongs to.
    pub recording_id: RecordingId,
    /// Diarization cluster, when the row came from a diarization run.
    pub cluster_key: Option<ClusterKey>,
    /// Roster person this row is linked to, when one has been linked (VP-010).
    pub person_id: Option<PersonId>,
    /// The machine-assigned label this row started with, e.g. `Speaker 2`.
    pub original_label: String,
    /// Current label. Equals `original_label` until a rename.
    pub label: String,
    /// User-entered name, when one has been typed.
    pub name: Option<String>,
    /// Identity provenance.
    pub provenance: SpeakerProvenance,
    /// Whether the user confirmed the name as this person's identity.
    pub user_confirmed: bool,
    /// When the row was last renamed.
    pub renamed_at: Option<Timestamp>,
    /// When the name was confirmed.
    pub confirmed_at: Option<Timestamp>,
}

impl Speaker {
    /// Whether this row is still an inferred cluster.
    #[must_use]
    pub const fn is_inferred(&self) -> bool {
        matches!(self.provenance, SpeakerProvenance::Inferred)
    }
}

/// How a speaker should be presented, including the honesty qualifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerDisplay {
    /// Row identity.
    pub speaker_id: SpeakerId,
    /// The text to show as the speaker's name.
    pub label: String,
    /// Whether the user confirmed this as an identity.
    pub confirmed: bool,
    /// A short qualifier that must accompany the label wherever identity
    /// could be inferred, including exports.
    pub qualifier: &'static str,
}

impl SpeakerDisplay {
    /// Label and qualifier as one string, for plain-text surfaces.
    #[must_use]
    pub fn render(&self) -> String {
        format!("{} ({})", self.label, self.qualifier)
    }
}

/// Why a labelling operation was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpeakerError {
    /// No such speaker in this recording.
    #[error("no such speaker in this recording")]
    UnknownSpeaker,
    /// A name was blank or whitespace only.
    #[error("a speaker name must not be empty")]
    EmptyName,
    /// A name exceeded the stored length.
    #[error("a speaker name is too long")]
    NameTooLong,
    /// Confirmation was requested for a row with no user-entered name.
    ///
    /// Spec 17.6: any named speaker requires confirmation, and there is
    /// nothing to confirm until a human has supplied a name.
    #[error("a speaker must be named before it can be confirmed")]
    NothingToConfirm,
    /// A structured output referenced a speaker that does not exist.
    #[error("the output references a speaker that does not exist")]
    DanglingReference,
}

/// Maximum stored speaker name length.
///
/// Re-stated from [`plaudon_domain::speaker`] so the registry and the text
/// field a user types into cannot disagree about the limit.
pub const MAX_NAME_CHARS: usize = plaudon_domain::speaker::MAX_NAME_CHARS;

/// Every speaker row for one recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerRegistry {
    /// Recording these rows belong to.
    pub recording_id: RecordingId,
    pub speakers: Vec<Speaker>,
}

impl SpeakerRegistry {
    /// An empty registry.
    #[must_use]
    pub const fn new(recording_id: RecordingId) -> Self {
        Self {
            recording_id,
            speakers: Vec::new(),
        }
    }

    /// Build rows from a reconciliation, one per cluster, labelled by first
    /// appearance.
    #[must_use]
    pub fn from_reconciliation(recording_id: RecordingId, reconciliation: &Reconciliation) -> Self {
        let speakers = reconciliation
            .cluster_order
            .iter()
            .enumerate()
            .map(|(index, cluster)| {
                let label = format!("Speaker {}", index + 1);
                Speaker {
                    id: SpeakerId::new(),
                    recording_id,
                    cluster_key: Some(cluster.clone()),
                    person_id: None,
                    original_label: label.clone(),
                    label,
                    name: None,
                    provenance: SpeakerProvenance::Inferred,
                    user_confirmed: false,
                    renamed_at: None,
                    confirmed_at: None,
                }
            })
            .collect();
        Self {
            recording_id,
            speakers,
        }
    }

    /// All rows, in label order.
    #[must_use]
    pub fn speakers(&self) -> &[Speaker] {
        &self.speakers
    }

    /// Look a row up by identity.
    #[must_use]
    pub fn get(&self, id: SpeakerId) -> Option<&Speaker> {
        self.speakers.iter().find(|s| s.id == id)
    }

    /// Look a row up by diarization cluster.
    #[must_use]
    pub fn by_cluster(&self, cluster: &ClusterKey) -> Option<&Speaker> {
        self.speakers
            .iter()
            .find(|s| s.cluster_key.as_ref() == Some(cluster))
    }

    /// Rename a cluster.
    ///
    /// The name is recorded but the row stays [`SpeakerProvenance::Inferred`]
    /// until it is confirmed: typing a name is a guess, confirming it is a
    /// claim.
    ///
    /// # Errors
    ///
    /// Refuses an unknown row and a blank or over-long name.
    pub fn rename(
        &mut self,
        id: SpeakerId,
        name: &str,
        at: Timestamp,
    ) -> Result<&Speaker, SpeakerError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(SpeakerError::EmptyName);
        }
        if trimmed.chars().count() > MAX_NAME_CHARS {
            return Err(SpeakerError::NameTooLong);
        }
        let speaker = self
            .speakers
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(SpeakerError::UnknownSpeaker)?;
        speaker.name = Some(trimmed.to_string());
        speaker.label = trimmed.to_string();
        speaker.renamed_at = Some(at);
        // Renaming never promotes provenance on its own.
        speaker.user_confirmed = false;
        speaker.provenance = SpeakerProvenance::Inferred;
        speaker.confirmed_at = None;
        Ok(speaker)
    }

    /// Confirm that the typed name identifies this speaker.
    ///
    /// # Errors
    ///
    /// Refuses an unknown row and a row that has not been named.
    pub fn confirm(&mut self, id: SpeakerId, at: Timestamp) -> Result<&Speaker, SpeakerError> {
        let speaker = self
            .speakers
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(SpeakerError::UnknownSpeaker)?;
        if speaker.name.is_none() {
            return Err(SpeakerError::NothingToConfirm);
        }
        speaker.user_confirmed = true;
        speaker.provenance = SpeakerProvenance::UserConfirmed;
        speaker.confirmed_at = Some(at);
        Ok(speaker)
    }

    /// Withdraw a confirmation, keeping the typed name.
    ///
    /// # Errors
    ///
    /// Refuses an unknown row.
    pub fn unconfirm(&mut self, id: SpeakerId) -> Result<&Speaker, SpeakerError> {
        let speaker = self
            .speakers
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(SpeakerError::UnknownSpeaker)?;
        speaker.user_confirmed = false;
        speaker.provenance = SpeakerProvenance::Inferred;
        speaker.confirmed_at = None;
        Ok(speaker)
    }

    /// Restore the machine-assigned label, discarding the name.
    ///
    /// # Errors
    ///
    /// Refuses an unknown row.
    pub fn reset(&mut self, id: SpeakerId) -> Result<&Speaker, SpeakerError> {
        let speaker = self
            .speakers
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(SpeakerError::UnknownSpeaker)?;
        speaker.label = speaker.original_label.clone();
        speaker.name = None;
        speaker.user_confirmed = false;
        speaker.provenance = SpeakerProvenance::Inferred;
        speaker.renamed_at = None;
        speaker.confirmed_at = None;
        Ok(speaker)
    }

    /// How a row should be presented.
    ///
    /// The qualifier is [`plaudon_domain::speaker::qualifier`]: one wording,
    /// shared with the export bundle and the desktop interface.
    #[must_use]
    pub fn display(&self, id: SpeakerId) -> Option<SpeakerDisplay> {
        self.get(id).map(|speaker| SpeakerDisplay {
            speaker_id: speaker.id,
            label: speaker.label.clone(),
            confirmed: speaker.user_confirmed,
            qualifier: plaudon_domain::speaker::qualifier(speaker.user_confirmed),
        })
    }

    /// How a reconciled segment should be presented.
    ///
    /// Returns `None` for an unassigned segment: the absence of an
    /// attribution is shown as absence, not as an extra speaker.
    #[must_use]
    pub fn display_for_segment(
        &self,
        assignment: &SegmentAssignment,
    ) -> Option<Vec<SpeakerDisplay>> {
        match assignment {
            SegmentAssignment::Unassigned => None,
            SegmentAssignment::Single { cluster } => {
                let speaker = self.by_cluster(cluster)?;
                self.display(speaker.id).map(|display| vec![display])
            }
            SegmentAssignment::Overlapping { clusters } => {
                let displays: Vec<SpeakerDisplay> = clusters
                    .iter()
                    .filter_map(|cluster| self.by_cluster(cluster))
                    .filter_map(|speaker| self.display(speaker.id))
                    .collect();
                if displays.is_empty() {
                    None
                } else {
                    Some(displays)
                }
            }
        }
    }

    /// Validate that every speaker reference in a structured output resolves.
    ///
    /// Spec 9.5 requires rejecting dangling references rather than rendering
    /// an empty name.
    ///
    /// # Errors
    ///
    /// Returns [`SpeakerError::DanglingReference`] for the first reference
    /// that does not resolve.
    pub fn validate_references(&self, references: &[SpeakerId]) -> Result<(), SpeakerError> {
        if references.iter().all(|id| self.get(*id).is_some()) {
            Ok(())
        } else {
            Err(SpeakerError::DanglingReference)
        }
    }

    /// Resolve speaker references by label, for outputs that cite labels.
    #[must_use]
    pub fn by_label(&self, label: &str) -> Option<&Speaker> {
        self.speakers.iter().find(|s| s.label == label)
    }

    /// Label-to-display map, for export rendering.
    #[must_use]
    pub fn display_map(&self) -> BTreeMap<String, SpeakerDisplay> {
        self.speakers
            .iter()
            .filter_map(|speaker| {
                self.display(speaker.id)
                    .map(|display| (speaker.original_label.clone(), display))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diarization::{reconcile, result_from_turns};
    use plaudon_ai_diarization::contract::SpeakerTurn;
    use plaudon_domain::media::Sha256Digest;
    use plaudon_domain::model::{EngineId, ModelIdentity};
    use plaudon_domain::transcript::{
        AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptRevision,
        TranscriptRevisionDraft, TranscriptSegment,
    };

    fn registry() -> (SpeakerRegistry, Reconciliation) {
        let revision = TranscriptRevision::try_new(TranscriptRevisionDraft {
            recording_id: RecordingId::new(),
            parent_id: None,
            origin: RevisionOrigin::Machine,
            input_sha256: Sha256Digest::of(b"fixture"),
            engine: EngineId::new("whisper.cpp"),
            model: ModelIdentity::new(EngineId::new("whisper.cpp"), "base.en"),
            locale: None,
            alignment_origin: AlignmentOrigin::Native,
            status: RevisionStatus::Complete,
            normalization_profile: "fixture".to_string(),
            chunking_strategy: "fixture".to_string(),
            duration_ms: Some(4_000),
            created_at: Timestamp::from_millis(0),
            segments: vec![
                TranscriptSegment::timed(0, 0, 2_000, "first"),
                TranscriptSegment::timed(1, 2_000, 4_000, "second"),
            ],
        })
        .unwrap();
        let result = result_from_turns(
            "fake-deterministic",
            vec![
                SpeakerTurn::new("A", 0, 2_000),
                SpeakerTurn::new("B", 2_000, 4_000),
            ],
        );
        let reconciliation = reconcile(&revision, &result).unwrap();
        (
            SpeakerRegistry::from_reconciliation(revision.recording_id, &reconciliation),
            reconciliation,
        )
    }

    #[test]
    fn clusters_become_generic_labels_that_are_not_identities() {
        let (registry, _) = registry();
        assert_eq!(registry.speakers().len(), 2);
        assert_eq!(registry.speakers()[0].label, "Speaker 1");
        assert!(registry.speakers()[0].is_inferred());
        assert!(!registry.speakers()[0].user_confirmed);
        let display = registry.display(registry.speakers()[0].id).unwrap();
        assert_eq!(display.qualifier, "inferred cluster");
    }

    #[test]
    fn renaming_updates_every_segment_of_the_cluster_without_touching_transcript_text() {
        let (mut registry, reconciliation) = registry();
        let id = registry.speakers()[0].id;
        registry
            .rename(id, "Priya", Timestamp::from_millis(10))
            .unwrap();

        let displays = registry
            .display_for_segment(&reconciliation.segments[0].assignment)
            .unwrap();
        assert_eq!(displays[0].label, "Priya");
        // The other cluster is untouched.
        let other = registry
            .display_for_segment(&reconciliation.segments[1].assignment)
            .unwrap();
        assert_eq!(other[0].label, "Speaker 2");
    }

    #[test]
    fn a_rename_is_reversible() {
        let (mut registry, _) = registry();
        let id = registry.speakers()[0].id;
        registry
            .rename(id, "Priya", Timestamp::from_millis(10))
            .unwrap();
        registry.confirm(id, Timestamp::from_millis(11)).unwrap();
        registry.reset(id).unwrap();

        let speaker = registry.get(id).unwrap();
        assert_eq!(speaker.label, "Speaker 1");
        assert_eq!(speaker.name, None);
        assert!(!speaker.user_confirmed);
        assert_eq!(speaker.provenance, SpeakerProvenance::Inferred);
        assert_eq!(speaker.renamed_at, None);
    }

    #[test]
    fn typing_a_name_does_not_by_itself_claim_identity() {
        let (mut registry, _) = registry();
        let id = registry.speakers()[0].id;
        registry
            .rename(id, "Priya", Timestamp::from_millis(10))
            .unwrap();
        let speaker = registry.get(id).unwrap();
        assert!(!speaker.user_confirmed);
        assert_eq!(speaker.provenance, SpeakerProvenance::Inferred);
        assert_eq!(registry.display(id).unwrap().qualifier, "inferred cluster");
    }

    #[test]
    fn confirming_promotes_provenance_and_unconfirming_withdraws_it() {
        let (mut registry, _) = registry();
        let id = registry.speakers()[0].id;
        registry
            .rename(id, "Priya", Timestamp::from_millis(10))
            .unwrap();
        registry.confirm(id, Timestamp::from_millis(11)).unwrap();
        assert_eq!(
            registry.get(id).unwrap().provenance,
            SpeakerProvenance::UserConfirmed
        );
        assert_eq!(registry.display(id).unwrap().qualifier, "confirmed");

        registry.unconfirm(id).unwrap();
        let speaker = registry.get(id).unwrap();
        assert!(!speaker.user_confirmed);
        assert_eq!(speaker.name.as_deref(), Some("Priya"));
    }

    #[test]
    fn an_unnamed_cluster_cannot_be_confirmed() {
        let (mut registry, _) = registry();
        let id = registry.speakers()[0].id;
        assert_eq!(
            registry.confirm(id, Timestamp::from_millis(1)),
            Err(SpeakerError::NothingToConfirm)
        );
    }

    #[test]
    fn a_blank_or_oversized_name_is_refused() {
        let (mut registry, _) = registry();
        let id = registry.speakers()[0].id;
        assert_eq!(
            registry.rename(id, "   ", Timestamp::from_millis(1)),
            Err(SpeakerError::EmptyName)
        );
        let long = "x".repeat(MAX_NAME_CHARS + 1);
        assert_eq!(
            registry.rename(id, &long, Timestamp::from_millis(1)),
            Err(SpeakerError::NameTooLong)
        );
    }

    #[test]
    fn operations_on_an_unknown_speaker_are_refused() {
        let (mut registry, _) = registry();
        let foreign = SpeakerId::new();
        assert_eq!(
            registry.rename(foreign, "x", Timestamp::from_millis(1)),
            Err(SpeakerError::UnknownSpeaker)
        );
        assert_eq!(
            registry.confirm(foreign, Timestamp::from_millis(1)),
            Err(SpeakerError::UnknownSpeaker)
        );
        assert_eq!(registry.reset(foreign), Err(SpeakerError::UnknownSpeaker));
    }

    #[test]
    fn a_dangling_speaker_reference_fails_validation() {
        let (registry, _) = registry();
        let known = registry.speakers()[0].id;
        assert!(registry.validate_references(&[known]).is_ok());
        assert_eq!(
            registry.validate_references(&[known, SpeakerId::new()]),
            Err(SpeakerError::DanglingReference)
        );
    }

    #[test]
    fn an_overlapping_segment_shows_every_speaker_present() {
        let (registry, _) = registry();
        let clusters = vec![ClusterKey::new("A"), ClusterKey::new("B")];
        let displays = registry
            .display_for_segment(&SegmentAssignment::Overlapping { clusters })
            .unwrap();
        assert_eq!(displays.len(), 2);
    }

    #[test]
    fn an_unassigned_segment_shows_no_speaker_at_all() {
        let (registry, _) = registry();
        assert!(registry
            .display_for_segment(&SegmentAssignment::Unassigned)
            .is_none());
    }

    #[test]
    fn rendering_always_carries_the_honesty_qualifier() {
        let (mut registry, _) = registry();
        let id = registry.speakers()[0].id;
        assert_eq!(
            registry.display(id).unwrap().render(),
            "Speaker 1 (inferred cluster)"
        );
        registry
            .rename(id, "Priya", Timestamp::from_millis(1))
            .unwrap();
        registry.confirm(id, Timestamp::from_millis(2)).unwrap();
        assert_eq!(registry.display(id).unwrap().render(), "Priya (confirmed)");
    }

    #[test]
    fn the_display_map_is_keyed_by_the_original_machine_label() {
        let (mut registry, _) = registry();
        let id = registry.speakers()[0].id;
        registry
            .rename(id, "Priya", Timestamp::from_millis(1))
            .unwrap();
        let map = registry.display_map();
        assert_eq!(map.get("Speaker 1").unwrap().label, "Priya");
        assert!(map.contains_key("Speaker 2"));
    }
}
