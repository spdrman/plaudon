//! The bridge between the peak worker pool and a repainting view.
//!
//! The pool is UI-free; this is the twenty lines that make an idle egui
//! application wake up when a pyramid is ready. filmcraft's waveform worker
//! has no equivalent and its own notes record the consequence: a finished
//! waveform can sit unseen until something else causes a repaint.

use std::path::PathBuf;
use std::sync::Arc;

use plaudon_audio_playback::cache::PeakCache;
use plaudon_audio_playback::peaks::PeakPyramid;
use plaudon_audio_playback::worker::{
    PeakOutcome, PeakRequest, PeakRequestId, PeakWorkerPool, WorkerConfig,
};

/// Where a recording's peaks are in their lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeakStatus {
    /// Nothing has been asked for.
    Idle,
    /// A build is queued or running.
    Building,
    /// A pyramid is available.
    Ready,
    /// The build failed; the message is safe to show.
    Failed(String),
}

/// Owns the worker pool and the pyramid currently on screen.
#[derive(Debug)]
pub struct PeakSource {
    pool: PeakWorkerPool,
    current: Option<(PeakRequestId, Arc<PeakPyramid>)>,
    pending: Option<PeakRequestId>,
    status: PeakStatus,
}

impl PeakSource {
    /// A source that repaints `ctx` whenever a build finishes.
    ///
    /// `cache_dir` is where pyramids are kept between runs; `None` disables
    /// the disk cache, which is what the tests use.
    #[must_use]
    pub fn new(ctx: &egui::Context, cache_dir: Option<PathBuf>) -> Self {
        let repaint = ctx.clone();
        let cache = cache_dir.and_then(|dir| match PeakCache::open(&dir) {
            Ok(cache) => Some(Arc::new(cache)),
            Err(error) => {
                // A cache that cannot be opened is a slow start, not a
                // broken player.
                let _ = error;
                None
            }
        });
        Self {
            pool: PeakWorkerPool::new(WorkerConfig {
                waker: Some(Arc::new(move || repaint.request_repaint())),
                cache,
                ..WorkerConfig::default()
            }),
            current: None,
            pending: None,
            status: PeakStatus::Idle,
        }
    }

    /// The worker pool, for progress and cancellation.
    #[must_use]
    pub const fn pool(&self) -> &PeakWorkerPool {
        &self.pool
    }

    /// Ask for a recording's peaks. Repeated calls for the same request are
    /// coalesced by the pool, so this is safe to call every frame.
    pub fn request(&mut self, request: PeakRequest) {
        let id = self.pool.request(request);
        if self.pending == Some(id) || self.current.as_ref().is_some_and(|(held, _)| *held == id) {
            return;
        }
        self.pending = Some(id);
        self.status = PeakStatus::Building;
    }

    /// Forget the current pyramid and cancel anything in flight.
    pub fn clear(&mut self) {
        self.pool.cancel_all();
        self.current = None;
        self.pending = None;
        self.status = PeakStatus::Idle;
    }

    /// Drain finished builds. Call once per frame, before drawing.
    pub fn poll(&mut self) {
        for completion in self.pool.drain_completions() {
            match completion.outcome {
                PeakOutcome::Ready { pyramid, .. } => {
                    self.current = Some((completion.id, pyramid));
                    if self.pending == Some(completion.id) {
                        self.pending = None;
                    }
                    self.status = PeakStatus::Ready;
                }
                PeakOutcome::Failed(message) => {
                    if self.pending == Some(completion.id) {
                        self.pending = None;
                        self.status = PeakStatus::Failed(message);
                    }
                }
                PeakOutcome::Cancelled => {
                    if self.pending == Some(completion.id) {
                        self.pending = None;
                        self.status = PeakStatus::Idle;
                    }
                }
            }
        }
    }

    /// The pyramid to draw, if one has arrived.
    #[must_use]
    pub fn pyramid(&self) -> Option<&PeakPyramid> {
        self.current.as_ref().map(|(_, pyramid)| pyramid.as_ref())
    }

    /// Lifecycle state, for the "generating waveform" affordance.
    #[must_use]
    pub fn status(&self) -> &PeakStatus {
        &self.status
    }

    /// Completed fraction of the build in flight, when it has reported one.
    #[must_use]
    pub fn progress(&self) -> Option<f32> {
        self.pool.progress(self.pending?)
    }
}
