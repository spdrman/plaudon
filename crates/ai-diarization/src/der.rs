//! Diarization Error Rate with overlapping-speech handling (spec 17.4).
//!
//! DER is scored on a frame grid rather than on turn boundaries, because
//! overlapping speech cannot be expressed as a single label per instant. At
//! each frame the reference holds a *set* of speakers and the hypothesis holds
//! a *set* of clusters, and the three standard components are accumulated:
//!
//! * **missed speech** — reference speakers with no hypothesis slot,
//! * **false alarm** — hypothesis clusters with no reference speaker,
//! * **confusion** — matched slots where the mapped cluster is the wrong one.
//!
//! `DER = (missed + false_alarm + confusion) / total_reference_speech`, where
//! the denominator counts overlapping speech once per simultaneous speaker.
//! That is the definition that makes a system which ignores overlap score
//! worse than one that reports it, which is the behaviour we want to measure.
//!
//! The cluster-to-speaker mapping is chosen to minimize the error: exhaustive
//! for the small speaker counts a meeting actually has, greedy beyond that.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::contract::SpeakerTurn;

/// Frame size used for scoring, in milliseconds.
pub const FRAME_MS: u64 = 10;

/// Largest speaker count for which the optimal mapping is searched
/// exhaustively. Six simultaneous speakers is already beyond what the
/// reference corpora contain.
pub const EXHAUSTIVE_MAPPING_LIMIT: usize = 6;

/// One labelled region of reference speech.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceTurn {
    /// Reference speaker name.
    pub speaker: String,
    /// Inclusive start in milliseconds.
    pub start_ms: u64,
    /// Exclusive end in milliseconds.
    pub end_ms: u64,
}

impl ReferenceTurn {
    /// Construct a reference turn.
    #[must_use]
    pub fn new(speaker: impl Into<String>, start_ms: u64, end_ms: u64) -> Self {
        Self {
            speaker: speaker.into(),
            start_ms,
            end_ms,
        }
    }
}

/// Options for one scoring run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerOptions {
    /// Milliseconds excluded on each side of a reference boundary, to absorb
    /// annotation imprecision. Zero by default: a collar flatters a system,
    /// and we would rather report the harsher honest number.
    pub collar_ms: u64,
    /// Whether frames with more than one reference speaker are scored.
    /// Excluding them is common in published numbers and always overstates
    /// quality, so it is opt-in and recorded in the report.
    pub skip_overlap_regions: bool,
}

impl Default for DerOptions {
    fn default() -> Self {
        Self {
            collar_ms: 0,
            skip_overlap_regions: false,
        }
    }
}

/// A DER measurement with the sample sizes behind it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerReport {
    /// `missed + false_alarm + confusion` over total reference speech.
    pub der: f64,
    /// Reference speech present but not hypothesized, in milliseconds.
    pub missed_ms: u64,
    /// Hypothesized speech with no reference speaker, in milliseconds.
    pub false_alarm_ms: u64,
    /// Matched speech attributed to the wrong cluster, in milliseconds.
    pub confusion_ms: u64,
    /// Total reference speech, counting overlap once per speaker.
    pub total_reference_ms: u64,
    /// Reference speech where two or more speakers are active.
    pub overlapping_reference_ms: u64,
    /// Distinct reference speakers.
    pub reference_speaker_count: usize,
    /// Distinct hypothesis clusters.
    pub hypothesis_cluster_count: usize,
    /// Frames scored.
    pub scored_frames: usize,
    /// Frames excluded by the collar or the overlap option.
    pub excluded_frames: usize,
    /// Whether overlap regions were excluded.
    pub overlap_regions_skipped: bool,
    /// Collar applied, in milliseconds.
    pub collar_ms: u64,
}

impl DerReport {
    /// Whether the measurement covered any overlapping speech at all.
    ///
    /// A DER over a corpus with no overlap says nothing about overlap
    /// handling, so this is reported rather than left for the reader to guess.
    #[must_use]
    pub const fn covers_overlapping_speech(&self) -> bool {
        self.overlapping_reference_ms > 0
    }
}

