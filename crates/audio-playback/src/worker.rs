//! A bounded pool of peak-building workers.
//!
//! filmcraft spawns one `std::thread` per waveform request, with no cap, no
//! cancellation and no repaint on completion. Opening a library of long
//! recordings that way starts as many full decodes as there are rows. This
//! pool fixes all three: a fixed number of threads, a bounded queue that
//! drops the stalest request rather than growing, a cancellation token per
//! job, and a waker invoked on every completion so an idle UI repaints
//! exactly once per finished pyramid.
//!
//! The pool is deliberately free of any UI type. The waker is a plain
//! `Fn()`; the egui layer passes `move || ctx.request_repaint()`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use plaudon_ai_contracts::job::{JobContext, Progress, ProgressSink};
use plaudon_domain::ids::JobId;
use tokio_util::sync::CancellationToken;

use crate::cache::{PeakCache, PeakCacheKey};
use crate::generate::{fold_media, validate_for_peaks, PeakBuildError};
use crate::peaks::{PeakParams, PeakPyramid};

/// Called once whenever a job finishes, so an idle renderer wakes up.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Handle for one submitted build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeakRequestId(u64);

impl PeakRequestId {
    /// The raw counter value, for logging.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// What a caller asks the pool for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeakRequest {
    /// Media file to fold.
    pub media_path: PathBuf,
    /// Fold parameters.
    pub params: PeakParams,
    /// SHA-256 of the media bytes, when the caller already knows it (the
    /// library does). Supplying it turns a cache hit into a file read with no
    /// hashing at all.
    pub content_hash: Option<[u8; 32]>,
}

impl PeakRequest {
    /// A request for `path` with the default parameters.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            media_path: path.into(),
            params: PeakParams::default(),
            content_hash: None,
        }
    }

    /// Override the fold parameters.
    #[must_use]
    pub fn with_params(mut self, params: PeakParams) -> Self {
        self.params = params;
        self
    }

    /// Supply a known content hash.
    #[must_use]
    pub fn with_content_hash(mut self, hash: [u8; 32]) -> Self {
        self.content_hash = Some(hash);
        self
    }

    fn coalesces_with(&self, other: &Self) -> bool {
        self.media_path == other.media_path && self.params == other.params
    }
}

/// How a build ended.
#[derive(Debug, Clone)]
pub enum PeakOutcome {
    /// A pyramid, from the cache or freshly folded.
    Ready {
        /// The pyramid, shared so the UI can hold it cheaply.
        pyramid: Arc<PeakPyramid>,
        /// Whether it came from the disk cache.
        from_cache: bool,
    },
    /// The job was cancelled, or dropped because the queue was full.
    Cancelled,
    /// The build failed; the message is already redaction-safe (it names a
    /// path the user chose and a decoder reason).
    Failed(String),
}

/// A finished job.
#[derive(Debug, Clone)]
pub struct PeakCompletion {
    /// The handle returned by [`PeakWorkerPool::request`].
    pub id: PeakRequestId,
    /// The request as submitted.
    pub request: PeakRequest,
    /// Result.
    pub outcome: PeakOutcome,
}

/// Pool sizing and wiring.
#[derive(Clone)]
pub struct WorkerConfig {
    /// Worker threads. Clamped to at least one.
    pub threads: usize,
    /// Queued-but-not-started jobs retained. The oldest is dropped when full.
    pub queue_capacity: usize,
    /// Invoked after every completion.
    pub waker: Option<Waker>,
    /// Disk cache consulted before and written after every build.
    pub cache: Option<Arc<PeakCache>>,
}

impl std::fmt::Debug for WorkerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerConfig")
            .field("threads", &self.threads)
            .field("queue_capacity", &self.queue_capacity)
            .field("waker", &self.waker.as_ref().map(|_| "set"))
            .field(
                "cache",
                &self.cache.as_ref().map(|cache| cache.root().to_path_buf()),
            )
            .finish()
    }
}

