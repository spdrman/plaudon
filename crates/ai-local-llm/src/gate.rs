//! The footprint and licence gate (spec 3.1, 12).
//!
//! Spec 3.1 lists cross-platform local LLM summarization as P1 "after
//! footprint and model-license gate". Spec 12 requires measured laptop-CPU,
//! Metal and Windows-GPU behaviour before the feature is user-visible.
//!
//! The gate is therefore a **checked artifact**, not a code comment: a JSON
//! document under `docs/benchmarks/` recording the measurement and the
//! redistribution review. The feature reports itself unavailable until that
//! document exists, parses, covers the host platform and passes its
//! thresholds. There is no build flag, environment variable or developer
//! override that turns the feature on without the measurement, because any
//! such switch would be the thing that ships by accident.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Path of the gate artifact, relative to the repository root.
pub const GATE_ARTIFACT_PATH: &str = "docs/benchmarks/local-llm-footprint.json";

/// Schema version of the gate artifact.
pub const GATE_SCHEMA_VERSION: u32 = 1;

/// Thresholds a measurement must meet for the feature to be offered.
///
/// They are deliberately modest: the question the gate answers is "does this
/// run acceptably on a laptop", not "is this competitive with a cloud model".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FootprintThresholds {
    /// Peak resident set size the measurement must stay under.
    pub max_peak_rss_bytes: u64,
    /// Minimum generation throughput.
    pub min_tokens_per_second: u32,
    /// Minimum verified context length.
    pub min_context_tokens: u64,
    /// Largest model file the gate will approve, so a 40 GB weight file
    /// cannot pass by being fast on one machine.
    pub max_model_bytes: u64,
}

impl Default for FootprintThresholds {
    fn default() -> Self {
        Self {
            max_peak_rss_bytes: 8 * 1024 * 1024 * 1024,
            min_tokens_per_second: 5,
            min_context_tokens: 8_192,
            max_model_bytes: 12 * 1024 * 1024 * 1024,
        }
    }
}

/// One measured platform.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlatformMeasurement {
    /// `macos-arm64`, `windows-x86_64`, `linux-x86_64`.
    pub platform: String,
    /// Compute backend exercised: `cpu`, `metal`, `cuda`, `vulkan`.
    pub backend: String,
    /// Host description, so the number is attributable.
    pub hardware: String,
    /// OS build the measurement ran on.
    pub os_build: String,
    /// Peak resident set size observed, in bytes.
    pub peak_rss_bytes: u64,
    /// Generation throughput observed.
    pub tokens_per_second: f64,
    /// Context length the runtime actually accepted.
    pub verified_context_tokens: u64,
}

/// The licence review that accompanies the measurement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenceReview {
    /// Model identifier the review covers.
    pub model_id: String,
    /// SPDX identifier or licence name.
    pub licence: String,
    /// Whether the licence permits us to redistribute the weights.
    ///
    /// `false` is a perfectly good outcome: it means the model manager
    /// downloads from the publisher instead, which spec 12 already requires
    /// to be an explicit user action.
    pub redistribution_permitted: bool,
    /// Whether obtaining the weights requires an account or gated token.
    pub requires_gated_token: bool,
    /// Who reviewed it and when.
    pub reviewed_by: String,
    /// ISO-8601 date of the review.
    pub reviewed_at: String,
}

/// The gate artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FootprintGateArtifact {
    /// Schema version.
    pub schema_version: u32,
    /// Model the gate was measured against.
    pub model_id: String,
    /// Size of the model file in bytes.
    pub model_bytes: u64,
    /// Measurements, at least one per supported platform.
    pub measurements: Vec<PlatformMeasurement>,
    /// Licence review.
    pub licence: LicenceReview,
}

