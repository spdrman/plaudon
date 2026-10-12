//! Stale-output detection and targeted regeneration (spec 9.6A, task I2.4).
//!
//! A transcript edit never rewrites an output. It marks every output that read
//! the edited revision `stale`, preserving the output, its claim states and any
//! human confirmation exactly as they were. Regeneration happens only when the
//! user asks, and it produces a *new* output revision.
//!
//! Regeneration is targeted: a section summary whose segment range contains no
//! edited segment, and whose canonical input digest is unchanged, is reused
//! rather than re-billed. Reuse is visible in the provenance record through
//! [`crate::pipeline::StageProvenance::reused_from`].

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use plaudon_domain::ids::{OutputId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::TranscriptRevision;
use serde::{Deserialize, Serialize};

use crate::pipeline::SectionSummary;

/// Why an output is stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleReason {
    /// The transcript revision the output read has been superseded by an edit.
    TranscriptEdited,
    /// The transcript revision was superseded by a different engine's run.
    TranscriptReplaced,
    /// The template the output used has been upgraded.
    TemplateUpgraded,
}

impl StaleReason {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TranscriptEdited => "transcript_edited",
            Self::TranscriptReplaced => "transcript_replaced",
            Self::TemplateUpgraded => "template_upgraded",
        }
    }

    /// Parse a persisted wire string.
    #[must_use]
    pub fn from_str_opt(raw: &str) -> Option<Self> {
        match raw {
            "transcript_edited" => Some(Self::TranscriptEdited),
            "transcript_replaced" => Some(Self::TranscriptReplaced),
            "template_upgraded" => Some(Self::TemplateUpgraded),
            _ => None,
        }
    }
}

/// One edge of the output dependency graph (spec 9.6A).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputDependency {
    /// Output that depends on the revision.
    pub output_id: OutputId,
    /// Transcript revision the output read.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Canonical digest of the transcript content the output read.
    pub input_sha256: Sha256Digest,
    /// Whether the output is already marked stale.
    pub stale: bool,
}

/// An output newly marked stale by an edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleMarking {
    /// Output that became stale.
    pub output_id: OutputId,
    /// Why.
    pub reason: StaleReason,
    /// When the staleness was recorded.
    pub marked_at: Timestamp,
}

/// The dependency graph of outputs on transcript revisions.
///
/// Edges are only ever added; an output is never removed from the graph when
/// it goes stale, because a stale output stays fully retrievable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DependencyGraph {
    by_revision: BTreeMap<TranscriptRevisionId, Vec<OutputDependency>>,
}

impl DependencyGraph {
    /// An empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a graph from persisted edges.
    #[must_use]
    pub fn from_edges(edges: impl IntoIterator<Item = OutputDependency>) -> Self {
        let mut graph = Self::new();
        for edge in edges {
            graph.insert(edge);
        }
        graph
    }

    /// Record that `output` read `revision`.
    pub fn insert(&mut self, edge: OutputDependency) {
        self.by_revision
            .entry(edge.transcript_revision_id)
            .or_default()
            .push(edge);
    }

    /// Outputs that read `revision`, stale or not.
    #[must_use]
    pub fn dependents(&self, revision: TranscriptRevisionId) -> &[OutputDependency] {
        self.by_revision.get(&revision).map_or(&[], Vec::as_slice)
    }

    /// Everything that must be marked stale because `superseded` now has a child.
    ///
    /// The closure is complete over the supplied ancestry: an output that read
    /// any revision in `superseded_lineage` is included exactly once, and an
    /// output already marked stale is not marked twice.
    #[must_use]
    pub fn stale_closure(
        &self,
        superseded_lineage: &[TranscriptRevisionId],
        reason: StaleReason,
        marked_at: Timestamp,
    ) -> Vec<StaleMarking> {
        let mut seen: BTreeSet<OutputId> = BTreeSet::new();
        let mut markings = Vec::new();
        for revision in superseded_lineage {
            for edge in self.dependents(*revision) {
                if edge.stale || !seen.insert(edge.output_id) {
                    continue;
                }
                markings.push(StaleMarking {
                    output_id: edge.output_id,
                    reason,
                    marked_at,
                });
            }
        }
        markings
    }

    /// Number of edges in the graph.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_revision.values().map(Vec::len).sum()
    }

    /// Whether the graph has no edges.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_revision.is_empty()
    }
}

