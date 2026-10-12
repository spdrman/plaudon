//! Mac-class compatibility gates and their evidence (task H2.4, spec 11.5).
//!
//! Spec 11.5 names three cells that must be tested: a supported Apple Silicon
//! Mac with the requisite OS and models, an Apple Silicon Mac with Apple
//! Intelligence disabled or unready, and an Intel or otherwise unsupported
//! Mac. DoD 22(17) adds the rule that makes the matrix worth keeping: a
//! macOS compatibility claim must be backed by a named real-hardware test,
//! and an untested combination is labelled Experimental or Unsupported,
//! never Supported.
//!
//! This module is the mechanism for that rule. It is pure and hermetic: the
//! default test run exercises the labelling logic against recorded evidence
//! records and never touches hardware. The opt-in suite under
//! `tests/hardware/` produces the records.

use std::collections::BTreeMap;

use plaudon_domain::capability::CapabilityStatus;
use serde::{Deserialize, Serialize};

use crate::probe::AppleAvailability;

/// The hardware and configuration classes spec 11.5 requires us to test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MacClass {
    /// Apple Silicon with the requisite OS and Apple Intelligence ready.
    AppleSiliconReady,
    /// Apple Silicon with Apple Intelligence disabled, unready or ineligible.
    AppleSiliconUnready,
    /// An Intel Mac, or any Mac the platform declares ineligible.
    IntelOrUnsupported,
}

impl MacClass {
    /// Every class, in the order spec 11.5 lists them.
    pub const ALL: [Self; 3] = [
        Self::AppleSiliconReady,
        Self::AppleSiliconUnready,
        Self::IntelOrUnsupported,
    ];

    /// Stable identifier used in the evidence bundle and the register.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AppleSiliconReady => "apple_silicon_ready",
            Self::AppleSiliconUnready => "apple_silicon_unready",
            Self::IntelOrUnsupported => "intel_or_unsupported",
        }
    }

    /// What the interface tells the user about this class.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::AppleSiliconReady => {
                "Apple Silicon Mac with the required macOS and Apple Intelligence ready"
            }
            Self::AppleSiliconUnready => {
                "Apple Silicon Mac with Apple Intelligence disabled or still preparing"
            }
            Self::IntelOrUnsupported => "Intel Mac, or a Mac the platform declares ineligible",
        }
    }

    /// Classify a live probe snapshot.
    ///
    /// Classification reads the platform's own verdicts. An Intel Mac is not
    /// identified by parsing a CPU string for the word "Intel" — a rename
    /// would silently reclassify every host — but by the platform reporting
    /// the device ineligible or the frameworks absent.
    #[must_use]
    pub fn classify(availability: &AppleAvailability) -> Self {
        use crate::envelope::BridgeErrorCode as Code;

        let text_code = availability
            .text_model
            .as_ref()
            .err()
            .map(|e| e.code.clone());
        let speech_code = availability.speech.as_ref().err().map(|e| e.code.clone());

        // The platform refusing speech outright is the clearest statement
        // that this machine is outside the feature's hardware or OS range.
        if matches!(
            speech_code,
            Some(Code::PlatformUnsupported | Code::HardwareUnsupported)
        ) {
            return Self::IntelOrUnsupported;
        }
        if availability
            .text_model
            .as_ref()
            .is_ok_and(|probe| probe.available)
        {
            return Self::AppleSiliconReady;
        }
        if matches!(
            text_code,
            Some(Code::PlatformUnsupported | Code::HardwareUnsupported)
        ) {
            return Self::IntelOrUnsupported;
        }
        // Speech answered but the language model did not: the machine is
        // eligible and Apple Intelligence is off, unready or still preparing.
        Self::AppleSiliconUnready
    }
}

/// How strongly a cell is evidenced (spec 1.6, 2.5B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLevel {
    /// No record exists for this cell. Also the decoding of any level string
    /// this build does not recognize, so an invented level can never outrank
    /// a real one.
    #[default]
    None,
    /// Only a vendor document or an inference supports it.
    Documented,
    /// A hermetic fixture reproduces the behaviour, with no real hardware.
    Simulated,
    /// A named real-hardware run is archived with build and date.
    RealHardware,
}

