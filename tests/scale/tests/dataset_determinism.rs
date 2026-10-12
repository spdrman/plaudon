//! The dataset must be reproducible, or no recorded measurement is.
//!
//! Spec 18.9 wants positive and negative coverage of the things a result
//! depends on. A benchmark number is only meaningful if the library it ran
//! against can be recreated exactly, so these tests pin the generator: same
//! seed, same bytes; different seed, different bytes; and the row contents the
//! benchmarks assert on are a pure function of `(seed, index, seq_index)`.

use std::collections::BTreeSet;

use plaudon_scale_tests::{DatasetSpec, Rng, SyntheticLibrary, Tier, SEARCH_NEEDLES};

fn small_spec(seed: u64) -> DatasetSpec {
    // Small enough to fingerprint repeatedly in a unit test, with the same
    // shape as the real tiers: many ordinary recordings plus one long one.
    DatasetSpec {
        tier: Tier::Ci,
        seed,
        recordings: 40,
        segments_per_recording: 6,
        marathon_segments: 900,
        blob_bytes: 256,
    }
}

#[test]
fn two_generations_from_one_seed_are_identical() {
    let spec = small_spec(0xD15EA5E);
    let first = SyntheticLibrary::new(spec).fingerprint();
    let second = SyntheticLibrary::new(spec).fingerprint();
    assert_eq!(first, second, "the generator is not deterministic");
    assert_eq!(first.len(), 64, "fingerprint must be a SHA-256 hex digest");
}

#[test]
fn a_different_seed_produces_a_different_library() {
    let a = SyntheticLibrary::new(small_spec(1)).fingerprint();
    let b = SyntheticLibrary::new(small_spec(2)).fingerprint();
    assert_ne!(a, b);
}

#[test]
fn a_different_size_produces_a_different_fingerprint() {
    let mut spec = small_spec(3);
    let baseline = SyntheticLibrary::new(spec).fingerprint();
    spec.marathon_segments += 1;
    assert_ne!(baseline, SyntheticLibrary::new(spec).fingerprint());
}

#[test]
fn rows_are_generated_independently_of_the_order_they_are_asked_for() {
    let library = SyntheticLibrary::new(small_spec(0xBEEF));
    let forward: Vec<_> = (0..10).map(|index| library.recording(index)).collect();
    let backward: Vec<_> = (0..10).rev().map(|index| library.recording(index)).collect();
    for (index, recording) in forward.iter().enumerate() {
        let mirrored = &backward[9 - index];
        assert_eq!(recording, mirrored);
    }

    // A windowed segment fetch must agree with a whole-transcript fetch, which
    // is what lets a 100k-segment transcript be streamed in batches.
    let marathon = library.recording(library.spec().marathon_index());
    let whole = library.segments_of(&marathon, 0, marathon.segment_count);
    let window = library.segments_of(&marathon, 500, 64);
    assert_eq!(window.len(), 64);
    assert_eq!(window.as_slice(), &whole[500..564]);
}

#[test]
fn identities_are_unique_across_the_library() {
    let library = SyntheticLibrary::new(small_spec(0xC0FFEE));
    let spec = *library.spec();
    let mut recording_ids = BTreeSet::new();
    let mut media_ids = BTreeSet::new();
    let mut digests = BTreeSet::new();
    for index in 0..=spec.marathon_index() {
        let recording = library.recording(index);
        assert!(recording_ids.insert(recording.id), "duplicate recording id");
        assert!(media_ids.insert(recording.media.id), "duplicate media id");
        assert!(
            digests.insert(recording.media.identity.sha256.to_hex()),
            "duplicate content hash would collapse two archive blobs into one"
        );
        assert_eq!(
            recording.media.identity.byte_length as usize,
            recording.media.bytes.len()
        );
    }

    let marathon = library.recording(spec.marathon_index());
    let segments = library.segments_of(&marathon, 0, 400);
    let segment_ids: BTreeSet<_> = segments.iter().map(|segment| segment.id).collect();
    assert_eq!(segment_ids.len(), segments.len(), "duplicate segment id");
}

