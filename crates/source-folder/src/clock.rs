//! An injected clock.
//!
//! The completion barrier is defined in terms of elapsed time, and a test that
//! proves "a file still being written is never imported" must be able to move
//! time without actually waiting. Every time source in this crate therefore
//! goes through [`Clock`]; nothing calls `Instant::now` directly.

use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock milliseconds since the Unix epoch.
pub trait Clock: fmt::Debug + Send + Sync {
    /// The current instant.
    fn now_ms(&self) -> i64;
}

/// The operating system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
            .unwrap_or(0)
    }
}

/// A clock the caller advances explicitly.
#[derive(Debug, Clone, Default)]
pub struct ManualClock {
    now_ms: Arc<AtomicI64>,
}

impl ManualClock {
    /// A clock starting at `start_ms`.
    #[must_use]
    pub fn starting_at(start_ms: i64) -> Self {
        Self {
            now_ms: Arc::new(AtomicI64::new(start_ms)),
        }
    }

    /// Move time forward.
    pub fn advance_ms(&self, delta: i64) {
        self.now_ms.fetch_add(delta, Ordering::SeqCst);
    }

    /// Jump to an absolute instant.
    pub fn set_ms(&self, value: i64) {
        self.now_ms.store(value, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}
