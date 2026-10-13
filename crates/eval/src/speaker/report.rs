//! The committed speaker-matching artifact (VP-180, ADR-0081 §4).
//!
//! ADR-0081's gate names one file per model —
//! `docs/benchmarks/speaker-match-<model>-eer.json` — alongside the
//! diarization DER artifact, and says plainly that "single-machine research
//! measurements are not release evidence". This type is that file, and it is
//! built so the distinction survives contact with a reader:
//!
//! * [`ReleaseEvidence`] is computed, never asserted. A report built from
//!   scripted vectors, or without the runtime figures the gate requires,
//!   carries its blockers in the document itself.
//! * The numbers are per condition (VP-184) and carry their sample sizes, so a
//!   rate nobody can resolve cannot be quoted as a rate.
//! * **No embedding is in it.** The document holds ids, counts, scores and
//!   rates. VP-002 and VP-112 forbid an embedding in a log or an export, and a
//!   file committed to a public repository is the strongest form of both.
//!
//! The JSON Schema is `schemas/speaker-match-report.v1.schema.json`, pinned by
//! a content hash in `crates/eval/tests/speaker_report_schema.rs` exactly like
//! the evaluation report's.

use plaudon_app_core::voice_profiles::matching::Band;
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::time::Timestamp;
use serde::{Deserialize, Serialize};

use super::calibration::ThresholdCalibration;
use super::det::{EqualErrorRate, OperatingPoint};
use super::embeddings::SourceKind;
use super::fixtures::{AttributionRow, Condition};

/// Schema version of the artifact this build writes and reads.
pub const SPEAKER_MATCH_REPORT_SCHEMA_VERSION: u32 = 1;

/// Checkpoint the scores are bound to (VP-183: pinned to the model sha256).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRecord {
    /// Model identifier, e.g. `wespeaker-resnet34-lm`.
    pub model_id: String,
    /// Checkpoint version string.
    pub model_version: String,
    /// Checkpoint digest, lowercase hex.
    pub model_sha256: String,
    /// Embedding dimension.
    pub dim: usize,
    /// How the embeddings were obtained.
    pub embedding_source: SourceKind,
}

/// Runtime footprint the ADR-0081 §4 gate requires, when it was measured.
///
/// Every field is optional and nothing is derived: an unmeasured platform is
/// `null`, never an estimate. The presence of these figures is one of the
/// conditions [`ReleaseEvidence`] checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeRecord {
    /// Platform token, e.g. `macos-arm64` or `windows-x64`.
    pub platform: String,
    /// CPU description as reported by the operator.
    pub host_cpu: Option<String>,
    /// Peak resident set size during the run.
    pub peak_rss_bytes: Option<u64>,
    /// Time from process start to first embedding.
    pub startup_ms: Option<u64>,
    /// Real-time factor of embedding extraction.
    pub real_time_factor: Option<f64>,
}

/// Whether this artifact may be cited as ADR-0081 §4 release evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseEvidence {
    /// True only when no blocker remains.
    pub qualifies: bool,
    /// What is missing, in review order. Empty when `qualifies`.
    pub blockers: Vec<String>,
}

/// Which fixtures were measured, and what they oblige us to attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureSummary {
    /// Library identifier.
    pub library_id: String,
    /// Digest of the library manifest.
    pub manifest_sha256: String,
    /// Fixture rows in the library.
    pub fixtures: usize,
    /// Distinct speakers across the library.
    pub speakers: usize,
    /// Attribution obligations (VP-181).
    pub attributions: Vec<AttributionRow>,
}

/// How the derived thresholds partition one condition's trials.
///
/// Counted by running the matching core a second time with the derived
/// [`ThresholdCalibration`] installed, so these are the bands the product
/// would have shown, not a re-implementation of the band rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BandCounts {
    /// Target trials in the suggestion band (correct suggestions).
    pub target_suggest: usize,
    /// Target trials in the weak band.
    pub target_weak: usize,
    /// Target trials below the floor (missed entirely).
    pub target_none: usize,
    /// Non-target trials in the suggestion band (wrong suggestions).
    pub nontarget_suggest: usize,
    /// Non-target trials in the weak band.
    pub nontarget_weak: usize,
    /// Non-target trials below the floor (the desired outcome).
    pub nontarget_none: usize,
}

/// Results for one channel condition (VP-184).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionReport {
    /// Condition token, or `overall` for the pooled curve.
    pub condition: String,
    /// Same-speaker trials.
    pub targets: usize,
    /// Different-speaker trials.
    pub nontargets: usize,
    /// Smallest non-zero false-accept rate this sample can express.
    pub false_accept_resolution: f64,
    /// Equal-error point of this condition's curve.
    pub equal_error_rate: EqualErrorRate,
    /// Observed thresholds at the published false-accept ceilings.
    pub operating_points: Vec<OperatingPoint>,
    /// Band partition under the derived thresholds, when they were derived.
    pub bands: Option<BandCounts>,
}