fn frame_sets(
    reference: &[ReferenceTurn],
    hypothesis: &[SpeakerTurn],
) -> (Vec<BTreeSet<String>>, Vec<BTreeSet<String>>, u64, u64) {
    let end = reference
        .iter()
        .map(|t| t.end_ms)
        .chain(hypothesis.iter().map(|t| t.end_ms))
        .max()
        .unwrap_or(0);
    let start = reference
        .iter()
        .map(|t| t.start_ms)
        .chain(hypothesis.iter().map(|t| t.start_ms))
        .min()
        .unwrap_or(0);
    let frame_start = start - (start % FRAME_MS);
    let frame_count =
        usize::try_from(end.saturating_sub(frame_start).div_ceil(FRAME_MS)).unwrap_or(usize::MAX);

    let mut ref_frames = vec![BTreeSet::new(); frame_count];
    let mut hyp_frames = vec![BTreeSet::new(); frame_count];

    for turn in reference {
        for (index, set) in ref_frames.iter_mut().enumerate() {
            let frame = frame_start + index as u64 * FRAME_MS;
            if frame >= turn.start_ms && frame < turn.end_ms {
                set.insert(turn.speaker.clone());
            }
        }
    }
    for turn in hypothesis {
        for (index, set) in hyp_frames.iter_mut().enumerate() {
            let frame = frame_start + index as u64 * FRAME_MS;
            if frame >= turn.start_ms && frame < turn.end_ms {
                set.insert(turn.cluster.as_str().to_string());
            }
        }
    }
    (ref_frames, hyp_frames, frame_start, end)
}

fn boundary_excluded(frame_ms: u64, reference: &[ReferenceTurn], collar_ms: u64) -> bool {
    if collar_ms == 0 {
        return false;
    }
    reference.iter().any(|turn| {
        frame_ms.abs_diff(turn.start_ms) < collar_ms || frame_ms.abs_diff(turn.end_ms) < collar_ms
    })
}

/// Co-occurrence duration between each hypothesis cluster and each reference
/// speaker, in frames.
fn cooccurrence(
    ref_frames: &[BTreeSet<String>],
    hyp_frames: &[BTreeSet<String>],
    scored: &[bool],
) -> BTreeMap<(String, String), usize> {
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (index, keep) in scored.iter().enumerate() {
        if !keep {
            continue;
        }
        for cluster in &hyp_frames[index] {
            for speaker in &ref_frames[index] {
                *counts
                    .entry((cluster.clone(), speaker.clone()))
                    .or_insert(0) += 1;
            }
        }
    }
    counts
}

fn permutations(items: &[String]) -> Vec<Vec<String>> {
    if items.is_empty() {
        return vec![Vec::new()];
    }
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let mut rest: Vec<String> = items.to_vec();
        rest.remove(index);
        for mut tail in permutations(&rest) {
            let mut candidate = vec![item.clone()];
            candidate.append(&mut tail);
            out.push(candidate);
        }
    }
    out
}

