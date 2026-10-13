//! The provider-independent half of a segmentation-based diarization
//! pipeline: powerset decoding, constrained clustering and aggregation.
//!
//! A segmentation checkpoint such as pyannote `segmentation-3.0` answers a
//! *local* question — "which of up to three people are speaking in each frame
//! of this ten-second window" — and it answers it as a powerset: one class per
//! representable set of simultaneous speakers, so overlap is a class and not a
//! pair of independent probabilities. Turning that into a session is three
//! steps, and none of them need an inference runtime:
//!
//! 1. **Decode** each frame's class scores into the set of active local
//!    speakers ([`decode_powerset_frame`]).
//! 2. **Cluster** the (window, local speaker) tracks into session-global
//!    speakers using embeddings of their own speech ([`cluster_tracks`]).
//!    "Local speaker 2 of window 7" and "local speaker 0 of window 8" are the
//!    same person only if their voices say so; the segmentation model has no
//!    opinion across windows.
//! 3. **Aggregate** overlapping windows into one timeline and binarise it
//!    ([`assemble_turns`]).
//!
//! Keeping these here, in the crate that owns the contract and the DER metric,
//! has a practical consequence: they are ordinary Rust, tested in the default
//! build, while the part that genuinely needs ONNX Runtime — two forward
//! passes — stays in `plaudon-ai-speaker`'s off-by-default `onnx` feature.
//!
//! **Nothing here is a validated provider.** The output is still subject to
//! [`crate::provider::run_guarded`], and no implementation built on this
//! module is in [`crate::provider::VALIDATED_PROVIDERS`] until every ADR-0400
//! condition is met.

use std::collections::BTreeMap;

use crate::contract::{ClusterKey, SpeakerTurn};

/// Local speakers one segmentation window can carry.
///
/// pyannote `segmentation-3.0` declares `num_speakers=3` in its ONNX
/// metadata. Three is a property of the checkpoint, not of the session: a
/// meeting with eight people is covered because each ten-second window rarely
/// holds more than three of them, and the clustering step is what makes the
/// session-global count unbounded.
pub const LOCAL_SPEAKERS: usize = 3;

/// Powerset classes over those speakers with at most two simultaneous.
///
/// The checkpoint declares `powerset_max_classes=2` and `num_classes=7`.
pub const POWERSET_CLASSES: usize = 7;

/// Class index to the set of local speakers it represents.
///
/// The order is the one pyannote's `Powerset` builds — sets of increasing
/// size, each size in `itertools.combinations` order — and it is *not*
/// arbitrary: reading the classes in another order would silently relabel
/// every speaker and still produce plausible-looking turns.
pub const POWERSET_SETS: [&[usize]; POWERSET_CLASSES] =
    [&[], &[0], &[1], &[2], &[0, 1], &[0, 2], &[1, 2]];

/// Decode one frame of powerset scores into the active local speakers.
///
/// The decision is `argmax`, which is what makes the output a *set*: the
/// classes partition the possibilities, so the winning class already says
/// "these two people and nobody else". Thresholding the classes independently
/// would be a different, worse model of overlap.
///
/// Ties go to the lower class index, which makes a degenerate run of equal
/// scores decode to silence rather than to an arbitrary speaker.
#[must_use]
pub fn decode_powerset_frame(scores: &[f32; POWERSET_CLASSES]) -> [bool; LOCAL_SPEAKERS] {
    let mut best = 0usize;
    for (index, score) in scores.iter().enumerate().skip(1) {
        if *score > scores[best] {
            best = index;
        }
    }
    let mut active = [false; LOCAL_SPEAKERS];
    for speaker in POWERSET_SETS[best] {
        active[*speaker] = true;
    }
    active
}

/// How a checkpoint's output frames map onto time.
///
/// A segmentation frame is not an interval, it is a *centre*: the network sees
/// `receptive_field_samples` of audio and emits one decision every
/// `step_samples`. The interval a frame owns is therefore half a step on each
/// side of that centre, which is also what makes consecutive frames exactly
/// contiguous and a turn boundary land between two decisions rather than at
/// the edge of a receptive field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameGrid {
    /// Sample rate the checkpoint consumes.
    pub sample_rate_hz: u32,
    /// Samples between consecutive frame centres (`receptive_field_shift`).
    pub step_samples: u32,
    /// Samples one frame sees (`receptive_field_size`).
    pub receptive_field_samples: u32,
}