impl<'de> Deserialize<'de> for EvidenceLevel {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Written by hand rather than with `#[serde(other)]`, which requires
        // the fallback to be the last variant and would invert the ordering
        // that makes `max()` mean "strongest evidence".
        let raw = String::deserialize(deserializer)?;
        Ok(match raw.as_str() {
            "documented" => Self::Documented,
            "simulated" => Self::Simulated,
            "real_hardware" => Self::RealHardware,
            _ => Self::None,
        })
    }
}

impl EvidenceLevel {
    /// Stable identifier used in the evidence bundle.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Documented => "documented",
            Self::Simulated => "simulated",
            Self::RealHardware => "real_hardware",
        }
    }
}

/// Comparison of this engine against whisper.cpp on a shared fixture
/// (spec 17.1, 17.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonMetrics {
    /// Fixture the comparison ran on.
    pub fixture: String,
    /// Fixture media duration.
    pub audio_duration_ms: u64,
    /// Wall-clock time the Apple engine took.
    pub apple_elapsed_ms: u64,
    /// Wall-clock time whisper.cpp took, when it was run.
    pub whisper_elapsed_ms: Option<u64>,
    /// SHA-256 of the Apple transcript text, so a regression is detectable.
    pub apple_text_sha256: String,
    /// SHA-256 of the whisper.cpp transcript text, when it was run.
    pub whisper_text_sha256: Option<String>,
    /// Normalized token-level agreement in `[0, 1]`, when both engines ran.
    ///
    /// A similarity figure, not an accuracy figure: neither engine is ground
    /// truth, and spec 1.5 forbids claiming accuracy without a reference
    /// corpus.
    pub agreement: Option<f64>,
}

/// One archived observation about one cell.
///
/// The booleans are independent observations rather than a state machine:
/// a run can transcribe without generating, generate without transcribing,
/// or fall back while doing neither, and collapsing them into an enum would
/// make the impossible combinations the representable ones.
///
/// Spec 2.5B's register schema in miniature: what ran, where, when, and what
/// happened. A cell is only ever as strong as the record behind it.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    /// Class the record describes.
    pub class: MacClass,
    /// Identifier of the run that produced it.
    pub test_run_id: String,
    /// RFC 3339 timestamp of the run.
    pub observed_at: String,
    /// OS build string the host reported.
    pub os_build: String,
    /// OS marketing version.
    pub os_version: String,
    /// Hardware string the host reported.
    pub hardware: String,
    /// Whether Apple Intelligence was ready during the run.
    pub apple_intelligence_ready: bool,
    /// Locale the run used.
    pub locale: String,
    /// Asset state observed for that locale.
    pub asset_status: String,
    /// Whether speech transcription produced a finalized transcript.
    pub speech_transcribed: bool,
    /// Whether the on-device text model produced a schema-valid document.
    pub text_generated: bool,
    /// Whether the run fell back to whisper.cpp, and succeeded there.
    pub fell_back_to_whisper: bool,
    /// Whether any network request was observed during the run.
    pub network_requests: u32,
    /// How strong the record is.
    pub evidence_level: EvidenceLevel,
    /// Comparison against whisper.cpp, when the harness ran both.
    #[serde(default)]
    pub comparison: Option<ComparisonMetrics>,
    /// Known limitations recorded by the operator.
    #[serde(default)]
    pub known_limitations: Vec<String>,
}

impl EvidenceRecord {
    /// Whether this record can support a `Supported` label.
    ///
    /// Four conditions, all necessary: the record is from real hardware, the
    /// feature actually produced output, nothing left the machine, and the
    /// class is the one where the feature is supposed to work.
    #[must_use]
    pub fn supports_a_supported_label(&self) -> bool {
        self.evidence_level == EvidenceLevel::RealHardware
            && self.class == MacClass::AppleSiliconReady
            && self.speech_transcribed
            && self.network_requests == 0
    }