#[test]
fn metadata_is_realistic_enough_for_the_queries_that_use_it() {
    let library = SyntheticLibrary::new(small_spec(0x5EED));
    let spec = *library.spec();
    let mut locales = BTreeSet::new();
    let mut sources = BTreeSet::new();
    for index in 0..spec.recordings {
        let recording = library.recording(index);
        assert!(!recording.title.is_empty());
        assert!(recording.captured_at < recording.created_at);
        assert!(recording.duration_ms > 0);
        locales.insert(recording.locale.as_str().to_owned());
        sources.insert(recording.source_kind);

        let segments = library.segments_of(&recording, 0, recording.segment_count);
        assert_eq!(segments.len(), recording.segment_count as usize);
        let mut previous_end = 0;
        for (position, segment) in segments.iter().enumerate() {
            assert_eq!(segment.seq_index as usize, position);
            assert!(segment.end_ms >= segment.start_ms);
            assert!(
                segment.start_ms >= previous_end,
                "segments must not overlap on the timeline"
            );
            previous_end = segment.end_ms;
            assert!(!segment.text.is_empty());
            assert!((0.0..=1.0).contains(&segment.confidence));
            if let Some(slot) = segment.speaker_slot {
                assert!(usize::from(slot) < recording.speakers.len());
            }
        }
    }
    assert!(locales.len() > 1, "a single-locale library tests nothing");
    assert!(sources.len() > 1, "provenance must vary across the library");
}

#[test]
fn search_needles_appear_exactly_where_the_generator_says_they_do() {
    let library = SyntheticLibrary::new(small_spec(0xA11CE));
    let spec = *library.spec();
    let mut counted = vec![0u64; SEARCH_NEEDLES.len()];
    for index in 0..=spec.marathon_index() {
        let recording = library.recording(index);
        for segment in library.segments_of(&recording, 0, recording.segment_count) {
            for (slot, needle) in SEARCH_NEEDLES.iter().enumerate() {
                if segment.text.contains(needle) {
                    counted[slot] += 1;
                    assert_eq!(
                        library.needle_for(index, segment.seq_index),
                        Some(*needle),
                        "a needle appeared in text the injection rule did not place it in"
                    );
                }
            }
        }
    }
    for (slot, needle) in SEARCH_NEEDLES.iter().enumerate() {
        assert_eq!(
            counted[slot],
            library.expected_needle_matches(needle),
            "predicted match count for `{needle}` disagrees with the generated text"
        );
        assert!(
            counted[slot] > 0,
            "`{needle}` never occurs, so the search benchmark would measure an empty result"
        );
    }
}

#[test]
fn the_prng_is_reproducible_and_bounded() {
    let mut a = Rng::seeded(42);
    let mut b = Rng::seeded(42);
    let mut c = Rng::seeded(43);
    let first: Vec<u64> = (0..16).map(|_| a.next_u64()).collect();
    let second: Vec<u64> = (0..16).map(|_| b.next_u64()).collect();
    assert_eq!(first, second);
    assert_ne!(first, (0..16).map(|_| c.next_u64()).collect::<Vec<_>>());

    let mut rng = Rng::seeded(7);
    let mut seen = BTreeSet::new();
    for _ in 0..4_000 {
        let value = rng.below(10);
        assert!(value < 10);
        seen.insert(value);
        let ranged = rng.range(5, 9);
        assert!((5..=9).contains(&ranged));
    }
    assert_eq!(seen.len(), 10, "`below` must reach every value in its range");
    assert_eq!(Rng::seeded(0).next_u64(), Rng::seeded(0).next_u64());
}

#[test]
fn the_declared_tiers_match_the_sizes_the_spec_asks_for() {
    let ci = DatasetSpec::ci(1);
    assert_eq!(ci.tier, Tier::Ci);
    assert!(ci.total_segments() >= 5_000, "the CI tier must still be a scale test");

    let full = DatasetSpec::full(1);
    assert_eq!(full.recordings, 10_000, "spec 17.5 asks for 10,000 recordings");
    assert_eq!(
        full.marathon_segments, 100_000,
        "spec 18.6A's UI row is a 100,000-segment transcript"
    );
    assert!(
        full.total_segments() >= 100_000,
        "spec 17.5 asks for at least 100,000 transcript segments, got {}",
        full.total_segments()
    );
    assert_eq!(full.total_recordings(), 10_001);
    assert_eq!(Tier::parse("full").unwrap(), Tier::Full);
    assert_eq!(Tier::parse("CI").unwrap(), Tier::Ci);
    assert!(Tier::parse("huge").is_err(), "an unknown tier must not be guessed");
}
