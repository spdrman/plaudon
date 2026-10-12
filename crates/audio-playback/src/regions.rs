//! Speaker regions on the timeline.
//!
//! The diarization tables give speaker turns as millisecond ranges that may
//! overlap, repeat a speaker, or arrive out of order. The waveform needs the
//! opposite: a sorted, non-overlapping band it can binary-search per frame,
//! with a stable colour per speaker so a speaker does not change colour when
//! a turn is added. Normalisation happens once, here.

use std::collections::BTreeMap;

use plaudon_domain::ids::SpeakerId;

use crate::time::{Micros, TimeSpan};

/// One speaker's stretch of the recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerRegion {
    /// The diarized speaker, or `None` for an unattributed stretch.
    pub speaker_id: Option<SpeakerId>,
    /// Display label, as the speaker registry resolved it.
    pub label: String,
    /// When the speaker holds the floor.
    pub span: TimeSpan,
}

impl SpeakerRegion {
    /// A region for `speaker_id` over `span`.
    #[must_use]
    pub fn new(speaker_id: Option<SpeakerId>, label: impl Into<String>, span: TimeSpan) -> Self {
        Self {
            speaker_id,
            label: label.into(),
            span,
        }
    }
}

/// Sorted, non-overlapping speaker regions with stable colour indices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpeakerRegions {
    regions: Vec<SpeakerRegion>,
    palette: BTreeMap<Option<SpeakerId>, usize>,
}

impl SpeakerRegions {
    /// Normalise arbitrary turns into a drawable band.
    ///
    /// Turns are sorted by start; an overlap truncates the earlier region at
    /// the later one's start (the newer attribution wins, which is what a
    /// re-run of diarization means); zero-length regions are dropped; and
    /// consecutive regions for the same speaker are merged so the waveform
    /// does not draw a seam inside one turn.
    #[must_use]
    pub fn from_turns(turns: impl IntoIterator<Item = SpeakerRegion>) -> Self {
        let mut regions: Vec<SpeakerRegion> = turns
            .into_iter()
            .filter(|region| !region.span.is_empty())
            .collect();
        regions.sort_by_key(|region| (region.span.start(), region.span.end()));

        let mut normalised: Vec<SpeakerRegion> = Vec::with_capacity(regions.len());
        for region in regions {
            if let Some(previous) = normalised.last_mut() {
                if previous.span.end() > region.span.start() {
                    previous.span = TimeSpan::new(previous.span.start(), region.span.start());
                }
                if previous.span.is_empty() {
                    normalised.pop();
                }
            }
            if let Some(previous) = normalised.last_mut() {
                if previous.speaker_id == region.speaker_id
                    && previous.label == region.label
                    && previous.span.end() >= region.span.start()
                {
                    previous.span = TimeSpan::new(previous.span.start(), region.span.end());
                    continue;
                }
            }
            normalised.push(region);
        }
        normalised.retain(|region| !region.span.is_empty());

        let mut palette = BTreeMap::new();
        for region in &normalised {
            let next = palette.len();
            palette.entry(region.speaker_id).or_insert(next);
        }
        Self {
            regions: normalised,
            palette,
        }
    }

    /// The normalised regions, in time order.
    #[must_use]
    pub fn as_slice(&self) -> &[SpeakerRegion] {
        &self.regions
    }

    /// Number of regions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    /// Whether there are no regions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// Distinct speakers, in first-appearance order.
    #[must_use]
    pub fn speaker_count(&self) -> usize {
        self.palette.len()
    }

    /// A stable index for a speaker's colour slot. Unknown speakers get the
    /// last slot, so adding a turn never recolours an existing speaker.
    #[must_use]
    pub fn color_index(&self, speaker_id: Option<SpeakerId>) -> usize {
        self.palette
            .get(&speaker_id)
            .copied()
            .unwrap_or(self.palette.len())
    }

    /// The region covering `time`, if any. `O(log n)`.
    #[must_use]
    pub fn at(&self, time: Micros) -> Option<&SpeakerRegion> {
        let index = self
            .regions
            .partition_point(|region| region.span.start() <= time);
        let candidate = self.regions.get(index.checked_sub(1)?)?;
        candidate.span.contains(time).then_some(candidate)
    }

