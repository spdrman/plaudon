//! The committed DER artifact: `docs/benchmarks/diarization-<provider>-der.json`
//! (ADR-0400 condition 4).
//!
//! ADR-0400 lets a provider into [`crate::provider::VALIDATED_PROVIDERS`] only
//! once five conditions hold, and the fourth is a measurement: "the provider
//! passes the DER harness on labelled fixtures including overlapping speech,
//! with sample sizes and hardware noted, and the measurement artifact committed
//! under `docs/benchmarks/diarization-<provider>-der.json`". This module is
//! that document's type, so the tool that writes it and the schema that
//! validates it cannot drift apart.
//!
//! Three properties are deliberate:
//!
//! * **Overlap is reported separately, always.** [`SessionMeasurement::der`] is
//!   a [`DerBreakdown`], not a number, because a diarization figure quoted
//!   without saying what it did with simultaneous speech is not a measurement
//!   anyone can act on.
//! * **Qualification is computed, never asserted.** [`open_conditions`] derives
//!   what this document can see for itself — the allowlist, the presence of
//!   overlap, the platforms measured — and [`ReleaseEvidence::new`] is `true`
//!   only when nothing is left.
//! * **It carries no audio and no vector.** Sessions are identified by digest;
//!   the speech behind them is rebuilt from a recipe by whoever re-runs the
//!   measurement (VP-002, VP-181).

use serde::{Deserialize, Serialize};

use crate::der::{DerBreakdown, DerReport};
use crate::provider::is_validated;

/// Schema version of the document this module writes.
pub const DIARIZATION_DER_REPORT_SCHEMA_VERSION: u32 = 1;

/// One model the provider loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRecord {
    /// Stable artifact id, as compiled into the model-pack facts.
    pub model_id: String,
    /// Artifact version.
    pub version: String,
    /// SHA-256 of the exact file that ran.
    pub sha256: String,
    /// SPDX identifier of the weights' licence.
    pub licence_spdx: String,
    /// What the model did in this pipeline.
    pub role: String,
}

/// One tunable the run used, and where its value came from.
///
/// Recorded because a DER without its operating point is unreproducible, and
/// because a threshold tuned on the fixtures it is then scored on is not a
/// measurement — the provenance string is what makes that visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParameterRecord {
    /// Parameter name as the provider spells it.
    pub name: String,
    /// Value used, rendered.
    pub value: String,
    /// Where the value came from.
    pub provenance: String,
}

/// The provider under measurement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRecord {
    /// Provider id, as [`crate::contract::DiarizationCapabilities`] reports it.
    pub provider_id: String,
    /// Provider version string.
    pub provider_version: String,
    /// Whether the id is in the allowlist at the time of the run.
    pub validated: bool,
    /// Models the run loaded, in pipeline order.
    pub models: Vec<ModelRecord>,
    /// Operating point.
    pub parameters: Vec<ParameterRecord>,
}

/// Machine and runtime facts behind the figures (ADR-0400 condition 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeRecord {
    /// Platform token, for example `macos-arm64`.
    pub platform: String,
    /// Host CPU, as reported by the machine.
    pub host_cpu: String,
    /// OS build string.
    pub os_build: String,
    /// Threads ONNX Runtime was allowed inside one operator.
    pub intra_op_threads: usize,
    /// Peak resident set size of the measuring process, in bytes.
    pub peak_rss_bytes: u64,
    /// Wall time to verify both checkpoints and create both sessions.
    pub startup_ms: u64,
    /// Processing wall time over audio duration, pooled over the run.
    pub real_time_factor: f64,
}

/// Where the labelled audio came from (VP-181, VP-182).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusRecord {
    /// Identifier of the fixture library.
    pub library_id: String,
    /// Corpus token the fixtures derive from.
    pub corpus: String,
    /// Licence of that corpus.
    pub licence: String,
    /// Attribution line the licence obliges.
    pub attribution: String,
    /// Whether the audio is committed. It is not, and the schema allows only
    /// `false`: committing LibriSpeech-derived speech would add a `NOTICE`
    /// obligation and gigabytes to the tree.
    pub audio_committed: bool,
    /// How the audio is rebuilt from the corpus.
    pub recipe: String,
}

