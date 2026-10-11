//! Job context carried into every long-running AI call (spec 6.5).

use std::sync::{Arc, Mutex};

use plaudon_domain::ids::JobId;
pub use tokio_util::sync::CancellationToken;

/// Progress reported by a running engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    /// Completed fraction in `[0, 1]`, when the engine can estimate one.
    pub fraction: Option<f32>,
    /// Milliseconds of audio processed so far, when known.
    pub processed_ms: Option<u64>,
}

impl Progress {
    /// Progress expressed as a fraction.
    #[must_use]
    pub fn fraction(value: f32) -> Self {
        Self { fraction: Some(value.clamp(0.0, 1.0)), processed_ms: None }
    }

    /// Progress expressed as processed audio duration.
    #[must_use]
    pub const fn processed_ms(value: u64) -> Self {
        Self { fraction: None, processed_ms: Some(value) }
    }
}

/// Sink for progress notifications.
///
/// Implementations must never block the engine: a dropped progress update is
/// always preferable to a stalled transcription.
pub trait ProgressSink: Send + Sync + std::fmt::Debug {
    /// Report progress. Failures are the sink's problem, never the engine's.
    fn report(&self, progress: Progress);
}

/// A progress sink that discards every update.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullProgressSink;

impl ProgressSink for NullProgressSink {
    fn report(&self, _progress: Progress) {}
}

/// A progress sink that records updates, for tests.
#[derive(Debug, Default)]
pub struct RecordingProgressSink {
    updates: Mutex<Vec<Progress>>,
}

impl RecordingProgressSink {
    /// Create an empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of everything reported so far.
    #[must_use]
    pub fn updates(&self) -> Vec<Progress> {
        self.updates.lock().map(|guard| guard.clone()).unwrap_or_default()
    }
}

impl ProgressSink for RecordingProgressSink {
    fn report(&self, progress: Progress) {
        if let Ok(mut guard) = self.updates.lock() {
            guard.push(progress);
        }
    }
}

/// Context handed to every engine call.
///
/// Spec 6.5 keeps cancellation tokens in the job context rather than inventing
/// per-adapter cancellation mechanisms.
#[derive(Debug, Clone)]
pub struct JobContext {
    /// Job this call belongs to.
    pub job_id: JobId,
    /// Cooperative cancellation token.
    pub cancel: CancellationToken,
    /// Progress sink.
    pub progress: Arc<dyn ProgressSink>,
}

impl JobContext {
    /// Create a context with a fresh cancellation token and no progress sink.
    #[must_use]
    pub fn new(job_id: JobId) -> Self {
        Self {
            job_id,
            cancel: CancellationToken::new(),
            progress: Arc::new(NullProgressSink),
        }
    }

    /// Attach a progress sink.
    #[must_use]
    pub fn with_progress(mut self, sink: Arc<dyn ProgressSink>) -> Self {
        self.progress = sink;
        self
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Report progress.
    pub fn report(&self, progress: Progress) {
        self.progress.report(progress);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_observable_through_the_context() {
        let ctx = JobContext::new(JobId::new());
        assert!(!ctx.is_cancelled());
        ctx.cancel.cancel();
        assert!(ctx.is_cancelled());
    }

    #[test]
    fn progress_fractions_are_clamped() {
        assert_eq!(Progress::fraction(1.7).fraction, Some(1.0));
        assert_eq!(Progress::fraction(-1.0).fraction, Some(0.0));
    }

    #[test]
    fn a_recording_sink_captures_reports() {
        let sink = Arc::new(RecordingProgressSink::new());
        let ctx = JobContext::new(JobId::new()).with_progress(sink.clone());
        ctx.report(Progress::processed_ms(1_000));
        assert_eq!(sink.updates(), [Progress::processed_ms(1_000)]);
    }
}
