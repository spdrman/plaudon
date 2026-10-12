//! Versioned performance budgets, loaded from `thresholds.toml`.
//!
//! The budgets are data on purpose. A benchmark that drifts past its budget
//! fails the test run, and the only way to accept the new number is to edit
//! `tests/scale/thresholds.toml` — a reviewable, blameable change with the
//! justification next to it. No environment variable and no code path can
//! loosen a threshold at run time.
//!
//! The file is parsed by the small reader below rather than by a TOML crate:
//! the grammar used here is `[section]` headers plus `key = number | string`,
//! and a scale suite should not pull a parser into the dependency set for
//! that. Anything outside the grammar is a hard error, never a silent skip.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

/// The committed budget file, embedded so a run can never read a modified copy
/// from somewhere else on disk.
pub const THRESHOLDS_TOML: &str = include_str!("../thresholds.toml");

/// Path of the file, for failure messages.
pub const THRESHOLDS_PATH: &str = "tests/scale/thresholds.toml";

/// Why a budget file could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThresholdError {
    /// A line did not match the accepted grammar.
    Syntax {
        /// One-based line number.
        line: usize,
        /// What was wrong.
        detail: String,
    },
    /// A required table was absent.
    MissingTable(String),
    /// A required key was absent from a present table.
    MissingKey {
        /// Table the key was expected in.
        table: String,
        /// Key name.
        key: String,
    },
    /// A key existed but had the wrong kind of value.
    WrongType {
        /// Table the key is in.
        table: String,
        /// Key name.
        key: String,
        /// What was expected.
        expected: &'static str,
    },
}

impl fmt::Display for ThresholdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { line, detail } => {
                write!(f, "{THRESHOLDS_PATH}:{line}: {detail}")
            }
            Self::MissingTable(table) => {
                write!(f, "{THRESHOLDS_PATH}: missing table [{table}]")
            }
            Self::MissingKey { table, key } => {
                write!(f, "{THRESHOLDS_PATH}: [{table}] has no key `{key}`")
            }
            Self::WrongType {
                table,
                key,
                expected,
            } => write!(f, "{THRESHOLDS_PATH}: [{table}] `{key}` is not {expected}"),
        }
    }
}

impl std::error::Error for ThresholdError {}

/// A parsed scalar.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// An integer literal, underscores permitted.
    Integer(i64),
    /// A floating point literal.
    Float(f64),
    /// A double-quoted string.
    Text(String),
}

impl Value {
    /// Numeric view: integers widen to floats so a budget may be written `5`.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            #[allow(clippy::cast_precision_loss)]
            Self::Integer(value) => Some(*value as f64),
            Self::Float(value) => Some(*value),
            Self::Text(_) => None,
        }
    }

    /// Non-negative integer view.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Integer(value) => u64::try_from(*value).ok(),
            _ => None,
        }
    }

    /// String view.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }
}

/// Latency budget for one benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Budget {
    /// Median ceiling.
    pub p50_ms: f64,
    /// 95th percentile ceiling.
    pub p95_ms: f64,
    /// Worst-case ceiling.
    pub max_ms: f64,
}

/// The delta policy that governs threshold edits.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Policy {
    /// Format version of the budget file.
    pub schema_version: u64,
    /// Headroom the recorded budgets are expected to carry over a healthy
    /// reference run, as a fraction (0.30 means budgets sit 30% above the
    /// measured number).
    pub headroom_ratio: f64,
    /// How much a budget may be raised in one commit without an ADR.
    pub max_single_raise_ratio: f64,
    /// Floor for every p50 budget.
    ///
    /// An operation whose reference latency is a few hundred microseconds
    /// cannot be budgeted as a multiple of its own measurement: one scheduler
    /// preemption on a busy machine moves it by an order of magnitude while
    /// nothing in the code has changed.
    pub noise_floor_ms: f64,
    /// Floor for every p95 budget.
    pub tail_floor_p95_ms: f64,
    /// Floor for every max budget.
    pub tail_floor_max_ms: f64,
    /// The human rule, reproduced in published results.
    pub rule: String,
}

impl Policy {
    /// Whether a budget respects the floors this policy declares.
    ///
    /// A budget below the floor is not a stricter guard, it is a flaky test.
    #[must_use]
    pub fn respects_floors(&self, budget: Budget) -> bool {
        budget.p50_ms >= self.noise_floor_ms
            && budget.p95_ms >= self.tail_floor_p95_ms
            && budget.max_ms >= self.tail_floor_max_ms
    }
}

/// Memory-bound budget for the chunk-window harness.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MemoryBudget {
    /// Peak working-set growth allowed, as a multiple of the active window.
    pub window_multiple: f64,
    /// Fixed allowance on top, covering allocator and runtime noise.
    pub slack_bytes: u64,
    /// The non-windowed control must grow by at least this fraction of the
    /// total decoded PCM, otherwise the harness is not sensitive enough to
    /// prove anything.
    pub control_detection_ratio: f64,
}