/// Map hypothesis clusters onto reference speakers so that the mapped
/// co-occurrence is maximal.
fn best_mapping(
    clusters: &[String],
    speakers: &[String],
    counts: &BTreeMap<(String, String), usize>,
) -> BTreeMap<String, String> {
    let mut mapping = BTreeMap::new();
    if clusters.is_empty() || speakers.is_empty() {
        return mapping;
    }

    if clusters.len() <= EXHAUSTIVE_MAPPING_LIMIT && speakers.len() <= EXHAUSTIVE_MAPPING_LIMIT {
        let (longer, shorter, cluster_first) = if clusters.len() >= speakers.len() {
            (clusters, speakers, true)
        } else {
            (speakers, clusters, false)
        };
        let mut best_score = 0usize;
        let mut best: Vec<String> = Vec::new();
        for permutation in permutations(longer) {
            let score: usize = shorter
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let key = if cluster_first {
                        (permutation[index].clone(), item.clone())
                    } else {
                        (item.clone(), permutation[index].clone())
                    };
                    *counts.get(&key).unwrap_or(&0)
                })
                .sum();
            if score > best_score || best.is_empty() {
                best_score = score;
                best = permutation;
            }
        }
        for (index, item) in shorter.iter().enumerate() {
            if cluster_first {
                mapping.insert(best[index].clone(), item.clone());
            } else {
                mapping.insert(item.clone(), best[index].clone());
            }
        }
        return mapping;
    }

    // Greedy fallback for speaker counts no meeting corpus reaches: take the
    // highest remaining co-occurrence until one side is exhausted.
    let mut pairs: Vec<(&(String, String), &usize)> = counts.iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let mut used_speakers: BTreeSet<&String> = BTreeSet::new();
    for ((cluster, speaker), _) in pairs {
        if mapping.contains_key(cluster) || used_speakers.contains(speaker) {
            continue;
        }
        mapping.insert(cluster.clone(), speaker.clone());
        used_speakers.insert(speaker);
    }
    mapping
}