    /// Every region intersecting `span`, as a contiguous slice. `O(log n)`
    /// plus the number of regions returned — never the total count.
    #[must_use]
    pub fn overlapping(&self, span: TimeSpan) -> &[SpeakerRegion] {
        if self.regions.is_empty() || span.is_empty() {
            return &[];
        }
        let first = self
            .regions
            .partition_point(|region| region.span.end() <= span.start());
        let last = self
            .regions
            .partition_point(|region| region.span.start() < span.end());
        self.regions.get(first..last).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(label: &str, start_ms: i64, end_ms: i64, id: Option<SpeakerId>) -> SpeakerRegion {
        SpeakerRegion::new(
            id,
            label,
            TimeSpan::new(Micros::from_millis(start_ms), Micros::from_millis(end_ms)),
        )
    }

    #[test]
    fn turns_are_sorted_and_overlaps_resolved_in_favour_of_the_later_turn() {
        let a = SpeakerId::new();
        let b = SpeakerId::new();
        let regions = SpeakerRegions::from_turns(vec![
            region("B", 400, 900, Some(b)),
            region("A", 0, 500, Some(a)),
        ]);
        assert_eq!(regions.len(), 2);
        assert_eq!(regions.as_slice()[0].span.end(), Micros::from_millis(400));
        assert_eq!(regions.as_slice()[1].span.start(), Micros::from_millis(400));
    }

    #[test]
    fn consecutive_turns_of_one_speaker_merge() {
        let a = SpeakerId::new();
        let regions = SpeakerRegions::from_turns(vec![
            region("A", 0, 500, Some(a)),
            region("A", 500, 900, Some(a)),
        ]);
        assert_eq!(regions.len(), 1);
        assert_eq!(
            regions.as_slice()[0].span.duration(),
            Micros::from_millis(900)
        );
    }

    #[test]
    fn empty_and_inverted_turns_are_dropped() {
        let a = SpeakerId::new();
        let regions = SpeakerRegions::from_turns(vec![
            region("A", 500, 500, Some(a)),
            region("A", 100, 200, Some(a)),
        ]);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions.as_slice()[0].span.start(), Micros::from_millis(100));
    }

    #[test]
    fn lookup_by_time_is_exact_at_the_boundaries() {
        let a = SpeakerId::new();
        let b = SpeakerId::new();
        let regions = SpeakerRegions::from_turns(vec![
            region("A", 0, 1_000, Some(a)),
            region("B", 1_000, 2_000, Some(b)),
        ]);
        assert_eq!(
            regions
                .at(Micros::from_millis(999))
                .map(|r| r.label.clone()),
            Some("A".into())
        );
        assert_eq!(
            regions
                .at(Micros::from_millis(1_000))
                .map(|r| r.label.clone()),
            Some("B".into())
        );
        assert!(regions.at(Micros::from_millis(2_000)).is_none());
        assert!(regions.at(Micros::from_millis(-1)).is_none());
    }

    #[test]
    fn overlapping_returns_only_the_visible_window() {
        let regions = SpeakerRegions::from_turns(
            (0..1_000)
                .map(|index| {
                    region(
                        "S",
                        index * 1_000,
                        index * 1_000 + 900,
                        Some(SpeakerId::new()),
                    )
                })
                .collect::<Vec<_>>(),
        );
        let window = TimeSpan::new(Micros::from_secs(10), Micros::from_secs(15));
        let visible = regions.overlapping(window);
        assert_eq!(visible.len(), 5);
        assert!(visible
            .iter()
            .all(|candidate| candidate.span.overlaps(window)));
    }

    #[test]
    fn colour_indices_are_stable_and_unknown_speakers_get_the_last_slot() {
        let a = SpeakerId::new();
        let b = SpeakerId::new();
        let regions = SpeakerRegions::from_turns(vec![
            region("A", 0, 1_000, Some(a)),
            region("B", 1_000, 2_000, Some(b)),
            region("A", 2_000, 3_000, Some(a)),
        ]);
        assert_eq!(regions.speaker_count(), 2);
        assert_ne!(regions.color_index(Some(a)), regions.color_index(Some(b)));
        assert_eq!(regions.color_index(Some(a)), regions.color_index(Some(a)));
        assert_eq!(regions.color_index(Some(SpeakerId::new())), 2);
    }

    #[test]
    fn an_empty_band_answers_every_query() {
        let regions = SpeakerRegions::default();
        assert!(regions.is_empty());
        assert!(regions.at(Micros::ZERO).is_none());
        assert_eq!(
            regions.overlapping(TimeSpan::new(Micros::ZERO, Micros::from_secs(1))),
            &[]
        );
    }
}