impl FrameGrid {
    /// Centre of `frame`, in samples from the start of the audio.
    #[must_use]
    pub const fn centre_samples(&self, frame: u64) -> u64 {
        frame * self.step_samples as u64 + self.receptive_field_samples as u64 / 2
    }

    /// Inclusive start of the interval `frame` owns, in milliseconds.
    #[must_use]
    pub const fn start_ms(&self, frame: u64) -> u64 {
        let centre = self.centre_samples(frame);
        let half = self.step_samples as u64 / 2;
        centre.saturating_sub(half) * 1_000 / self.sample_rate_hz as u64
    }

    /// Exclusive end of the interval `frame` owns, in milliseconds.
    #[must_use]
    pub const fn end_ms(&self, frame: u64) -> u64 {
        let centre = self.centre_samples(frame);
        let half = self.step_samples as u64 / 2;
        (centre + half) * 1_000 / self.sample_rate_hz as u64
    }

    /// Frames a buffer of `samples` yields when it is the whole session.
    #[must_use]
    pub const fn frames_for(&self, samples: u64) -> u64 {
        samples.div_ceil(self.step_samples as u64)
    }
}

/// One local speaker of one window, and the global frames it is active on.
///
/// This is the unit everything downstream works in: it carries enough audio to
/// be embedded, it belongs to exactly one window — which is what the
/// cannot-link constraint in [`cluster_tracks`] is about — and its frames are
/// already expressed on the session-global grid, so aggregation never has to
/// know how the windows were laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTrack {
    /// Index of the window this track was decoded from.
    pub window: usize,
    /// Local speaker slot within that window, `0..LOCAL_SPEAKERS`.
    pub local_speaker: usize,
    /// First global frame of that window.
    pub window_base: u64,
    /// Frames that window carries, including the ones this slot is silent on.
    pub window_frames: usize,
    /// Global frame indices the slot is active on, ascending.
    pub frames: Vec<u64>,
}

/// Why the pipeline refused to produce a result.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PipelineError {
    /// More tracks than the clustering step will hold a distance matrix for.
    #[error("{tracks} tracks exceed the clustering limit of {limit}")]
    TooManyTracks {
        /// Tracks the run produced.
        tracks: usize,
        /// Configured limit.
        limit: usize,
    },
    /// The vectors handed to the clustering step are not all one length.
    #[error("the embeddings are not all {expected} values long")]
    RaggedEmbeddings {
        /// Length the first vector declared.
        expected: usize,
    },
    /// A vector with no magnitude cannot be compared by angle.
    #[error("an embedding has no magnitude and cannot be clustered")]
    DegenerateEmbedding,
}

/// How the tracks are clustered into session-global speakers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClusterOptions {
    /// Cosine distance above which two clusters are different people.
    ///
    /// A measured property of the embedding checkpoint, never a free choice:
    /// the value a run used belongs in the run's artifact.
    pub threshold: f64,
    /// Stop at exactly this many clusters, when the caller knows the count.
    ///
    /// A user's "there were three of us" hint, honoured over the threshold
    /// because it is information the audio does not carry.
    pub target_clusters: Option<usize>,
    /// Tracks a cluster needs before it is a speaker rather than a fragment.
    ///
    /// A session-global speaker appears in many windows. A cluster that
    /// appears in two is usually one unlucky stretch of a speaker who is
    /// already represented elsewhere, so it is dissolved into the nearest
    /// cluster it is allowed to join instead of becoming a fifth person in a
    /// four-person meeting.
    pub min_cluster_tracks: usize,
    /// Largest number of tracks the quadratic distance matrix may hold.
    pub max_tracks: usize,
}

impl Default for ClusterOptions {
    fn default() -> Self {
        Self {
            // The two values `pyannote/speaker-diarization-3.1` publishes for
            // this exact checkpoint pair — `segmentation-3.0` for the windows
            // and WeSpeaker ResNet34-LM for the voices. Taking somebody else's
            // published operating point is not tuning; deriving one from the
            // sessions a DER is then reported on would be.
            threshold: 0.7045,
            target_clusters: None,
            min_cluster_tracks: 12,
            max_tracks: 4_096,
        }
    }
}