/// One scored session under one channel condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeasurement {
    /// Session identifier, matching the RTTM file column.
    pub session_id: String,
    /// Channel condition the mix was put through.
    pub condition: String,
    /// SHA-256 of the exact wav that was diarized.
    pub media_sha256: String,
    /// Length of the mix.
    pub duration_ms: u64,
    /// Speakers in the reference.
    pub reference_speakers: usize,
    /// Reference speech, counting overlap once per simultaneous speaker.
    pub reference_speech_ms: u64,
    /// Reference speech where two or more speakers are active.
    pub overlapping_reference_ms: u64,
    /// Clusters the provider returned.
    pub hypothesis_clusters: usize,
    /// Turns the provider returned.
    pub hypothesis_turns: usize,
    /// Wall time the provider spent on this session.
    pub elapsed_ms: u64,
    /// The measurement, split by overlapping speech.
    pub der: DerBreakdown,
}

/// Error components pooled over several sessions.
///
/// Pooled by summing the components and dividing once, never by averaging
/// per-session rates: a ten-second session and a ten-minute one do not weigh
/// the same. Speaker and cluster counts are deliberately absent — identifiers
/// are per-session, so a pooled count would be a number with no meaning
/// (spec 17.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PooledFigures {
    /// `missed + false_alarm + confusion` over total reference speech.
    pub der: f64,
    /// Reference speech present but not hypothesized.
    pub missed_ms: u64,
    /// Hypothesized speech with no reference speaker.
    pub false_alarm_ms: u64,
    /// Matched speech attributed to the wrong cluster.
    pub confusion_ms: u64,
    /// Total reference speech in the denominator.
    pub total_reference_ms: u64,
    /// Frames scored.
    pub scored_frames: usize,
}

impl PooledFigures {
    /// Pool the components of several reports.
    #[must_use]
    pub fn pool<'a>(reports: impl IntoIterator<Item = &'a DerReport>) -> Self {
        let mut pooled = Self {
            der: 0.0,
            missed_ms: 0,
            false_alarm_ms: 0,
            confusion_ms: 0,
            total_reference_ms: 0,
            scored_frames: 0,
        };
        for report in reports {
            pooled.missed_ms += report.missed_ms;
            pooled.false_alarm_ms += report.false_alarm_ms;
            pooled.confusion_ms += report.confusion_ms;
            pooled.total_reference_ms += report.total_reference_ms;
            pooled.scored_frames += report.scored_frames;
        }
        pooled.der = if pooled.total_reference_ms == 0 {
            0.0
        } else {
            (pooled.missed_ms + pooled.false_alarm_ms + pooled.confusion_ms) as f64
                / pooled.total_reference_ms as f64
        };
        pooled
    }
}

/// One channel condition's sessions, pooled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionPool {
    /// Condition token, as the fixture recipe spells it.
    pub condition: String,
    /// The sessions of that condition, pooled.
    pub pooled: PooledDer,
}

/// Every session pooled, split the same way one session is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PooledDer {
    /// Sessions behind the figures.
    pub sessions: usize,
    /// Every scored frame.
    pub overall: PooledFigures,
    /// Frames with at most one reference speaker.
    pub single_speaker: PooledFigures,
    /// Frames with two or more simultaneous reference speakers.
    pub overlapping: PooledFigures,
}

impl ConditionPool {
    /// Pool each condition's sessions, in the order the conditions first
    /// appear.
    #[must_use]
    pub fn of(sessions: &[SessionMeasurement]) -> Vec<Self> {
        let mut conditions: Vec<String> = Vec::new();
        for session in sessions {
            if !conditions.contains(&session.condition) {
                conditions.push(session.condition.clone());
            }
        }
        conditions
            .into_iter()
            .map(|condition| {
                let matching: Vec<SessionMeasurement> = sessions
                    .iter()
                    .filter(|session| session.condition == condition)
                    .cloned()
                    .collect();
                Self {
                    pooled: PooledDer::of(&matching),
                    condition,
                }
            })
            .collect()
    }
}