/// Segment indices whose text differs between two revisions.
///
/// An index present in one revision and absent from the other counts as
/// changed: adding or deleting a segment invalidates the sections around it.
#[must_use]
pub fn changed_segments(previous: &TranscriptRevision, next: &TranscriptRevision) -> BTreeSet<u32> {
    let previous_by_index: BTreeMap<u32, &str> = previous
        .segments()
        .iter()
        .map(|segment| (segment.seq_index, segment.text.as_str()))
        .collect();
    let next_by_index: BTreeMap<u32, &str> = next
        .segments()
        .iter()
        .map(|segment| (segment.seq_index, segment.text.as_str()))
        .collect();

    let mut changed = BTreeSet::new();
    for (index, text) in &previous_by_index {
        match next_by_index.get(index) {
            Some(other) if other == text => {}
            _ => {
                changed.insert(*index);
            }
        }
    }
    for index in next_by_index.keys() {
        if !previous_by_index.contains_key(index) {
            changed.insert(*index);
        }
    }
    changed
}

/// A plan for regenerating one stale output against a new revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegenerationPlan {
    /// Output being replaced. It is preserved, not mutated.
    pub previous_output_id: OutputId,
    /// Revision the regeneration will read.
    pub target_revision_id: TranscriptRevisionId,
    /// Section summaries that may be reused verbatim.
    pub reusable_sections: Vec<SectionSummary>,
    /// Segment ranges that must be re-summarized.
    pub recompute_ranges: Vec<Range<u32>>,
    /// Segment indices whose text changed.
    pub changed_segments: Vec<u32>,
}

impl RegenerationPlan {
    /// Whether anything can be reused at all.
    #[must_use]
    pub fn reuses_anything(&self) -> bool {
        !self.reusable_sections.is_empty()
    }
}