impl Default for WorkerConfig {
    /// Two threads. Peak building is a full decode: it must leave the
    /// machine to transcription and to the UI.
    fn default() -> Self {
        Self {
            threads: 2,
            queue_capacity: 32,
            waker: None,
            cache: None,
        }
    }
}

#[derive(Debug)]
struct Queued {
    id: PeakRequestId,
    request: PeakRequest,
    cancel: CancellationToken,
    progress: Arc<PeakProgress>,
}

#[derive(Debug)]
struct PeakProgress {
    /// Completed fraction, in thousandths; `u32::MAX` until first reported.
    per_mille: AtomicU32,
}

impl Default for PeakProgress {
    fn default() -> Self {
        Self {
            per_mille: AtomicU32::new(u32::MAX),
        }
    }
}

impl ProgressSink for PeakProgress {
    fn report(&self, progress: Progress) {
        if let Some(fraction) = progress.fraction {
            let value = (fraction.clamp(0.0, 1.0) * 1000.0) as u32;
            self.per_mille.store(value, Ordering::Relaxed);
        }
    }
}

#[derive(Debug, Default)]
struct Inner {
    pending: VecDeque<Queued>,
    running: Vec<(
        PeakRequestId,
        PeakRequest,
        CancellationToken,
        Arc<PeakProgress>,
    )>,
    results: Vec<PeakCompletion>,
    stopping: bool,
}

struct Shared {
    inner: Mutex<Inner>,
    ready: Condvar,
    next_id: AtomicU64,
    cache: Option<Arc<PeakCache>>,
    waker: Option<Waker>,
    queue_capacity: usize,
    stop: AtomicBool,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("queue_capacity", &self.queue_capacity)
            .finish_non_exhaustive()
    }
}

/// The pool. Dropping it stops and joins every worker.
#[derive(Debug)]
pub struct PeakWorkerPool {
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl PeakWorkerPool {
    /// Start a pool.
    #[must_use]
    pub fn new(config: WorkerConfig) -> Self {
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner::default()),
            ready: Condvar::new(),
            next_id: AtomicU64::new(1),
            cache: config.cache,
            waker: config.waker,
            queue_capacity: config.queue_capacity.max(1),
            stop: AtomicBool::new(false),
        });
        let mut threads = Vec::with_capacity(config.threads.max(1));
        for index in 0..config.threads.max(1) {
            let shared = Arc::clone(&shared);
            let handle = std::thread::Builder::new()
                .name(format!("plaudon-peaks-{index}"))
                .spawn(move || worker_loop(&shared))
                .expect("spawn peak worker");
            threads.push(handle);
        }
        Self { shared, threads }
    }

    /// Submit a build.
    ///
    /// An identical request that has not finished yet is coalesced: the
    /// existing handle is returned and no second decode starts.
    pub fn request(&self, request: PeakRequest) -> PeakRequestId {
        let mut inner = self.shared.inner.lock().expect("peak queue");
        if let Some(existing) = inner
            .pending
            .iter()
            .find(|queued| queued.request.coalesces_with(&request))
        {
            return existing.id;
        }
        if let Some((id, _, _, _)) = inner
            .running
            .iter()
            .find(|(_, running, _, _)| running.coalesces_with(&request))
        {
            return *id;
        }
        let id = PeakRequestId(self.shared.next_id.fetch_add(1, Ordering::Relaxed));
        while inner.pending.len() >= self.shared.queue_capacity {
            if let Some(dropped) = inner.pending.pop_front() {
                dropped.cancel.cancel();
                inner.results.push(PeakCompletion {
                    id: dropped.id,
                    request: dropped.request,
                    outcome: PeakOutcome::Cancelled,
                });
            }
        }
        inner.pending.push_back(Queued {
            id,
            request,
            cancel: CancellationToken::new(),
            progress: Arc::new(PeakProgress::default()),
        });
        drop(inner);
        self.shared.ready.notify_one();
        id
    }

    /// Cancel a job whether it is queued or running.
    pub fn cancel(&self, id: PeakRequestId) {
        let mut inner = self.shared.inner.lock().expect("peak queue");
        if let Some(position) = inner.pending.iter().position(|queued| queued.id == id) {
            if let Some(queued) = inner.pending.remove(position) {
                queued.cancel.cancel();
                inner.results.push(PeakCompletion {
                    id: queued.id,
                    request: queued.request,
                    outcome: PeakOutcome::Cancelled,
                });
            }
            drop(inner);
            self.wake();
            return;
        }
        if let Some((_, _, token, _)) = inner.running.iter().find(|(other, _, _, _)| *other == id) {
            token.cancel();
        }
    }

    /// Cancel everything queued or running.
    pub fn cancel_all(&self) {
        let ids: Vec<PeakRequestId> = {
            let inner = self.shared.inner.lock().expect("peak queue");
            inner
                .pending
                .iter()
                .map(|queued| queued.id)
                .chain(inner.running.iter().map(|(id, _, _, _)| *id))
                .collect()
        };
        for id in ids {
            self.cancel(id);
        }
    }

    /// Take every completion reported since the last call. Never blocks.
    pub fn drain_completions(&self) -> Vec<PeakCompletion> {
        let mut inner = self.shared.inner.lock().expect("peak queue");
        std::mem::take(&mut inner.results)
    }

    /// Completed fraction of a running job, if it has reported any.
    #[must_use]
    pub fn progress(&self, id: PeakRequestId) -> Option<f32> {
        let inner = self.shared.inner.lock().expect("peak queue");
        let (_, _, _, progress) = inner.running.iter().find(|(other, _, _, _)| *other == id)?;
        let value = progress.per_mille.load(Ordering::Relaxed);
        (value != u32::MAX).then(|| value as f32 / 1000.0)
    }

    /// Jobs queued but not started.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.shared.inner.lock().expect("peak queue").pending.len()
    }

    /// Jobs currently decoding.
    #[must_use]
    pub fn running(&self) -> usize {
        self.shared.inner.lock().expect("peak queue").running.len()
    }

    /// Block until nothing is queued or running, or `timeout` elapses.
    /// Returns whether the pool went idle. The UI polls instead; this exists
    /// for tests and for an orderly shutdown.
    pub fn wait_idle(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.queued() == 0 && self.running() == 0 {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        false
    }

    fn wake(&self) {
        if let Some(waker) = self.shared.waker.as_ref() {
            waker();
        }
    }
}