/// Diarization error on the synthetic session, via `crates/ai-diarization`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiarizationSection {
    /// Session the figures belong to.
    pub session_id: String,
    /// How the hypothesis was produced.
    pub hypothesis_source: String,
    /// `missed + false_alarm + confusion` over total reference speech.
    pub der: f64,
    /// Reference speech not hypothesized.
    pub missed_ms: u64,
    /// Hypothesized speech with no reference speaker.
    pub false_alarm_ms: u64,
    /// Matched speech attributed to the wrong cluster.
    pub confusion_ms: u64,
    /// Total reference speech, counting overlap once per speaker.
    pub reference_speech_ms: u64,
    /// Milliseconds of simultaneous speech in the reference.
    pub overlap_ms: u64,
    /// Collar applied, in milliseconds (zero: we report the harsher number).
    pub collar_ms: u64,
    /// Whether overlapped frames were excluded from scoring.
    pub skip_overlap_regions: bool,
}

/// One golden expectation: what the product would show for this cluster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoldenSuggestion {
    /// Cluster key the suggestion is about.
    pub cluster_key: String,
    /// Person the score belongs to.
    pub person_id: String,
    /// Band the score fell in.
    pub band: Band,
    /// The AS-Norm score.
    pub score: f64,
}

/// The artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerMatchReport {
    /// Must equal [`SPEAKER_MATCH_REPORT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Crate that produced the document.
    pub generator: String,
    /// When the run happened, when a caller recorded it. `null` for a
    /// deterministic scripted run, which has no measurement moment.
    ///
    /// Written as RFC 3339, not as epoch milliseconds: this document is read
    /// by people in review comments and release notes, and the published
    /// shape (`schemas/speaker-match-report.v1.schema.json`) has always said
    /// `date-time`. Until P2.5 produced the first measured artifact every
    /// committed document had `null` here, so the two never disagreed out
    /// loud.
    #[serde(
        serialize_with = "serialize_measured_on",
        deserialize_with = "deserialize_measured_on"
    )]
    pub measured_on: Option<Timestamp>,
    /// Commit the run was made at, when the caller recorded one.
    pub commit: Option<String>,
    /// Checkpoint the scores are bound to.
    pub model: ModelRecord,
    /// Runtime footprint, when measured.
    pub runtime: Option<RuntimeRecord>,
    /// Whether this document is ADR-0081 §4 release evidence.
    pub release_evidence: ReleaseEvidence,
    /// Fixtures behind the trials.
    pub fixtures: FixtureSummary,
    /// Pooled results across every condition.
    pub overall: ConditionReport,
    /// Per-condition results (VP-184).
    pub conditions: Vec<ConditionReport>,
    /// The derived thresholds and their provenance (VP-062).
    pub calibration: ThresholdCalibration,
    /// Diarization error on the synthetic session, when one was scored.
    pub diarization: Option<DiarizationSection>,
    /// Golden suggestions for the pinned fixture (VP-183).
    pub golden_suggestions: Vec<GoldenSuggestion>,
    /// Caveats a reader must carry away with the numbers (VP-170, VP-173).
    pub caveats: Vec<String>,
}

/// Write `measured_on` as RFC 3339, falling back to epoch milliseconds for an
/// instant outside the range that can be formatted.
#[allow(
    clippy::ref_option,
    reason = "serde's serialize_with signature takes a reference"
)]
fn serialize_measured_on<S: serde::Serializer>(
    value: &Option<Timestamp>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value.and_then(Timestamp::to_rfc3339) {
        Some(text) => serializer.serialize_some(&text),
        None => serializer.serialize_none(),
    }
}

/// Read `measured_on` back from RFC 3339, or from the millisecond form an
/// older document may carry.
fn deserialize_measured_on<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Timestamp>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Recorded {
        Rfc3339(String),
        Millis(i64),
    }

    let Some(recorded) = Option::<Recorded>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match recorded {
        Recorded::Millis(millis) => Ok(Some(Timestamp::from_millis(millis))),
        Recorded::Rfc3339(text) => {
            let parsed =
                time::OffsetDateTime::parse(&text, &time::format_description::well_known::Rfc3339)
                    .map_err(serde::de::Error::custom)?;
            let millis = i64::try_from(parsed.unix_timestamp_nanos() / 1_000_000)
                .map_err(serde::de::Error::custom)?;
            Ok(Some(Timestamp::from_millis(millis)))
        }
    }
}

