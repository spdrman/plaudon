//! Local, in-process quality metrics (§17.4).
//!
//! §17.4 requires us to *measure* watcher import latency after true file
//! completion, duplicate-event coalescing, cloud-placeholder retries, safe
//! preservation of input files, and the fraction of missed changes recovered
//! by reconciliation. "Measure" here means locally queryable numbers, not
//! telemetry: nothing in this module serialises to a network destination, and
//! the registry holds counters and millisecond samples only — never a path, a
//! file name or any user content (§16.6).
//!
//! The registry is deliberately tiny. A sample is a count, a sum and the
//! extremes, which is enough to answer "did the watcher import promptly" and
//! "did a burst of events collapse into one attempt" without accumulating an
//! unbounded series in memory.

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;

/// The §17.4 indicators the watcher and the inventory sweep record.
///
/// The variants are the complete set: a metric that is not here is not
/// recorded, rather than being invented at a call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WatcherMetric {
    /// Milliseconds between a file genuinely becoming complete and its import
    /// being committed.
    ImportLatencyMs,
    /// Filesystem event hints accepted into the coalescing queue.
    EventHintsObserved,
    /// Import attempts actually started. One burst should produce one attempt.
    ImportAttempts,
    /// Times a candidate was deferred because its bytes were not local.
    PlaceholderRetries,
    /// Times a source file was verified to be byte-identical after an import.
    SourcePreservationChecks,
    /// Times such a verification failed. Must stay zero.
    SourcePreservationFailures,
    /// Changes the watcher provably did not report (dropped or overflowed).
    ChangesMissedByWatcher,
    /// Of those, the ones a bounded inventory sweep subsequently found.
    ChangesRecoveredByInventory,
}

impl WatcherMetric {
    /// Stable key, used when a snapshot is rendered for the diagnostics pane.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::ImportLatencyMs => "watcher.import_latency_ms",
            Self::EventHintsObserved => "watcher.event_hints_observed",
            Self::ImportAttempts => "watcher.import_attempts",
            Self::PlaceholderRetries => "watcher.placeholder_retries",
            Self::SourcePreservationChecks => "watcher.source_preservation_checks",
            Self::SourcePreservationFailures => "watcher.source_preservation_failures",
            Self::ChangesMissedByWatcher => "watcher.changes_missed",
            Self::ChangesRecoveredByInventory => "watcher.changes_recovered_by_inventory",
        }
    }

    /// Every variant, for exhaustive snapshot tests.
    pub const ALL: [Self; 8] = [
        Self::ImportLatencyMs,
        Self::EventHintsObserved,
        Self::ImportAttempts,
        Self::PlaceholderRetries,
        Self::SourcePreservationChecks,
        Self::SourcePreservationFailures,
        Self::ChangesMissedByWatcher,
        Self::ChangesRecoveredByInventory,
    ];
}

/// Aggregate of one metric.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetricSeries {
    /// Number of observations.
    pub count: u64,
    /// Sum of the observed values. For a counter every value is one, so this
    /// equals `count`.
    pub sum: u64,
    /// Smallest observation, if any.
    pub min: Option<u64>,
    /// Largest observation, if any.
    pub max: Option<u64>,
}

impl MetricSeries {
    /// Arithmetic mean, or `None` when nothing was observed.
    ///
    /// Returning `None` rather than zero matters: "no import has completed
    /// yet" and "imports complete instantly" are different statements.
    #[must_use]
    pub fn mean(&self) -> Option<f64> {
        if self.count == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        Some(self.sum as f64 / self.count as f64)
    }

    fn observe(&mut self, value: u64) {
        self.count += 1;
        self.sum = self.sum.saturating_add(value);
        self.min = Some(self.min.map_or(value, |current| current.min(value)));
        self.max = Some(self.max.map_or(value, |current| current.max(value)));
    }
}

/// The §17.4 view a diagnostics pane shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WatcherMetricsSnapshot {
    /// Import latency after true completion.
    pub import_latency: MetricSeries,
    /// Event hints accepted.
    pub event_hints: u64,
    /// Import attempts started.
    pub import_attempts: u64,
    /// Placeholder retries.
    pub placeholder_retries: u64,
    /// Source files verified unchanged after import.
    pub source_preservation_checks: u64,
    /// Verifications that failed.
    pub source_preservation_failures: u64,
    /// Changes the watcher missed.
    pub changes_missed: u64,
    /// Missed changes a sweep recovered.
    pub changes_recovered_by_inventory: u64,
}

impl WatcherMetricsSnapshot {
    /// Fraction of event hints that did *not* turn into a separate import
    /// attempt, i.e. how well bursts coalesced. `None` before any hint.
    #[must_use]
    pub fn coalescing_ratio(&self) -> Option<f64> {
        if self.event_hints == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let ratio = 1.0 - (self.import_attempts as f64 / self.event_hints as f64);
        Some(ratio.clamp(0.0, 1.0))
    }

