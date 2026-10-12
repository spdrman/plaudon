//! The versioned evaluation report (spec 17.2, 17.6).
//!
//! > We will publish sample sizes, locale, reference quality, and hardware
//! > details rather than unsupported ranking claims.
//!
//! Everything that sentence names is a required field here, so a report that
//! cannot say who measured what, on which corpus, on which machine, does not
//! serialize into a schema-valid document. The committed JSON Schema is
//! `schemas/evaluation-report.v1.schema.json` and a regression test pins it.
//!
//! Spec 17.6 deliberately defers numeric quality thresholds, so this crate
//! asserts none. [`FrozenThresholds`] is the place they land once the project
//! freezes them: it starts `deferred`, carries the spec reference, and is
//! rendered into the release evidence bundle either way, so a release reviewer
//! sees "thresholds not frozen" rather than nothing at all.

use std::collections::BTreeMap;

use plaudon_domain::locale::LanguageTag;
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::time::Timestamp;
use serde::{Deserialize, Serialize};

use crate::corpus::ItemRejection;
use crate::hypothesis::{Deployment, HypothesisAlignment};
use crate::metrics::{
    DomainTermAccuracy, DomainTermOutcome, EditCounts, NumericAccuracy, NumericEntity, NumericKind,
    NumericTally, RuntimeMeasurement, SilenceHallucination, TimedWord, TimestampError,
};

/// Schema version of the evaluation report this build writes and reads.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Which engine and model the report describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineRecord {
    /// Engine identifier, for example `whisper.cpp`.
    pub engine_id: String,
    /// Model identifier inside that engine.
    pub model_id: String,
    /// Model revision, when the producer recorded one.
    pub model_version: Option<String>,
    /// Whether the hypotheses came from a local or metered-cloud run.
    pub deployment: Deployment,
    /// Whether the operator gave the cloud opt-in for this run.
    pub cloud_opt_in: bool,
}

/// Which corpus the report measured, and what it refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusRecord {
    /// Corpus identifier from the manifest.
    pub corpus_id: String,
    /// Content hash of the manifest, so a gate can detect a changed corpus.
    pub manifest_sha256: Sha256Digest,
    /// Rows the manifest declares.
    pub items_declared: usize,
    /// Rows that passed consent clearance.
    pub items_cleared: usize,
    /// Rows actually measured.
    pub items_evaluated: usize,
    /// Rows excluded by the consent register, with typed reasons.
    pub excluded: Vec<ItemRejection>,
    /// Cleared rows with no hypothesis for this engine.
    pub missing_hypotheses: Vec<String>,
}

/// Sample description (spec 17.2 requires sample size and locale).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleRecord {
    /// Number of measured items.
    pub evaluated_items: usize,
    /// Measured items per locale.
    pub locale_breakdown: BTreeMap<String, usize>,
    /// Measured items per nominal duration bucket.
    pub duration_bucket_breakdown: BTreeMap<String, usize>,
    /// Measured items that contain overlapping speech.
    pub items_with_overlapping_speech: usize,
    /// Measured items with more than one speaker.
    pub multi_speaker_items: usize,
    /// Total measured audio, when every item recorded a duration.
    pub total_audio_ms: Option<u64>,
}

/// Where the measurement was taken (spec 17.2 "hardware details").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentRecord {
    /// Operating system family of the measuring host.
    pub os: String,
    /// Operating system version, when supplied.
    pub os_version: Option<String>,
    /// CPU description, when supplied.
    pub cpu: Option<String>,
    /// Installed RAM in bytes, when supplied.
    pub ram_bytes: Option<u64>,
    /// Source commit the hypotheses were produced from, when supplied.
    pub commit: Option<String>,
    /// When the measurement was taken.
    pub measured_on: Timestamp,
}

impl EnvironmentRecord {
    /// Evidence fields spec 17.2 expects a published measurement to carry.
    ///
    /// Used by the gate: a `Recommended` claim may not rest on a measurement
    /// that cannot say what machine or build produced it.
    #[must_use]
    pub fn missing_evidence(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.os.trim().is_empty() {
            missing.push("os");
        }
        if self.cpu.as_ref().is_none_or(|cpu| cpu.trim().is_empty()) {
            missing.push("cpu");
        }
        if self
            .commit
            .as_ref()
            .is_none_or(|commit| commit.trim().is_empty())
        {
            missing.push("commit");
        }
        missing
    }
}

