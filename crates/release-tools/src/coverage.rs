//! Coverage policy enforcement for critical modules (spec 18.9).
//!
//! Spec 18.9 is deliberately narrow: at least 90% *meaningful branch* coverage
//! in security policy, job state transitions, parsing and archive accounting;
//! explicitly no global line-coverage gate for UI adapters or vendor
//! frameworks; and a critical state transition must carry positive, negative
//! and crash-recovery tests regardless of the number.
//!
//! Both halves are enforced here. The numeric half reads a `cargo llvm-cov
//! --json` export; the checklist half reads the versioned policy file, so a
//! threshold change or an exemption is a reviewable diff rather than a flag in
//! a CI script.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The versioned policy file, `coverage-policy.toml` at the repository root.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    /// Policy schema version.
    pub schema_version: u32,
    /// Branch-coverage percentage every critical module must reach.
    pub branch_threshold: f64,
    /// Modules spec 18.9 calls critical.
    pub critical: Vec<CriticalModule>,
    /// Modules explicitly outside the numeric gate, each with a reason.
    pub exempt: Vec<ExemptModule>,
    /// Critical state transitions requiring the three-test checklist.
    pub transitions: Vec<CriticalTransition>,
}

/// A module the numeric branch gate applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriticalModule {
    /// Cargo package the module lives in.
    pub package: String,
    /// Path prefix, relative to the repository root, selecting the files.
    pub path_prefix: String,
    /// Why spec 18.9 treats this module as critical.
    pub rationale: String,
}

/// A module deliberately outside the numeric gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExemptModule {
    /// Path prefix, relative to the repository root.
    pub path_prefix: String,
    /// Why the numeric gate does not apply (spec 18.9 exempts UI adapters and
    /// vendor frameworks).
    pub reason: String,
}

/// A critical state transition and the tests that must cover it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriticalTransition {
    /// Stable identifier, e.g. `jobs::running_to_completed`.
    pub id: String,
    /// Spec section the transition comes from.
    pub spec: String,
    /// Test function name proving the transition happens when it should.
    pub positive_test: String,
    /// Test function name proving it is refused when it must not happen.
    pub negative_test: String,
    /// Test function name proving recovery after an interruption.
    pub crash_recovery_test: String,
    /// Files the three tests live in, relative to the repository root.
    pub test_files: Vec<String>,
}

/// A coverage policy failure.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// A critical module is below the branch threshold.
    BelowThreshold {
        /// Offending module path prefix.
        path_prefix: String,
        /// Measured branch coverage.
        measured: f64,
        /// Required branch coverage.
        required: f64,
    },
    /// A critical module produced no coverage data at all, which must never be
    /// read as "passing".
    NoData {
        /// Offending module path prefix.
        path_prefix: String,
    },
    /// A listed transition is missing one of its three required tests.
    MissingTransitionTest {
        /// Transition id.
        id: String,
        /// Which kind of test is missing.
        kind: String,
        /// Name of the test that could not be found.
        test: String,
    },
    /// A path is listed both as critical and as exempt.
    ContradictoryPolicy {
        /// The duplicated path prefix.
        path_prefix: String,
    },
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BelowThreshold {
                path_prefix,
                measured,
                required,
            } => write!(
                f,
                "{path_prefix}: branch coverage {measured:.2}% is below the required {required:.2}% (spec 18.9)"
            ),
            Self::NoData { path_prefix } => write!(
                f,
                "{path_prefix}: no coverage data was produced; an unmeasured critical module is a failure, not a pass"
            ),
            Self::MissingTransitionTest { id, kind, test } => write!(
                f,
                "critical transition `{id}` has no {kind} test: `{test}` was not found in its listed test files (spec 18.9)"
            ),
            Self::ContradictoryPolicy { path_prefix } => write!(
                f,
                "{path_prefix} is listed as both critical and exempt; the policy must say one thing"
            ),
        }
    }
}

/// Errors raised while loading inputs.
#[derive(Debug, thiserror::Error)]
pub enum CoverageError {
    /// A file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// Offending path.
        path: String,
        /// Underlying error.
        source: std::io::Error,
    },
    /// A file could not be parsed.
    #[error("cannot parse {path}: {detail}")]
    Parse {
        /// Offending path.
        path: String,
        /// Parser message.
        detail: String,
    },
}

/// Loads the policy file.
pub fn load_policy(path: &Path) -> Result<Policy, CoverageError> {
    let text = std::fs::read_to_string(path).map_err(|source| CoverageError::Io {
        path: path.display().to_string(),
        source,
    })?;
    toml::from_str(&text).map_err(|e| CoverageError::Parse {
        path: path.display().to_string(),
        detail: e.to_string(),
    })
}

