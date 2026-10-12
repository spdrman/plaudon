//! The deterministic, offline evaluation runner (spec 17.1, 17.2, 17.6).
//!
//! The runner scores hypothesis transcripts that some other process already
//! produced. It never starts an engine, never reads audio and never opens a
//! socket, which is what makes an evaluation reproducible from committed
//! artefacts and safe to run in CI.
//!
//! Two refusals live here:
//!
//! * **Uncleared rows never reach a metric.** The consent register decides
//!   what may be measured; refused rows travel into the report with their
//!   typed reasons.
//! * **Cloud evaluation is opt-in and capped.** A hypothesis produced by a
//!   metered provider is only scored when the operator passed the cloud flag
//!   *and* the environment carries the opt-in variable and a cost cap. The
//!   runner still makes no network call: the opt-in governs whether
//!   provider-metered material may be admitted and how much declared spend the
//!   run will accept.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::corpus::{CorpusItem, CorpusManifest};
use crate::hypothesis::{
    Deployment, HypothesisDocument, HypothesisSet, ReferenceTranscript, TranscriptError,
};
use crate::metrics::{
    self, DomainTermAccuracy, EditCounts, NumericAccuracy, SilenceHallucination, SilencePolicy,
    TimestampError,
};
use crate::report::{
    AggregateMetrics, CorpusRecord, DomainTermReport, EngineRecord, EnvironmentRecord,
    ErrorRateReport, EvaluationReport, FrozenThresholds, ItemReport, ItemRuntimeReport,
    NumericReport, RuntimeAggregate, SampleRecord, SilenceReport, REPORT_SCHEMA_VERSION,
};

/// Environment variable that records the operator's cloud opt-in.
pub const CLOUD_OPT_IN_ENV: &str = "PLAUDON_EVAL_CLOUD_OPT_IN";

/// Environment variable that caps declared cloud spend, in micro-USD.
pub const CLOUD_COST_CAP_ENV: &str = "PLAUDON_EVAL_COST_CAP_MICRO_USD";

/// Whether metered-cloud hypotheses may be scored, and up to what spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudPolicy {
    /// Cloud material is refused. This is the default.
    Refused,
    /// Cloud material is admitted up to a declared spend cap.
    Allowed {
        /// Maximum declared metered spend for the whole run, in micro-USD.
        cost_cap_micro_usd: u64,
    },
}

/// Why a cloud opt-in could not be established.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CloudOptInError {
    /// The flag was passed without the opt-in variable.
    #[error("cloud evaluation requires the explicit opt-in environment variable {variable}=1")]
    OptInMissing {
        /// Variable the operator must set.
        variable: &'static str,
    },
    /// The opt-in variable held something other than an affirmative value.
    #[error("{variable}=`{value}` is not an opt-in; set it to `1` or `true`")]
    OptInNotAffirmative {
        /// Variable inspected.
        variable: &'static str,
        /// Value found.
        value: String,
    },
    /// The opt-in was given without a spend cap.
    #[error("cloud evaluation is cost-capped: set {variable} to the maximum declared spend in micro-USD")]
    CostCapMissing {
        /// Variable the operator must set.
        variable: &'static str,
    },
    /// The spend cap was not an integer.
    #[error("{variable}=`{value}` is not an integer number of micro-USD")]
    CostCapUnparseable {
        /// Variable inspected.
        variable: &'static str,
        /// Value found.
        value: String,
    },
}