/// An error rate with the operation mix behind it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ErrorRateReport {
    /// The rate, or `null` when the reference stream was empty.
    pub rate: Option<f64>,
    /// Total edits.
    pub edits: usize,
    /// Substitutions.
    pub substitutions: usize,
    /// Deletions.
    pub deletions: usize,
    /// Insertions.
    pub insertions: usize,
    /// Reference stream length the rate divides by.
    pub reference_length: usize,
}

impl From<EditCounts> for ErrorRateReport {
    fn from(counts: EditCounts) -> Self {
        Self {
            rate: counts.rate(),
            edits: counts.edits(),
            substitutions: counts.substitutions,
            deletions: counts.deletions,
            insertions: counts.insertions,
            reference_length: counts.reference_length,
        }
    }
}

/// Domain-term recall over the supplied vocabulary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DomainTermReport {
    /// Pooled recall, or `null` when no supplied term occurs in the reference.
    pub recall: Option<f64>,
    /// Term occurrences recovered.
    pub matched: usize,
    /// Term occurrences the reference contains.
    pub expected: usize,
    /// Per-term counts.
    pub terms: Vec<DomainTermOutcome>,
    /// Supplied terms the reference never says, excluded from recall.
    pub absent_from_reference: Vec<String>,
}

impl From<&DomainTermAccuracy> for DomainTermReport {
    fn from(accuracy: &DomainTermAccuracy) -> Self {
        Self {
            recall: accuracy.recall(),
            matched: accuracy.matched(),
            expected: accuracy.expected(),
            terms: accuracy.terms.clone(),
            absent_from_reference: accuracy.absent_from_reference.clone(),
        }
    }
}

/// Number/date/amount recall, with any invented numbers listed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NumericReport {
    /// Pooled recall, or `null` when the reference contains no numerics.
    pub recall: Option<f64>,
    /// Reference entities recovered.
    pub matched: usize,
    /// Reference entities expected.
    pub expected: usize,
    /// Counts per class.
    pub per_kind: BTreeMap<NumericKind, NumericTally>,
    /// Hypothesis entities with no counterpart in the reference.
    pub invented: Vec<NumericEntity>,
}

impl From<&NumericAccuracy> for NumericReport {
    fn from(accuracy: &NumericAccuracy) -> Self {
        Self {
            recall: accuracy.recall(),
            matched: accuracy.matched(),
            expected: accuracy.expected(),
            per_kind: accuracy.per_kind.clone(),
            invented: accuracy.invented.clone(),
        }
    }
}

/// Hallucinated content measured inside reference silence.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SilenceReport {
    /// Silent spans considered.
    pub spans: usize,
    /// Total reference silence, in milliseconds.
    pub silent_ms: u64,
    /// Silent spans containing hypothesis words.
    pub spans_with_content: usize,
    /// Hypothesis words inside silence.
    pub words_in_silence: usize,
    /// Hypothesis words overall.
    pub hypothesis_words: usize,
    /// Words per minute of silence, or `null` when no silence was marked.
    pub words_per_silent_minute: Option<f64>,
    /// Share of the hypothesis inside silence, or `null` when it is empty.
    pub fraction_of_hypothesis_words: Option<f64>,
}

impl From<&SilenceHallucination> for SilenceReport {
    fn from(measured: &SilenceHallucination) -> Self {
        Self {
            spans: measured.spans,
            silent_ms: measured.silent_ms,
            spans_with_content: measured.spans_with_content,
            words_in_silence: measured.words_in_silence,
            hypothesis_words: measured.hypothesis_words,
            words_per_silent_minute: measured.words_per_silent_minute(),
            fraction_of_hypothesis_words: measured.fraction_of_hypothesis_words(),
        }
    }
}

/// Runtime figures for one item, as recorded by the producing process.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ItemRuntimeReport {
    /// Audio length, in milliseconds.
    pub audio_duration_ms: u64,
    /// Wall-clock transcription time, in milliseconds.
    pub wall_clock_ms: u64,
    /// Wall-clock time over audio length.
    pub real_time_factor: Option<f64>,
    /// Time to the first emitted result, in milliseconds.
    pub time_to_first_result_ms: Option<u64>,
    /// Peak resident set size, in bytes.
    pub peak_rss_bytes: Option<u64>,
    /// Metered provider cost, in micro-USD.
    pub metered_cost_micro_usd: Option<u64>,
}