impl PooledDer {
    /// Pool a run's sessions.
    #[must_use]
    pub fn of(sessions: &[SessionMeasurement]) -> Self {
        Self {
            sessions: sessions.len(),
            overall: PooledFigures::pool(sessions.iter().map(|session| &session.der.overall)),
            single_speaker: PooledFigures::pool(
                sessions.iter().map(|session| &session.der.single_speaker),
            ),
            overlapping: PooledFigures::pool(
                sessions.iter().map(|session| &session.der.overlapping),
            ),
        }
    }
}

/// Whether this document may be cited as release evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseEvidence {
    /// True only when nothing is open.
    pub qualifies: bool,
    /// What is still open, in the words a reader needs.
    pub blockers: Vec<String>,
}

impl ReleaseEvidence {
    /// Build the verdict from its blockers.
    ///
    /// There is no constructor that sets `qualifies` directly: a document that
    /// could claim to qualify while listing blockers is exactly the document
    /// this type exists to prevent.
    #[must_use]
    pub fn new(blockers: Vec<String>) -> Self {
        Self {
            qualifies: blockers.is_empty(),
            blockers,
        }
    }
}

/// Platforms ADR-0081 §4 and ADR-0400 condition 1 require together.
pub const REFERENCE_PLATFORMS: &[&str] = &["macos-arm64", "windows-x64"];

/// The conditions this document can decide for itself.
///
/// Three of ADR-0400's five are visible from the measurement: whether the
/// provider is in the allowlist, whether the reference carried overlapping
/// speech at all, and whether both reference platforms were measured. The
/// others — licence review, gated access, out-of-process execution — are facts
/// about the build, and the caller passes those in.
#[must_use]
pub fn open_conditions(
    provider: &ProviderRecord,
    runtime: &RuntimeRecord,
    sessions: &[SessionMeasurement],
) -> Vec<String> {
    let mut blockers = Vec::new();
    if !is_validated(&provider.provider_id) {
        blockers.push(format!(
            "`{}` is not in VALIDATED_PROVIDERS: ADR-0400 admits a provider only \
             once all five conditions are met, and this document closes one of them",
            provider.provider_id
        ));
    }
    if !sessions
        .iter()
        .any(|session| session.overlapping_reference_ms > 0)
    {
        blockers.push(
            "no session carried overlapping speech, so ADR-0400 condition 4 is not \
             addressed by this run"
                .to_owned(),
        );
    }
    let missing: Vec<&str> = REFERENCE_PLATFORMS
        .iter()
        .copied()
        .filter(|platform| *platform != runtime.platform)
        .collect();
    if !missing.is_empty() {
        blockers.push(format!(
            "measured on {} only; ADR-0400 condition 1 and ADR-0081 §4 also require {}",
            runtime.platform,
            missing.join(", ")
        ));
    }
    blockers
}