impl Drop for PeakWorkerPool {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        {
            let mut inner = self.shared.inner.lock().expect("peak queue");
            inner.stopping = true;
            for queued in inner.pending.drain(..) {
                queued.cancel.cancel();
            }
            for (_, _, token, _) in &inner.running {
                token.cancel();
            }
        }
        self.shared.ready.notify_all();
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

fn worker_loop(shared: &Arc<Shared>) {
    loop {
        let job = {
            let mut inner = shared.inner.lock().expect("peak queue");
            loop {
                if shared.stop.load(Ordering::Acquire) {
                    return;
                }
                if let Some(job) = inner.pending.pop_front() {
                    inner.running.push((
                        job.id,
                        job.request.clone(),
                        job.cancel.clone(),
                        Arc::clone(&job.progress),
                    ));
                    break job;
                }
                inner = shared.ready.wait(inner).expect("peak queue");
            }
        };

        let ctx = JobContext::new(JobId::new())
            .with_progress(Arc::clone(&job.progress) as Arc<dyn ProgressSink>);
        let ctx = JobContext {
            cancel: job.cancel.clone(),
            ..ctx
        };
        // A decoder panic must cost one waveform, not the application.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_job(shared.cache.as_deref(), &job.request, &ctx)
        }))
        .unwrap_or_else(|_| PeakOutcome::Failed("peak worker panicked while decoding".to_owned()));

        {
            let mut inner = shared.inner.lock().expect("peak queue");
            inner.running.retain(|(id, _, _, _)| *id != job.id);
            inner.results.push(PeakCompletion {
                id: job.id,
                request: job.request,
                outcome,
            });
        }
        if let Some(waker) = shared.waker.as_ref() {
            waker();
        }
    }
}