impl CloudPolicy {
    /// Resolve the policy from the operator's flag and the environment.
    ///
    /// The flag alone is never enough: spec 17.6 treats metered cloud
    /// evaluation as an opt-in, cost-capped activity, so the operator must
    /// also set [`CLOUD_OPT_IN_ENV`] and [`CLOUD_COST_CAP_ENV`]. Without the
    /// flag the policy is [`CloudPolicy::Refused`] whatever the environment
    /// says, so a stray variable cannot silently enable spend.
    pub fn resolve(
        allow_cloud: bool,
        environment: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, CloudOptInError> {
        if !allow_cloud {
            return Ok(Self::Refused);
        }
        let opt_in = environment(CLOUD_OPT_IN_ENV).ok_or(CloudOptInError::OptInMissing {
            variable: CLOUD_OPT_IN_ENV,
        })?;
        if !matches!(opt_in.trim(), "1" | "true") {
            return Err(CloudOptInError::OptInNotAffirmative {
                variable: CLOUD_OPT_IN_ENV,
                value: opt_in,
            });
        }
        let cap = environment(CLOUD_COST_CAP_ENV).ok_or(CloudOptInError::CostCapMissing {
            variable: CLOUD_COST_CAP_ENV,
        })?;
        let cost_cap_micro_usd =
            cap.trim()
                .parse::<u64>()
                .map_err(|_| CloudOptInError::CostCapUnparseable {
                    variable: CLOUD_COST_CAP_ENV,
                    value: cap.clone(),
                })?;
        Ok(Self::Allowed { cost_cap_micro_usd })
    }

    /// Whether metered-cloud material may be scored.
    #[must_use]
    pub const fn permits_cloud(self) -> bool {
        matches!(self, Self::Allowed { .. })
    }
}

/// Everything the runner needs that the corpus and hypotheses do not supply.
#[derive(Debug, Clone, PartialEq)]
pub struct RunContext {
    /// Measuring host and build, published with the report (spec 17.2).
    pub environment: EnvironmentRecord,
    /// Version of the tool writing the report.
    pub tool_version: String,
    /// Frozen release thresholds, deferred by default (spec 17.6).
    pub frozen_thresholds: FrozenThresholds,
    /// How much of a word must sit in silence to count as hallucinated.
    pub silence_policy: SilencePolicy,
    /// Whether metered-cloud hypotheses are admitted.
    pub cloud_policy: CloudPolicy,
}

impl RunContext {
    /// A context with the default silence policy, deferred thresholds and no
    /// cloud opt-in.
    #[must_use]
    pub fn new(environment: EnvironmentRecord, tool_version: impl Into<String>) -> Self {
        Self {
            environment,
            tool_version: tool_version.into(),
            frozen_thresholds: FrozenThresholds::deferred(),
            silence_policy: SilencePolicy::default(),
            cloud_policy: CloudPolicy::Refused,
        }
    }
}

/// Why a run could not produce a report.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// A reference transcript could not be read.
    #[error("item `{item_id}`: {source}")]
    Reference {
        /// Item whose reference failed.
        item_id: String,
        /// Underlying transcript error.
        source: TranscriptError,
    },
    /// A reference transcript belongs to a different item.
    #[error("reference {path} declares item `{found}`, expected `{expected}`")]
    ReferenceItemMismatch {
        /// Path of the reference.
        path: PathBuf,
        /// Item the manifest expected.
        expected: String,
        /// Item the document declares.
        found: String,
    },
    /// No cleared item has a hypothesis for the requested engine.
    #[error("no hypothesis for engine `{engine_id}` covers any cleared corpus item; loaded engines: {}", available.join(", "))]
    NoHypothesesForEngine {
        /// Engine requested.
        engine_id: String,
        /// Engines actually present in the hypothesis set.
        available: Vec<String>,
    },
    /// Hypotheses for one engine disagree about the model.
    #[error("hypotheses for engine `{engine_id}` mix models `{first}` and `{second}`; evaluate one model at a time")]
    InconsistentModel {
        /// Engine requested.
        engine_id: String,
        /// First model seen.
        first: String,
        /// Conflicting model.
        second: String,
    },
    /// Hypotheses for one engine mix local and cloud production.
    #[error("hypotheses for engine `{engine_id}` mix local and cloud deployments")]
    InconsistentDeployment {
        /// Engine requested.
        engine_id: String,
    },
    /// Cloud material was supplied without the opt-in.
    #[error("item `{item_id}` was produced by a metered cloud engine; re-run with the cloud opt-in (spec 17.6)")]
    CloudEvaluationNotPermitted {
        /// First cloud item found.
        item_id: String,
    },
    /// A cloud hypothesis did not declare its metered cost.
    #[error("item `{item_id}` is a cloud hypothesis with no declared metered cost; the cost cap cannot be enforced")]
    MeteredCostNotRecorded {
        /// Item missing a cost.
        item_id: String,
    },
    /// Declared cloud spend exceeds the cap.
    #[error("declared metered cost {declared_micro_usd} micro-USD exceeds the cap of {cap_micro_usd} micro-USD")]
    CostCapExceeded {
        /// Sum of declared costs.
        declared_micro_usd: u64,
        /// The configured cap.
        cap_micro_usd: u64,
    },
    /// The whole corpus was refused by the consent register.
    #[error("corpus `{corpus_id}` has no consent-cleared items; {rejected} row(s) were refused")]
    NothingCleared {
        /// Corpus identifier.
        corpus_id: String,
        /// Rows refused.
        rejected: usize,
    },
}

