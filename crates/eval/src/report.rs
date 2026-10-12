//! JSON report generation for evaluation results.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::metrics::EvalMetrics;

const REPORT_SCHEMA_VERSION: &str = "1";

/// Full evaluation report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    /// Schema version.
    pub schema_version: String,
    /// When the report was generated (ISO 8601).
    pub generated_at: String,
    /// Pipeline version.
    pub pipeline_version: String,
    /// Template ID.
    pub template_id: String,
    /// Template version.
    pub template_version: String,
    /// Prompt version.
    pub prompt_version: String,
    /// Locale.
    pub locale: String,
    /// Per-corpus metrics.
    pub per_corpus: HashMap<String, CorpusMetrics>,
    /// Aggregate metrics across all corpora.
    pub aggregate: EvalMetrics,
    /// Fixture provenance (file name → SHA256).
    pub fixture_provenance: HashMap<String, String>,
}

/// Metrics for one corpus fixture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorpusMetrics {
    /// Corpus ID.
    pub corpus_id: String,
    /// Template ID used.
    pub template_id: String,
    /// Metrics.
    pub metrics: EvalMetrics,
}

/// Error generating or writing a report.
#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    /// I/O failure.
    #[error("failed to write report: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization failure.
    #[error("failed to serialize report: {0}")]
    Json(#[from] serde_json::Error),
}

impl EvalReport {
    /// Create a new report.
    #[must_use]
    pub fn new(
        pipeline_version: impl Into<String>,
        template_id: impl Into<String>,
        template_version: impl Into<String>,
        prompt_version: impl Into<String>,
        locale: impl Into<String>,
    ) -> Self {
        let now = OffsetDateTime::now_utc();
        let generated_at = now
            .format(&time::format_description::well_known::Iso8601::DEFAULT)
            .unwrap_or_else(|_| "unknown".to_owned());

        Self {
            schema_version: REPORT_SCHEMA_VERSION.to_owned(),
            generated_at,
            pipeline_version: pipeline_version.into(),
            template_id: template_id.into(),
            template_version: template_version.into(),
            prompt_version: prompt_version.into(),
            locale: locale.into(),
            per_corpus: HashMap::new(),
            aggregate: EvalMetrics {
                decision_precision: None,
                decision_recall: None,
                decision_f1: None,
                decision_sample_size: 0,
                action_item_precision: None,
                action_item_recall: None,
                action_item_f1: None,
                action_item_sample_size: 0,
                owner_accuracy: None,
                owner_sample_size: 0,
                due_date_accuracy: None,
                due_date_sample_size: 0,
                schema_validity_rate: None,
                schema_sample_size: 0,
                anchor_correctness: None,
                anchor_sample_size: 0,
                unsupported_claim_rate: None,
                unsupported_claim_sample_size: 0,
            },
            fixture_provenance: HashMap::new(),
        }
    }

    /// Add metrics for a corpus.
    pub fn add_corpus(
        &mut self,
        corpus_id: impl Into<String>,
        template_id: impl Into<String>,
        metrics: EvalMetrics,
        file_name: impl Into<String>,
        sha256: impl Into<String>,
    ) {
        let corpus_id_string = corpus_id.into();
        self.per_corpus.insert(
            corpus_id_string.clone(),
            CorpusMetrics {
                corpus_id: corpus_id_string,
                template_id: template_id.into(),
                metrics,
            },
        );
        self.fixture_provenance
            .insert(file_name.into(), sha256.into());
    }

