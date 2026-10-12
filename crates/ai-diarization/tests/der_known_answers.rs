//! DER known-answer tests against hand-calculated fixtures (spec 17.4).

use plaudon_ai_diarization::contract::SpeakerTurn;
use plaudon_ai_diarization::der::{diarization_error_rate, DerOptions, ReferenceTurn};
use serde::Deserialize;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct FixtureExpected {
    der: f64,
    missed_ms: u64,
    false_alarm_ms: u64,
    confusion_ms: u64,
    total_reference_ms: u64,
    overlapping_reference_ms: u64,
}

#[derive(Debug, Deserialize)]
struct FixtureTurn {
    #[serde(default)]
    speaker: Option<String>,
    #[serde(default)]
    cluster: Option<String>,
    start_ms: u64,
    end_ms: u64,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    provenance: String,
    /// Present in every fixture file; the metric itself derives duration from
    /// the turns, so this is only read back when a fixture is hand-checked.
    #[allow(dead_code)]
    duration_ms: u64,
    reference: Vec<FixtureTurn>,
    hypothesis: Vec<FixtureTurn>,
    expected: FixtureExpected,
}

fn load_fixture(path: &Path) -> Fixture {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", path.display(), e));
    serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {}", path.display(), e))
}

fn verify_fixture(fixture: &Fixture) {
    let reference: Vec<ReferenceTurn> = fixture
        .reference
        .iter()
        .map(|t| {
            ReferenceTurn::new(
                t.speaker.as_ref().expect("reference needs speaker"),
                t.start_ms,
                t.end_ms,
            )
        })
        .collect();

    let hypothesis: Vec<SpeakerTurn> = fixture
        .hypothesis
        .iter()
        .map(|t| {
            SpeakerTurn::new(
                t.cluster.as_ref().expect("hypothesis needs cluster"),
                t.start_ms,
                t.end_ms,
            )
        })
        .collect();

    let report = diarization_error_rate(&reference, &hypothesis, DerOptions::default());

    // Verify all expected fields
    assert_eq!(
        report.der, fixture.expected.der,
        "{}: DER mismatch",
        fixture.name
    );
    assert_eq!(
        report.missed_ms, fixture.expected.missed_ms,
        "{}: missed_ms mismatch",
        fixture.name
    );
    assert_eq!(
        report.false_alarm_ms, fixture.expected.false_alarm_ms,
        "{}: false_alarm_ms mismatch",
        fixture.name
    );
    assert_eq!(
        report.confusion_ms, fixture.expected.confusion_ms,
        "{}: confusion_ms mismatch",
        fixture.name
    );
    assert_eq!(
        report.total_reference_ms, fixture.expected.total_reference_ms,
        "{}: total_reference_ms mismatch",
        fixture.name
    );
    assert_eq!(
        report.overlapping_reference_ms, fixture.expected.overlapping_reference_ms,
        "{}: overlapping_reference_ms mismatch",
        fixture.name
    );

    // Verify components sum to reported rate
    if report.total_reference_ms > 0 {
        let computed_der = (report.missed_ms + report.false_alarm_ms + report.confusion_ms) as f64
            / report.total_reference_ms as f64;
        assert!(
            (computed_der - report.der).abs() < 1e-9,
            "{}: components don't sum to DER: {} vs {}",
            fixture.name,
            computed_der,
            report.der
        );
    }
}

#[test]
fn two_speaker_clean_matches_hand_calculation() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/diarization/two-speaker-clean.json");
    let fixture = load_fixture(&fixture_path);
    assert_eq!(
        fixture.provenance,
        "Synthetic annotation authored for the plaudon-m repository"
    );
    verify_fixture(&fixture);

    // Verify no overlapping speech
    let reference: Vec<ReferenceTurn> = fixture
        .reference
        .iter()
        .map(|t| ReferenceTurn::new(t.speaker.as_ref().unwrap(), t.start_ms, t.end_ms))
        .collect();
    let hypothesis: Vec<SpeakerTurn> = fixture
        .hypothesis
        .iter()
        .map(|t| SpeakerTurn::new(t.cluster.as_ref().unwrap(), t.start_ms, t.end_ms))
        .collect();
    let report = diarization_error_rate(&reference, &hypothesis, DerOptions::default());
    assert!(
        !report.covers_overlapping_speech(),
        "two-speaker-clean should not cover overlapping speech"
    );
}

#[test]
fn three_speaker_overlap_matches_hand_calculation() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/diarization/three-speaker-overlap.json");
    let fixture = load_fixture(&fixture_path);
    assert_eq!(
        fixture.provenance,
        "Synthetic annotation authored for the plaudon-m repository"
    );
    verify_fixture(&fixture);

    // Verify overlapping speech is detected
    let reference: Vec<ReferenceTurn> = fixture
        .reference
        .iter()
        .map(|t| ReferenceTurn::new(t.speaker.as_ref().unwrap(), t.start_ms, t.end_ms))
        .collect();
    let hypothesis: Vec<SpeakerTurn> = fixture
        .hypothesis
        .iter()
        .map(|t| SpeakerTurn::new(t.cluster.as_ref().unwrap(), t.start_ms, t.end_ms))
        .collect();
    let report = diarization_error_rate(&reference, &hypothesis, DerOptions::default());
    assert!(
        report.covers_overlapping_speech(),
        "three-speaker-overlap should cover overlapping speech"
    );
    assert_eq!(
        report.overlapping_reference_ms, 1000,
        "Expected 1000ms of overlapping speech"
    );
}

#[test]
fn single_speaker_silence_matches_hand_calculation() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/diarization/single-speaker-silence.json");
    let fixture = load_fixture(&fixture_path);
    assert_eq!(
        fixture.provenance,
        "Synthetic annotation authored for the plaudon-m repository"
    );
    verify_fixture(&fixture);

    // Verify false alarms in silence
    assert_eq!(
        fixture.expected.false_alarm_ms, 1000,
        "Expected 1000ms of false alarms"
    );
    assert_eq!(
        fixture.expected.der, 1.0,
        "Expected DER of 1.0 (100% false alarms)"
    );
}