/// Pooled metric state while the runner walks the corpus.
#[derive(Debug, Default)]
struct Accumulator {
    word: EditCounts,
    character: EditCounts,
    domain_terms: DomainTermAccuracy,
    numeric: NumericAccuracy,
    silence: Option<SilenceHallucination>,
    boundary_errors: Vec<u64>,
    runtime: RuntimeAggregate,
    time_to_first_result_total: u64,
}

/// Score a set of hypothesis transcripts against a corpus.
///
/// `manifest_dir` resolves the manifest's relative reference-transcript paths.
pub fn run(
    manifest: &CorpusManifest,
    manifest_dir: &Path,
    hypotheses: &HypothesisSet,
    engine_id: &str,
    context: &RunContext,
) -> Result<EvaluationReport, RunError> {
    let cleared = manifest.clear();
    if cleared.accepted().is_empty() {
        return Err(RunError::NothingCleared {
            corpus_id: manifest.corpus_id.clone(),
            rejected: cleared.rejected().len(),
        });
    }

    let mut accumulator = Accumulator::default();
    let mut per_item = Vec::new();
    let mut missing_hypotheses = Vec::new();
    let mut locale_breakdown: BTreeMap<String, usize> = BTreeMap::new();
    let mut duration_breakdown: BTreeMap<String, usize> = BTreeMap::new();
    let mut overlap_items = 0;
    let mut multi_speaker_items = 0;
    let mut model: Option<(String, Option<String>)> = None;
    let mut deployment: Option<Deployment> = None;
    let mut declared_cost: u64 = 0;
    let mut cloud_items = 0usize;

    for item in cleared.accepted() {
        let Some(document) = hypotheses.get(&item.id, engine_id) else {
            missing_hypotheses.push(item.id.clone());
            continue;
        };
        check_model(engine_id, document, &mut model)?;
        check_deployment(engine_id, document, &mut deployment)?;
        if document.deployment == Deployment::Cloud {
            cloud_items += 1;
            if !context.cloud_policy.permits_cloud() {
                return Err(RunError::CloudEvaluationNotPermitted {
                    item_id: item.id.clone(),
                });
            }
            let cost = document
                .runtime
                .and_then(|runtime| runtime.metered_cost_micro_usd)
                .ok_or_else(|| RunError::MeteredCostNotRecorded {
                    item_id: item.id.clone(),
                })?;
            declared_cost = declared_cost.saturating_add(cost);
        }

        let reference = load_reference(manifest_dir, item)?;
        let report = score_item(item, &reference, document, context, &mut accumulator);

        *locale_breakdown
            .entry(item.locale.as_str().to_owned())
            .or_insert(0) += 1;
        *duration_breakdown
            .entry(item.duration_bucket.as_str().to_owned())
            .or_insert(0) += 1;
        if item.overlapping_speech {
            overlap_items += 1;
        }
        if item.speaker_count > 1 {
            multi_speaker_items += 1;
        }
        per_item.push(report);
    }

    if per_item.is_empty() {
        let mut available: Vec<String> = hypotheses
            .engines()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect();
        available.sort();
        return Err(RunError::NoHypothesesForEngine {
            engine_id: engine_id.to_owned(),
            available,
        });
    }

    if let CloudPolicy::Allowed { cost_cap_micro_usd } = context.cloud_policy {
        if declared_cost > cost_cap_micro_usd {
            return Err(RunError::CostCapExceeded {
                declared_micro_usd: declared_cost,
                cap_micro_usd: cost_cap_micro_usd,
            });
        }
    }

    let (model_id, model_version) = model.unwrap_or_else(|| (String::new(), None));
    let deployment = deployment.unwrap_or(Deployment::Local);
    let total_audio_ms = if accumulator.runtime.items_without_runtime == 0 {
        Some(accumulator.runtime.total_audio_ms)
    } else {
        None
    };

    Ok(EvaluationReport {
        schema_version: REPORT_SCHEMA_VERSION,
        tool_version: context.tool_version.clone(),
        engine: EngineRecord {
            engine_id: engine_id.to_owned(),
            model_id,
            model_version,
            deployment,
            cloud_opt_in: cloud_items > 0 && context.cloud_policy.permits_cloud(),
        },
        corpus: CorpusRecord {
            corpus_id: manifest.corpus_id.clone(),
            manifest_sha256: manifest.digest(),
            items_declared: manifest.items.len(),
            items_cleared: cleared.accepted().len(),
            items_evaluated: per_item.len(),
            excluded: cleared.rejected().to_vec(),
            missing_hypotheses,
        },
        sample: SampleRecord {
            evaluated_items: per_item.len(),
            locale_breakdown,
            duration_bucket_breakdown: duration_breakdown,
            items_with_overlapping_speech: overlap_items,
            multi_speaker_items,
            total_audio_ms,
        },
        reference_quality: manifest.reference_quality.clone(),
        environment: context.environment.clone(),
        metrics: accumulator.finish(declared_cost, cloud_items > 0),
        per_item,
        frozen_thresholds: context.frozen_thresholds.clone(),
    })
}