/// Branch coverage per source file, keyed by repository-relative path.
pub type BranchCoverage = BTreeMap<String, FileCoverage>;

/// Measured branch counts for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FileCoverage {
    /// Branches that were taken at least once.
    pub covered: u64,
    /// Total branches in the file.
    pub total: u64,
}

impl FileCoverage {
    /// Branch coverage percentage; a file with no branches counts as fully
    /// covered so a straight-line module does not fail the gate.
    pub fn percent(self) -> f64 {
        if self.total == 0 {
            100.0
        } else {
            (self.covered as f64) * 100.0 / (self.total as f64)
        }
    }
}

/// Parses a `cargo llvm-cov --json` export into per-file branch counts.
///
/// `repo_root` is stripped from the absolute filenames llvm-cov emits so the
/// result can be matched against the policy's relative path prefixes.
pub fn parse_llvm_cov(json: &str, repo_root: &Path) -> Result<BranchCoverage, CoverageError> {
    let document: serde_json::Value =
        serde_json::from_str(json).map_err(|e| CoverageError::Parse {
            path: "llvm-cov json".to_owned(),
            detail: e.to_string(),
        })?;
    let root = repo_root.to_string_lossy().replace('\\', "/");
    let mut coverage = BranchCoverage::new();
    let files = document
        .get("data")
        .and_then(|data| data.get(0))
        .and_then(|first| first.get("files"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| CoverageError::Parse {
            path: "llvm-cov json".to_owned(),
            detail: "expected data[0].files".to_owned(),
        })?;
    for file in files {
        let Some(filename) = file.get("filename").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let normalized = filename.replace('\\', "/");
        let relative = normalized
            .strip_prefix(&root)
            .map_or(normalized.clone(), |rest| {
                rest.trim_start_matches('/').to_owned()
            });
        let branches = file.get("summary").and_then(|s| s.get("branches"));
        let covered = branches
            .and_then(|b| b.get("covered"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let total = branches
            .and_then(|b| b.get("count"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        coverage.insert(relative, FileCoverage { covered, total });
    }
    Ok(coverage)
}

/// Applies the numeric half of the policy.
pub fn enforce_thresholds(policy: &Policy, coverage: &BranchCoverage) -> Vec<Failure> {
    let mut failures = Vec::new();

    for critical in &policy.critical {
        if policy
            .exempt
            .iter()
            .any(|e| e.path_prefix == critical.path_prefix)
        {
            failures.push(Failure::ContradictoryPolicy {
                path_prefix: critical.path_prefix.clone(),
            });
            continue;
        }
        let mut aggregate = FileCoverage::default();
        let mut matched = false;
        for (path, file) in coverage {
            if !path.starts_with(&critical.path_prefix) {
                continue;
            }
            if policy
                .exempt
                .iter()
                .any(|e| path.starts_with(&e.path_prefix))
            {
                continue;
            }
            matched = true;
            aggregate.covered += file.covered;
            aggregate.total += file.total;
        }
        if !matched {
            failures.push(Failure::NoData {
                path_prefix: critical.path_prefix.clone(),
            });
            continue;
        }
        let measured = aggregate.percent();
        if measured + f64::EPSILON < policy.branch_threshold {
            failures.push(Failure::BelowThreshold {
                path_prefix: critical.path_prefix.clone(),
                measured,
                required: policy.branch_threshold,
            });
        }
    }

    failures
}

/// Applies the checklist half of the policy: every listed transition must have
/// a positive, a negative and a crash-recovery test that actually exists.
pub fn enforce_transition_checklist(policy: &Policy, repo_root: &Path) -> Vec<Failure> {
    let mut failures = Vec::new();
    for transition in &policy.transitions {
        let corpus: String = transition
            .test_files
            .iter()
            .filter_map(|file| std::fs::read_to_string(repo_root.join(file)).ok())
            .collect::<Vec<String>>()
            .join("\n");
        for (kind, test) in [
            ("positive", &transition.positive_test),
            ("negative", &transition.negative_test),
            ("crash-recovery", &transition.crash_recovery_test),
        ] {
            if !corpus.contains(test.as_str()) {
                failures.push(Failure::MissingTransitionTest {
                    id: transition.id.clone(),
                    kind: kind.to_owned(),
                    test: test.clone(),
                });
            }
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            schema_version: 1,
            branch_threshold: 90.0,
            critical: vec![CriticalModule {
                package: "plaudon-jobs".to_owned(),
                path_prefix: "crates/jobs/src".to_owned(),
                rationale: "job state transitions (spec 18.9)".to_owned(),
            }],
            exempt: vec![ExemptModule {
                path_prefix: "apps/desktop".to_owned(),
                reason: "UI adapter (spec 18.9)".to_owned(),
            }],
            transitions: Vec::new(),
        }
    }

    fn coverage(pairs: &[(&str, u64, u64)]) -> BranchCoverage {
        pairs
            .iter()
            .map(|(path, covered, total)| {
                (
                    (*path).to_owned(),
                    FileCoverage {
                        covered: *covered,
                        total: *total,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn a_critical_module_below_the_threshold_fails() {
        let failures = enforce_thresholds(
            &policy(),
            &coverage(&[("crates/jobs/src/state.rs", 80, 100)]),
        );
        assert_eq!(
            failures,
            vec![Failure::BelowThreshold {
                path_prefix: "crates/jobs/src".to_owned(),
                measured: 80.0,
                required: 90.0,
            }]
        );
    }

    #[test]
    fn a_critical_module_at_the_threshold_passes() {
        let failures = enforce_thresholds(
            &policy(),
            &coverage(&[("crates/jobs/src/state.rs", 90, 100)]),
        );
        assert!(failures.is_empty(), "{failures:?}");
    }

    #[test]
    fn an_exempt_module_below_the_threshold_passes() {
        let failures = enforce_thresholds(
            &policy(),
            &coverage(&[
                ("crates/jobs/src/state.rs", 95, 100),
                ("apps/desktop/src-tauri/src/lib.rs", 1, 100),
            ]),
        );
        assert!(failures.is_empty(), "{failures:?}");
    }

    #[test]
    fn a_critical_module_with_no_data_fails_rather_than_silently_passing() {
        let failures =
            enforce_thresholds(&policy(), &coverage(&[("crates/audio/src/lib.rs", 1, 1)]));
        assert_eq!(
            failures,
            vec![Failure::NoData {
                path_prefix: "crates/jobs/src".to_owned()
            }]
        );
    }

    #[test]
    fn a_path_listed_as_both_critical_and_exempt_is_a_policy_error() {
        let mut policy = policy();
        policy.exempt.push(ExemptModule {
            path_prefix: "crates/jobs/src".to_owned(),
            reason: "contradiction".to_owned(),
        });
        let failures =
            enforce_thresholds(&policy, &coverage(&[("crates/jobs/src/state.rs", 99, 100)]));
        assert_eq!(
            failures,
            vec![Failure::ContradictoryPolicy {
                path_prefix: "crates/jobs/src".to_owned()
            }]
        );
    }

    #[test]
    fn llvm_cov_json_is_parsed_into_relative_branch_counts() {
        let json = r#"{"data":[{"files":[
            {"filename":"/repo/crates/jobs/src/state.rs","summary":{"branches":{"covered":9,"count":10}}}
        ]}]}"#;
        let parsed = parse_llvm_cov(json, Path::new("/repo")).expect("parse");
        assert_eq!(
            parsed.get("crates/jobs/src/state.rs").copied(),
            Some(FileCoverage {
                covered: 9,
                total: 10
            })
        );
    }

    #[test]
    fn a_transition_missing_its_crash_recovery_test_fails_the_checklist() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("tests.rs"),
            "fn it_runs() {}\nfn it_refuses() {}\n",
        )
        .expect("write");
        let mut policy = policy();
        policy.transitions.push(CriticalTransition {
            id: "jobs::running_to_completed".to_owned(),
            spec: "14.7".to_owned(),
            positive_test: "it_runs".to_owned(),
            negative_test: "it_refuses".to_owned(),
            crash_recovery_test: "it_recovers".to_owned(),
            test_files: vec!["tests.rs".to_owned()],
        });
        let failures = enforce_transition_checklist(&policy, dir.path());
        assert_eq!(
            failures,
            vec![Failure::MissingTransitionTest {
                id: "jobs::running_to_completed".to_owned(),
                kind: "crash-recovery".to_owned(),
                test: "it_recovers".to_owned(),
            }]
        );
    }

    #[test]
    fn a_fully_tested_transition_passes_the_checklist() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("tests.rs"),
            "fn it_runs() {}\nfn it_refuses() {}\nfn it_recovers() {}\n",
        )
        .expect("write");
        let mut policy = policy();
        policy.transitions.push(CriticalTransition {
            id: "jobs::running_to_completed".to_owned(),
            spec: "14.7".to_owned(),
            positive_test: "it_runs".to_owned(),
            negative_test: "it_refuses".to_owned(),
            crash_recovery_test: "it_recovers".to_owned(),
            test_files: vec!["tests.rs".to_owned()],
        });
        let failures = enforce_transition_checklist(&policy, dir.path());
        assert!(failures.is_empty(), "{failures:?}");
    }
}