    /// Compute aggregate metrics from per-corpus metrics.
    pub fn compute_aggregate(&mut self) {
        if self.per_corpus.is_empty() {
            return;
        }

        // Aggregate by weighted averaging where appropriate
        let mut total_decision_precision = 0.0;
        let mut total_decision_recall = 0.0;
        let mut total_decision_sample_size = 0u32;
        let mut decision_count = 0;

        let mut total_action_precision = 0.0;
        let mut total_action_recall = 0.0;
        let mut total_action_sample_size = 0u32;
        let mut action_count = 0;

        let mut total_owner_accuracy = 0.0;
        let mut total_owner_sample_size = 0u32;
        let mut owner_count = 0;

        let mut total_due_date_accuracy = 0.0;
        let mut total_due_date_sample_size = 0u32;
        let mut due_date_count = 0;

        let mut total_schema_valid_count = 0u32;
        let mut total_schema_sample_size = 0u32;

        let mut total_anchor_verified = 0u32;
        let mut total_anchor_sample_size = 0u32;

        let mut total_unsupported = 0u32;
        let mut total_claim_count = 0u32;

        for corpus in self.per_corpus.values() {
            let m = &corpus.metrics;

            if let Some(p) = m.decision_precision {
                total_decision_precision += p;
                decision_count += 1;
            }
            if let Some(r) = m.decision_recall {
                total_decision_recall += r;
            }
            total_decision_sample_size += m.decision_sample_size;

            if let Some(p) = m.action_item_precision {
                total_action_precision += p;
                action_count += 1;
            }
            if let Some(r) = m.action_item_recall {
                total_action_recall += r;
            }
            total_action_sample_size += m.action_item_sample_size;

            if let Some(a) = m.owner_accuracy {
                total_owner_accuracy += a;
                owner_count += 1;
            }
            total_owner_sample_size += m.owner_sample_size;

            if let Some(a) = m.due_date_accuracy {
                total_due_date_accuracy += a;
                due_date_count += 1;
            }
            total_due_date_sample_size += m.due_date_sample_size;

            if let Some(rate) = m.schema_validity_rate {
                total_schema_valid_count += (rate * f64::from(m.schema_sample_size)).round() as u32;
            }
            total_schema_sample_size += m.schema_sample_size;

            if let Some(correctness) = m.anchor_correctness {
                total_anchor_verified +=
                    (correctness * f64::from(m.anchor_sample_size)).round() as u32;
            }
            total_anchor_sample_size += m.anchor_sample_size;

            if let Some(rate) = m.unsupported_claim_rate {
                total_unsupported +=
                    (rate * f64::from(m.unsupported_claim_sample_size)).round() as u32;
            }
            total_claim_count += m.unsupported_claim_sample_size;
        }

        self.aggregate = EvalMetrics {
            decision_precision: if decision_count > 0 {
                Some(total_decision_precision / decision_count as f64)
            } else {
                None
            },
            decision_recall: if decision_count > 0 {
                Some(total_decision_recall / decision_count as f64)
            } else {
                None
            },
            decision_f1: None, // Computed from precision/recall below
            decision_sample_size: total_decision_sample_size,

            action_item_precision: if action_count > 0 {
                Some(total_action_precision / action_count as f64)
            } else {
                None
            },
            action_item_recall: if action_count > 0 {
                Some(total_action_recall / action_count as f64)
            } else {
                None
            },
            action_item_f1: None, // Computed from precision/recall below
            action_item_sample_size: total_action_sample_size,

            owner_accuracy: if owner_count > 0 {
                Some(total_owner_accuracy / owner_count as f64)
            } else {
                None
            },
            owner_sample_size: total_owner_sample_size,

            due_date_accuracy: if due_date_count > 0 {
                Some(total_due_date_accuracy / due_date_count as f64)
            } else {
                None
            },
            due_date_sample_size: total_due_date_sample_size,

            schema_validity_rate: if total_schema_sample_size > 0 {
                Some(f64::from(total_schema_valid_count) / f64::from(total_schema_sample_size))
            } else {
                None
            },
            schema_sample_size: total_schema_sample_size,

            anchor_correctness: if total_anchor_sample_size > 0 {
                Some(f64::from(total_anchor_verified) / f64::from(total_anchor_sample_size))
            } else {
                None
            },
            anchor_sample_size: total_anchor_sample_size,

            unsupported_claim_rate: if total_claim_count > 0 {
                Some(f64::from(total_unsupported) / f64::from(total_claim_count))
            } else {
                None
            },
            unsupported_claim_sample_size: total_claim_count,
        };

        // Compute F1 scores from aggregated precision/recall
        if let (Some(p), Some(r)) = (
            self.aggregate.decision_precision,
            self.aggregate.decision_recall,
        ) {
            if p + r > 0.0 {
                self.aggregate.decision_f1 = Some(2.0 * p * r / (p + r));
            }
        }

        if let (Some(p), Some(r)) = (
            self.aggregate.action_item_precision,
            self.aggregate.action_item_recall,
        ) {
            if p + r > 0.0 {
                self.aggregate.action_item_f1 = Some(2.0 * p * r / (p + r));
            }
        }
    }

    /// Write the report to a file.
    pub fn write_to_file(&self, path: impl AsRef<Path>) -> Result<(), ReportError> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}

/// Generate a report (public API).
pub fn generate_report(
    pipeline_version: impl Into<String>,
    template_id: impl Into<String>,
    template_version: impl Into<String>,
    prompt_version: impl Into<String>,
    locale: impl Into<String>,
) -> EvalReport {
    EvalReport::new(
        pipeline_version,
        template_id,
        template_version,
        prompt_version,
        locale,
    )
}