/// Why the feature is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum GateRefusal {
    /// The artifact does not exist.
    ArtifactMissing {
        /// Path that was looked for.
        path: String,
    },
    /// The artifact exists but could not be read as the expected schema.
    ArtifactUnreadable {
        /// Parser message.
        detail: String,
    },
    /// The artifact is a schema version this build does not understand.
    ArtifactVersionUnsupported {
        /// Version found.
        found: u32,
    },
    /// No measurement covers the running platform.
    PlatformNotMeasured {
        /// Platform that was looked for.
        platform: String,
    },
    /// A measurement exists but failed a threshold.
    ThresholdNotMet {
        /// Which threshold.
        metric: String,
        /// Threshold value, rendered.
        threshold: String,
        /// Observed value, rendered.
        observed: String,
    },
    /// The licence review has not been completed.
    LicenceUnreviewed,
}

impl GateRefusal {
    /// A short sentence explaining the refusal to a user.
    #[must_use]
    pub fn explain(&self) -> String {
        match self {
            Self::ArtifactMissing { .. } => {
                "Local text generation has not been benchmarked for this build, so it is \
                 turned off."
                    .to_string()
            }
            Self::ArtifactUnreadable { .. } | Self::ArtifactVersionUnsupported { .. } => {
                "The local text generation benchmark record could not be read, so the feature \
                 stays off."
                    .to_string()
            }
            Self::PlatformNotMeasured { .. } => {
                "Local text generation has not been benchmarked on this kind of machine, so it \
                 is unavailable here."
                    .to_string()
            }
            Self::ThresholdNotMet { metric, .. } => format!(
                "Local text generation did not meet the {metric} requirement on this platform, \
                 so it is unavailable."
            ),
            Self::LicenceUnreviewed => {
                "The licence for the local model has not been reviewed, so the feature stays \
                 off."
                    .to_string()
            }
        }
    }
}

/// The gate's verdict for one host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GateVerdict {
    /// The feature may be offered.
    Passed {
        /// Model the gate approved.
        model_id: String,
        /// Context length the measurement verified on this platform.
        verified_context_tokens: u64,
        /// Backend the measurement used.
        backend: String,
    },
    /// The feature must stay hidden.
    Refused(GateRefusal),
}

impl GateVerdict {
    /// Whether the feature may be shown at all.
    #[must_use]
    pub const fn is_passed(&self) -> bool {
        matches!(self, Self::Passed { .. })
    }

    /// The verified context length, when the gate passed.
    #[must_use]
    pub const fn verified_context_tokens(&self) -> Option<u64> {
        match self {
            Self::Passed {
                verified_context_tokens,
                ..
            } => Some(*verified_context_tokens),
            Self::Refused(_) => None,
        }
    }
}

/// The platform string used to match measurements, for the running host.
#[must_use]
pub fn current_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Evaluate the gate from an in-memory artifact.
#[must_use]
pub fn evaluate(
    artifact: &FootprintGateArtifact,
    platform: &str,
    thresholds: FootprintThresholds,
) -> GateVerdict {
    if artifact.schema_version != GATE_SCHEMA_VERSION {
        return GateVerdict::Refused(GateRefusal::ArtifactVersionUnsupported {
            found: artifact.schema_version,
        });
    }
    if artifact.licence.licence.trim().is_empty() || artifact.licence.reviewed_by.trim().is_empty()
    {
        return GateVerdict::Refused(GateRefusal::LicenceUnreviewed);
    }
    if artifact.model_bytes > thresholds.max_model_bytes {
        return GateVerdict::Refused(GateRefusal::ThresholdNotMet {
            metric: "model size".to_string(),
            threshold: thresholds.max_model_bytes.to_string(),
            observed: artifact.model_bytes.to_string(),
        });
    }

    let Some(measurement) = artifact
        .measurements
        .iter()
        .find(|m| m.platform == platform)
    else {
        return GateVerdict::Refused(GateRefusal::PlatformNotMeasured {
            platform: platform.to_string(),
        });
    };

    if measurement.peak_rss_bytes > thresholds.max_peak_rss_bytes {
        return GateVerdict::Refused(GateRefusal::ThresholdNotMet {
            metric: "peak memory".to_string(),
            threshold: thresholds.max_peak_rss_bytes.to_string(),
            observed: measurement.peak_rss_bytes.to_string(),
        });
    }
    if measurement.tokens_per_second < f64::from(thresholds.min_tokens_per_second) {
        return GateVerdict::Refused(GateRefusal::ThresholdNotMet {
            metric: "throughput".to_string(),
            threshold: thresholds.min_tokens_per_second.to_string(),
            observed: format!("{:.2}", measurement.tokens_per_second),
        });
    }
    if measurement.verified_context_tokens < thresholds.min_context_tokens {
        return GateVerdict::Refused(GateRefusal::ThresholdNotMet {
            metric: "context length".to_string(),
            threshold: thresholds.min_context_tokens.to_string(),
            observed: measurement.verified_context_tokens.to_string(),
        });
    }

    GateVerdict::Passed {
        model_id: artifact.model_id.clone(),
        verified_context_tokens: measurement.verified_context_tokens,
        backend: measurement.backend.clone(),
    }
}