impl From<&RuntimeMeasurement> for ItemRuntimeReport {
    fn from(measurement: &RuntimeMeasurement) -> Self {
        Self {
            audio_duration_ms: measurement.audio_duration_ms,
            wall_clock_ms: measurement.wall_clock_ms,
            real_time_factor: measurement.real_time_factor(),
            time_to_first_result_ms: measurement.time_to_first_result_ms,
            peak_rss_bytes: measurement.peak_rss_bytes,
            metered_cost_micro_usd: measurement.metered_cost_micro_usd,
        }
    }
}

/// Pooled runtime figures across the measured items.
///
/// Every field here comes from a recorded measurement. Items whose producer
/// recorded nothing are counted in `items_without_runtime` instead of being
/// treated as zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct RuntimeAggregate {
    /// Items whose producer recorded runtime figures.
    pub items_with_runtime: usize,
    /// Items whose producer recorded nothing.
    pub items_without_runtime: usize,
    /// Pooled audio length, in milliseconds.
    pub total_audio_ms: u64,
    /// Pooled wall-clock time, in milliseconds.
    pub total_wall_clock_ms: u64,
    /// Pooled wall-clock time over pooled audio length.
    pub real_time_factor: Option<f64>,
    /// Mean time to first result over the items that recorded it.
    pub mean_time_to_first_result_ms: Option<f64>,
    /// How many items recorded a time to first result.
    pub time_to_first_result_samples: usize,
    /// Largest peak RSS observed, in bytes.
    pub peak_rss_bytes: Option<u64>,
    /// Pooled metered cost, in micro-USD, for cloud runs.
    pub metered_cost_micro_usd: Option<u64>,
}

/// The pooled spec 17.2 metric set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggregateMetrics {
    /// Pooled word error rate.
    pub word_error_rate: ErrorRateReport,
    /// Pooled character error rate.
    pub character_error_rate: ErrorRateReport,
    /// Pooled domain-term recall.
    pub domain_term_accuracy: DomainTermReport,
    /// Pooled number/date/amount recall.
    pub numeric_accuracy: NumericReport,
    /// Pooled silence hallucination, or `null` when no reference marked any
    /// silence or no hypothesis carried word times.
    pub silence_hallucination: Option<SilenceReport>,
    /// Pooled timestamp error, or `null` when no hypothesis carried a usable
    /// alignment. Never `0.0` for an absent alignment.
    pub timestamp_error: Option<TimestampError>,
    /// Pooled runtime figures.
    pub runtime: RuntimeAggregate,
}

/// Per-item results, in corpus declaration order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemReport {
    /// Corpus item identifier.
    pub item_id: String,
    /// Item locale.
    pub locale: LanguageTag,
    /// Nominal duration bucket.
    pub duration_bucket: String,
    /// Declared speaker count.
    pub speaker_count: u16,
    /// Alignment origin the hypothesis declared.
    pub alignment_origin: HypothesisAlignment,
    /// Word error rate for this item.
    pub word_error_rate: ErrorRateReport,
    /// Character error rate for this item.
    pub character_error_rate: ErrorRateReport,
    /// Domain-term recall for this item.
    pub domain_term_accuracy: DomainTermReport,
    /// Number/date/amount recall for this item.
    pub numeric_accuracy: NumericReport,
    /// Silence hallucination for this item, when measurable.
    pub silence_hallucination: Option<SilenceReport>,
    /// The words found inside reference silence, as evidence.
    pub hallucinated_words: Vec<TimedWord>,
    /// Timestamp error for this item, when the alignment permits it.
    pub timestamp_error: Option<TimestampError>,
    /// Runtime figures recorded for this item.
    pub runtime: Option<ItemRuntimeReport>,
}

/// A metric a frozen threshold can be stated against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdMetric {
    /// Pooled word error rate.
    WordErrorRate,
    /// Pooled character error rate.
    CharacterErrorRate,
    /// Pooled domain-term recall.
    DomainTermRecall,
    /// Pooled number/date/amount recall.
    NumericRecall,
    /// Hallucinated words per minute of reference silence.
    SilenceWordsPerMinute,
    /// Mean absolute timestamp error, in milliseconds.
    TimestampMeanAbsMs,
    /// Pooled real-time factor.
    RealTimeFactor,
    /// Largest peak RSS, in bytes.
    PeakRssBytes,
}

/// Direction of a frozen threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparator {
    /// The measurement must not exceed the value.
    AtMost,
    /// The measurement must reach the value.
    AtLeast,
}

