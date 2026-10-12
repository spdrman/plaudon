//! Coalesced progress reporting.
//!
//! An engine reports progress as often as it likes — whisper.cpp reports per
//! decoded window, a cloud upload per chunk — and every report would
//! otherwise be a row in `job_steps` and a UI event. The reporter keeps only
//! the latest fraction and writes at most one row per [`FLUSH_INTERVAL`],
//! and only when the value actually moved.
//!
//! A fraction is never invented: an engine that cannot measure one reports
//! `None`, the row carries no ratio, and the interface renders an
//! indeterminate indicator (matrix row J3).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use plaudon_ai_contracts::job::{Progress, ProgressSink};
use plaudon_domain::ids::JobId;
use tokio_util::sync::CancellationToken;

use crate::backend::LocalBackend;

/// Shortest interval between two persisted progress rows.
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);

/// Smallest change worth a row, as a fraction of the whole.
const MIN_DELTA: f32 = 0.01;

/// Sentinel for "nothing reported yet", outside the 0..=1 range.
const NOTHING: u32 = u32::MAX;

/// The sink handed to an engine, plus the task that drains it.
#[derive(Debug)]
pub struct ProgressReporter {
    sink: Arc<CoalescingSink>,
    stop: CancellationToken,
    task: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ProgressReporter {
    /// Start reporting progress for `job_id` under `stage`.
    pub fn start(backend: Arc<LocalBackend>, job_id: JobId, stage: &'static str) -> Self {
        let sink = Arc::new(CoalescingSink::new());
        let stop = CancellationToken::new();
        let task = tokio::spawn({
            let sink = Arc::clone(&sink);
            let stop = stop.clone();
            async move {
                let mut written = NOTHING;
                loop {
                    tokio::select! {
                        biased;
                        () = stop.cancelled() => return,
                        () = tokio::time::sleep(FLUSH_INTERVAL) => {}
                    }
                    let latest = sink.latest.load(Ordering::Relaxed);
                    if latest == NOTHING || !moved(written, latest) {
                        continue;
                    }
                    written = latest;
                    let ratio = f64::from(latest) / f64::from(SCALE);
                    if let Err(error) = super::write_step(
                        &backend,
                        job_id,
                        plaudon_jobs::state::JobState::RunningLocal.as_str(),
                        Some(stage),
                        Some(ratio),
                    )
                    .await
                    {
                        tracing::warn!(
                            code = error.code().as_str(),
                            "a progress row was not recorded"
                        );
                        return;
                    }
                    backend.publish_job(job_id).await;
                }
            }
        });
        Self {
            sink,
            stop,
            task: parking_lot::Mutex::new(Some(task)),
        }
    }

    /// The sink to hand to the engine.
    #[must_use]
    pub fn sink(&self) -> Arc<dyn ProgressSink> {
        Arc::clone(&self.sink) as Arc<dyn ProgressSink>
    }

    /// Stop reporting. The terminal transition publishes the final state, so
    /// no last flush is needed and a late row could contradict it.
    pub async fn stop(&self) {
        self.stop.cancel();
        let task = self.task.lock().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

/// Fixed-point scale of a stored fraction.
const SCALE: u32 = 10_000;

fn moved(written: u32, latest: u32) -> bool {
    if written == NOTHING {
        return true;
    }
    #[allow(clippy::cast_precision_loss)]
    let delta = (latest as f32 - written as f32).abs() / SCALE as f32;
    delta >= MIN_DELTA
}

/// Keeps the latest reported fraction and nothing else.
#[derive(Debug)]
struct CoalescingSink {
    latest: AtomicU32,
}

impl CoalescingSink {
    const fn new() -> Self {
        Self {
            latest: AtomicU32::new(NOTHING),
        }
    }
}

impl ProgressSink for CoalescingSink {
    fn report(&self, progress: Progress) {
        // Only a fraction is persisted: `processed_ms` without a duration is
        // not a ratio, and the row must not imply one.
        let Some(fraction) = progress.fraction else {
            return;
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let scaled = (fraction.clamp(0.0, 1.0) * SCALE as f32).round() as u32;
        self.latest.store(scaled.min(SCALE), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_without_a_fraction_persists_nothing() {
        let sink = CoalescingSink::new();
        sink.report(Progress::processed_ms(1_000));
        assert_eq!(sink.latest.load(Ordering::Relaxed), NOTHING);
    }

    #[test]
    fn only_the_latest_fraction_survives_and_small_moves_are_dropped() {
        let sink = CoalescingSink::new();
        for step in 1..=50 {
            #[allow(clippy::cast_precision_loss)]
            sink.report(Progress::fraction(step as f32 / 100.0));
        }
        assert_eq!(sink.latest.load(Ordering::Relaxed), SCALE / 2);
        assert!(moved(NOTHING, SCALE / 2), "the first value is always worth a row");
        assert!(!moved(5_000, 5_050), "half a percent is not worth a row");
        assert!(moved(5_000, 5_200), "two percent is");
    }
}