    /// Whether this record proves graceful degradation for its class.
    ///
    /// The degradation claim is not "Apple was unavailable"; it is "Apple was
    /// unavailable *and the recording was still transcribed locally, with no
    /// cloud dispatch*" (spec 22(4), E2E 18.7(10)).
    #[must_use]
    pub fn proves_graceful_degradation(&self) -> bool {
        matches!(
            self.class,
            MacClass::AppleSiliconUnready | MacClass::IntelOrUnsupported
        ) && self.fell_back_to_whisper
            && self.network_requests == 0
    }
}

/// The label one cell carries in the interface and the release bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompatibilityCell {
    /// Class the label applies to.
    pub class: MacClass,
    /// Status the evidence justifies.
    pub status: CapabilityStatus,
    /// Strongest evidence level behind the label.
    pub evidence_level: EvidenceLevel,
    /// Why the label is what it is, in one sentence the interface can show.
    pub explanation: String,
    /// Identifiers of the runs the label rests on.
    pub test_run_ids: Vec<String>,
}

/// Derive the label a cell deserves from its records.
///
/// The only path to `Supported` is a real-hardware record that produced
/// output with no network traffic. Everything else is `Experimental` when
/// something was observed and `Unsupported` when the platform refused; the
/// absence of any record is `Unknown`, which is not selectable either.
#[must_use]
pub fn label_for(class: MacClass, records: &[EvidenceRecord]) -> CompatibilityCell {
    let mine: Vec<&EvidenceRecord> = records.iter().filter(|r| r.class == class).collect();
    let test_run_ids: Vec<String> = mine.iter().map(|r| r.test_run_id.clone()).collect();
    let evidence_level = mine
        .iter()
        .map(|r| r.evidence_level)
        .max()
        .unwrap_or(EvidenceLevel::None);

    if mine.is_empty() {
        return CompatibilityCell {
            class,
            status: CapabilityStatus::Unknown,
            evidence_level,
            explanation: format!(
                "No test record exists for {}; the combination is untested.",
                class.description()
            ),
            test_run_ids,
        };
    }

    match class {
        MacClass::AppleSiliconReady => {
            if mine
                .iter()
                .any(|record| record.supports_a_supported_label())
            {
                CompatibilityCell {
                    class,
                    status: CapabilityStatus::Supported,
                    evidence_level,
                    explanation:
                        "Apple native speech produced a finalized transcript on real hardware \
                         with no network request."
                            .to_owned(),
                    test_run_ids,
                }
            } else if evidence_level >= EvidenceLevel::Simulated {
                CompatibilityCell {
                    class,
                    status: CapabilityStatus::Experimental,
                    evidence_level,
                    explanation:
                        "Only simulated or incomplete evidence exists for this combination; a \
                         real-hardware run is required before it is advertised as supported."
                            .to_owned(),
                    test_run_ids,
                }
            } else {
                CompatibilityCell {
                    class,
                    status: CapabilityStatus::Unknown,
                    evidence_level,
                    explanation: "Only documentation supports this combination.".to_owned(),
                    test_run_ids,
                }
            }
        }
        MacClass::AppleSiliconUnready | MacClass::IntelOrUnsupported => {
            // Apple AI is *expected* to be unavailable here. The cell is
            // Unsupported for Apple AI, and the question the evidence answers
            // is whether the product still works.
            let degrades = mine
                .iter()
                .any(|record| record.proves_graceful_degradation());
            CompatibilityCell {
                class,
                status: CapabilityStatus::Unsupported,
                evidence_level,
                explanation: if degrades {
                    format!(
                        "Apple native AI is unavailable on {}; transcription completed locally \
                         through whisper.cpp with no network request.",
                        class.description()
                    )
                } else {
                    format!(
                        "Apple native AI is unavailable on {}, and no record yet shows the \
                         local whisper.cpp fallback completing there.",
                        class.description()
                    )
                },
                test_run_ids,
            }
        }
    }
}

/// The full matrix, as the release evidence bundle records it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompatibilityMatrix {
    /// One cell per class spec 11.5 names.
    pub cells: Vec<CompatibilityCell>,
    /// Every record the cells were derived from.
    pub records: Vec<EvidenceRecord>,
}