impl Comparator {
    /// Whether a measurement satisfies this comparator.
    #[must_use]
    pub fn satisfied_by(self, measured: f64, value: f64) -> bool {
        match self {
            Self::AtMost => measured <= value,
            Self::AtLeast => measured >= value,
        }
    }
}

/// One frozen release threshold.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrozenThreshold {
    /// Metric the threshold constrains.
    pub metric: ThresholdMetric,
    /// Direction of the constraint.
    pub comparator: Comparator,
    /// Value the measurement is compared against.
    pub value: f64,
    /// When the project froze this threshold.
    pub frozen_on: Option<Timestamp>,
}

/// Whether numeric quality thresholds have been frozen yet (spec 17.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdStatus {
    /// Spec 17.6 defers the numbers; none are asserted.
    Deferred,
    /// The project has frozen numbers and they are listed.
    Frozen,
}

/// The frozen-threshold section carried into the release evidence bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrozenThresholds {
    /// Whether numbers exist yet.
    pub status: ThresholdStatus,
    /// Spec section that governs these numbers.
    pub spec_reference: String,
    /// Why the section looks the way it does.
    pub note: String,
    /// Identifier of the release evidence bundle that carries this section.
    pub evidence_bundle: Option<String>,
    /// The frozen numbers, empty while deferred.
    pub thresholds: Vec<FrozenThreshold>,
}

impl Default for FrozenThresholds {
    fn default() -> Self {
        Self::deferred()
    }
}

impl FrozenThresholds {
    /// The state spec 17.6 mandates until the project freezes numbers.
    #[must_use]
    pub fn deferred() -> Self {
        Self {
            status: ThresholdStatus::Deferred,
            spec_reference: "17.6".to_owned(),
            note: "Spec 17.6 defers numeric quality thresholds; this run asserts none."
                .to_owned(),
            evidence_bundle: None,
            thresholds: Vec::new(),
        }
    }

    /// A populated section, as used once the project freezes numbers.
    #[must_use]
    pub fn frozen(note: impl Into<String>, thresholds: Vec<FrozenThreshold>) -> Self {
        Self {
            status: ThresholdStatus::Frozen,
            spec_reference: "17.6".to_owned(),
            note: note.into(),
            evidence_bundle: None,
            thresholds,
        }
    }

    /// Attach the evidence bundle identifier this section is published in.
    #[must_use]
    pub fn in_evidence_bundle(mut self, bundle: impl Into<String>) -> Self {
        self.evidence_bundle = Some(bundle.into());
        self
    }
}

/// How one frozen threshold compares against the measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThresholdOutcome {
    /// The threshold evaluated.
    pub threshold: FrozenThreshold,
    /// The measurement, or `None` when the report does not carry it.
    pub measured: Option<f64>,
    /// Whether the threshold is satisfied, or `None` when unmeasured.
    pub satisfied: Option<bool>,
}

/// A complete evaluation report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Version of the tool that wrote the report.
    pub tool_version: String,
    /// Engine and model measured.
    pub engine: EngineRecord,
    /// Corpus measured, and what was excluded from it.
    pub corpus: CorpusRecord,
    /// Sample description.
    pub sample: SampleRecord,
    /// How the reference transcripts were produced and checked.
    pub reference_quality: String,
    /// Measuring host and build.
    pub environment: EnvironmentRecord,
    /// Pooled metrics.
    pub metrics: AggregateMetrics,
    /// Per-item metrics.
    pub per_item: Vec<ItemReport>,
    /// Frozen release thresholds, deferred until the project sets them.
    pub frozen_thresholds: FrozenThresholds,
}

impl EvaluationReport {
    /// Content hash over the canonical JSON form, cited by gate decisions.
    #[must_use]
    pub fn digest(&self) -> Sha256Digest {
        Sha256Digest::of(&serde_json::to_vec(self).unwrap_or_default())
    }