/// The committed artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerArtifact {
    /// Must equal [`DIARIZATION_DER_REPORT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Tool and version that wrote the document.
    pub generator: String,
    /// When the run happened, RFC 3339.
    pub measured_on: String,
    /// Commit the run was made from.
    pub commit: String,
    /// The provider under measurement.
    pub provider: ProviderRecord,
    /// The machine it ran on.
    pub runtime: RuntimeRecord,
    /// The labelled audio it ran over.
    pub corpus: CorpusRecord,
    /// One entry per session and condition.
    pub sessions: Vec<SessionMeasurement>,
    /// Those sessions pooled per channel condition, in first-seen order.
    ///
    /// Pooling across conditions answers "how does this provider do", which is
    /// rarely the question: a band-limited channel and a clean one are
    /// different measurements of the same provider, and the one a reader needs
    /// depends on the recording they have.
    pub by_condition: Vec<ConditionPool>,
    /// Those sessions pooled.
    pub overall: PooledDer,
    /// Whether the document may be cited.
    pub release_evidence: ReleaseEvidence,
    /// Caveats that must travel with any number taken out of the file.
    pub caveats: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::der::DerOptions;
    use crate::{diarization_error_rate_by_overlap, ReferenceTurn, SpeakerTurn};

    fn runtime(platform: &str) -> RuntimeRecord {
        RuntimeRecord {
            platform: platform.to_owned(),
            host_cpu: "test".to_owned(),
            os_build: "test".to_owned(),
            intra_op_threads: 1,
            peak_rss_bytes: 1,
            startup_ms: 1,
            real_time_factor: 1.0,
        }
    }

    fn session(id: &str, overlap_ms: u64) -> SessionMeasurement {
        let reference = vec![
            ReferenceTurn::new("alice", 0, 2_000),
            ReferenceTurn::new("bob", 2_000 - overlap_ms, 4_000),
        ];
        let hypothesis = vec![
            SpeakerTurn::new("cluster-01", 0, 2_000),
            SpeakerTurn::new("cluster-02", 2_000, 4_000),
        ];
        let der = diarization_error_rate_by_overlap(&reference, &hypothesis, DerOptions::default());
        SessionMeasurement {
            session_id: id.to_owned(),
            condition: "clean".to_owned(),
            media_sha256: "0".repeat(64),
            duration_ms: 4_000,
            reference_speakers: 2,
            reference_speech_ms: der.overall.total_reference_ms,
            overlapping_reference_ms: der.overall.overlapping_reference_ms,
            hypothesis_clusters: 2,
            hypothesis_turns: 2,
            elapsed_ms: 10,
            der,
        }
    }

    fn provider(id: &str) -> ProviderRecord {
        ProviderRecord {
            provider_id: id.to_owned(),
            provider_version: "1".to_owned(),
            validated: is_validated(id),
            models: Vec::new(),
            parameters: Vec::new(),
        }
    }

    #[test]
    fn pooling_sums_components_rather_than_averaging_rates() {
        let sessions = vec![session("a", 500), session("b", 500)];
        let pooled = PooledDer::of(&sessions);
        assert_eq!(pooled.sessions, 2);
        assert_eq!(
            pooled.overall.total_reference_ms,
            sessions
                .iter()
                .map(|session| session.der.overall.total_reference_ms)
                .sum::<u64>()
        );
        assert_eq!(
            pooled.single_speaker.total_reference_ms + pooled.overlapping.total_reference_ms,
            pooled.overall.total_reference_ms
        );
        let expected = (pooled.overall.missed_ms
            + pooled.overall.false_alarm_ms
            + pooled.overall.confusion_ms) as f64
            / pooled.overall.total_reference_ms as f64;
        assert!((pooled.overall.der - expected).abs() < 1e-12);
    }

    #[test]
    fn an_unvalidated_provider_is_a_blocker_and_blocks_the_verdict() {
        let sessions = vec![session("a", 500)];
        let blockers = open_conditions(
            &provider("onnx-pyannote-wespeaker"),
            &runtime("macos-arm64"),
            &sessions,
        );
        assert!(blockers
            .iter()
            .any(|line| line.contains("VALIDATED_PROVIDERS")));
        assert!(!ReleaseEvidence::new(blockers).qualifies);
    }

    #[test]
    fn a_reference_without_overlap_cannot_close_condition_four() {
        let sessions = vec![session("a", 0)];
        let blockers = open_conditions(
            &provider("fake-deterministic"),
            &runtime("macos-arm64"),
            &sessions,
        );
        assert!(blockers
            .iter()
            .any(|line| line.contains("overlapping speech")));
    }

    #[test]
    fn one_platform_is_never_both_reference_platforms() {
        let sessions = vec![session("a", 500)];
        let blockers = open_conditions(
            &provider("fake-deterministic"),
            &runtime("macos-arm64"),
            &sessions,
        );
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("windows-x64"));
    }

    #[test]
    fn the_verdict_cannot_be_true_while_blockers_are_listed() {
        assert!(!ReleaseEvidence::new(vec!["something".to_owned()]).qualifies);
        assert!(ReleaseEvidence::new(Vec::new()).qualifies);
    }
}