    /// Fraction of provably missed changes that the inventory sweep recovered.
    /// `None` when nothing was missed, because 0/0 is not 100 %.
    #[must_use]
    pub fn missed_change_recovery_fraction(&self) -> Option<f64> {
        if self.changes_missed == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let fraction = self.changes_recovered_by_inventory as f64 / self.changes_missed as f64;
        Some(fraction.clamp(0.0, 1.0))
    }

    /// Whether every preservation check passed. §22(11): an import never
    /// modifies the file it read.
    #[must_use]
    pub const fn source_files_preserved(&self) -> bool {
        self.source_preservation_failures == 0
    }
}

/// A process-local metric registry.
///
/// Cloning shares the same storage, so a watcher, an inventory sweep and the
/// UI can hold handles to one set of numbers without an ambient global.
#[derive(Debug, Clone, Default)]
pub struct MetricsRegistry {
    series: Arc<Mutex<BTreeMap<&'static str, MetricSeries>>>,
}

impl MetricsRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one observation of `metric`.
    pub fn observe(&self, metric: WatcherMetric, value: u64) {
        self.series
            .lock()
            .entry(metric.key())
            .or_default()
            .observe(value);
    }

    /// Record one occurrence of a counting metric.
    pub fn increment(&self, metric: WatcherMetric) {
        self.observe(metric, 1);
    }

    /// Aggregate for one metric; all-zero when nothing was recorded.
    #[must_use]
    pub fn series(&self, metric: WatcherMetric) -> MetricSeries {
        self.series
            .lock()
            .get(metric.key())
            .copied()
            .unwrap_or_default()
    }

    /// Number of observations of a counting metric.
    #[must_use]
    pub fn count(&self, metric: WatcherMetric) -> u64 {
        self.series(metric).count
    }

    /// The §17.4 snapshot.
    #[must_use]
    pub fn watcher_snapshot(&self) -> WatcherMetricsSnapshot {
        WatcherMetricsSnapshot {
            import_latency: self.series(WatcherMetric::ImportLatencyMs),
            event_hints: self.count(WatcherMetric::EventHintsObserved),
            import_attempts: self.count(WatcherMetric::ImportAttempts),
            placeholder_retries: self.count(WatcherMetric::PlaceholderRetries),
            source_preservation_checks: self.count(WatcherMetric::SourcePreservationChecks),
            source_preservation_failures: self.count(WatcherMetric::SourcePreservationFailures),
            changes_missed: self.count(WatcherMetric::ChangesMissedByWatcher),
            changes_recovered_by_inventory: self.count(WatcherMetric::ChangesRecoveredByInventory),
        }
    }

    /// Every recorded series, keyed by [`WatcherMetric::key`].
    #[must_use]
    pub fn all(&self) -> BTreeMap<&'static str, MetricSeries> {
        self.series.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unobserved_metric_reads_as_empty_rather_than_zero_valued() {
        let registry = MetricsRegistry::new();
        assert_eq!(registry.series(WatcherMetric::ImportLatencyMs).mean(), None);
        assert_eq!(registry.watcher_snapshot().coalescing_ratio(), None);
        assert_eq!(
            registry
                .watcher_snapshot()
                .missed_change_recovery_fraction(),
            None
        );
    }

    #[test]
    fn a_burst_that_produces_one_attempt_reports_a_high_coalescing_ratio() {
        let registry = MetricsRegistry::new();
        for _ in 0..10 {
            registry.increment(WatcherMetric::EventHintsObserved);
        }
        registry.increment(WatcherMetric::ImportAttempts);
        let snapshot = registry.watcher_snapshot();
        assert!((snapshot.coalescing_ratio().unwrap() - 0.9).abs() < 1e-9);
    }

    #[test]
    fn latency_samples_keep_their_extremes() {
        let registry = MetricsRegistry::new();
        registry.observe(WatcherMetric::ImportLatencyMs, 40);
        registry.observe(WatcherMetric::ImportLatencyMs, 100);
        let series = registry.series(WatcherMetric::ImportLatencyMs);
        assert_eq!(
            (series.min, series.max, series.count),
            (Some(40), Some(100), 2)
        );
        assert!((series.mean().unwrap() - 70.0).abs() < 1e-9);
    }

    #[test]
    fn every_metric_has_a_distinct_key() {
        let keys: std::collections::BTreeSet<&str> =
            WatcherMetric::ALL.iter().map(|m| m.key()).collect();
        assert_eq!(keys.len(), WatcherMetric::ALL.len());
    }
}