/// Cosine distance between two vectors, clamped to the range it can take.
///
/// Zero for identical directions, one for orthogonal, two for opposite. A
/// vector with no magnitude has no direction, so it is maximally distant from
/// everything rather than a division by zero.
fn cosine_distance(left: &[f64], right: &[f64]) -> f64 {
    let dot: f64 = left.iter().zip(right).map(|(a, b)| a * b).sum();
    let left_norm: f64 = left.iter().map(|value| value * value).sum::<f64>().sqrt();
    let right_norm: f64 = right.iter().map(|value| value * value).sum::<f64>().sqrt();
    if left_norm <= f64::EPSILON || right_norm <= f64::EPSILON {
        return 2.0;
    }
    (1.0 - dot / (left_norm * right_norm)).clamp(0.0, 2.0)
}

/// Cluster tracks by the angle between their embeddings.
///
/// Centroid-linkage agglomerative clustering over cosine distance — the
/// linkage `pyannote/speaker-diarization-3.1` publishes a threshold for — with
/// one constraint that is not decoration: two tracks of the *same* window are
/// two different people by construction — the segmentation model separated
/// them —
/// so they may never land in the same cluster. Without that constraint a
/// window whose two speakers sound similar collapses into one, and the session
/// loses a speaker that was detected correctly.
///
/// `groups[i]` is the window of track `i`. `fit[i]` says whether track `i` is
/// allowed to *shape* the clustering: a track that carries half a second of
/// speech has a vector, but not one that should decide where a cluster
/// boundary lies. Those tracks are assigned to the nearest cluster once the
/// merging is done, under the same constraint. Excluding them from the output
/// instead would be worse than it sounds — their frames would silently vote
/// for silence in the aggregation, and short tracks cluster at turn edges,
/// where exactly the boundaries of a turn would then erode.
///
/// The returned labels are dense, `0..cluster_count`, numbered by the first
/// track that joined each cluster.
///
/// # Errors
///
/// [`PipelineError::TooManyTracks`] when the quadratic matrix would exceed the
/// configured limit, and [`PipelineError::RaggedEmbeddings`] or
/// [`PipelineError::DegenerateEmbedding`] when a vector cannot be compared.
pub fn cluster_tracks(
    vectors: &[Vec<f32>],
    groups: &[usize],
    fit: &[bool],
    options: ClusterOptions,
) -> Result<Vec<usize>, PipelineError> {
    debug_assert_eq!(vectors.len(), groups.len());
    debug_assert_eq!(vectors.len(), fit.len());
    let count = vectors.len();
    if count == 0 {
        return Ok(Vec::new());
    }
    if count > options.max_tracks {
        return Err(PipelineError::TooManyTracks {
            tracks: count,
            limit: options.max_tracks,
        });
    }
    let dim = vectors[0].len();
    if vectors.iter().any(|vector| vector.len() != dim) {
        return Err(PipelineError::RaggedEmbeddings { expected: dim });
    }

    let unit: Vec<Vec<f64>> = vectors
        .iter()
        .map(|vector| {
            let norm = vector
                .iter()
                .map(|value| f64::from(*value) * f64::from(*value))
                .sum::<f64>()
                .sqrt();
            if norm <= f64::EPSILON {
                return Err(PipelineError::DegenerateEmbedding);
            }
            Ok(vector
                .iter()
                .map(|value| f64::from(*value) / norm)
                .collect())
        })
        .collect::<Result<_, _>>()?;

    // Pairwise cosine distance between cluster centroids. Centroid linkage,
    // not average: the published threshold this pipeline uses as its default
    // was tuned for it, and the two linkages put a cluster boundary in
    // different places, so borrowing one without the other would be borrowing
    // a number that means something else.
    let mut sums: Vec<Vec<f64>> = unit;
    let mut distance = vec![0.0f64; count * count];
    for left in 0..count {
        for right in (left + 1)..count {
            let value = cosine_distance(&sums[left], &sums[right]);
            distance[left * count + right] = value;
            distance[right * count + left] = value;
        }
    }

    let mut members: Vec<Vec<usize>> = (0..count).map(|index| vec![index]).collect();
    let mut alive: Vec<bool> = vec![true; count];
    // Two tracks of one window are two people: their clusters may never merge,
    // and the constraint is inherited by every merge that follows.
    let mut blocked = vec![false; count * count];
    for left in 0..count {
        for right in (left + 1)..count {
            if groups[left] == groups[right] {
                blocked[left * count + right] = true;
                blocked[right * count + left] = true;
            }
        }
    }

    // A cluster may merge while every track in it was allowed to fit. Tracks
    // that were not stay singletons until the assignment pass below.
    let mut fitting: Vec<bool> = if fit.iter().any(|allowed| *allowed) {
        fit.to_vec()
    } else {
        // Nothing carries enough speech to fit: clustering them against each
        // other is still better than returning one cluster per track.
        vec![true; count]
    };

    let mut clusters = fitting.iter().filter(|allowed| **allowed).count();
    loop {
        let target_reached = options
            .target_clusters
            .is_some_and(|target| clusters <= target.max(1));
        if clusters <= 1 || target_reached {
            break;
        }
        let mut best: Option<(usize, usize, f64)> = None;
        for left in 0..count {
            if !alive[left] || !fitting[left] {
                continue;
            }
            for right in (left + 1)..count {
                if !alive[right] || !fitting[right] || blocked[left * count + right] {
                    continue;
                }
                let value = distance[left * count + right];
                if best.is_none_or(|(_, _, current)| value < current) {
                    best = Some((left, right, value));
                }
            }
        }
        let Some((left, right, value)) = best else {
            break;
        };
        if options.target_clusters.is_none() && value > options.threshold {
            break;
        }

        let absorbed_sum = std::mem::take(&mut sums[right]);
        for (slot, value) in sums[left].iter_mut().zip(&absorbed_sum) {
            *slot += value;
        }
        for other in 0..count {
            if !alive[other] || other == left || other == right {
                continue;
            }
            let merged = cosine_distance(&sums[left], &sums[other]);
            distance[left * count + other] = merged;
            distance[other * count + left] = merged;
            let inherited = blocked[left * count + other] || blocked[right * count + other];
            blocked[left * count + other] = inherited;
            blocked[other * count + left] = inherited;
        }
        let absorbed = std::mem::take(&mut members[right]);
        members[left].extend(absorbed);
        alive[right] = false;
        clusters -= 1;
    }

    // A cluster that only a handful of windows ever saw is a fragment of a
    // speaker, not a speaker: dissolve it and let the pass below place its
    // tracks, unless dissolving it would leave nothing to place them in.
    let large = |members: &Vec<Vec<usize>>, cluster: usize| {
        members[cluster].len() >= options.min_cluster_tracks
    };
    if options.target_clusters.is_none()
        && (0..count).any(|cluster| alive[cluster] && fitting[cluster] && large(&members, cluster))
    {
        for cluster in 0..count {
            if alive[cluster] && fitting[cluster] && !large(&members, cluster) {
                fitting[cluster] = false;
            }
        }
    }

    // The short tracks join the nearest cluster they are allowed to join.
    // `distance` already holds the distance from each of them to every
    // surviving cluster's centroid, which is the criterion the merging used.
    for pending in 0..count {
        if !alive[pending] || fitting[pending] {
            continue;
        }
        let mut best: Option<(usize, f64)> = None;
        for cluster in 0..count {
            if !alive[cluster] || !fitting[cluster] || blocked[pending * count + cluster] {
                continue;
            }
            let value = distance[pending * count + cluster];
            if best.is_none_or(|(_, current)| value < current) {
                best = Some((cluster, value));
            }
        }
        let Some((cluster, _)) = best else {
            // No cluster this track may join: it is its own speaker, which is
            // the honest answer for a window whose other slots took every
            // cluster there is.
            fitting[pending] = true;
            continue;
        };
        let absorbed = std::mem::take(&mut members[pending]);
        members[cluster].extend(absorbed);
        alive[pending] = false;
        // One window contributes at most one track to a cluster, for the same
        // reason the merging could not: they are different people.
        for other in 0..count {
            if groups[other] == groups[pending] {
                blocked[cluster * count + other] = true;
                blocked[other * count + cluster] = true;
            }
        }
    }

    let mut labels = vec![0usize; count];
    let mut next = 0usize;
    for cluster in 0..count {
        if !alive[cluster] {
            continue;
        }
        let mut sorted = members[cluster].clone();
        sorted.sort_unstable();
        for member in sorted {
            labels[member] = next;
        }
        next += 1;
    }
    Ok(labels)
}