/// Caveats every speaker-matching artifact carries.
///
/// Published with the numbers rather than next to them, because a benchmark
/// file travels: it ends up in a release note, an issue comment or a slide,
/// and VP-170/VP-173 apply wherever it lands.
pub const STANDING_CAVEATS: &[&str] = &[
    "Speaker-verification accuracy is demographically skewed; smaller models can widen the gap \
     (Hutiri & Ding, arxiv.org/abs/2201.09486). These pooled figures hide subgroup differences and \
     must never be quoted as a single advertised accuracy number (VP-170, VP-171).",
    "No score in this document authorises an automatic assignment. Every outcome is a suggestion a \
     person confirms (VP-060, ADR-0082).",
    "Published model-card EERs (VoxCeleb1-O, full-length utterances) measure a different protocol \
     and must not be compared with these figures (VP-173).",
    "ADR-0081 §4 is a two-platform gate and one document covers one platform: a release needs this \
     artifact for macOS arm64 *and* for Windows x64, so `release_evidence.qualifies` means \
     \"usable as evidence for the platform named in `runtime`\", never \"the gate is passed\".",
];

impl SpeakerMatchReport {
    /// Assemble the artifact, computing the release-evidence verdict.
    #[must_use]
    pub fn new(parts: ReportParts) -> Self {
        let ReportParts {
            measured_on,
            commit,
            model,
            runtime,
            fixtures,
            overall,
            conditions,
            calibration,
            diarization,
            golden_suggestions,
            extra_caveats,
        } = parts;

        let mut caveats: Vec<String> = STANDING_CAVEATS.iter().map(|s| (*s).to_owned()).collect();
        caveats.extend(extra_caveats);

        let release_evidence = evidence_verdict(&model, runtime.as_ref(), &calibration);

        Self {
            schema_version: SPEAKER_MATCH_REPORT_SCHEMA_VERSION,
            generator: concat!("plaudon-eval ", env!("CARGO_PKG_VERSION")).to_owned(),
            measured_on,
            commit,
            model,
            runtime,
            release_evidence,
            fixtures,
            overall,
            conditions,
            calibration,
            diarization,
            golden_suggestions,
            caveats,
        }
    }

    /// Canonical file name for this report (ADR-0081 §4).
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("speaker-match-{}-eer.json", self.model.model_id)
    }

    /// Serialize as the committed artifact: pretty, newline-terminated.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut text =
            serde_json::to_string_pretty(self).expect("a speaker-match report always serializes");
        text.push('\n');
        text
    }

    /// Digest of the serialized artifact, for evidence bundles.
    #[must_use]
    pub fn digest(&self) -> Sha256Digest {
        Sha256Digest::of(self.to_json().as_bytes())
    }
}

/// Everything the harness measured, before the verdict is computed.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportParts {
    /// When the run happened, when the caller recorded it.
    pub measured_on: Option<Timestamp>,
    /// Commit of the run, when the caller recorded one.
    pub commit: Option<String>,
    /// Checkpoint record.
    pub model: ModelRecord,
    /// Runtime footprint, when measured.
    pub runtime: Option<RuntimeRecord>,
    /// Fixture summary.
    pub fixtures: FixtureSummary,
    /// Pooled condition report.
    pub overall: ConditionReport,
    /// Per-condition reports.
    pub conditions: Vec<ConditionReport>,
    /// Derived thresholds.
    pub calibration: ThresholdCalibration,
    /// Diarization section, when scored.
    pub diarization: Option<DiarizationSection>,
    /// Golden suggestions.
    pub golden_suggestions: Vec<GoldenSuggestion>,
    /// Run-specific caveats, appended to [`STANDING_CAVEATS`].
    pub extra_caveats: Vec<String>,
}

/// The ADR-0081 §4 conditions, checked rather than asserted.
fn evidence_verdict(
    model: &ModelRecord,
    runtime: Option<&RuntimeRecord>,
    calibration: &ThresholdCalibration,
) -> ReleaseEvidence {
    let mut blockers = Vec::new();
    if !model.embedding_source.is_model_measurement() {
        blockers.push(
            "embeddings were scripted, so no checkpoint was measured: this document exercises the \
             harness (VP-183), it is not an accuracy claim"
                .to_owned(),
        );
    }
    match runtime {
        None => blockers.push(
            "ADR-0081 §4 requires peak RSS, startup and RTF on reference hardware for both \
             platforms; none were recorded"
                .to_owned(),
        ),
        Some(runtime) => {
            if runtime.peak_rss_bytes.is_none() {
                blockers.push(format!("peak RSS was not measured on {}", runtime.platform));
            }
            if runtime.startup_ms.is_none() {
                blockers.push(format!("startup was not measured on {}", runtime.platform));
            }
            if runtime.real_time_factor.is_none() {
                blockers.push(format!(
                    "real-time factor was not measured on {}",
                    runtime.platform
                ));
            }
        }
    }
    if !calibration.status.is_derived() {
        blockers.push("the DET curve yielded no operating points".to_owned());
    }
    ReleaseEvidence {
        qualifies: blockers.is_empty(),
        blockers,
    }
}

/// Condition token used for the pooled curve.
pub const OVERALL_CONDITION: &str = "overall";

/// Report token for a condition.
#[must_use]
pub fn condition_token(condition: Condition) -> String {
    condition.as_str().to_owned()
}