impl CompatibilityMatrix {
    /// Build the matrix from a set of records.
    #[must_use]
    pub fn from_records(records: Vec<EvidenceRecord>) -> Self {
        let cells = MacClass::ALL
            .iter()
            .map(|class| label_for(*class, &records))
            .collect();
        Self { cells, records }
    }

    /// The cell for one class.
    #[must_use]
    pub fn cell(&self, class: MacClass) -> Option<&CompatibilityCell> {
        self.cells.iter().find(|cell| cell.class == class)
    }

    /// Whether the release gate passes.
    ///
    /// Two conditions, from DoD 22(4) and 22(17): every class has at least
    /// one record, and both degradation classes have a record showing the
    /// local fallback completing. A `Supported` ready cell is *not* required
    /// — spec 1.6 keeps Apple native AI conditional — but claiming it without
    /// real hardware is, which [`label_for`] already prevents.
    #[must_use]
    pub fn gate_failures(&self) -> Vec<String> {
        let mut failures = Vec::new();
        for class in MacClass::ALL {
            let Some(cell) = self.cell(class) else {
                failures.push(format!("{}: no cell", class.as_str()));
                continue;
            };
            if cell.test_run_ids.is_empty() {
                failures.push(format!(
                    "{}: no named test record; spec 22(17) forbids an unevidenced claim",
                    class.as_str()
                ));
            }
            if matches!(
                class,
                MacClass::AppleSiliconUnready | MacClass::IntelOrUnsupported
            ) && !self
                .records
                .iter()
                .filter(|record| record.class == class)
                .any(EvidenceRecord::proves_graceful_degradation)
            {
                failures.push(format!(
                    "{}: no record shows whisper.cpp completing locally with zero network \
                     requests; spec 22(4) requires graceful degradation",
                    class.as_str()
                ));
            }
            if cell.status == CapabilityStatus::Supported
                && cell.evidence_level != EvidenceLevel::RealHardware
            {
                failures.push(format!(
                    "{}: labelled Supported without a real-hardware record",
                    class.as_str()
                ));
            }
        }
        failures
    }

