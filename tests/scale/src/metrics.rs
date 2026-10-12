//! Latency sampling and threshold verdicts.
//!
//! A single mean is not evidence of an interaction budget: spec 17.5 talks
//! about UI interaction latency, so the tail is the number that matters. Every
//! benchmark therefore records a distribution and is judged on p50, p95 and
//! max simultaneously.

use std::time::{Duration, Instant};

use serde::Serialize;

use crate::thresholds::Budget;

/// A named latency distribution.
#[derive(Debug, Clone)]
pub struct Samples {
    name: &'static str,
    unit: &'static str,
    durations: Vec<Duration>,
}

impl Samples {
    /// Start collecting for a benchmark id.
    ///
    /// `unit` names what one sample is (for example `page` or `query`) so the
    /// published result cannot be read as "per recording" when it is not.
    #[must_use]
    pub fn new(name: &'static str, unit: &'static str) -> Self {
        Self {
            name,
            unit,
            durations: Vec::new(),
        }
    }

    /// Time one operation and keep its result.
    pub fn measure<T>(&mut self, operation: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let value = operation();
        self.durations.push(start.elapsed());
        value
    }

    /// Record an externally timed sample.
    pub fn push(&mut self, elapsed: Duration) {
        self.durations.push(elapsed);
    }

    /// Benchmark id, matching the key in `thresholds.toml`.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Number of samples.
    #[must_use]
    pub fn len(&self) -> usize {
        self.durations.len()
    }

    /// Whether nothing was measured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.durations.is_empty()
    }

    /// Nearest-rank percentile in milliseconds.
    ///
    /// Nearest-rank is used rather than interpolation so a reported p95 is
    /// always a latency that actually occurred.
    #[must_use]
    pub fn percentile_ms(&self, percentile: f64) -> f64 {
        assert!(
            (0.0..=100.0).contains(&percentile),
            "percentile must be within 0..=100"
        );
        if self.durations.is_empty() {
            return 0.0;
        }
        let mut sorted = self.durations.clone();
        sorted.sort_unstable();
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let rank = (percentile / 100.0 * sorted.len() as f64).ceil() as usize;
        let index = rank.saturating_sub(1).min(sorted.len() - 1);
        to_ms(sorted[index])
    }

    /// Slowest sample in milliseconds.
    #[must_use]
    pub fn max_ms(&self) -> f64 {
        self.durations.iter().copied().max().map_or(0.0, to_ms)
    }

    /// Arithmetic mean in milliseconds.
    #[must_use]
    pub fn mean_ms(&self) -> f64 {
        if self.durations.is_empty() {
            return 0.0;
        }
        let total: Duration = self.durations.iter().sum();
        #[allow(clippy::cast_precision_loss)]
        {
            to_ms(total) / self.durations.len() as f64
        }
    }

    /// Total time spent in the measured operations.
    #[must_use]
    pub fn total_ms(&self) -> f64 {
        to_ms(self.durations.iter().sum())
    }

    /// Freeze the distribution into a publishable summary.
    #[must_use]
    pub fn summarize(&self) -> Measurement {
        Measurement {
            name: self.name.to_owned(),
            unit: self.unit.to_owned(),
            samples: self.durations.len() as u64,
            p50_ms: round3(self.percentile_ms(50.0)),
            p95_ms: round3(self.percentile_ms(95.0)),
            max_ms: round3(self.max_ms()),
            mean_ms: round3(self.mean_ms()),
            total_ms: round3(self.total_ms()),
        }
    }
}

fn to_ms(duration: Duration) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        duration.as_nanos() as f64 / 1_000_000.0
    }
}

fn round3(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}

/// A publishable latency summary.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Measurement {
    /// Benchmark id.
    pub name: String,
    /// What one sample measures.
    pub unit: String,
    /// Number of samples behind the percentiles.
    pub samples: u64,
    /// Median latency.
    pub p50_ms: f64,
    /// 95th percentile latency.
    pub p95_ms: f64,
    /// Slowest observed latency.
    pub max_ms: f64,
    /// Mean latency.
    pub mean_ms: f64,
    /// Wall time inside the measured operations.
    pub total_ms: f64,
}

/// Which statistic exceeded its budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Statistic {
    /// Median.
    P50,
    /// 95th percentile.
    P95,
    /// Worst case.
    Max,
}

impl Statistic {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::P50 => "p50",
            Self::P95 => "p95",
            Self::Max => "max",
        }
    }
}

