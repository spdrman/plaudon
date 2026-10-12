//! Threshold-based gating for evaluation metrics.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::report::EvalReport;

/// Threshold configuration (§17.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    /// Minimum anchor correctness for Perfect profile.
    pub anchor_correctness_perfect: f64,
    /// Minimum schema validity rate for Perfect profile.
    pub schema_validity_perfect: f64,
}

impl Thresholds {
    /// Load thresholds from a JSON file.
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, ThresholdError> {
        let content = std::fs::read_to_string(path)?;
        let thresholds: Self = serde_json::from_str(&content)?;
        Ok(thresholds)
    }

    /// Default thresholds (will be overwritten after baseline measurement).
    #[must_use]
    pub fn default_thresholds() -> Self {
        Self {
            anchor_correctness_perfect: 0.95,
            schema_validity_perfect: 1.0,
        }
    }
}

/// Result of applying the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateResult {
    /// All checks passed.
    Pass,
    /// One or more checks failed.
    Fail {
        /// Reasons for failure.
        reasons: Vec<String>,
    },
}

impl GateResult {
    /// Whether the gate passed.
    #[must_use]
    pub const fn passed(&self) -> bool {
        matches!(self, Self::Pass)
    }

    /// Failure reasons, if any.
    #[must_use]
    pub fn reasons(&self) -> Vec<String> {
        match self {
            Self::Pass => Vec::new(),
            Self::Fail { reasons } => reasons.clone(),
        }
    }
}

/// Error loading or applying thresholds.
#[derive(Debug, thiserror::Error)]
pub enum ThresholdError {
    /// I/O failure.
    #[error("failed to read threshold file: {0}")]
    Io(#[from] std::io::Error),
    /// JSON deserialization failure.
    #[error("failed to parse thresholds: {0}")]
    Json(#[from] serde_json::Error),
}

/// Apply the gate to a report.
///
/// For the Perfect profile, anchor correctness and schema validity must meet
/// or exceed the configured thresholds. Regression below the floor fails the gate.
#[must_use]
pub fn apply_gate(report: &EvalReport, thresholds: &Thresholds) -> GateResult {
    let mut reasons = Vec::new();

    // Check anchor correctness
    if let Some(anchor_correctness) = report.aggregate.anchor_correctness {
        if anchor_correctness < thresholds.anchor_correctness_perfect {
            reasons.push(format!(
                "Anchor correctness {:.4} below threshold {:.4}",
                anchor_correctness, thresholds.anchor_correctness_perfect
            ));
        }
    } else {
        reasons.push("Anchor correctness is undefined (no anchors)".to_owned());
    }

    // Check schema validity
    if let Some(schema_validity) = report.aggregate.schema_validity_rate {
        if schema_validity < thresholds.schema_validity_perfect {
            reasons.push(format!(
                "Schema validity rate {:.4} below threshold {:.4}",
                schema_validity, thresholds.schema_validity_perfect
            ));
        }
    } else {
        reasons.push("Schema validity rate is undefined (no items)".to_owned());
    }

    if reasons.is_empty() {
        GateResult::Pass
    } else {
        GateResult::Fail { reasons }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::EvalMetrics;
    use crate::report::EvalReport;

    #[test]
    fn gate_passes_when_metrics_exceed_thresholds() {
        let mut report = EvalReport::new("1.0.0", "general_meeting", "1.0.0", "1.0.0", "en-US");
        report.aggregate = EvalMetrics {
            anchor_correctness: Some(1.0),
            anchor_sample_size: 100,
            schema_validity_rate: Some(1.0),
            schema_sample_size: 50,
            ..Default::default()
        };

        let thresholds = Thresholds {
            anchor_correctness_perfect: 0.95,
            schema_validity_perfect: 1.0,
        };

        let result = apply_gate(&report, &thresholds);
        assert!(result.passed());
    }

    #[test]
    fn gate_fails_when_anchor_correctness_below_threshold() {
        let mut report = EvalReport::new("1.0.0", "general_meeting", "1.0.0", "1.0.0", "en-US");
        report.aggregate = EvalMetrics {
            anchor_correctness: Some(0.90),
            anchor_sample_size: 100,
            schema_validity_rate: Some(1.0),
            schema_sample_size: 50,
            ..Default::default()
        };

        let thresholds = Thresholds {
            anchor_correctness_perfect: 0.95,
            schema_validity_perfect: 1.0,
        };

        let result = apply_gate(&report, &thresholds);
        assert!(!result.passed());
        assert_eq!(result.reasons().len(), 1);
    }
}