fn check_model(
    engine_id: &str,
    document: &HypothesisDocument,
    model: &mut Option<(String, Option<String>)>,
) -> Result<(), RunError> {
    match model {
        None => *model = Some((document.model_id.clone(), document.model_version.clone())),
        Some((first, _)) if *first != document.model_id => {
            return Err(RunError::InconsistentModel {
                engine_id: engine_id.to_owned(),
                first: first.clone(),
                second: document.model_id.clone(),
            })
        }
        Some(_) => {}
    }
    Ok(())
}

fn check_deployment(
    engine_id: &str,
    document: &HypothesisDocument,
    deployment: &mut Option<Deployment>,
) -> Result<(), RunError> {
    match deployment {
        None => *deployment = Some(document.deployment),
        Some(first) if *first != document.deployment => {
            return Err(RunError::InconsistentDeployment {
                engine_id: engine_id.to_owned(),
            })
        }
        Some(_) => {}
    }
    Ok(())
}

fn load_reference(manifest_dir: &Path, item: &CorpusItem) -> Result<ReferenceTranscript, RunError> {
    let path = manifest_dir.join(&item.reference_transcript);
    let reference = ReferenceTranscript::load(&path).map_err(|source| RunError::Reference {
        item_id: item.id.clone(),
        source,
    })?;
    if reference.item_id != item.id {
        return Err(RunError::ReferenceItemMismatch {
            path,
            expected: item.id.clone(),
            found: reference.item_id,
        });
    }
    Ok(reference)
}

fn score_item(
    item: &CorpusItem,
    reference: &ReferenceTranscript,
    document: &HypothesisDocument,
    context: &RunContext,
    accumulator: &mut Accumulator,
) -> ItemReport {
    let word = metrics::word_error_rate(&reference.text, &document.text);
    let character = metrics::character_error_rate(&reference.text, &document.text);
    let domain_terms =
        metrics::domain_term_accuracy(&reference.text, &document.text, &reference.domain_terms);
    let numeric = metrics::numeric_accuracy(&reference.text, &document.text);

    let words = document.timed_words();
    let silence = if reference.silence_spans_ms.is_empty() || words.is_empty() {
        None
    } else {
        Some(metrics::silence_hallucination(
            &reference.silence_spans_ms,
            words,
            context.silence_policy,
        ))
    };

    let boundary_errors = metrics::boundary_errors_ms(
        document.alignment_origin,
        &reference.segment_boundaries_ms,
        &document.segment_boundaries_ms,
    );
    let timestamp_error = boundary_errors
        .as_ref()
        .and_then(|errors| TimestampError::from_errors(errors));

    accumulator.word.absorb(word);
    accumulator.character.absorb(character);
    accumulator.domain_terms.absorb(domain_terms.clone());
    accumulator.numeric.absorb(numeric.clone());
    if let Some(measured) = &silence {
        match &mut accumulator.silence {
            Some(pooled) => pooled.absorb(measured.clone()),
            None => accumulator.silence = Some(measured.clone()),
        }
    }
    if let Some(errors) = boundary_errors {
        accumulator.boundary_errors.extend(errors);
    }
    match &document.runtime {
        Some(runtime) => {
            accumulator.runtime.items_with_runtime += 1;
            accumulator.runtime.total_audio_ms += runtime.audio_duration_ms;
            accumulator.runtime.total_wall_clock_ms += runtime.wall_clock_ms;
            if let Some(ttfr) = runtime.time_to_first_result_ms {
                accumulator.runtime.time_to_first_result_samples += 1;
                accumulator.time_to_first_result_total += ttfr;
            }
            if let Some(rss) = runtime.peak_rss_bytes {
                accumulator.runtime.peak_rss_bytes = Some(
                    accumulator
                        .runtime
                        .peak_rss_bytes
                        .map_or(rss, |e| e.max(rss)),
                );
            }
        }
        None => accumulator.runtime.items_without_runtime += 1,
    }

    ItemReport {
        item_id: item.id.clone(),
        locale: item.locale.clone(),
        duration_bucket: item.duration_bucket.as_str().to_owned(),
        speaker_count: item.speaker_count,
        alignment_origin: document.alignment_origin,
        word_error_rate: ErrorRateReport::from(word),
        character_error_rate: ErrorRateReport::from(character),
        domain_term_accuracy: DomainTermReport::from(&domain_terms),
        numeric_accuracy: NumericReport::from(&numeric),
        silence_hallucination: silence.as_ref().map(SilenceReport::from),
        hallucinated_words: silence
            .map(|measured| measured.offending_words)
            .unwrap_or_default(),
        timestamp_error,
        runtime: document.runtime.as_ref().map(ItemRuntimeReport::from),
    }
}

