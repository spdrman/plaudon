//! Reconciliation of diarization turns against transcript segments (spec 8.7).
//!
//! The pipeline is `Audio → STT` and, independently, `Audio → Diarization`.
//! This module performs the third step: mapping speaker turns onto the time
//! ranges of an existing transcript revision.
//!
//! Three rules shape the output:
//!
//! * **Nothing is guessed.** A segment that no turn covers is
//!   [`SegmentAssignment::Unassigned`], not "probably the previous speaker".
//! * **Overlap stays overlap.** A segment covered by two clusters is
//!   [`SegmentAssignment::Overlapping`] listing both, because collapsing it
//!   would assert something the audio does not support.
//! * **The transcript is not modified.** Reconciliation produces a separate
//!   assignment table keyed by `(revision, seq_index)`; the immutable revision
//!   is untouched, so STT output is identical whether or not diarization ran.
//!
//! Labels are assigned by first appearance in transcript order, so `Speaker 1`
//! is the first person heard. The label is a cluster label, never an identity
//! (spec 17.6).

use std::collections::BTreeMap;

use plaudon_ai_diarization::contract::{ClusterKey, DiarizationResult, SpeakerTurn};
use plaudon_domain::ids::TranscriptRevisionId;
use plaudon_domain::transcript::TranscriptRevision;
use serde::{Deserialize, Serialize};

/// Minimum fraction of a segment a cluster must cover to be considered present
/// in it. Below this, a few milliseconds of bleed from an adjacent turn would
/// turn every segment into an overlap.
pub const PRESENCE_FRACTION: f64 = 0.2;

/// Minimum absolute coverage, in milliseconds, for very short segments.
pub const PRESENCE_FLOOR_MS: u64 = 120;

/// What diarization concluded about one transcript segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "assignment", rename_all = "snake_case")]
pub enum SegmentAssignment {
    /// Exactly one cluster speaks here.
    Single {
        /// The cluster.
        cluster: ClusterKey,
    },
    /// More than one cluster speaks here; order is by covered duration.
    Overlapping {
        /// The clusters, most-covering first.
        clusters: Vec<ClusterKey>,
    },
    /// No turn covers enough of this segment to attribute it.
    Unassigned,
}

impl SegmentAssignment {
    /// The single cluster, when there is exactly one.
    #[must_use]
    pub fn single(&self) -> Option<&ClusterKey> {
        match self {
            Self::Single { cluster } => Some(cluster),
            _ => None,
        }
    }

    /// Stable wire discriminator.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Single { .. } => "single",
            Self::Overlapping { .. } => "overlapping",
            Self::Unassigned => "unassigned",
        }
    }
}

/// One reconciled segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciledSegment {
    /// Segment position in the revision.
    pub seq_index: u32,
    /// What diarization concluded.
    pub assignment: SegmentAssignment,
    /// Milliseconds of this segment covered by the attributed cluster(s).
    pub covered_ms: u64,
}

/// The result of reconciling one diarization run against one revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reconciliation {
    /// Revision the assignments belong to.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Per-segment assignments, in transcript order.
    pub segments: Vec<ReconciledSegment>,
    /// Clusters in order of first appearance, which is the order the
    /// `Speaker N` labels follow.
    pub cluster_order: Vec<ClusterKey>,
}

impl Reconciliation {
    /// The generic label for a cluster, one-based by first appearance.
    ///
    /// Spec 8.7 requires stable generic labels; spec 17.6 requires that they
    /// are never presented as identity.
    #[must_use]
    pub fn label_for(&self, cluster: &ClusterKey) -> Option<String> {
        self.cluster_order
            .iter()
            .position(|candidate| candidate == cluster)
            .map(|index| format!("Speaker {}", index + 1))
    }

    /// Segments that could not be attributed.
    #[must_use]
    pub fn unassigned_count(&self) -> usize {
        self.segments
            .iter()
            .filter(|s| s.assignment == SegmentAssignment::Unassigned)
            .count()
    }