    /// Serialize in the exact form the CLI writes: pretty JSON, trailing
    /// newline, so a committed report diffs line by line.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        Ok(text)
    }

    /// Parse a stored report.
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// The pooled measurement a frozen threshold is stated against, when the
    /// report carries it.
    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn measured(&self, metric: ThresholdMetric) -> Option<f64> {
        match metric {
            ThresholdMetric::WordErrorRate => self.metrics.word_error_rate.rate,
            ThresholdMetric::CharacterErrorRate => self.metrics.character_error_rate.rate,
            ThresholdMetric::DomainTermRecall => self.metrics.domain_term_accuracy.recall,
            ThresholdMetric::NumericRecall => self.metrics.numeric_accuracy.recall,
            ThresholdMetric::SilenceWordsPerMinute => self
                .metrics
                .silence_hallucination
                .and_then(|silence| silence.words_per_silent_minute),
            ThresholdMetric::TimestampMeanAbsMs => {
                self.metrics.timestamp_error.map(|error| error.mean_abs_ms)
            }
            ThresholdMetric::RealTimeFactor => self.metrics.runtime.real_time_factor,
            ThresholdMetric::PeakRssBytes => self
                .metrics
                .runtime
                .peak_rss_bytes
                .map(|bytes| bytes as f64),
        }
    }

    /// Compare every frozen threshold against the measurements.
    ///
    /// While the section is `deferred` this is empty by construction: spec
    /// 17.6 asserts no numbers, so neither does this crate.
    #[must_use]
    pub fn evaluate_thresholds(&self) -> Vec<ThresholdOutcome> {
        self.frozen_thresholds
            .thresholds
            .iter()
            .map(|threshold| {
                let measured = self.measured(threshold.metric);
                ThresholdOutcome {
                    threshold: *threshold,
                    measured,
                    satisfied: measured
                        .map(|value| threshold.comparator.satisfied_by(value, threshold.value)),
                }
            })
            .collect()
    }

    /// Human-readable rendering for a terminal or an evidence bundle.
    ///
    /// Unmeasured values render as `not measured`, never as `0`.
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        let measured_on = self
            .environment
            .measured_on
            .to_rfc3339()
            .unwrap_or_else(|| format!("{}ms", self.environment.measured_on.as_millis()));
        out.push_str(&format!(
            "plaudon evaluation report v{} (tool {})\n",
            self.schema_version, self.tool_version
        ));
        out.push_str(&format!(
            "engine: {} / {}{} ({}{})\n",
            self.engine.engine_id,
            self.engine.model_id,
            self.engine
                .model_version
                .as_ref()
                .map_or_else(String::new, |version| format!(" @ {version}")),
            self.engine.deployment.as_str(),
            if self.engine.cloud_opt_in {
                ", cloud opt-in given"
            } else {
                ""
            }
        ));
        out.push_str(&format!(
            "corpus: {} ({})\n",
            self.corpus.corpus_id,
            self.corpus.manifest_sha256.to_hex()
        ));
        out.push_str(&format!(
            "items: {} declared, {} cleared, {} evaluated\n",
            self.corpus.items_declared, self.corpus.items_cleared, self.corpus.items_evaluated
        ));
        for rejection in &self.corpus.excluded {
            let reasons: Vec<&str> = rejection
                .reasons
                .iter()
                .map(crate::corpus::ClearanceRejection::code)
                .collect();
            out.push_str(&format!(
                "  excluded {}: {}\n",
                rejection.item_id,
                reasons.join(", ")
            ));
        }
        for missing in &self.corpus.missing_hypotheses {
            out.push_str(&format!("  no hypothesis: {missing}\n"));
        }
        out.push_str(&format!(
            "sample: {} items, locales {}, buckets {}\n",
            self.sample.evaluated_items,
            render_counts(&self.sample.locale_breakdown),
            render_counts(&self.sample.duration_bucket_breakdown)
        ));
        out.push_str(&format!(
            "        {} multi-speaker, {} with overlap, audio {}\n",
            self.sample.multi_speaker_items,
            self.sample.items_with_overlapping_speech,
            self.sample
                .total_audio_ms
                .map_or_else(|| "not recorded".to_owned(), |ms| format!("{ms} ms"))
        ));
        out.push_str(&format!("reference quality: {}\n", self.reference_quality));
        out.push_str(&format!(
            "host: {} {} / {} / {} / commit {} / measured {}\n",
            self.environment.os,
            optional(self.environment.os_version.as_deref()),
            optional(self.environment.cpu.as_deref()),
            self.environment
                .ram_bytes
                .map_or_else(|| "ram unknown".to_owned(), |bytes| format!("{bytes} B")),
            optional(self.environment.commit.as_deref()),
            measured_on
        ));
        out.push_str("metrics:\n");
        out.push_str(&format!(
            "  WER {} ({} edits over {} words)\n",
            render_rate(self.metrics.word_error_rate.rate),
            self.metrics.word_error_rate.edits,
            self.metrics.word_error_rate.reference_length
        ));
        out.push_str(&format!(
            "  CER {} ({} edits over {} characters)\n",
            render_rate(self.metrics.character_error_rate.rate),
            self.metrics.character_error_rate.edits,
            self.metrics.character_error_rate.reference_length
        ));
        out.push_str(&format!(
            "  domain terms {} ({}/{})\n",
            render_rate(self.metrics.domain_term_accuracy.recall),
            self.metrics.domain_term_accuracy.matched,
            self.metrics.domain_term_accuracy.expected
        ));
        out.push_str(&format!(
            "  numbers/dates/amounts {} ({}/{}, {} invented)\n",
            render_rate(self.metrics.numeric_accuracy.recall),
            self.metrics.numeric_accuracy.matched,
            self.metrics.numeric_accuracy.expected,
            self.metrics.numeric_accuracy.invented.len()
        ));
        match &self.metrics.silence_hallucination {
            Some(silence) => out.push_str(&format!(
                "  silence hallucination {} words in {} ms of silence ({})\n",
                silence.words_in_silence,
                silence.silent_ms,
                silence
                    .words_per_silent_minute
                    .map_or_else(|| "no rate".to_owned(), |rate| format!("{rate:.2}/min"))
            )),
            None => out.push_str("  silence hallucination: not measured\n"),
        }
        match &self.metrics.timestamp_error {
            Some(error) => out.push_str(&format!(
                "  timestamp error mean {:.1} ms, median {:.1} ms, max {} ms over {} boundaries\n",
                error.mean_abs_ms,
                error.median_abs_ms,
                error.max_abs_ms,
                error.compared_boundaries
            )),
            None => out.push_str("  timestamp error: not measured (no usable alignment)\n"),
        }
        out.push_str(&format!(
            "  real-time factor {}, time to first result {}, peak RSS {}\n",
            self.metrics
                .runtime
                .real_time_factor
                .map_or_else(|| "not recorded".to_owned(), |rtf| format!("{rtf:.3}")),
            self.metrics
                .runtime
                .mean_time_to_first_result_ms
                .map_or_else(
                    || "not recorded".to_owned(),
                    |ms| format!("{ms:.0} ms (n={})", self.metrics.runtime.time_to_first_result_samples)
                ),
            self.metrics
                .runtime
                .peak_rss_bytes
                .map_or_else(|| "not recorded".to_owned(), |bytes| format!("{bytes} B"))
        ));
        if let Some(cost) = self.metrics.runtime.metered_cost_micro_usd {
            out.push_str(&format!("  metered cost {cost} micro-USD\n"));
        }
        out.push_str("per item:\n");
        for item in &self.per_item {
            out.push_str(&format!(
                "  {} [{} {}] WER {} CER {} alignment {}\n",
                item.item_id,
                item.locale.as_str(),
                item.duration_bucket,
                render_rate(item.word_error_rate.rate),
                render_rate(item.character_error_rate.rate),
                item.alignment_origin.as_str()
            ));
        }
        out.push_str(&format!(
            "frozen thresholds: {}\n",
            match self.frozen_thresholds.status {
                ThresholdStatus::Deferred => "deferred (spec 17.6)".to_owned(),
                ThresholdStatus::Frozen =>
                    format!("{} frozen", self.frozen_thresholds.thresholds.len()),
            }
        ));
        out.push_str(&format!("  {}\n", self.frozen_thresholds.note));
        if let Some(bundle) = &self.frozen_thresholds.evidence_bundle {
            out.push_str(&format!("  evidence bundle: {bundle}\n"));
        }
        for outcome in self.evaluate_thresholds() {
            out.push_str(&format!(
                "  {:?} {:?} {} => {}\n",
                outcome.threshold.metric,
                outcome.threshold.comparator,
                outcome.threshold.value,
                match outcome.satisfied {
                    Some(true) => "met".to_owned(),
                    Some(false) => format!("NOT met (measured {})", render_rate(outcome.measured)),
                    None => "not measured".to_owned(),
                }
            ));
        }
        out
    }
}