impl Accumulator {
    #[allow(clippy::cast_precision_loss)]
    fn finish(mut self, declared_cost_micro_usd: u64, cloud: bool) -> AggregateMetrics {
        if self.runtime.total_audio_ms > 0 {
            self.runtime.real_time_factor =
                Some(self.runtime.total_wall_clock_ms as f64 / self.runtime.total_audio_ms as f64);
        }
        if self.runtime.time_to_first_result_samples > 0 {
            self.runtime.mean_time_to_first_result_ms = Some(
                self.time_to_first_result_total as f64
                    / self.runtime.time_to_first_result_samples as f64,
            );
        }
        if cloud {
            self.runtime.metered_cost_micro_usd = Some(declared_cost_micro_usd);
        }
        AggregateMetrics {
            word_error_rate: ErrorRateReport::from(self.word),
            character_error_rate: ErrorRateReport::from(self.character),
            domain_term_accuracy: DomainTermReport::from(&self.domain_terms),
            numeric_accuracy: NumericReport::from(&self.numeric),
            silence_hallucination: self.silence.as_ref().map(SilenceReport::from),
            timestamp_error: TimestampError::from_errors(&self.boundary_errors),
            runtime: self.runtime,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use plaudon_domain::locale::LanguageTag;
    use plaudon_domain::time::Timestamp;

    use super::*;
    use crate::corpus::{
        ConsentStatus, DurationBucket, MediaReference, NoiseClass, SourceAttribution,
    };
    use crate::hypothesis::HypothesisAlignment;
    use crate::metrics::{RuntimeMeasurement, TimeSpanMs, TimedWord};

    fn environment() -> EnvironmentRecord {
        EnvironmentRecord {
            os: "linux".to_owned(),
            os_version: Some("6.1".to_owned()),
            cpu: Some("test".to_owned()),
            ram_bytes: Some(8 * 1024 * 1024 * 1024),
            commit: Some("deadbeef".to_owned()),
            measured_on: Timestamp::from_millis(1_780_000_000_000),
        }
    }

    fn item(id: &str, license: Option<&str>) -> CorpusItem {
        CorpusItem {
            id: id.to_owned(),
            media: MediaReference::Path(PathBuf::from(format!("{id}.wav"))),
            duration_bucket: DurationBucket::OneMinute,
            locale: LanguageTag::parse("en-US").expect("tag"),
            speaker_count: 1,
            overlapping_speech: false,
            noise_class: NoiseClass::Clean,
            content_tags: BTreeSet::new(),
            license: license.map(ToOwned::to_owned),
            consent: ConsentStatus::Synthetic,
            attribution: SourceAttribution {
                source: "generated".to_owned(),
                consent_record: None,
                acquired_on: None,
                confidential_real_client: false,
            },
            reference_transcript: PathBuf::from(format!("references/{id}.json")),
        }
    }

    fn manifest(items: Vec<CorpusItem>) -> CorpusManifest {
        CorpusManifest {
            schema_version: 1,
            corpus_id: "runner-unit".to_owned(),
            description: "runner unit corpus".to_owned(),
            reference_quality: "hand-written synthetic scripts".to_owned(),
            items,
        }
    }

    fn reference(id: &str, text: &str) -> ReferenceTranscript {
        ReferenceTranscript {
            schema_version: 1,
            item_id: id.to_owned(),
            text: text.to_owned(),
            domain_terms: vec!["rebar".to_owned()],
            silence_spans_ms: vec![TimeSpanMs {
                start_ms: 5_000,
                end_ms: 20_000,
            }],
            segment_boundaries_ms: vec![0, 4_000],
            quality_note: "hand-written".to_owned(),
        }
    }

    fn document(id: &str, text: &str) -> HypothesisDocument {
        HypothesisDocument {
            schema_version: 1,
            item_id: id.to_owned(),
            engine_id: "whisper.cpp".to_owned(),
            model_id: "ggml-small".to_owned(),
            model_version: None,
            deployment: Deployment::Local,
            alignment_origin: HypothesisAlignment::Native,
            text: text.to_owned(),
            words: vec![TimedWord {
                text: "rebar".to_owned(),
                start_ms: 1_000,
                end_ms: 1_400,
            }],
            segment_boundaries_ms: vec![0, 4_100],
            runtime: Some(RuntimeMeasurement {
                audio_duration_ms: 60_000,
                wall_clock_ms: 30_000,
                time_to_first_result_ms: Some(1_500),
                peak_rss_bytes: Some(1_024),
                metered_cost_micro_usd: None,
            }),
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        manifest: CorpusManifest,
    }

    impl Fixture {
        fn new(items: Vec<CorpusItem>) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::create_dir_all(dir.path().join("references")).expect("mkdir");
            Self {
                dir,
                manifest: manifest(items),
            }
        }

        fn write_reference(&self, reference: &ReferenceTranscript) {
            let path = self
                .dir
                .path()
                .join("references")
                .join(format!("{}.json", reference.item_id));
            std::fs::write(path, serde_json::to_string(reference).expect("json")).expect("write");
        }

        fn run(
            &self,
            set: &HypothesisSet,
            context: &RunContext,
        ) -> Result<EvaluationReport, RunError> {
            run(&self.manifest, self.dir.path(), set, "whisper.cpp", context)
        }
    }

    fn set(documents: Vec<HypothesisDocument>) -> HypothesisSet {
        let mut set = HypothesisSet::default();
        for (index, document) in documents.into_iter().enumerate() {
            set.insert(document, Path::new(&format!("{index}.json")))
                .expect("insert");
        }
        set
    }

    #[test]
    fn a_run_scores_cleared_items_and_excludes_refused_ones() {
        let fixture = Fixture::new(vec![item("a", Some("CC0-1.0")), item("b", None)]);
        fixture.write_reference(&reference("a", "we tied the rebar on 2026-03-14"));
        fixture.write_reference(&reference("b", "never scored"));
        let context = RunContext::new(environment(), "0.1.0");
        let report = fixture
            .run(
                &set(vec![
                    document("a", "we tied the rebar on 2026-03-14"),
                    document("b", "never scored"),
                ]),
                &context,
            )
            .expect("run");

        assert_eq!(report.corpus.items_declared, 2);
        assert_eq!(report.corpus.items_cleared, 1);
        assert_eq!(report.corpus.items_evaluated, 1);
        assert_eq!(report.corpus.excluded.len(), 1);
        assert_eq!(report.corpus.excluded[0].item_id, "b");
        assert_eq!(report.per_item.len(), 1);
        assert_eq!(report.metrics.word_error_rate.rate, Some(0.0));
        assert_eq!(report.reference_quality, "hand-written synthetic scripts");
        assert_eq!(report.sample.total_audio_ms, Some(60_000));
        assert_eq!(report.metrics.runtime.real_time_factor, Some(0.5));
        assert_eq!(report.metrics.runtime.peak_rss_bytes, Some(1_024));
        assert_eq!(
            report
                .metrics
                .timestamp_error
                .expect("alignment")
                .max_abs_ms,
            100
        );
    }

    #[test]
    fn a_run_is_byte_identical_across_invocations() {
        let fixture = Fixture::new(vec![item("a", Some("CC0-1.0"))]);
        fixture.write_reference(&reference("a", "we tied the rebar on 2026-03-14"));
        let context = RunContext::new(environment(), "0.1.0");
        let documents = set(vec![document("a", "we tied rebar on 2026-03-15")]);
        let first = fixture.run(&documents, &context).expect("run");
        let second = fixture.run(&documents, &context).expect("run");
        assert_eq!(
            first.to_json().expect("json"),
            second.to_json().expect("json")
        );
        assert_eq!(first.digest(), second.digest());
    }

    #[test]
    fn a_cleared_item_without_a_hypothesis_is_named_not_silently_dropped() {
        let fixture = Fixture::new(vec![item("a", Some("CC0-1.0")), item("c", Some("CC0-1.0"))]);
        fixture.write_reference(&reference("a", "we tied the rebar"));
        fixture.write_reference(&reference("c", "we tied the rebar"));
        let report = fixture
            .run(
                &set(vec![document("a", "we tied the rebar")]),
                &RunContext::new(environment(), "0.1.0"),
            )
            .expect("run");
        assert_eq!(report.corpus.missing_hypotheses, vec!["c".to_owned()]);
        assert_eq!(report.corpus.items_evaluated, 1);
    }

    #[test]
    fn an_unavailable_alignment_yields_no_timestamp_or_silence_metric() {
        let fixture = Fixture::new(vec![item("a", Some("CC0-1.0"))]);
        fixture.write_reference(&reference("a", "we tied the rebar"));
        let mut document = document("a", "we tied the rebar");
        document.alignment_origin = HypothesisAlignment::Unavailable;
        let report = fixture
            .run(
                &set(vec![document]),
                &RunContext::new(environment(), "0.1.0"),
            )
            .expect("run");
        assert!(report.metrics.timestamp_error.is_none());
        assert!(report.metrics.silence_hallucination.is_none());
        assert!(report.per_item[0].timestamp_error.is_none());
        assert!(report.per_item[0].silence_hallucination.is_none());
    }

    #[test]
    fn a_hypothesis_for_another_engine_does_not_count() {
        let fixture = Fixture::new(vec![item("a", Some("CC0-1.0"))]);
        fixture.write_reference(&reference("a", "we tied the rebar"));
        let mut document = document("a", "we tied the rebar");
        document.engine_id = "apple.speech".to_owned();
        let error = fixture
            .run(
                &set(vec![document]),
                &RunContext::new(environment(), "0.1.0"),
            )
            .expect_err("no hypotheses");
        assert!(matches!(
            error,
            RunError::NoHypothesesForEngine { ref available, .. } if available == &vec!["apple.speech".to_owned()]
        ));
    }

    #[test]
    fn mixing_two_models_of_one_engine_is_refused() {
        let fixture = Fixture::new(vec![item("a", Some("CC0-1.0")), item("c", Some("CC0-1.0"))]);
        fixture.write_reference(&reference("a", "we tied the rebar"));
        fixture.write_reference(&reference("c", "we tied the rebar"));
        let mut second = document("c", "we tied the rebar");
        second.model_id = "ggml-large-v3".to_owned();
        let error = fixture
            .run(
                &set(vec![document("a", "we tied the rebar"), second]),
                &RunContext::new(environment(), "0.1.0"),
            )
            .expect_err("mixed models");
        assert!(matches!(error, RunError::InconsistentModel { .. }));
    }

    #[test]
    fn a_missing_reference_transcript_fails_the_run() {
        let fixture = Fixture::new(vec![item("a", Some("CC0-1.0"))]);
        let error = fixture
            .run(
                &set(vec![document("a", "we tied the rebar")]),
                &RunContext::new(environment(), "0.1.0"),
            )
            .expect_err("missing reference");
        assert!(matches!(error, RunError::Reference { ref item_id, .. } if item_id == "a"));
    }

    #[test]
    fn a_reference_for_the_wrong_item_fails_the_run() {
        let fixture = Fixture::new(vec![item("a", Some("CC0-1.0"))]);
        let mut wrong = reference("a", "we tied the rebar");
        wrong.item_id = "elsewhere".to_owned();
        let path = fixture.dir.path().join("references/a.json");
        std::fs::write(path, serde_json::to_string(&wrong).expect("json")).expect("write");
        let error = fixture
            .run(
                &set(vec![document("a", "we tied the rebar")]),
                &RunContext::new(environment(), "0.1.0"),
            )
            .expect_err("wrong item");
        assert!(matches!(error, RunError::ReferenceItemMismatch { .. }));
    }

    #[test]
    fn a_corpus_with_nothing_cleared_cannot_be_run() {
        let fixture = Fixture::new(vec![item("a", None)]);
        let error = fixture
            .run(
                &set(vec![document("a", "we tied the rebar")]),
                &RunContext::new(environment(), "0.1.0"),
            )
            .expect_err("nothing cleared");
        assert!(matches!(
            error,
            RunError::NothingCleared { rejected: 1, .. }
        ));
    }

    fn cloud_document(id: &str, cost: Option<u64>) -> HypothesisDocument {
        let mut document = document(id, "we tied the rebar");
        document.deployment = Deployment::Cloud;
        if let Some(runtime) = document.runtime.as_mut() {
            runtime.metered_cost_micro_usd = cost;
        }
        document
    }

    #[test]
    fn cloud_hypotheses_are_refused_without_the_opt_in() {
        let fixture = Fixture::new(vec![item("a", Some("CC0-1.0"))]);
        fixture.write_reference(&reference("a", "we tied the rebar"));
        let error = fixture
            .run(
                &set(vec![cloud_document("a", Some(10))]),
                &RunContext::new(environment(), "0.1.0"),
            )
            .expect_err("no opt-in");
        assert!(matches!(
            error,
            RunError::CloudEvaluationNotPermitted { ref item_id } if item_id == "a"
        ));
    }

    #[test]
    fn cloud_hypotheses_run_under_an_opt_in_and_a_cap() {
        let fixture = Fixture::new(vec![item("a", Some("CC0-1.0"))]);
        fixture.write_reference(&reference("a", "we tied the rebar"));
        let mut context = RunContext::new(environment(), "0.1.0");
        context.cloud_policy = CloudPolicy::Allowed {
            cost_cap_micro_usd: 1_000,
        };
        let report = fixture
            .run(&set(vec![cloud_document("a", Some(250))]), &context)
            .expect("run");
        assert_eq!(report.engine.deployment, Deployment::Cloud);
        assert!(report.engine.cloud_opt_in);
        assert_eq!(report.metrics.runtime.metered_cost_micro_usd, Some(250));
    }

    #[test]
    fn a_cloud_run_over_the_cap_is_refused() {
        let fixture = Fixture::new(vec![item("a", Some("CC0-1.0"))]);
        fixture.write_reference(&reference("a", "we tied the rebar"));
        let mut context = RunContext::new(environment(), "0.1.0");
        context.cloud_policy = CloudPolicy::Allowed {
            cost_cap_micro_usd: 100,
        };
        let error = fixture
            .run(&set(vec![cloud_document("a", Some(250))]), &context)
            .expect_err("over cap");
        assert!(matches!(
            error,
            RunError::CostCapExceeded {
                declared_micro_usd: 250,
                cap_micro_usd: 100
            }
        ));
    }

    #[test]
    fn a_cloud_hypothesis_without_a_declared_cost_is_refused() {
        let fixture = Fixture::new(vec![item("a", Some("CC0-1.0"))]);
        fixture.write_reference(&reference("a", "we tied the rebar"));
        let mut context = RunContext::new(environment(), "0.1.0");
        context.cloud_policy = CloudPolicy::Allowed {
            cost_cap_micro_usd: 100,
        };
        let error = fixture
            .run(&set(vec![cloud_document("a", None)]), &context)
            .expect_err("no declared cost");
        assert!(matches!(error, RunError::MeteredCostNotRecorded { .. }));
    }

    #[test]
    fn the_cloud_policy_needs_both_the_flag_and_the_environment() {
        let empty = |_: &str| None;
        assert_eq!(
            CloudPolicy::resolve(false, empty).expect("no flag"),
            CloudPolicy::Refused
        );
        assert_eq!(
            CloudPolicy::resolve(true, empty).expect_err("flag alone"),
            CloudOptInError::OptInMissing {
                variable: CLOUD_OPT_IN_ENV
            }
        );
        let opt_in_only = |key: &str| (key == CLOUD_OPT_IN_ENV).then(|| "1".to_owned());
        assert_eq!(
            CloudPolicy::resolve(true, opt_in_only).expect_err("no cap"),
            CloudOptInError::CostCapMissing {
                variable: CLOUD_COST_CAP_ENV
            }
        );
        let wrong_value = |key: &str| match key {
            CLOUD_OPT_IN_ENV => Some("maybe".to_owned()),
            _ => None,
        };
        assert!(matches!(
            CloudPolicy::resolve(true, wrong_value),
            Err(CloudOptInError::OptInNotAffirmative { .. })
        ));
        let bad_cap = |key: &str| match key {
            CLOUD_OPT_IN_ENV => Some("true".to_owned()),
            CLOUD_COST_CAP_ENV => Some("lots".to_owned()),
            _ => None,
        };
        assert!(matches!(
            CloudPolicy::resolve(true, bad_cap),
            Err(CloudOptInError::CostCapUnparseable { .. })
        ));
        let good = |key: &str| match key {
            CLOUD_OPT_IN_ENV => Some("1".to_owned()),
            CLOUD_COST_CAP_ENV => Some("5000".to_owned()),
            _ => None,
        };
        assert_eq!(
            CloudPolicy::resolve(true, good).expect("opt-in"),
            CloudPolicy::Allowed {
                cost_cap_micro_usd: 5_000
            }
        );
        assert!(!CloudPolicy::Refused.permits_cloud());
    }
}