/// How the per-window decisions are turned back into one timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AssembleOptions {
    /// Fraction of the windows covering a frame that must call a speaker
    /// active for the session to call them active.
    pub activity_threshold: f64,
    /// Gaps shorter than this inside one speaker's speech are filled.
    pub min_duration_off_ms: u64,
    /// Turns shorter than this are dropped.
    pub min_duration_on_ms: u64,
}

impl Default for AssembleOptions {
    fn default() -> Self {
        Self {
            activity_threshold: 0.5,
            min_duration_off_ms: 0,
            min_duration_on_ms: 0,
        }
    }
}

/// Weight one window's frame carries in the aggregation.
///
/// A Hamming taper, as `pyannote.audio`'s own aggregation uses: a frame at the
/// edge of a ten-second window was decided with half its context missing, and
/// the window whose centre covers that moment knows better. The taper never
/// reaches zero, so a frame only one window saw is still heard.
#[must_use]
pub fn hamming_weight(position: usize, frames: usize) -> f64 {
    if frames <= 1 {
        return 1.0;
    }
    let phase = position.min(frames - 1) as f64 / (frames - 1) as f64;
    0.54 - 0.46 * (2.0 * std::f64::consts::PI * phase).cos()
}

/// One cluster's run of consecutive active frames.
#[derive(Debug, Clone, Copy)]
struct Run {
    cluster: usize,
    first: u64,
    last: u64,
    /// Mean vote fraction over the run: the provider's own confidence, which
    /// is comparable within a run and never across providers.
    confidence: f64,
}