/// The outcome of comparing one measurement against `thresholds.toml`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Verdict {
    /// Benchmark id.
    pub name: String,
    /// What was measured.
    pub measurement: Measurement,
    /// The budget it was judged against.
    pub budget: Budget,
    /// Breaches, empty when the benchmark passed.
    pub breaches: Vec<Breach>,
    /// Worst used fraction of budget across the three statistics, where 1.0 is
    /// exactly at budget.
    pub worst_budget_fraction: f64,
}

impl Verdict {
    /// Judge a measurement.
    #[must_use]
    pub fn judge(measurement: Measurement, budget: Budget) -> Self {
        let pairs = [
            (Statistic::P50, measurement.p50_ms, budget.p50_ms),
            (Statistic::P95, measurement.p95_ms, budget.p95_ms),
            (Statistic::Max, measurement.max_ms, budget.max_ms),
        ];
        let mut breaches = Vec::new();
        let mut worst = 0.0f64;
        for (statistic, measured, allowed) in pairs {
            let fraction = if allowed > 0.0 {
                measured / allowed
            } else {
                f64::INFINITY
            };
            worst = worst.max(fraction);
            if measured > allowed {
                breaches.push(Breach {
                    statistic,
                    measured_ms: measured,
                    allowed_ms: allowed,
                    over_by_ratio: round3(fraction - 1.0),
                });
            }
        }
        Self {
            name: measurement.name.clone(),
            measurement,
            budget,
            breaches,
            worst_budget_fraction: round3(worst),
        }
    }

    /// Whether every statistic stayed inside its budget.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.breaches.is_empty()
    }

    /// A one-line failure description naming the file that has to change.
    #[must_use]
    pub fn failure_report(&self) -> String {
        let detail = self
            .breaches
            .iter()
            .map(|breach| {
                format!(
                    "{} {:.3} ms > {:.3} ms (+{:.1}%)",
                    breach.statistic.as_str(),
                    breach.measured_ms,
                    breach.allowed_ms,
                    breach.over_by_ratio * 100.0
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{}: {detail}. Fix the regression, or justify and edit \
             tests/scale/thresholds.toml.",
            self.name
        )
    }
}

/// One statistic over its budget.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Breach {
    /// Which statistic.
    pub statistic: Statistic,
    /// What was measured.
    pub measured_ms: f64,
    /// What the budget allows.
    pub allowed_ms: f64,
    /// How far over, as a fraction of the budget.
    pub over_by_ratio: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples_from_ms(values: &[u64]) -> Samples {
        let mut samples = Samples::new("unit_test", "op");
        for value in values {
            samples.push(Duration::from_millis(*value));
        }
        samples
    }

    #[test]
    fn percentiles_use_nearest_rank_so_they_are_observed_values() {
        let samples = samples_from_ms(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert!((samples.percentile_ms(50.0) - 5.0).abs() < f64::EPSILON);
        assert!((samples.percentile_ms(95.0) - 10.0).abs() < f64::EPSILON);
        assert!((samples.max_ms() - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_single_slow_sample_breaches_only_the_statistics_it_affects() {
        let mut samples = samples_from_ms(&[1; 99]);
        samples.push(Duration::from_millis(50));
        let budget = Budget {
            p50_ms: 2.0,
            p95_ms: 2.0,
            max_ms: 10.0,
        };
        let verdict = Verdict::judge(samples.summarize(), budget);
        assert!(!verdict.passed());
        assert_eq!(verdict.breaches.len(), 1);
        assert_eq!(verdict.breaches[0].statistic, Statistic::Max);
        assert!(verdict.failure_report().contains("thresholds.toml"));
    }

    #[test]
    fn a_distribution_inside_budget_passes_and_reports_headroom() {
        let samples = samples_from_ms(&[1; 20]);
        let verdict = Verdict::judge(
            samples.summarize(),
            Budget {
                p50_ms: 4.0,
                p95_ms: 4.0,
                max_ms: 4.0,
            },
        );
        assert!(verdict.passed());
        assert!((verdict.worst_budget_fraction - 0.25).abs() < 1e-9);
    }

    #[test]
    fn an_empty_distribution_reports_zeros_rather_than_panicking() {
        let samples = Samples::new("empty", "op");
        assert!(samples.is_empty());
        let summary = samples.summarize();
        assert_eq!(summary.samples, 0);
        assert!((summary.p95_ms - 0.0).abs() < f64::EPSILON);
    }
}