    /// Render for the release evidence bundle (spec 24).
    #[must_use]
    pub fn to_bundle(&self) -> serde_json::Value {
        let cells: BTreeMap<&str, serde_json::Value> = self
            .cells
            .iter()
            .map(|cell| {
                (
                    cell.class.as_str(),
                    serde_json::json!({
                        "status": cell.status,
                        "evidence_level": cell.evidence_level.as_str(),
                        "explanation": cell.explanation,
                        "test_run_ids": cell.test_run_ids,
                    }),
                )
            })
            .collect();
        serde_json::json!({
            "component": "apple-native-ai",
            "spec_section": "11.5",
            "cells": cells,
            "records": self.records,
            "gate_failures": self.gate_failures(),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::bridge::{failure_document, success_document, FakeBridge};
    use crate::envelope::BridgeOp;
    use crate::probe::{probe_once, PROBE_BUDGET};

    use super::*;

    fn record(class: MacClass, level: EvidenceLevel) -> EvidenceRecord {
        EvidenceRecord {
            class,
            test_run_id: format!("run-{}", class.as_str()),
            observed_at: "2026-10-11T20:00:00Z".to_owned(),
            os_build: "26A434".to_owned(),
            os_version: "27.0.1".to_owned(),
            hardware: "Apple M4 Pro".to_owned(),
            apple_intelligence_ready: class == MacClass::AppleSiliconReady,
            locale: "en-US".to_owned(),
            asset_status: "installed".to_owned(),
            speech_transcribed: class == MacClass::AppleSiliconReady,
            text_generated: class == MacClass::AppleSiliconReady,
            fell_back_to_whisper: class != MacClass::AppleSiliconReady,
            network_requests: 0,
            evidence_level: level,
            comparison: None,
            known_limitations: Vec::new(),
        }
    }

    #[test]
    fn an_untested_cell_is_unknown_and_never_supported() {
        let cell = label_for(MacClass::AppleSiliconReady, &[]);
        assert_eq!(cell.status, CapabilityStatus::Unknown);
        assert_eq!(cell.evidence_level, EvidenceLevel::None);
        assert_eq!(cell.test_run_ids, Vec::<String>::new());
        assert!(cell.explanation.contains("untested"));
    }

    #[test]
    fn simulated_evidence_can_only_reach_experimental() {
        let records = vec![record(
            MacClass::AppleSiliconReady,
            EvidenceLevel::Simulated,
        )];
        let cell = label_for(MacClass::AppleSiliconReady, &records);
        assert_eq!(cell.status, CapabilityStatus::Experimental);
        assert_eq!(cell.test_run_ids, ["run-apple_silicon_ready"]);
    }

    #[test]
    fn documentation_alone_never_reaches_experimental() {
        let records = vec![record(
            MacClass::AppleSiliconReady,
            EvidenceLevel::Documented,
        )];
        assert_eq!(
            label_for(MacClass::AppleSiliconReady, &records).status,
            CapabilityStatus::Unknown
        );
    }

    #[test]
    fn real_hardware_with_output_and_no_network_reaches_supported() {
        let records = vec![record(
            MacClass::AppleSiliconReady,
            EvidenceLevel::RealHardware,
        )];
        let cell = label_for(MacClass::AppleSiliconReady, &records);
        assert_eq!(cell.status, CapabilityStatus::Supported);
        assert_eq!(cell.evidence_level, EvidenceLevel::RealHardware);
    }

    #[test]
    fn a_real_hardware_run_that_made_a_network_request_is_not_supported() {
        let mut only = record(MacClass::AppleSiliconReady, EvidenceLevel::RealHardware);
        only.network_requests = 1;
        let cell = label_for(MacClass::AppleSiliconReady, &[only]);
        assert_eq!(cell.status, CapabilityStatus::Experimental);
    }

    #[test]
    fn a_real_hardware_run_that_produced_nothing_is_not_supported() {
        let mut only = record(MacClass::AppleSiliconReady, EvidenceLevel::RealHardware);
        only.speech_transcribed = false;
        assert_eq!(
            label_for(MacClass::AppleSiliconReady, &[only]).status,
            CapabilityStatus::Experimental
        );
    }

    #[test]
    fn the_degradation_classes_are_unsupported_but_must_still_work() {
        for class in [MacClass::AppleSiliconUnready, MacClass::IntelOrUnsupported] {
            let cell = label_for(class, &[record(class, EvidenceLevel::RealHardware)]);
            assert_eq!(cell.status, CapabilityStatus::Unsupported);
            assert!(cell.explanation.contains("whisper.cpp"));
            assert!(cell.explanation.contains("no network request"));
        }
    }

    #[test]
    fn a_degradation_class_without_a_fallback_record_says_so() {
        let mut only = record(MacClass::IntelOrUnsupported, EvidenceLevel::RealHardware);
        only.fell_back_to_whisper = false;
        let cell = label_for(MacClass::IntelOrUnsupported, &[only]);
        assert!(cell.explanation.contains("no record yet shows"));
    }

    #[test]
    fn the_gate_fails_until_every_class_has_a_record() {
        let matrix = CompatibilityMatrix::from_records(vec![record(
            MacClass::AppleSiliconReady,
            EvidenceLevel::RealHardware,
        )]);
        let failures = matrix.gate_failures();
        assert!(failures.iter().any(|f| f.contains("apple_silicon_unready")));
        assert!(failures.iter().any(|f| f.contains("intel_or_unsupported")));
        assert!(!failures.iter().any(|f| f.contains("apple_silicon_ready:")));
    }

    #[test]
    fn a_complete_matrix_passes_the_gate() {
        let records = MacClass::ALL
            .iter()
            .map(|class| record(*class, EvidenceLevel::RealHardware))
            .collect();
        let matrix = CompatibilityMatrix::from_records(records);
        let failures = matrix.gate_failures();
        assert!(failures.is_empty(), "{failures:?}");
        let bundle = matrix.to_bundle();
        assert_eq!(bundle["component"], "apple-native-ai");
        assert_eq!(
            bundle["cells"]["apple_silicon_ready"]["status"],
            "supported"
        );
        assert_eq!(
            bundle["cells"]["intel_or_unsupported"]["status"],
            "unsupported"
        );
    }

    #[test]
    fn a_simulated_only_matrix_never_claims_support() {
        let records = MacClass::ALL
            .iter()
            .map(|class| record(*class, EvidenceLevel::Simulated))
            .collect();
        let matrix = CompatibilityMatrix::from_records(records);
        assert_ne!(
            matrix.cell(MacClass::AppleSiliconReady).unwrap().status,
            CapabilityStatus::Supported
        );
    }

    #[test]
    fn classification_reads_the_platform_verdict_not_a_cpu_string() {
        let ready = probe_once(
            &FakeBridge::new().with_default(|op| match op {
                BridgeOp::ProbeSpeech => success_document(
                    op,
                    serde_json::json!({
                        "available": true,
                        "is_transcriber": true,
                        "supports_on_device": true,
                        "asset_status": "installed",
                        "installed_locales": ["en_US"],
                        "hardware": "Apple M4 Pro",
                    }),
                ),
                _ => success_document(
                    op,
                    serde_json::json!({ "available": true, "is_transcriber": false }),
                ),
            }),
            Some("en-US"),
            PROBE_BUDGET,
        );
        assert_eq!(MacClass::classify(&ready), MacClass::AppleSiliconReady);

        let unready = probe_once(
            &FakeBridge::new().with_default(|op| match op {
                BridgeOp::ProbeSpeech => success_document(
                    op,
                    serde_json::json!({
                        "available": true,
                        "is_transcriber": true,
                        "supports_on_device": true,
                        "asset_status": "installed",
                    }),
                ),
                _ => failure_document(
                    op,
                    "disabled_by_user",
                    Some("apple_intelligence_not_enabled"),
                ),
            }),
            None,
            PROBE_BUDGET,
        );
        assert_eq!(MacClass::classify(&unready), MacClass::AppleSiliconUnready);

        let unsupported = probe_once(
            &FakeBridge::new()
                .with_default(|op| failure_document(op, "hardware_unsupported", Some("x"))),
            None,
            PROBE_BUDGET,
        );
        assert_eq!(
            MacClass::classify(&unsupported),
            MacClass::IntelOrUnsupported
        );

        let absent = probe_once(&crate::UnlinkedBridge, None, PROBE_BUDGET);
        assert_eq!(MacClass::classify(&absent), MacClass::IntelOrUnsupported);
    }

    #[test]
    fn evidence_levels_order_from_weakest_to_strongest() {
        assert!(EvidenceLevel::None < EvidenceLevel::Documented);
        assert!(EvidenceLevel::Documented < EvidenceLevel::Simulated);
        assert!(EvidenceLevel::Simulated < EvidenceLevel::RealHardware);
        let decoded: EvidenceLevel = serde_json::from_str("\"gut_feeling\"").unwrap();
        assert_eq!(decoded, EvidenceLevel::None);
    }

    #[test]
    fn a_record_round_trips_through_the_bundle_format() {
        let records = vec![EvidenceRecord {
            comparison: Some(ComparisonMetrics {
                fixture: "speech-7s.wav".to_owned(),
                audio_duration_ms: 7_636,
                apple_elapsed_ms: 1_210,
                whisper_elapsed_ms: Some(4_800),
                apple_text_sha256: "a".repeat(64),
                whisper_text_sha256: Some("b".repeat(64)),
                agreement: Some(0.93),
            }),
            ..record(MacClass::AppleSiliconReady, EvidenceLevel::RealHardware)
        }];
        let matrix = CompatibilityMatrix::from_records(records);
        let encoded = serde_json::to_string(&matrix).unwrap();
        let decoded: CompatibilityMatrix = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, matrix);
        assert_eq!(
            decoded.records[0].comparison.as_ref().unwrap().agreement,
            Some(0.93)
        );
    }
}