/// Score a hypothesis against a reference annotation.
#[must_use]
pub fn diarization_error_rate(
    reference: &[ReferenceTurn],
    hypothesis: &[SpeakerTurn],
    options: DerOptions,
) -> DerReport {
    let (ref_frames, hyp_frames, frame_start, _end) = frame_sets(reference, hypothesis);

    let mut scored = vec![true; ref_frames.len()];
    let mut excluded_frames = 0;
    for (index, keep) in scored.iter_mut().enumerate() {
        let frame_ms = frame_start + index as u64 * FRAME_MS;
        let overlap_frame = ref_frames[index].len() > 1;
        if boundary_excluded(frame_ms, reference, options.collar_ms)
            || (options.skip_overlap_regions && overlap_frame)
        {
            *keep = false;
            excluded_frames += 1;
        }
    }

    let clusters: Vec<String> = {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for frame in &hyp_frames {
            set.extend(frame.iter().cloned());
        }
        set.into_iter().collect()
    };
    let speakers: Vec<String> = {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for frame in &ref_frames {
            set.extend(frame.iter().cloned());
        }
        set.into_iter().collect()
    };

    let counts = cooccurrence(&ref_frames, &hyp_frames, &scored);
    let mapping = best_mapping(&clusters, &speakers, &counts);

    let mut missed = 0u64;
    let mut false_alarm = 0u64;
    let mut confusion = 0u64;
    let mut total_reference = 0u64;
    let mut overlapping_reference = 0u64;
    let mut scored_frames = 0usize;

    for (index, keep) in scored.iter().enumerate() {
        let reference_set = &ref_frames[index];
        if reference_set.len() > 1 {
            overlapping_reference += (reference_set.len() as u64 - 1) * FRAME_MS * u64::from(*keep);
        }
        if !keep {
            continue;
        }
        scored_frames += 1;
        let hypothesis_set = &hyp_frames[index];
        let n_ref = reference_set.len() as u64;
        let n_hyp = hypothesis_set.len() as u64;
        total_reference += n_ref * FRAME_MS;

        let correct = hypothesis_set
            .iter()
            .filter(|cluster| {
                mapping
                    .get(*cluster)
                    .is_some_and(|speaker| reference_set.contains(speaker))
            })
            .count() as u64;

        missed += n_ref.saturating_sub(n_hyp) * FRAME_MS;
        false_alarm += n_hyp.saturating_sub(n_ref) * FRAME_MS;
        confusion += (n_ref.min(n_hyp).saturating_sub(correct)) * FRAME_MS;
    }

    let der = if total_reference == 0 {
        0.0
    } else {
        (missed + false_alarm + confusion) as f64 / total_reference as f64
    };

    DerReport {
        der,
        missed_ms: missed,
        false_alarm_ms: false_alarm,
        confusion_ms: confusion,
        total_reference_ms: total_reference,
        overlapping_reference_ms: overlapping_reference,
        reference_speaker_count: speakers.len(),
        hypothesis_cluster_count: clusters.len(),
        scored_frames,
        excluded_frames,
        overlap_regions_skipped: options.skip_overlap_regions,
        collar_ms: options.collar_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f64, expected: f64) -> bool {
        (actual - expected).abs() < 1e-9
    }

    #[test]
    fn a_perfect_hypothesis_scores_zero() {
        let reference = vec![
            ReferenceTurn::new("alice", 0, 1_000),
            ReferenceTurn::new("bob", 1_000, 2_000),
        ];
        let hypothesis = vec![
            SpeakerTurn::new("c1", 0, 1_000),
            SpeakerTurn::new("c2", 1_000, 2_000),
        ];
        let report = diarization_error_rate(&reference, &hypothesis, DerOptions::default());
        assert!(close(report.der, 0.0), "{report:?}");
        assert_eq!(report.total_reference_ms, 2_000);
        assert_eq!(report.reference_speaker_count, 2);
        assert_eq!(report.hypothesis_cluster_count, 2);
    }

    #[test]
    fn cluster_names_do_not_matter_only_the_partition_does() {
        let reference = vec![
            ReferenceTurn::new("alice", 0, 1_000),
            ReferenceTurn::new("bob", 1_000, 2_000),
        ];
        // Swapped names; the optimal mapping must undo the swap.
        let hypothesis = vec![
            SpeakerTurn::new("bob", 0, 1_000),
            SpeakerTurn::new("alice", 1_000, 2_000),
        ];
        let report = diarization_error_rate(&reference, &hypothesis, DerOptions::default());
        assert!(close(report.der, 0.0), "{report:?}");
    }

    #[test]
    fn an_empty_hypothesis_misses_all_reference_speech() {
        let reference = vec![ReferenceTurn::new("alice", 0, 1_000)];
        let report = diarization_error_rate(&reference, &[], DerOptions::default());
        assert!(close(report.der, 1.0), "{report:?}");
        assert_eq!(report.missed_ms, 1_000);
        assert_eq!(report.false_alarm_ms, 0);
    }

    #[test]
    fn hypothesized_speech_in_silence_is_a_false_alarm() {
        let reference = vec![ReferenceTurn::new("alice", 0, 1_000)];
        let hypothesis = vec![
            SpeakerTurn::new("c1", 0, 1_000),
            SpeakerTurn::new("c1", 1_000, 1_500),
        ];
        let report = diarization_error_rate(&reference, &hypothesis, DerOptions::default());
        assert_eq!(report.false_alarm_ms, 500);
        assert!(close(report.der, 0.5), "{report:?}");
    }

    #[test]
    fn merging_two_speakers_into_one_cluster_is_confusion_not_a_free_pass() {
        let reference = vec![
            ReferenceTurn::new("alice", 0, 1_000),
            ReferenceTurn::new("bob", 1_000, 2_000),
        ];
        let hypothesis = vec![SpeakerTurn::new("c1", 0, 2_000)];
        let report = diarization_error_rate(&reference, &hypothesis, DerOptions::default());
        assert_eq!(report.confusion_ms, 1_000);
        assert!(close(report.der, 0.5), "{report:?}");
    }

    #[test]
    fn overlapping_reference_speech_counts_once_per_simultaneous_speaker() {
        let reference = vec![
            ReferenceTurn::new("alice", 0, 1_000),
            ReferenceTurn::new("bob", 500, 1_000),
        ];
        let hypothesis = vec![
            SpeakerTurn::new("c1", 0, 1_000),
            SpeakerTurn::new("c2", 500, 1_000),
        ];
        let report = diarization_error_rate(&reference, &hypothesis, DerOptions::default());
        assert_eq!(report.total_reference_ms, 1_500);
        assert_eq!(report.overlapping_reference_ms, 500);
        assert!(report.covers_overlapping_speech());
        assert!(close(report.der, 0.0), "{report:?}");
    }

    #[test]
    fn a_system_that_ignores_overlap_is_penalized_for_the_missed_speaker() {
        let reference = vec![
            ReferenceTurn::new("alice", 0, 1_000),
            ReferenceTurn::new("bob", 500, 1_000),
        ];
        let hypothesis = vec![SpeakerTurn::new("c1", 0, 1_000)];
        let report = diarization_error_rate(&reference, &hypothesis, DerOptions::default());
        assert_eq!(report.missed_ms, 500);
        assert_eq!(report.total_reference_ms, 1_500);
        assert!(close(report.der, 500.0 / 1_500.0), "{report:?}");
    }

    #[test]
    fn skipping_overlap_regions_is_recorded_in_the_report() {
        let reference = vec![
            ReferenceTurn::new("alice", 0, 1_000),
            ReferenceTurn::new("bob", 500, 1_000),
        ];
        let hypothesis = vec![SpeakerTurn::new("c1", 0, 1_000)];
        let report = diarization_error_rate(
            &reference,
            &hypothesis,
            DerOptions {
                collar_ms: 0,
                skip_overlap_regions: true,
            },
        );
        assert!(report.overlap_regions_skipped);
        assert_eq!(report.excluded_frames, 50);
        assert!(close(report.der, 0.0), "{report:?}");
    }

    #[test]
    fn a_collar_excludes_frames_around_reference_boundaries() {
        let reference = vec![ReferenceTurn::new("alice", 0, 1_000)];
        let hypothesis = vec![SpeakerTurn::new("c1", 0, 1_000)];
        let report = diarization_error_rate(
            &reference,
            &hypothesis,
            DerOptions {
                collar_ms: 100,
                skip_overlap_regions: false,
            },
        );
        assert_eq!(report.collar_ms, 100);
        assert!(report.excluded_frames > 0);
        assert!(report.scored_frames < 100);
    }

    #[test]
    fn an_empty_reference_yields_zero_rather_than_a_division_by_zero() {
        let report = diarization_error_rate(&[], &[], DerOptions::default());
        assert!(close(report.der, 0.0));
        assert_eq!(report.total_reference_ms, 0);
        assert!(!report.covers_overlapping_speech());
    }

    #[test]
    fn the_components_always_sum_to_the_reported_rate() {
        let reference = vec![
            ReferenceTurn::new("alice", 0, 2_000),
            ReferenceTurn::new("bob", 1_000, 3_000),
            ReferenceTurn::new("carol", 2_500, 4_000),
        ];
        let hypothesis = vec![
            SpeakerTurn::new("c1", 0, 1_500),
            SpeakerTurn::new("c2", 1_000, 3_200),
        ];
        let report = diarization_error_rate(&reference, &hypothesis, DerOptions::default());
        let sum = report.missed_ms + report.false_alarm_ms + report.confusion_ms;
        assert!(close(
            report.der,
            sum as f64 / report.total_reference_ms as f64
        ));
    }

    #[test]
    fn exhaustive_mapping_handles_three_speakers_optimally() {
        let reference = vec![
            ReferenceTurn::new("a", 0, 1_000),
            ReferenceTurn::new("b", 1_000, 2_000),
            ReferenceTurn::new("c", 2_000, 3_000),
        ];
        let hypothesis = vec![
            SpeakerTurn::new("z", 0, 1_000),
            SpeakerTurn::new("y", 1_000, 2_000),
            SpeakerTurn::new("x", 2_000, 3_000),
        ];
        let report = diarization_error_rate(&reference, &hypothesis, DerOptions::default());
        assert!(close(report.der, 0.0), "{report:?}");
    }
}