    /// Segments with simultaneous speakers.
    #[must_use]
    pub fn overlapping_count(&self) -> usize {
        self.segments
            .iter()
            .filter(|s| matches!(s.assignment, SegmentAssignment::Overlapping { .. }))
            .count()
    }
}

/// Why reconciliation could not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReconcileError {
    /// The revision has no times, so there is nothing to align turns to.
    ///
    /// Spec 8.6A: a text-only revision must not gain invented positions, and
    /// that includes positions implied by a speaker turn.
    #[error("the transcript revision carries no times, so turns cannot be aligned")]
    NoAlignment,
}

fn presence_threshold(segment_duration_ms: u64) -> u64 {
    let fractional = (segment_duration_ms as f64 * PRESENCE_FRACTION) as u64;
    fractional.clamp(1, PRESENCE_FLOOR_MS)
}

/// Map diarization turns onto the segments of a revision.
///
/// # Errors
///
/// Returns [`ReconcileError::NoAlignment`] when the revision has no source
/// times at all.
pub fn reconcile(
    revision: &TranscriptRevision,
    result: &DiarizationResult,
) -> Result<Reconciliation, ReconcileError> {
    if !revision.alignment_origin.permits_times() {
        return Err(ReconcileError::NoAlignment);
    }

    let mut segments = Vec::with_capacity(revision.segments().len());
    let mut cluster_order: Vec<ClusterKey> = Vec::new();

    for segment in revision.segments() {
        let (Some(start), Some(end)) = (segment.start, segment.end) else {
            segments.push(ReconciledSegment {
                seq_index: segment.seq_index,
                assignment: SegmentAssignment::Unassigned,
                covered_ms: 0,
            });
            continue;
        };
        let (start_ms, end_ms) = (start.get(), end.get());
        let duration = end_ms.saturating_sub(start_ms);

        let mut coverage: BTreeMap<&ClusterKey, u64> = BTreeMap::new();
        for turn in &result.turns {
            let overlap = turn.overlap_ms(start_ms, end_ms);
            if overlap > 0 {
                *coverage.entry(&turn.cluster).or_insert(0) += overlap;
            }
        }

        let threshold = presence_threshold(duration);
        let mut present: Vec<(&ClusterKey, u64)> = coverage
            .into_iter()
            .filter(|(_, ms)| *ms >= threshold)
            .collect();
        // Most-covering first; ties by cluster key so the order is stable.
        present.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.as_str().cmp(b.0.as_str())));

        let covered_ms: u64 = present.iter().map(|(_, ms)| *ms).sum();
        let assignment = match present.len() {
            0 => SegmentAssignment::Unassigned,
            1 => {
                let cluster = present[0].0.clone();
                remember(&mut cluster_order, &cluster);
                SegmentAssignment::Single { cluster }
            }
            _ => {
                let clusters: Vec<ClusterKey> =
                    present.iter().map(|(key, _)| (*key).clone()).collect();
                for cluster in &clusters {
                    remember(&mut cluster_order, cluster);
                }
                SegmentAssignment::Overlapping { clusters }
            }
        };

        segments.push(ReconciledSegment {
            seq_index: segment.seq_index,
            assignment,
            covered_ms,
        });
    }

    Ok(Reconciliation {
        transcript_revision_id: revision.id,
        segments,
        cluster_order,
    })
}

fn remember(order: &mut Vec<ClusterKey>, cluster: &ClusterKey) {
    if !order.contains(cluster) {
        order.push(cluster.clone());
    }
}

/// Total duration a cluster is credited with across a reconciliation.
#[must_use]
pub fn cluster_durations(result: &DiarizationResult) -> BTreeMap<String, u64> {
    let mut totals: BTreeMap<String, u64> = BTreeMap::new();
    for turn in &result.turns {
        *totals.entry(turn.cluster.to_string()).or_insert(0) += turn.duration_ms();
    }
    totals
}