impl Run {
    /// Close an accumulating run at `last`.
    fn close(cluster: usize, open: &OpenRun, last: u64) -> Self {
        Self {
            cluster,
            first: open.first,
            last,
            confidence: open.score_sum / open.frames as f64,
        }
    }

    /// Extend this run over `other`, keeping the duration-weighted mean.
    fn absorb(&mut self, other: &Self) {
        let own = (self.last - self.first + 1) as f64;
        let theirs = (other.last - other.first + 1) as f64;
        self.confidence = (self.confidence * own + other.confidence * theirs) / (own + theirs);
        self.last = other.last;
    }
}

/// A run of active frames still being accumulated.
#[derive(Debug, Clone, Copy)]
struct OpenRun {
    first: u64,
    score_sum: f64,
    frames: u64,
}

/// Aggregate clustered tracks into speaker turns.
///
/// `coverage[frame]` is the total weight of the windows that saw that frame,
/// the denominator that makes overlapping windows a vote rather than a union:
/// a speaker detected by one window out of ten is noise, a speaker detected by
/// six is speech. Frames no window saw are silence. The weights are
/// [`hamming_weight`], and the caller must accumulate `coverage` with the same
/// function or the ratio means nothing.
///
/// Cluster keys are `cluster-01`, `cluster-02`, … numbered by first
/// appearance in time, so the names are stable for one input and carry no
/// identity (spec 17.6).
#[must_use]
pub fn assemble_turns(
    tracks: &[LocalTrack],
    labels: &[usize],
    coverage: &[f64],
    grid: &FrameGrid,
    options: AssembleOptions,
) -> Vec<SpeakerTurn> {
    debug_assert_eq!(tracks.len(), labels.len());
    let frames = coverage.len();
    let cluster_count = labels.iter().copied().max().map_or(0, |max| max + 1);
    if frames == 0 || cluster_count == 0 {
        return Vec::new();
    }

    let mut votes = vec![0.0f64; frames * cluster_count];
    for (track, label) in tracks.iter().zip(labels) {
        for frame in &track.frames {
            let index = *frame as usize;
            if index < frames {
                votes[index * cluster_count + label] += hamming_weight(
                    frame.saturating_sub(track.window_base) as usize,
                    track.window_frames,
                );
            }
        }
    }

    let mut runs: Vec<Run> = Vec::new();
    for cluster in 0..cluster_count {
        let mut open: Option<OpenRun> = None;
        for frame in 0..frames {
            let seen = coverage[frame];
            let score = if seen <= f64::EPSILON {
                0.0
            } else {
                votes[frame * cluster_count + cluster] / seen
            };
            if score >= options.activity_threshold {
                match &mut open {
                    Some(run) => {
                        run.score_sum += score;
                        run.frames += 1;
                    }
                    None => {
                        open = Some(OpenRun {
                            first: frame as u64,
                            score_sum: score,
                            frames: 1,
                        });
                    }
                }
            } else if let Some(run) = open.take() {
                runs.push(Run::close(cluster, &run, frame as u64 - 1));
            }
        }
        if let Some(run) = open.take() {
            runs.push(Run::close(cluster, &run, frames as u64 - 1));
        }
    }

    // Fill short gaps, then drop short turns: the order matters, because a
    // sequence of brief detections separated by brief gaps is one turn, not a
    // list of turns that each fall below the floor.
    runs.sort_unstable_by_key(|run| (run.cluster, run.first));
    let mut merged: Vec<Run> = Vec::with_capacity(runs.len());
    for run in runs {
        match merged.last_mut() {
            Some(previous)
                if previous.cluster == run.cluster
                    && grid
                        .start_ms(run.first)
                        .saturating_sub(grid.end_ms(previous.last))
                        < options.min_duration_off_ms =>
            {
                previous.absorb(&run);
            }
            _ => merged.push(run),
        }
    }

    let mut turns: Vec<(usize, SpeakerTurn)> = merged
        .into_iter()
        .filter_map(|run| {
            let start_ms = grid.start_ms(run.first);
            let end_ms = grid.end_ms(run.last);
            (end_ms.saturating_sub(start_ms) >= options.min_duration_on_ms).then(|| {
                (
                    run.cluster,
                    SpeakerTurn {
                        cluster: ClusterKey::new(String::new()),
                        start_ms,
                        end_ms,
                        confidence: Some(run.confidence),
                    },
                )
            })
        })
        .collect();
    turns.sort_by_key(|(cluster, turn)| (turn.start_ms, turn.end_ms, *cluster));

    // Name the clusters by first appearance, so the keys of one run are stable
    // and say nothing about who the speaker is.
    let mut names: BTreeMap<usize, String> = BTreeMap::new();
    for (cluster, _) in &turns {
        let next = names.len() + 1;
        names
            .entry(*cluster)
            .or_insert_with(|| format!("cluster-{next:02}"));
    }
    turns
        .into_iter()
        .map(|(cluster, mut turn)| {
            turn.cluster = ClusterKey::new(names[&cluster].clone());
            turn
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRID: FrameGrid = FrameGrid {
        sample_rate_hz: 16_000,
        step_samples: 270,
        receptive_field_samples: 991,
    };

    #[test]
    fn the_powerset_table_is_the_one_the_checkpoint_was_exported_with() {
        // Sets of increasing size, each in combination order: silence, the
        // three singletons, then the three pairs. A different order would
        // decode every overlap as the wrong pair of people.
        assert_eq!(POWERSET_SETS[0], &[] as &[usize]);
        assert_eq!(POWERSET_SETS[1], &[0]);
        assert_eq!(POWERSET_SETS[3], &[2]);
        assert_eq!(POWERSET_SETS[4], &[0, 1]);
        assert_eq!(POWERSET_SETS[6], &[1, 2]);
    }

    #[test]
    fn a_frame_decodes_to_the_winning_set_including_the_empty_one() {
        assert_eq!(
            decode_powerset_frame(&[4.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            [false, false, false]
        );
        assert_eq!(
            decode_powerset_frame(&[0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 0.0]),
            [false, true, false]
        );
        assert_eq!(
            decode_powerset_frame(&[0.0, 1.0, 1.0, 0.0, 9.0, 0.0, 0.0]),
            [true, true, false]
        );
    }

    #[test]
    fn an_all_equal_frame_decodes_to_silence_rather_than_to_a_guess() {
        assert_eq!(
            decode_powerset_frame(&[1.0; POWERSET_CLASSES]),
            [false, false, false]
        );
    }

    #[test]
    fn frames_own_half_a_step_either_side_of_their_centre_and_are_contiguous() {
        // Centre of frame 0 is 495 samples in: 360..630 samples is 22..39 ms.
        assert_eq!(GRID.centre_samples(0), 495);
        assert_eq!(GRID.start_ms(0), 22);
        assert_eq!(GRID.end_ms(0), 39);
        for frame in 0..100 {
            assert_eq!(GRID.end_ms(frame), GRID.start_ms(frame + 1));
        }
    }

    #[test]
    fn clustering_separates_two_voices_and_joins_their_repeats() {
        let vectors = vec![
            vec![1.0, 0.0],
            vec![0.99, 0.14],
            vec![0.0, 1.0],
            vec![0.14, 0.99],
        ];
        let groups = vec![0, 1, 0, 1];
        let labels = cluster_tracks(&vectors, &groups, &[true; 4], ClusterOptions::default())
            .expect("clusters");
        assert_eq!(labels[0], labels[1]);
        assert_eq!(labels[2], labels[3]);
        assert_ne!(labels[0], labels[2]);
    }

    #[test]
    fn two_tracks_of_one_window_are_never_the_same_speaker() {
        // Identical vectors, same window: the segmentation model already said
        // these are two people, and no distance may overrule that.
        let vectors = vec![vec![1.0, 0.0], vec![1.0, 0.0]];
        let labels = cluster_tracks(&vectors, &[7, 7], &[true; 2], ClusterOptions::default())
            .expect("clusters");
        assert_ne!(labels[0], labels[1]);
    }

    #[test]
    fn a_speaker_count_hint_overrides_the_threshold() {
        let vectors = vec![
            vec![1.0, 0.0],
            vec![0.99, 0.14],
            vec![0.0, 1.0],
            vec![0.14, 0.99],
        ];
        let options = ClusterOptions {
            target_clusters: Some(1),
            ..ClusterOptions::default()
        };
        let labels =
            cluster_tracks(&vectors, &[0, 1, 2, 3], &[true; 4], options).expect("clusters");
        assert_eq!(labels, vec![0, 0, 0, 0]);
    }

    #[test]
    fn a_hint_cannot_merge_past_what_the_constraint_allows() {
        let vectors = vec![vec![1.0, 0.0], vec![1.0, 0.0]];
        let options = ClusterOptions {
            target_clusters: Some(1),
            ..ClusterOptions::default()
        };
        let labels = cluster_tracks(&vectors, &[3, 3], &[true; 2], options).expect("clusters");
        assert_ne!(labels[0], labels[1]);
    }

    #[test]
    fn a_track_count_beyond_the_matrix_limit_is_refused_rather_than_swapped() {
        let vectors = vec![vec![1.0, 0.0]; 8];
        let options = ClusterOptions {
            max_tracks: 4,
            ..ClusterOptions::default()
        };
        let error = cluster_tracks(&vectors, &[0, 1, 2, 3, 4, 5, 6, 7], &[true; 8], options)
            .expect_err("refused");
        assert_eq!(
            error,
            PipelineError::TooManyTracks {
                tracks: 8,
                limit: 4
            }
        );
    }

    #[test]
    fn a_track_too_short_to_fit_still_joins_the_cluster_it_sounds_like() {
        // Two long tracks define two speakers; the short one is nobody's
        // neighbour until it is assigned, and it must not become a third.
        let vectors = vec![
            vec![1.0, 0.0],
            vec![0.99, 0.14],
            vec![0.0, 1.0],
            vec![0.10, 0.99],
        ];
        let labels = cluster_tracks(
            &vectors,
            &[0, 1, 2, 3],
            &[true, true, true, false],
            ClusterOptions::default(),
        )
        .expect("clusters");
        assert_eq!(labels[3], labels[2]);
        assert_eq!(labels.iter().copied().max(), Some(1));
    }

    #[test]
    fn a_short_track_never_joins_a_cluster_its_own_window_already_holds() {
        // Both tracks are in window 5 and sound identical; the short one has
        // nowhere to go and stays a speaker of its own rather than merging.
        let vectors = vec![vec![1.0, 0.0], vec![1.0, 0.0]];
        let labels = cluster_tracks(&vectors, &[5, 5], &[true, false], ClusterOptions::default())
            .expect("clusters");
        assert_ne!(labels[0], labels[1]);
    }

    #[test]
    fn when_nothing_is_long_enough_to_fit_the_tracks_still_cluster() {
        let vectors = vec![vec![1.0, 0.0], vec![0.99, 0.14]];
        let labels = cluster_tracks(
            &vectors,
            &[0, 1],
            &[false, false],
            ClusterOptions::default(),
        )
        .expect("clusters");
        assert_eq!(labels[0], labels[1]);
    }

    #[test]
    fn a_frame_a_minority_of_windows_claims_is_not_speech() {
        // Four windows cover every frame; one of them hears a speaker.
        let tracks = vec![LocalTrack {
            window: 0,
            local_speaker: 0,
            window_base: 0,
            window_frames: 10,
            frames: (0..10).collect(),
        }];
        let coverage: Vec<f64> = (0..10)
            .map(|frame| 4.0 * hamming_weight(frame, 10))
            .collect();
        let turns = assemble_turns(&tracks, &[0], &coverage, &GRID, AssembleOptions::default());
        assert_eq!(turns, Vec::new());
    }

    #[test]
    fn a_frame_a_majority_of_windows_claims_becomes_a_turn_with_its_vote_as_confidence() {
        let tracks = vec![
            LocalTrack {
                window: 0,
                local_speaker: 0,
                window_base: 0,
                window_frames: 10,
                frames: (0..10).collect(),
            },
            LocalTrack {
                window: 1,
                local_speaker: 1,
                window_base: 0,
                window_frames: 10,
                frames: (0..10).collect(),
            },
        ];
        let coverage: Vec<f64> = (0..10)
            .map(|frame| 2.0 * hamming_weight(frame, 10))
            .collect();
        let turns = assemble_turns(
            &tracks,
            &[0, 0],
            &coverage,
            &GRID,
            AssembleOptions::default(),
        );
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].cluster.as_str(), "cluster-01");
        assert_eq!(turns[0].start_ms, GRID.start_ms(0));
        assert_eq!(turns[0].end_ms, GRID.end_ms(9));
        assert_eq!(turns[0].confidence, Some(1.0));
    }

    #[test]
    fn two_clusters_active_at_once_produce_two_overlapping_turns() {
        let tracks = vec![
            LocalTrack {
                window: 0,
                local_speaker: 0,
                window_base: 0,
                window_frames: 15,
                frames: (0..10).collect(),
            },
            LocalTrack {
                window: 0,
                local_speaker: 1,
                window_base: 0,
                window_frames: 15,
                frames: (5..15).collect(),
            },
        ];
        let coverage: Vec<f64> = (0..15).map(|frame| hamming_weight(frame, 15)).collect();
        let turns = assemble_turns(
            &tracks,
            &[0, 1],
            &coverage,
            &GRID,
            AssembleOptions::default(),
        );
        assert_eq!(turns.len(), 2);
        assert!(turns[0].overlap_ms(turns[1].start_ms, turns[1].end_ms) > 0);
        assert_eq!(turns[0].cluster.as_str(), "cluster-01");
        assert_eq!(turns[1].cluster.as_str(), "cluster-02");
    }

    #[test]
    fn a_short_gap_is_filled_and_a_short_turn_is_dropped() {
        let tracks = vec![LocalTrack {
            window: 0,
            local_speaker: 0,
            window_base: 0,
            window_frames: 61,
            frames: (0..10).chain(12..20).chain(60..61).collect(),
        }];
        let coverage: Vec<f64> = (0..61).map(|frame| hamming_weight(frame, 61)).collect();
        let options = AssembleOptions {
            activity_threshold: 0.5,
            min_duration_off_ms: 100,
            min_duration_on_ms: 100,
        };
        let turns = assemble_turns(&tracks, &[0], &coverage, &GRID, options);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].start_ms, GRID.start_ms(0));
        assert_eq!(turns[0].end_ms, GRID.end_ms(19));
    }

    #[test]
    fn frames_no_window_covered_are_silence_not_a_division_by_zero() {
        let tracks = vec![LocalTrack {
            window: 0,
            local_speaker: 0,
            window_base: 0,
            window_frames: 5,
            frames: (0..5).collect(),
        }];
        let coverage = vec![0.0f64; 5];
        let turns = assemble_turns(&tracks, &[0], &coverage, &GRID, AssembleOptions::default());
        assert_eq!(turns, Vec::new());
    }
}
