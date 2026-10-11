//! Minimal job-side context handed to AI adapters (§6.5).
//!
//! "We will keep cancellation tokens in `JobContext` and use a bounded worker
//! pool." This type is deliberately dependency-free so that `crates/ai-*` do
//! not take a dependency on the job engine; the job engine constructs it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Cooperative cancellation signal.
///
/// Cancellation is cooperative: a worker polls [`CancellationToken::is_cancelled`]
/// at bounded intervals and unwinds, discarding incomplete derivatives (§14.7).
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
}

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    #[inline]
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Convenience guard for loops: `ctx.cancel.bail_if_cancelled()?`.
    pub fn bail_if_cancelled(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("operation cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// A monotonic progress report.
///
/// `completed`/`total` are in engine-defined units (samples, chunks); `total`
/// is `None` when the engine genuinely cannot state one — we do not fabricate
/// a denominator to make a progress bar look determinate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub stage: String,
    pub completed: u64,
    pub total: Option<u64>,
    pub detail: Option<String>,
}

impl Progress {
    pub fn new(stage: impl Into<String>, completed: u64, total: Option<u64>) -> Self {
        Self {
            stage: stage.into(),
            completed,
            total,
            detail: None,
        }
    }

    #[must_use]
    pub fn fraction(&self) -> Option<f64> {
        match self.total {
            Some(total) if total > 0 => Some((self.completed as f64 / total as f64).min(1.0)),
            _ => None,
        }
    }
}

/// Sink for progress reports. Implementations must be non-blocking: a slow or
/// absent UI must never stall a worker (§17.5).
pub trait ProgressSink: Send + Sync {
    fn report(&self, progress: Progress);
}

/// Drops every report. Used when nothing is observing.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullProgressSink;

impl ProgressSink for NullProgressSink {
    fn report(&self, _progress: Progress) {}
}

/// Records reports in memory. Test double, and the basis of the
/// monotonicity assertion in F2.1.
#[derive(Debug, Default)]
pub struct RecordingProgressSink {
    reports: Mutex<Vec<Progress>>,
}

impl RecordingProgressSink {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn reports(&self) -> Vec<Progress> {
        self.reports.lock().expect("progress mutex").clone()
    }

    /// True when `completed` never decreases within a stage and never exceeds
    /// a stated total.
    #[must_use]
    pub fn is_monotonic(&self) -> bool {
        let reports = self.reports();
        let mut last: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for report in &reports {
            if let Some(total) = report.total {
                if report.completed > total {
                    return false;
                }
            }
            let entry = last.entry(report.stage.clone()).or_insert(0);
            if report.completed < *entry {
                return false;
            }
            *entry = report.completed;
        }
        true
    }
}

impl ProgressSink for RecordingProgressSink {
    fn report(&self, progress: Progress) {
        self.reports.lock().expect("progress mutex").push(progress);
    }
}

/// Context carried into every long-running AI operation (§6.5).
#[derive(Clone)]
pub struct JobContext {
    /// Opaque job identity for correlation in logs. Never a secret.
    pub job_id: String,
    pub cancel: CancellationToken,
    progress: Arc<dyn ProgressSink>,
    sequence: Arc<AtomicU64>,
}

impl std::fmt::Debug for JobContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobContext")
            .field("job_id", &self.job_id)
            .field("cancelled", &self.cancel.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl JobContext {
    pub fn new(job_id: impl Into<String>, cancel: CancellationToken, progress: Arc<dyn ProgressSink>) -> Self {
        Self {
            job_id: job_id.into(),
            cancel,
            progress,
            sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Context with no observer and no cancellation, for unit tests and
    /// synchronous internal calls.
    #[must_use]
    pub fn detached(job_id: impl Into<String>) -> Self {
        Self::new(job_id, CancellationToken::new(), Arc::new(NullProgressSink))
    }

    pub fn report(&self, progress: Progress) {
        self.sequence.fetch_add(1, Ordering::Relaxed);
        self.progress.report(progress);
    }

    #[must_use]
    pub fn reports_emitted(&self) -> u64 {
        self.sequence.load(Ordering::Relaxed)
    }

    pub fn bail_if_cancelled(&self) -> Result<(), Cancelled> {
        self.cancel.bail_if_cancelled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_observable_through_clones() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
        assert_eq!(clone.bail_if_cancelled(), Err(Cancelled));
    }

    #[test]
    fn progress_without_total_has_no_fraction() {
        let progress = Progress::new("decode", 10, None);
        assert_eq!(progress.fraction(), None);
    }

    #[test]
    fn recording_sink_detects_regression() {
        let sink = RecordingProgressSink::new();
        sink.report(Progress::new("decode", 5, Some(10)));
        sink.report(Progress::new("decode", 7, Some(10)));
        assert!(sink.is_monotonic());
        sink.report(Progress::new("decode", 6, Some(10)));
        assert!(!sink.is_monotonic());
    }

    #[test]
    fn recording_sink_rejects_overshoot() {
        let sink = RecordingProgressSink::new();
        sink.report(Progress::new("decode", 11, Some(10)));
        assert!(!sink.is_monotonic());
    }

    #[test]
    fn stages_track_monotonicity_independently() {
        let sink = RecordingProgressSink::new();
        sink.report(Progress::new("decode", 9, Some(10)));
        sink.report(Progress::new("transcribe", 1, Some(10)));
        assert!(sink.is_monotonic());
    }
}