fn run_job(cache: Option<&PeakCache>, request: &PeakRequest, ctx: &JobContext) -> PeakOutcome {
    if ctx.is_cancelled() {
        return PeakOutcome::Cancelled;
    }
    // Fast path: a caller-supplied hash lets the cache answer without
    // touching the media file at all.
    if let (Some(cache), Some(hash)) = (cache, request.content_hash) {
        let key = PeakCacheKey::new(hash, request.params);
        if let Some(pyramid) = cache.load(&key) {
            return PeakOutcome::Ready {
                pyramid: Arc::new(pyramid),
                from_cache: true,
            };
        }
    }

    let (media, hash) = match validate_for_peaks(&request.media_path) {
        Ok(value) => value,
        Err(error) => return failure(error),
    };
    if ctx.is_cancelled() {
        return PeakOutcome::Cancelled;
    }
    let key = PeakCacheKey::new(hash, request.params);
    if request.content_hash != Some(hash) {
        if let Some(cache) = cache {
            if let Some(pyramid) = cache.load(&key) {
                return PeakOutcome::Ready {
                    pyramid: Arc::new(pyramid),
                    from_cache: true,
                };
            }
        }
    }

    match fold_media(&media, request.params, ctx) {
        Ok(pyramid) => {
            if let Some(cache) = cache {
                if let Err(error) = cache.store(&key, &pyramid) {
                    tracing::debug!(%error, "peak cache write failed; the pyramid is still usable");
                }
            }
            PeakOutcome::Ready {
                pyramid: Arc::new(pyramid),
                from_cache: false,
            }
        }
        Err(error) => failure(error),
    }
}

fn failure(error: PeakBuildError) -> PeakOutcome {
    match error {
        PeakBuildError::Cancelled => PeakOutcome::Cancelled,
        other => PeakOutcome::Failed(other.to_string()),
    }
}

/// Convenience wrapper for the common UI shape: one pyramid per recording,
/// with the request coalesced and the result kept until replaced.
#[derive(Debug)]
pub struct PeakStore {
    pool: PeakWorkerPool,
    current: Mutex<Option<(PeakRequestId, Arc<PeakPyramid>)>>,
}

impl PeakStore {
    /// Wrap a pool.
    #[must_use]
    pub fn new(config: WorkerConfig) -> Self {
        Self {
            pool: PeakWorkerPool::new(config),
            current: Mutex::new(None),
        }
    }

    /// The pool, for progress and cancellation.
    #[must_use]
    pub const fn pool(&self) -> &PeakWorkerPool {
        &self.pool
    }

    /// Ask for `path`'s pyramid. Returns the one already held, if any; the
    /// build result arrives through [`PeakStore::poll`].
    pub fn request(&self, request: PeakRequest) -> Option<Arc<PeakPyramid>> {
        let id = self.pool.request(request);
        let current = self.current.lock().expect("peak store");
        current
            .as_ref()
            .and_then(|(held, pyramid)| (*held == id).then(|| Arc::clone(pyramid)))
    }

    /// Drain completions, keeping the newest pyramid. Returns the
    /// completions so the caller can surface failures.
    pub fn poll(&self) -> Vec<PeakCompletion> {
        let completions = self.pool.drain_completions();
        let mut current = self.current.lock().expect("peak store");
        for completion in &completions {
            if let PeakOutcome::Ready { pyramid, .. } = &completion.outcome {
                *current = Some((completion.id, Arc::clone(pyramid)));
            }
        }
        completions
    }

    /// The pyramid currently held, if any.
    #[must_use]
    pub fn pyramid(&self) -> Option<Arc<PeakPyramid>> {
        self.current
            .lock()
            .expect("peak store")
            .as_ref()
            .map(|(_, pyramid)| Arc::clone(pyramid))
    }
}