fn optional(value: Option<&str>) -> String {
    value.map_or_else(|| "unknown".to_owned(), ToOwned::to_owned)
}

fn render_rate(rate: Option<f64>) -> String {
    rate.map_or_else(|| "not measured".to_owned(), |value| format!("{value:.4}"))
}

fn render_counts(counts: &BTreeMap<String, usize>) -> String {
    if counts.is_empty() {
        return "none".to_owned();
    }
    counts
        .iter()
        .map(|(key, count)| format!("{key}={count}"))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sample_report() -> EvaluationReport {
        EvaluationReport {
            schema_version: REPORT_SCHEMA_VERSION,
            tool_version: "0.1.0".to_owned(),
            engine: EngineRecord {
                engine_id: "whisper.cpp".to_owned(),
                model_id: "ggml-small".to_owned(),
                model_version: None,
                deployment: Deployment::Local,
                cloud_opt_in: false,
            },
            corpus: CorpusRecord {
                corpus_id: "unit".to_owned(),
                manifest_sha256: Sha256Digest::of(b"unit"),
                items_declared: 1,
                items_cleared: 1,
                items_evaluated: 1,
                excluded: Vec::new(),
                missing_hypotheses: Vec::new(),
            },
            sample: SampleRecord {
                evaluated_items: 1,
                locale_breakdown: BTreeMap::from([("en-US".to_owned(), 1)]),
                duration_bucket_breakdown: BTreeMap::from([("1min".to_owned(), 1)]),
                items_with_overlapping_speech: 0,
                multi_speaker_items: 0,
                total_audio_ms: Some(60_000),
            },
            reference_quality: "hand-checked verbatim".to_owned(),
            environment: EnvironmentRecord {
                os: "macos".to_owned(),
                os_version: Some("15.0".to_owned()),
                cpu: Some("Apple M2".to_owned()),
                ram_bytes: Some(16 * 1024 * 1024 * 1024),
                commit: Some("abc1234".to_owned()),
                measured_on: Timestamp::from_millis(1_780_000_000_000),
            },
            metrics: AggregateMetrics {
                word_error_rate: ErrorRateReport::from(EditCounts {
                    substitutions: 1,
                    deletions: 0,
                    insertions: 0,
                    reference_length: 10,
                }),
                character_error_rate: ErrorRateReport::from(EditCounts {
                    substitutions: 2,
                    deletions: 0,
                    insertions: 0,
                    reference_length: 50,
                }),
                domain_term_accuracy: DomainTermReport {
                    recall: Some(1.0),
                    matched: 2,
                    expected: 2,
                    terms: vec![DomainTermOutcome {
                        term: "rebar".to_owned(),
                        reference_occurrences: 2,
                        hypothesis_occurrences: 2,
                    }],
                    absent_from_reference: Vec::new(),
                },
                numeric_accuracy: NumericReport {
                    recall: Some(1.0),
                    matched: 1,
                    expected: 1,
                    per_kind: BTreeMap::from([(
                        NumericKind::Number,
                        NumericTally {
                            matched: 1,
                            expected: 1,
                        },
                    )]),
                    invented: Vec::new(),
                },
                silence_hallucination: Some(SilenceReport {
                    spans: 1,
                    silent_ms: 15_000,
                    spans_with_content: 0,
                    words_in_silence: 0,
                    hypothesis_words: 42,
                    words_per_silent_minute: Some(0.0),
                    fraction_of_hypothesis_words: Some(0.0),
                }),
                timestamp_error: Some(TimestampError {
                    mean_abs_ms: 42.0,
                    median_abs_ms: 30.0,
                    max_abs_ms: 90,
                    compared_boundaries: 3,
                }),
                runtime: RuntimeAggregate {
                    items_with_runtime: 1,
                    items_without_runtime: 0,
                    total_audio_ms: 60_000,
                    total_wall_clock_ms: 15_000,
                    real_time_factor: Some(0.25),
                    mean_time_to_first_result_ms: Some(2_000.0),
                    time_to_first_result_samples: 1,
                    peak_rss_bytes: Some(512 * 1024 * 1024),
                    metered_cost_micro_usd: None,
                },
            },
            per_item: vec![ItemReport {
                item_id: "syn-001".to_owned(),
                locale: LanguageTag::parse("en-US").expect("tag"),
                duration_bucket: "1min".to_owned(),
                speaker_count: 1,
                alignment_origin: HypothesisAlignment::Native,
                word_error_rate: ErrorRateReport::from(EditCounts {
                    substitutions: 1,
                    deletions: 0,
                    insertions: 0,
                    reference_length: 10,
                }),
                character_error_rate: ErrorRateReport::from(EditCounts {
                    substitutions: 2,
                    deletions: 0,
                    insertions: 0,
                    reference_length: 50,
                }),
                domain_term_accuracy: DomainTermReport {
                    recall: Some(1.0),
                    matched: 2,
                    expected: 2,
                    terms: Vec::new(),
                    absent_from_reference: Vec::new(),
                },
                numeric_accuracy: NumericReport {
                    recall: Some(1.0),
                    matched: 1,
                    expected: 1,
                    per_kind: BTreeMap::new(),
                    invented: Vec::new(),
                },
                silence_hallucination: None,
                hallucinated_words: Vec::new(),
                timestamp_error: None,
                runtime: None,
            }],
            frozen_thresholds: FrozenThresholds::deferred(),
        }
    }

    #[test]
    fn a_report_round_trips_through_json() {
        let report = sample_report();
        let text = report.to_json().expect("json");
        assert!(text.ends_with('\n'));
        assert_eq!(EvaluationReport::from_json(&text).expect("parse"), report);
    }

    #[test]
    fn the_digest_changes_with_any_measured_field() {
        let report = sample_report();
        let mut changed = report.clone();
        changed.metrics.word_error_rate.rate = Some(0.5);
        assert_ne!(report.digest(), changed.digest());
    }

    #[test]
    fn thresholds_start_deferred_and_assert_nothing() {
        let report = sample_report();
        assert_eq!(report.frozen_thresholds.status, ThresholdStatus::Deferred);
        assert!(report.frozen_thresholds.thresholds.is_empty());
        assert!(report.evaluate_thresholds().is_empty());
        assert_eq!(report.frozen_thresholds.spec_reference, "17.6");
    }

    #[test]
    fn a_frozen_threshold_is_compared_against_the_measurement() {
        let mut report = sample_report();
        report.frozen_thresholds = FrozenThresholds::frozen(
            "example freeze",
            vec![
                FrozenThreshold {
                    metric: ThresholdMetric::WordErrorRate,
                    comparator: Comparator::AtMost,
                    value: 0.2,
                    frozen_on: None,
                },
                FrozenThreshold {
                    metric: ThresholdMetric::DomainTermRecall,
                    comparator: Comparator::AtLeast,
                    value: 1.0,
                    frozen_on: None,
                },
            ],
        )
        .in_evidence_bundle("release-0.1.0");
        let outcomes = report.evaluate_thresholds();
        assert_eq!(outcomes[0].satisfied, Some(true));
        assert_eq!(outcomes[1].satisfied, Some(true));
        assert_eq!(
            report.frozen_thresholds.evidence_bundle.as_deref(),
            Some("release-0.1.0")
        );
    }

    #[test]
    fn an_unmeasured_threshold_is_unsatisfied_rather_than_passed() {
        let mut report = sample_report();
        report.metrics.timestamp_error = None;
        report.frozen_thresholds = FrozenThresholds::frozen(
            "example freeze",
            vec![FrozenThreshold {
                metric: ThresholdMetric::TimestampMeanAbsMs,
                comparator: Comparator::AtMost,
                value: 500.0,
                frozen_on: None,
            }],
        );
        let outcomes = report.evaluate_thresholds();
        assert_eq!(outcomes[0].measured, None);
        assert_eq!(outcomes[0].satisfied, None);
    }

    #[test]
    fn the_text_rendering_names_unmeasured_values_instead_of_printing_zero() {
        let mut report = sample_report();
        report.metrics.timestamp_error = None;
        report.metrics.silence_hallucination = None;
        report.metrics.runtime.real_time_factor = None;
        report.metrics.runtime.peak_rss_bytes = None;
        let text = report.render_text();
        assert!(text.contains("timestamp error: not measured (no usable alignment)"));
        assert!(text.contains("silence hallucination: not measured"));
        assert!(text.contains("real-time factor not recorded"));
        assert!(text.contains("frozen thresholds: deferred (spec 17.6)"));
        assert!(text.contains("reference quality: hand-checked verbatim"));
        assert!(text.contains("corpus: unit"));
    }

    #[test]
    fn the_text_rendering_is_deterministic() {
        assert_eq!(sample_report().render_text(), sample_report().render_text());
    }

    #[test]
    fn missing_hardware_evidence_is_named_field_by_field() {
        let mut environment = sample_report().environment;
        assert!(environment.missing_evidence().is_empty());
        environment.cpu = None;
        environment.commit = Some("  ".to_owned());
        assert_eq!(environment.missing_evidence(), vec!["cpu", "commit"]);
    }
}