/// Build a diarization result from turns, for callers assembling one by hand.
#[must_use]
pub fn result_from_turns(provider_id: &str, turns: Vec<SpeakerTurn>) -> DiarizationResult {
    DiarizationResult {
        payload_version: plaudon_ai_diarization::DIARIZATION_CONTRACT_VERSION,
        provider_id: provider_id.to_string(),
        provider_version: None,
        turns,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_domain::ids::RecordingId;
    use plaudon_domain::media::Sha256Digest;
    use plaudon_domain::model::{EngineId, ModelIdentity};
    use plaudon_domain::time::Timestamp;
    use plaudon_domain::transcript::{
        AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptRevisionDraft, TranscriptSegment,
    };

    fn revision(origin: AlignmentOrigin, segments: Vec<TranscriptSegment>) -> TranscriptRevision {
        TranscriptRevision::try_new(TranscriptRevisionDraft {
            recording_id: RecordingId::new(),
            parent_id: None,
            origin: RevisionOrigin::Machine,
            input_sha256: Sha256Digest::of(b"fixture"),
            engine: EngineId::new("whisper.cpp"),
            model: ModelIdentity::new(EngineId::new("whisper.cpp"), "base.en"),
            locale: None,
            alignment_origin: origin,
            status: RevisionStatus::Complete,
            normalization_profile: "fixture".to_string(),
            chunking_strategy: "fixture".to_string(),
            duration_ms: Some(10_000),
            created_at: Timestamp::from_millis(0),
            segments,
        })
        .unwrap()
    }

    fn timed_revision() -> TranscriptRevision {
        revision(
            AlignmentOrigin::Native,
            vec![
                TranscriptSegment::timed(0, 0, 2_000, "first"),
                TranscriptSegment::timed(1, 2_000, 4_000, "second"),
                TranscriptSegment::timed(2, 4_000, 6_000, "third"),
            ],
        )
    }

    #[test]
    fn each_segment_takes_the_cluster_that_covers_it() {
        let revision = timed_revision();
        let result = result_from_turns(
            "fake-deterministic",
            vec![
                SpeakerTurn::new("A", 0, 2_000),
                SpeakerTurn::new("B", 2_000, 4_000),
                SpeakerTurn::new("A", 4_000, 6_000),
            ],
        );
        let reconciliation = reconcile(&revision, &result).unwrap();
        assert_eq!(
            reconciliation.segments[0]
                .assignment
                .single()
                .unwrap()
                .as_str(),
            "A"
        );
        assert_eq!(
            reconciliation.segments[1]
                .assignment
                .single()
                .unwrap()
                .as_str(),
            "B"
        );
        assert_eq!(reconciliation.cluster_order.len(), 2);
    }

    #[test]
    fn labels_follow_first_appearance_in_transcript_order() {
        let revision = timed_revision();
        let result = result_from_turns(
            "fake-deterministic",
            vec![
                SpeakerTurn::new("zeta", 0, 2_000),
                SpeakerTurn::new("alpha", 2_000, 4_000),
            ],
        );
        let reconciliation = reconcile(&revision, &result).unwrap();
        assert_eq!(
            reconciliation.label_for(&ClusterKey::new("zeta")).unwrap(),
            "Speaker 1"
        );
        assert_eq!(
            reconciliation.label_for(&ClusterKey::new("alpha")).unwrap(),
            "Speaker 2"
        );
        assert_eq!(reconciliation.label_for(&ClusterKey::new("nope")), None);
    }

    #[test]
    fn an_uncovered_segment_is_unassigned_not_inherited_from_its_neighbour() {
        let revision = timed_revision();
        let result = result_from_turns("fake-deterministic", vec![SpeakerTurn::new("A", 0, 2_000)]);
        let reconciliation = reconcile(&revision, &result).unwrap();
        assert_eq!(
            reconciliation.segments[1].assignment,
            SegmentAssignment::Unassigned
        );
        assert_eq!(reconciliation.unassigned_count(), 2);
    }

    #[test]
    fn simultaneous_speakers_are_reported_as_overlap_not_collapsed() {
        let revision = timed_revision();
        let result = result_from_turns(
            "fake-deterministic",
            vec![
                SpeakerTurn::new("A", 0, 2_000),
                SpeakerTurn::new("B", 500, 2_000),
            ],
        );
        let reconciliation = reconcile(&revision, &result).unwrap();
        let SegmentAssignment::Overlapping { clusters } = &reconciliation.segments[0].assignment
        else {
            panic!("expected overlap, got {:?}", reconciliation.segments[0]);
        };
        assert_eq!(clusters.len(), 2);
        // Most-covering first.
        assert_eq!(clusters[0].as_str(), "A");
        assert_eq!(reconciliation.overlapping_count(), 1);
    }

    #[test]
    fn a_few_milliseconds_of_bleed_does_not_create_an_overlap() {
        let revision = timed_revision();
        let result = result_from_turns(
            "fake-deterministic",
            vec![
                SpeakerTurn::new("A", 0, 2_000),
                SpeakerTurn::new("B", 1_950, 4_000),
            ],
        );
        let reconciliation = reconcile(&revision, &result).unwrap();
        assert_eq!(
            reconciliation.segments[0].assignment,
            SegmentAssignment::Single {
                cluster: ClusterKey::new("A")
            }
        );
    }

    #[test]
    fn a_text_only_revision_is_refused_rather_than_given_invented_positions() {
        let revision = revision(
            AlignmentOrigin::None,
            vec![TranscriptSegment::text_only(0, "first")],
        );
        let result = result_from_turns("fake-deterministic", vec![SpeakerTurn::new("A", 0, 2_000)]);
        assert_eq!(
            reconcile(&revision, &result),
            Err(ReconcileError::NoAlignment)
        );
    }

    #[test]
    fn reconciliation_never_mutates_the_revision() {
        let revision = timed_revision();
        let before = revision.clone();
        let result = result_from_turns("fake-deterministic", vec![SpeakerTurn::new("A", 0, 6_000)]);
        reconcile(&revision, &result).unwrap();
        assert_eq!(revision, before);
        assert!(revision.segments().iter().all(|s| s.speaker_id.is_none()));
    }

    #[test]
    fn an_empty_diarization_result_leaves_every_segment_unassigned() {
        let revision = timed_revision();
        let result = result_from_turns("fake-deterministic", Vec::new());
        let reconciliation = reconcile(&revision, &result).unwrap();
        assert_eq!(reconciliation.unassigned_count(), 3);
        assert!(reconciliation.cluster_order.is_empty());
    }

    #[test]
    fn every_segment_of_the_revision_appears_exactly_once_in_order() {
        let revision = timed_revision();
        let result = result_from_turns("fake-deterministic", vec![SpeakerTurn::new("A", 0, 6_000)]);
        let reconciliation = reconcile(&revision, &result).unwrap();
        let indices: Vec<u32> = reconciliation
            .segments
            .iter()
            .map(|s| s.seq_index)
            .collect();
        assert_eq!(indices, vec![0, 1, 2]);
    }

    #[test]
    fn cluster_durations_sum_turn_lengths() {
        let result = result_from_turns(
            "fake-deterministic",
            vec![
                SpeakerTurn::new("A", 0, 1_000),
                SpeakerTurn::new("A", 2_000, 2_500),
                SpeakerTurn::new("B", 1_000, 2_000),
            ],
        );
        let totals = cluster_durations(&result);
        assert_eq!(totals.get("A"), Some(&1_500));
        assert_eq!(totals.get("B"), Some(&1_000));
    }
}