/// Read the gate artifact and evaluate it for the running host.
#[must_use]
pub fn evaluate_at(path: &Path, platform: &str, thresholds: FootprintThresholds) -> GateVerdict {
    if !path.exists() {
        return GateVerdict::Refused(GateRefusal::ArtifactMissing {
            path: path.display().to_string(),
        });
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            return GateVerdict::Refused(GateRefusal::ArtifactUnreadable {
                detail: error.to_string(),
            })
        }
    };
    match serde_json::from_str::<FootprintGateArtifact>(&text) {
        Ok(artifact) => evaluate(&artifact, platform, thresholds),
        Err(error) => GateVerdict::Refused(GateRefusal::ArtifactUnreadable {
            detail: error.to_string(),
        }),
    }
}

/// Locate the gate artifact relative to a repository or installation root.
#[must_use]
pub fn artifact_path(root: &Path) -> PathBuf {
    root.join(GATE_ARTIFACT_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact() -> FootprintGateArtifact {
        FootprintGateArtifact {
            schema_version: GATE_SCHEMA_VERSION,
            model_id: "fixture-3b-instruct-q4".to_string(),
            model_bytes: 2 * 1024 * 1024 * 1024,
            measurements: vec![PlatformMeasurement {
                platform: "macos-aarch64".to_string(),
                backend: "metal".to_string(),
                hardware: "Apple M2, 16 GB".to_string(),
                os_build: "macOS 15.0 (24A335)".to_string(),
                peak_rss_bytes: 3 * 1024 * 1024 * 1024,
                tokens_per_second: 24.0,
                verified_context_tokens: 32_768,
            }],
            licence: LicenceReview {
                model_id: "fixture-3b-instruct-q4".to_string(),
                licence: "Apache-2.0".to_string(),
                redistribution_permitted: true,
                requires_gated_token: false,
                reviewed_by: "plaudon maintainers".to_string(),
                reviewed_at: "2026-01-15".to_string(),
            },
        }
    }

    #[test]
    fn a_measured_platform_within_budget_passes() {
        let verdict = evaluate(&artifact(), "macos-aarch64", FootprintThresholds::default());
        assert!(verdict.is_passed());
        assert_eq!(verdict.verified_context_tokens(), Some(32_768));
    }

    #[test]
    fn an_unmeasured_platform_is_refused_with_an_explanation() {
        let verdict = evaluate(
            &artifact(),
            "windows-x86_64",
            FootprintThresholds::default(),
        );
        assert_eq!(
            verdict,
            GateVerdict::Refused(GateRefusal::PlatformNotMeasured {
                platform: "windows-x86_64".to_string()
            })
        );
        let GateVerdict::Refused(refusal) = verdict else {
            unreachable!()
        };
        assert!(refusal.explain().contains("not been benchmarked"));
    }

    #[test]
    fn a_missing_artifact_keeps_the_feature_off() {
        let dir = tempfile::tempdir().unwrap();
        let verdict = evaluate_at(
            &artifact_path(dir.path()),
            "macos-aarch64",
            FootprintThresholds::default(),
        );
        assert!(!verdict.is_passed());
        assert!(matches!(
            verdict,
            GateVerdict::Refused(GateRefusal::ArtifactMissing { .. })
        ));
    }

    #[test]
    fn a_corrupt_artifact_keeps_the_feature_off_rather_than_defaulting_on() {
        let dir = tempfile::tempdir().unwrap();
        let path = artifact_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            evaluate_at(&path, "macos-aarch64", FootprintThresholds::default()),
            GateVerdict::Refused(GateRefusal::ArtifactUnreadable { .. })
        ));
    }

    #[test]
    fn a_readable_artifact_on_disk_passes_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = artifact_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&artifact()).unwrap()).unwrap();
        assert!(evaluate_at(&path, "macos-aarch64", FootprintThresholds::default()).is_passed());
    }

    #[test]
    fn exceeding_the_memory_budget_is_refused() {
        let mut artifact = artifact();
        artifact.measurements[0].peak_rss_bytes = 64 * 1024 * 1024 * 1024;
        let verdict = evaluate(&artifact, "macos-aarch64", FootprintThresholds::default());
        assert!(matches!(
            verdict,
            GateVerdict::Refused(GateRefusal::ThresholdNotMet { ref metric, .. }) if metric == "peak memory"
        ));
    }

    #[test]
    fn too_slow_or_too_short_a_context_is_refused() {
        let mut slow = artifact();
        slow.measurements[0].tokens_per_second = 0.4;
        assert!(!evaluate(&slow, "macos-aarch64", FootprintThresholds::default()).is_passed());

        let mut short = artifact();
        short.measurements[0].verified_context_tokens = 2_048;
        assert!(!evaluate(&short, "macos-aarch64", FootprintThresholds::default()).is_passed());
    }

    #[test]
    fn an_oversized_model_is_refused_even_on_a_fast_machine() {
        let mut artifact = artifact();
        artifact.model_bytes = 64 * 1024 * 1024 * 1024;
        assert!(!evaluate(&artifact, "macos-aarch64", FootprintThresholds::default()).is_passed());
    }

    #[test]
    fn an_unreviewed_licence_is_refused() {
        let mut artifact = artifact();
        artifact.licence.licence = String::new();
        assert_eq!(
            evaluate(&artifact, "macos-aarch64", FootprintThresholds::default()),
            GateVerdict::Refused(GateRefusal::LicenceUnreviewed)
        );
    }

    #[test]
    fn a_future_schema_version_is_refused_rather_than_guessed_at() {
        let mut artifact = artifact();
        artifact.schema_version = 99;
        assert_eq!(
            evaluate(&artifact, "macos-aarch64", FootprintThresholds::default()),
            GateVerdict::Refused(GateRefusal::ArtifactVersionUnsupported { found: 99 })
        );
    }

    #[test]
    fn the_shipped_repository_state_keeps_the_feature_off() {
        // No measurement has been taken on real hardware for this build, so
        // the committed tree must not enable local text generation. If this
        // test starts failing, a gate artifact was added and that is the
        // moment to review it.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let verdict = evaluate_at(
            &artifact_path(root),
            &current_platform(),
            FootprintThresholds::default(),
        );
        assert!(
            !verdict.is_passed(),
            "the committed tree must not ship a passing local-LLM gate: {verdict:?}"
        );
    }

    #[test]
    fn the_current_platform_string_is_well_formed() {
        let platform = current_platform();
        assert!(platform.contains('-'));
        assert!(!platform.starts_with('-'));
    }
}