/// Byte-for-byte content hash of a file, for callers that want to pre-seed
/// [`PeakRequest::content_hash`] without running validation.
pub fn hash_file(path: &Path) -> std::io::Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use super::*;

    fn write_wav(path: &Path, seconds: f64) {
        let rate = 16_000u32;
        let frames = (f64::from(rate) * seconds) as u32;
        let data_bytes = frames * 2;
        let mut file = std::fs::File::create(path).expect("create wav");
        file.write_all(b"RIFF").expect("write");
        file.write_all(&(36 + data_bytes).to_le_bytes())
            .expect("write");
        file.write_all(b"WAVEfmt ").expect("write");
        file.write_all(&16u32.to_le_bytes()).expect("write");
        file.write_all(&1u16.to_le_bytes()).expect("write");
        file.write_all(&1u16.to_le_bytes()).expect("write");
        file.write_all(&rate.to_le_bytes()).expect("write");
        file.write_all(&(rate * 2).to_le_bytes()).expect("write");
        file.write_all(&2u16.to_le_bytes()).expect("write");
        file.write_all(&16u16.to_le_bytes()).expect("write");
        file.write_all(b"data").expect("write");
        file.write_all(&data_bytes.to_le_bytes()).expect("write");
        for frame in 0..frames {
            let phase = f64::from(frame) * 180.0 * std::f64::consts::TAU / f64::from(rate);
            let sample = (phase.sin() * 0.4 * f64::from(i16::MAX)) as i16;
            file.write_all(&sample.to_le_bytes()).expect("write");
        }
        file.flush().expect("flush");
    }

    #[test]
    fn a_build_completes_and_wakes_the_renderer_exactly_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.wav");
        write_wav(&path, 1.0);
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let pool = PeakWorkerPool::new(WorkerConfig {
            threads: 1,
            waker: Some(Arc::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
            })),
            ..WorkerConfig::default()
        });
        let id = pool.request(PeakRequest::new(&path));
        assert!(pool.wait_idle(Duration::from_secs(30)));
        let completions = pool.drain_completions();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].id, id);
        assert!(matches!(
            completions[0].outcome,
            PeakOutcome::Ready {
                from_cache: false,
                ..
            }
        ));
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn an_identical_request_is_coalesced_rather_than_decoded_twice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.wav");
        write_wav(&path, 1.0);
        let pool = PeakWorkerPool::new(WorkerConfig {
            threads: 1,
            ..WorkerConfig::default()
        });
        let first = pool.request(PeakRequest::new(&path));
        let second = pool.request(PeakRequest::new(&path));
        assert_eq!(first, second);
        assert!(pool.wait_idle(Duration::from_secs(30)));
        assert_eq!(pool.drain_completions().len(), 1);
    }

    #[test]
    fn the_second_build_of_the_same_file_comes_from_the_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.wav");
        write_wav(&path, 1.0);
        let cache = Arc::new(PeakCache::open(dir.path().join("cache")).expect("cache"));
        let config = || WorkerConfig {
            threads: 1,
            cache: Some(Arc::clone(&cache)),
            ..WorkerConfig::default()
        };

        let pool = PeakWorkerPool::new(config());
        pool.request(PeakRequest::new(&path));
        assert!(pool.wait_idle(Duration::from_secs(30)));
        let first = pool.drain_completions();
        assert!(matches!(
            first[0].outcome,
            PeakOutcome::Ready {
                from_cache: false,
                ..
            }
        ));
        drop(pool);

        let pool = PeakWorkerPool::new(config());
        pool.request(PeakRequest::new(&path));
        assert!(pool.wait_idle(Duration::from_secs(30)));
        let second = pool.drain_completions();
        assert!(
            matches!(
                second[0].outcome,
                PeakOutcome::Ready {
                    from_cache: true,
                    ..
                }
            ),
            "{:?}",
            second[0].outcome
        );

        // And the same pyramid, byte for byte.
        let (PeakOutcome::Ready { pyramid: a, .. }, PeakOutcome::Ready { pyramid: b, .. }) =
            (&first[0].outcome, &second[0].outcome)
        else {
            panic!("expected two ready outcomes");
        };
        assert_eq!(a, b);
    }

    #[test]
    fn a_known_content_hash_hits_the_cache_without_touching_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.wav");
        write_wav(&path, 1.0);
        let hash = hash_file(&path).expect("hash");
        let cache = Arc::new(PeakCache::open(dir.path().join("cache")).expect("cache"));

        let pool = PeakWorkerPool::new(WorkerConfig {
            threads: 1,
            cache: Some(Arc::clone(&cache)),
            ..WorkerConfig::default()
        });
        pool.request(PeakRequest::new(&path).with_content_hash(hash));
        assert!(pool.wait_idle(Duration::from_secs(30)));
        pool.drain_completions();
        drop(pool);

        // Remove the media: only the cache can answer now.
        std::fs::remove_file(&path).expect("remove");
        let pool = PeakWorkerPool::new(WorkerConfig {
            threads: 1,
            cache: Some(cache),
            ..WorkerConfig::default()
        });
        pool.request(PeakRequest::new(&path).with_content_hash(hash));
        assert!(pool.wait_idle(Duration::from_secs(30)));
        let completions = pool.drain_completions();
        assert!(matches!(
            completions[0].outcome,
            PeakOutcome::Ready {
                from_cache: true,
                ..
            }
        ));
    }

    #[test]
    fn a_full_queue_drops_the_oldest_request_instead_of_growing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = PeakWorkerPool::new(WorkerConfig {
            threads: 1,
            queue_capacity: 2,
            ..WorkerConfig::default()
        });
        // Nonexistent paths fail fast; what matters is the queue bound, so
        // submit far more than the capacity and check it never exceeds it.
        let mut ids = Vec::new();
        for index in 0..50 {
            ids.push(pool.request(PeakRequest::new(dir.path().join(format!("{index}.wav")))));
        }
        assert!(pool.queued() <= 2, "queued {}", pool.queued());
        assert!(pool.wait_idle(Duration::from_secs(30)));
        let completions = pool.drain_completions();
        assert_eq!(completions.len(), ids.len());
        assert!(completions
            .iter()
            .any(|c| matches!(c.outcome, PeakOutcome::Cancelled)));
        assert!(completions
            .iter()
            .any(|c| matches!(c.outcome, PeakOutcome::Failed(_))));
    }

    #[test]
    fn cancelling_a_queued_job_reports_it_cancelled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.wav");
        write_wav(&path, 1.0);
        let pool = PeakWorkerPool::new(WorkerConfig {
            threads: 1,
            ..WorkerConfig::default()
        });
        let first = pool.request(PeakRequest::new(&path));
        let second = pool.request(PeakRequest::new(&path).with_params(PeakParams {
            base_bucket_frames: 128,
            ..PeakParams::default()
        }));
        pool.cancel(second);
        assert!(pool.wait_idle(Duration::from_secs(30)));
        let completions = pool.drain_completions();
        let cancelled = completions
            .iter()
            .find(|completion| completion.id == second)
            .expect("second completion");
        assert!(matches!(cancelled.outcome, PeakOutcome::Cancelled));
        assert!(completions.iter().any(|c| c.id == first));
    }

    #[test]
    fn a_missing_file_is_a_failure_not_a_hang() {
        let pool = PeakWorkerPool::new(WorkerConfig {
            threads: 1,
            ..WorkerConfig::default()
        });
        pool.request(PeakRequest::new("/definitely/not/here.wav"));
        assert!(pool.wait_idle(Duration::from_secs(30)));
        let completions = pool.drain_completions();
        assert!(matches!(completions[0].outcome, PeakOutcome::Failed(_)));
    }

    #[test]
    fn the_store_keeps_the_newest_pyramid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.wav");
        write_wav(&path, 1.0);
        let store = PeakStore::new(WorkerConfig {
            threads: 1,
            ..WorkerConfig::default()
        });
        assert!(store.request(PeakRequest::new(&path)).is_none());
        assert!(store.pool().wait_idle(Duration::from_secs(30)));
        store.poll();
        let pyramid = store.pyramid().expect("pyramid");
        assert_eq!(pyramid.duration().as_millis(), 1_000);
    }
}