/// All budgets from the file.
#[derive(Debug, Clone, PartialEq)]
pub struct Thresholds {
    tables: BTreeMap<String, BTreeMap<String, Value>>,
}

impl Thresholds {
    /// Load the embedded budget file.
    pub fn load() -> Result<Self, ThresholdError> {
        Self::parse(THRESHOLDS_TOML)
    }

    /// Parse budget text.
    pub fn parse(text: &str) -> Result<Self, ThresholdError> {
        let mut tables: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
        let mut current: Option<String> = None;

        for (offset, raw) in text.lines().enumerate() {
            let line_number = offset + 1;
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                let name = rest
                    .strip_suffix(']')
                    .ok_or_else(|| ThresholdError::Syntax {
                        line: line_number,
                        detail: "table header is not closed with `]`".to_owned(),
                    })?;
                let name = name.trim();
                if name.is_empty() || !name.chars().all(is_key_char) {
                    return Err(ThresholdError::Syntax {
                        line: line_number,
                        detail: format!("`{name}` is not a valid table name"),
                    });
                }
                if tables.contains_key(name) {
                    return Err(ThresholdError::Syntax {
                        line: line_number,
                        detail: format!("table [{name}] is declared twice"),
                    });
                }
                tables.insert(name.to_owned(), BTreeMap::new());
                current = Some(name.to_owned());
                continue;
            }

            let table = current.clone().ok_or_else(|| ThresholdError::Syntax {
                line: line_number,
                detail: "key appears before any table header".to_owned(),
            })?;
            let (key, value) = line.split_once('=').ok_or_else(|| ThresholdError::Syntax {
                line: line_number,
                detail: "expected `key = value`".to_owned(),
            })?;
            let key = key.trim();
            if key.is_empty() || !key.chars().all(is_key_char) {
                return Err(ThresholdError::Syntax {
                    line: line_number,
                    detail: format!("`{key}` is not a valid key"),
                });
            }
            let value = parse_value(value.trim(), line_number)?;
            let entry = tables
                .get_mut(&table)
                .expect("the current table was inserted above");
            if entry.insert(key.to_owned(), value).is_some() {
                return Err(ThresholdError::Syntax {
                    line: line_number,
                    detail: format!("key `{key}` is set twice in [{table}]"),
                });
            }
        }

        Ok(Self { tables })
    }

    /// Every table name, sorted.
    #[must_use]
    pub fn table_names(&self) -> Vec<&str> {
        self.tables.keys().map(String::as_str).collect()
    }

    /// Benchmark ids defined for a tier, sorted.
    #[must_use]
    pub fn benchmark_ids(&self, tier: &str) -> Vec<String> {
        let prefix = format!("{tier}.");
        self.tables
            .keys()
            .filter_map(|name| name.strip_prefix(&prefix).map(ToOwned::to_owned))
            .collect()
    }

    fn table(&self, name: &str) -> Result<&BTreeMap<String, Value>, ThresholdError> {
        self.tables
            .get(name)
            .ok_or_else(|| ThresholdError::MissingTable(name.to_owned()))
    }

    fn float(&self, table: &str, key: &str) -> Result<f64, ThresholdError> {
        self.table(table)?
            .get(key)
            .ok_or_else(|| ThresholdError::MissingKey {
                table: table.to_owned(),
                key: key.to_owned(),
            })?
            .as_f64()
            .ok_or_else(|| ThresholdError::WrongType {
                table: table.to_owned(),
                key: key.to_owned(),
                expected: "a number",
            })
    }

    fn integer(&self, table: &str, key: &str) -> Result<u64, ThresholdError> {
        self.table(table)?
            .get(key)
            .ok_or_else(|| ThresholdError::MissingKey {
                table: table.to_owned(),
                key: key.to_owned(),
            })?
            .as_u64()
            .ok_or_else(|| ThresholdError::WrongType {
                table: table.to_owned(),
                key: key.to_owned(),
                expected: "a non-negative integer",
            })
    }

    fn text(&self, table: &str, key: &str) -> Result<String, ThresholdError> {
        self.table(table)?
            .get(key)
            .ok_or_else(|| ThresholdError::MissingKey {
                table: table.to_owned(),
                key: key.to_owned(),
            })?
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| ThresholdError::WrongType {
                table: table.to_owned(),
                key: key.to_owned(),
                expected: "a quoted string",
            })
    }

    /// The latency budget for `benchmark` at `tier`.
    pub fn budget(&self, tier: &str, benchmark: &str) -> Result<Budget, ThresholdError> {
        let table = format!("{tier}.{benchmark}");
        Ok(Budget {
            p50_ms: self.float(&table, "p50_ms")?,
            p95_ms: self.float(&table, "p95_ms")?,
            max_ms: self.float(&table, "max_ms")?,
        })
    }

    /// The delta policy.
    pub fn policy(&self) -> Result<Policy, ThresholdError> {
        Ok(Policy {
            schema_version: self.integer("policy", "schema_version")?,
            headroom_ratio: self.float("policy", "headroom_ratio")?,
            max_single_raise_ratio: self.float("policy", "max_single_raise_ratio")?,
            noise_floor_ms: self.float("policy", "noise_floor_ms")?,
            tail_floor_p95_ms: self.float("policy", "tail_floor_p95_ms")?,
            tail_floor_max_ms: self.float("policy", "tail_floor_max_ms")?,
            rule: self.text("policy", "rule")?,
        })
    }

    /// The memory-bound budget.
    pub fn memory(&self) -> Result<MemoryBudget, ThresholdError> {
        Ok(MemoryBudget {
            window_multiple: self.float("memory", "window_multiple")?,
            slack_bytes: self.integer("memory", "slack_bytes")?,
            control_detection_ratio: self.float("memory", "control_detection_ratio")?,
        })
    }
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-'
}

fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'"' => in_string = !in_string,
            b'#' if !in_string => return &line[..index],
            _ => {}
        }
    }
    line
}

fn parse_value(raw: &str, line: usize) -> Result<Value, ThresholdError> {
    if let Some(inner) = raw.strip_prefix('"') {
        let text = inner
            .strip_suffix('"')
            .ok_or_else(|| ThresholdError::Syntax {
                line,
                detail: "string value is not closed with `\"`".to_owned(),
            })?;
        if text.contains('"') {
            return Err(ThresholdError::Syntax {
                line,
                detail: "string values may not contain quotes".to_owned(),
            });
        }
        return Ok(Value::Text(text.to_owned()));
    }
    let digits = raw.replace('_', "");
    if digits.is_empty() {
        return Err(ThresholdError::Syntax {
            line,
            detail: "value is empty".to_owned(),
        });
    }
    if digits.contains('.') || digits.contains('e') || digits.contains('E') {
        return digits
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| ThresholdError::Syntax {
                line,
                detail: format!("`{raw}` is not a number"),
            });
    }
    digits
        .parse::<i64>()
        .map(Value::Integer)
        .map_err(|_| ThresholdError::Syntax {
            line,
            detail: format!("`{raw}` is not a number or a quoted string"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_file_parses_and_exposes_its_policy() {
        let thresholds = Thresholds::load().expect("committed thresholds must parse");
        let policy = thresholds.policy().expect("policy table");
        assert_eq!(policy.schema_version, 1);
        assert!(policy.headroom_ratio > 0.0);
        assert_ne!(policy.rule, "", "the delta policy must be documented");
    }

    #[test]
    fn numbers_integers_and_strings_round_trip() {
        let parsed = Thresholds::parse(
            "# comment\n[a.b]\np50_ms = 1.5\np95_ms = 3\nmax_ms = 10\n\n[c]\nnote = \"hi # there\"\n",
        )
        .unwrap();
        let budget = parsed.budget("a", "b").unwrap();
        assert!((budget.p50_ms - 1.5).abs() < f64::EPSILON);
        assert!((budget.p95_ms - 3.0).abs() < f64::EPSILON);
        assert_eq!(parsed.text("c", "note").unwrap(), "hi # there");
        assert_eq!(parsed.benchmark_ids("a"), vec!["b".to_owned()]);
    }

    #[test]
    fn malformed_input_is_rejected_rather_than_skipped() {
        assert!(matches!(
            Thresholds::parse("p50_ms = 1"),
            Err(ThresholdError::Syntax { line: 1, .. })
        ));
        assert!(matches!(
            Thresholds::parse("[a\np50_ms = 1"),
            Err(ThresholdError::Syntax { line: 1, .. })
        ));
        assert!(matches!(
            Thresholds::parse("[a]\np50_ms = 1\np50_ms = 2"),
            Err(ThresholdError::Syntax { line: 3, .. })
        ));
        assert!(matches!(
            Thresholds::parse("[a]\np50_ms = oops"),
            Err(ThresholdError::Syntax { line: 2, .. })
        ));
        assert!(matches!(
            Thresholds::parse("[a]\n[a]\n"),
            Err(ThresholdError::Syntax { line: 2, .. })
        ));
    }

    #[test]
    fn a_missing_budget_is_an_error_and_names_the_file() {
        let thresholds = Thresholds::load().unwrap();
        let error = thresholds.budget("ci", "no_such_benchmark").unwrap_err();
        assert!(error.to_string().contains(THRESHOLDS_PATH));
        let wrong_type = Thresholds::parse("[ci.x]\np50_ms = \"fast\"\n")
            .unwrap()
            .budget("ci", "x")
            .unwrap_err();
        assert!(matches!(wrong_type, ThresholdError::WrongType { .. }));
    }
}