/// Plan a targeted regeneration of `previous_output_id` against `next`.
///
/// A section is reusable when none of its segments changed **and** its stored
/// input digest still matches the same segments in the new revision. The digest
/// check is what makes reuse safe: an index-preserving edit elsewhere cannot
/// smuggle different text into a "reused" section.
#[must_use]
pub fn plan_regeneration(
    previous_output_id: OutputId,
    previous: &TranscriptRevision,
    next: &TranscriptRevision,
    sections: &[SectionSummary],
    digest_of: impl Fn(&TranscriptRevision, &Range<u32>) -> Sha256Digest,
) -> RegenerationPlan {
    let changed = changed_segments(previous, next);
    let mut reusable = Vec::new();
    let mut recompute = Vec::new();

    for section in sections {
        let touched = changed.iter().any(|index| section.segments.contains(index));
        let digest_matches = digest_of(next, &section.segments) == section.input_sha256;
        if touched || !digest_matches {
            recompute.push(section.segments.clone());
        } else {
            reusable.push(section.clone());
        }
    }

    RegenerationPlan {
        previous_output_id,
        target_revision_id: next.id,
        reusable_sections: reusable,
        recompute_ranges: recompute,
        changed_segments: changed.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::pipeline::test_support::{revision, revision_with_parent};
    use crate::pipeline::{canonical_transcript_digest, SummaryResponse};
    use plaudon_domain::ids::StageRunId;

    fn section(chunk_index: u32, segments: Range<u32>, digest: Sha256Digest) -> SectionSummary {
        SectionSummary {
            stage_run_id: StageRunId::new(),
            chunk_index,
            segments,
            input_sha256: digest,
            response: SummaryResponse {
                bullets: Vec::new(),
                covered_first_segment: 0,
                covered_last_segment: 0,
            },
        }
    }

    fn range_digest(revision: &TranscriptRevision, range: &Range<u32>) -> Sha256Digest {
        crate::pipeline::canonical_range_digest(revision, range)
    }

    #[test]
    fn an_edit_marks_every_dependent_output_stale_exactly_once() {
        let base = revision(&["one", "two"]);
        let output_a = OutputId::new();
        let output_b = OutputId::new();
        let graph = DependencyGraph::from_edges([
            OutputDependency {
                output_id: output_a,
                transcript_revision_id: base.id,
                input_sha256: canonical_transcript_digest(&base),
                stale: false,
            },
            OutputDependency {
                output_id: output_b,
                transcript_revision_id: base.id,
                input_sha256: canonical_transcript_digest(&base),
                stale: false,
            },
        ]);
        let markings = graph.stale_closure(
            &[base.id],
            StaleReason::TranscriptEdited,
            Timestamp::from_millis(5),
        );
        assert_eq!(markings.len(), 2);
        let ids: BTreeSet<OutputId> = markings.iter().map(|m| m.output_id).collect();
        assert!(ids.contains(&output_a) && ids.contains(&output_b));
        assert!(markings
            .iter()
            .all(|m| m.reason == StaleReason::TranscriptEdited));
    }

    #[test]
    fn an_already_stale_output_is_not_marked_again() {
        let base = revision(&["one"]);
        let graph = DependencyGraph::from_edges([OutputDependency {
            output_id: OutputId::new(),
            transcript_revision_id: base.id,
            input_sha256: canonical_transcript_digest(&base),
            stale: true,
        }]);
        assert!(graph
            .stale_closure(
                &[base.id],
                StaleReason::TranscriptEdited,
                Timestamp::from_millis(1)
            )
            .is_empty());
    }

    #[test]
    fn an_output_reading_an_untouched_revision_is_not_marked() {
        let touched = revision(&["one"]);
        let untouched = revision(&["two"]);
        let graph = DependencyGraph::from_edges([OutputDependency {
            output_id: OutputId::new(),
            transcript_revision_id: untouched.id,
            input_sha256: canonical_transcript_digest(&untouched),
            stale: false,
        }]);
        assert!(graph
            .stale_closure(
                &[touched.id],
                StaleReason::TranscriptEdited,
                Timestamp::from_millis(1)
            )
            .is_empty());
    }

    #[test]
    fn changed_segments_finds_exactly_the_edited_indices() {
        let base = revision(&["alpha", "beta", "gamma"]);
        let edited = revision_with_parent(&base, &["alpha", "beta corrected", "gamma"]);
        assert_eq!(changed_segments(&base, &edited), BTreeSet::from([1_u32]));
    }

    #[test]
    fn appending_a_segment_counts_as_a_change() {
        let base = revision(&["alpha"]);
        let edited = revision_with_parent(&base, &["alpha", "beta"]);
        assert_eq!(changed_segments(&base, &edited), BTreeSet::from([1_u32]));
    }

    #[test]
    fn targeted_regeneration_reuses_untouched_sections_only() {
        let base = revision(&["alpha", "beta", "gamma", "delta"]);
        let edited = revision_with_parent(&base, &["alpha", "beta", "gamma changed", "delta"]);
        let sections = vec![
            section(0, 0..2, range_digest(&base, &(0..2))),
            section(1, 2..4, range_digest(&base, &(2..4))),
        ];
        let plan = plan_regeneration(OutputId::new(), &base, &edited, &sections, range_digest);
        assert_eq!(plan.changed_segments, vec![2]);
        assert_eq!(plan.recompute_ranges, vec![2..4]);
        assert_eq!(plan.reusable_sections.len(), 1);
        assert_eq!(plan.reusable_sections[0].segments, 0..2);
        assert!(plan.reuses_anything());
    }

    #[test]
    fn an_unchanged_range_is_reused_across_revisions() {
        let base = revision(&["alpha", "beta"]);
        let edited = revision_with_parent(&base, &["alpha", "beta"]);
        // Section digests are content-addressed, so a new revision whose text
        // is identical in a range reuses that range's summary rather than
        // paying for it again.
        let sections = vec![section(0, 0..2, range_digest(&base, &(0..2)))];
        let plan = plan_regeneration(OutputId::new(), &base, &edited, &sections, range_digest);
        assert!(plan.recompute_ranges.is_empty());
        assert!(plan.reuses_anything());
    }

    #[test]
    fn a_section_whose_stored_digest_does_not_match_is_recomputed() {
        let base = revision(&["alpha", "beta"]);
        let edited = revision_with_parent(&base, &["alpha", "beta"]);
        // A stored digest that does not describe the new text is never
        // trusted, even when no index in the section was reported as changed.
        let sections = vec![section(0, 0..2, Sha256Digest::of(b"something else"))];
        let plan = plan_regeneration(OutputId::new(), &base, &edited, &sections, range_digest);
        assert_eq!(plan.recompute_ranges, vec![0..2]);
        assert!(!plan.reuses_anything());
    }

    #[test]
    fn stale_reasons_round_trip_through_their_wire_strings() {
        for reason in [
            StaleReason::TranscriptEdited,
            StaleReason::TranscriptReplaced,
            StaleReason::TemplateUpgraded,
        ] {
            assert_eq!(StaleReason::from_str_opt(reason.as_str()), Some(reason));
        }
        assert_eq!(StaleReason::from_str_opt("whatever"), None);
    }
}
