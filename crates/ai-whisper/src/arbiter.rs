//! Local-workload concurrency arbitration (spec §14.4, §14.7 — F2.5).
//!
//! > We will allow one active local model workload by default on
//! > memory-constrained systems … surface cancel semantics and GPU busy
//! > conditions.
//!
//! The arbiter is a counted permit with an explicit "busy" answer rather than
//! a blocking queue, because §14.7 requires the busy state to reach the UI
//! instead of a job appearing to hang. A [`WorkloadLease`] releases its permit
//! on drop, which is what makes cancellation, a panic in the worker and a
//! clean finish all release GPU and audio resources on the same path.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::backend::Backend;

/// Refusal returned when every permit is taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a local model workload is already running on `{running_backend}` ({active} of {limit} permits in use)")]
pub struct WorkloadBusy {
    /// Backend the running workload occupies.
    pub running_backend: &'static str,
    /// Permits currently held.
    pub active: usize,
    /// Configured permit count.
    pub limit: usize,
}

#[derive(Debug)]
struct ArbiterState {
    limit: usize,
    active: AtomicUsize,
    /// Backend of the most recently acquired lease, for the busy message.
    last_backend: std::sync::Mutex<Backend>,
}

/// Grants permission to run one local model workload at a time.
#[derive(Debug, Clone)]
pub struct LocalWorkloadArbiter {
    state: Arc<ArbiterState>,
}

impl Default for LocalWorkloadArbiter {
    fn default() -> Self {
        Self::single()
    }
}

impl LocalWorkloadArbiter {
    /// The default: exactly one concurrent local workload (§14.4).
    #[must_use]
    pub fn single() -> Self {
        Self::with_limit(1)
    }

    /// A configurable permit count.
    ///
    /// A limit of zero is not representable: it would make local STT
    /// permanently unavailable, which is never a configuration we want to be
    /// able to reach by accident.
    #[must_use]
    pub fn with_limit(limit: usize) -> Self {
        Self {
            state: Arc::new(ArbiterState {
                limit: limit.max(1),
                active: AtomicUsize::new(0),
                last_backend: std::sync::Mutex::new(Backend::Cpu),
            }),
        }
    }

    /// Configured permit count.
    #[must_use]
    pub fn limit(&self) -> usize {
        self.state.limit
    }

    /// Permits currently held.
    #[must_use]
    pub fn active(&self) -> usize {
        self.state.active.load(Ordering::SeqCst)
    }

    /// Take a permit, or report busy.
    pub fn try_acquire(&self, backend: Backend) -> Result<WorkloadLease, WorkloadBusy> {
        let mut current = self.state.active.load(Ordering::SeqCst);
        loop {
            if current >= self.state.limit {
                let running_backend = self
                    .state
                    .last_backend
                    .lock()
                    .map_or(Backend::Cpu, |guard| *guard);
                return Err(WorkloadBusy {
                    running_backend: running_backend.as_str(),
                    active: current,
                    limit: self.state.limit,
                });
            }
            match self.state.active.compare_exchange(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        if let Ok(mut guard) = self.state.last_backend.lock() {
            *guard = backend;
        }
        Ok(WorkloadLease {
            state: Arc::clone(&self.state),
            backend,
            released: false,
        })
    }
}

/// A held permit. Releases on drop.
#[derive(Debug)]
pub struct WorkloadLease {
    state: Arc<ArbiterState>,
    backend: Backend,
    released: bool,
}

impl WorkloadLease {
    /// Backend this lease was taken for.
    #[must_use]
    pub const fn backend(&self) -> Backend {
        self.backend
    }

    /// Release early. Idempotent; dropping also releases.
    pub fn release(&mut self) {
        if !self.released {
            self.released = true;
            self.state.active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for WorkloadLease {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_workload_runs_by_default() {
        let arbiter = LocalWorkloadArbiter::single();
        let first = arbiter.try_acquire(Backend::Metal).expect("first lease");
        assert_eq!(arbiter.active(), 1);

        let busy = arbiter
            .try_acquire(Backend::Cpu)
            .expect_err("a second workload must be refused");
        assert_eq!(busy.running_backend, "metal");
        assert_eq!(busy.active, 1);
        assert_eq!(busy.limit, 1);

        drop(first);
        assert_eq!(arbiter.active(), 0);
        assert!(arbiter.try_acquire(Backend::Cpu).is_ok());
    }

    #[test]
    fn the_limit_is_configurable() {
        let arbiter = LocalWorkloadArbiter::with_limit(2);
        let _a = arbiter.try_acquire(Backend::Cpu).expect("first");
        let _b = arbiter.try_acquire(Backend::Cpu).expect("second");
        assert!(arbiter.try_acquire(Backend::Cpu).is_err());
        assert_eq!(arbiter.active(), 2);
    }

    #[test]
    fn a_zero_limit_is_clamped_to_one() {
        let arbiter = LocalWorkloadArbiter::with_limit(0);
        assert_eq!(arbiter.limit(), 1);
        assert!(arbiter.try_acquire(Backend::Cpu).is_ok());
    }

    #[test]
    fn releasing_is_idempotent() {
        let arbiter = LocalWorkloadArbiter::single();
        let mut lease = arbiter.try_acquire(Backend::Cpu).expect("lease");
        lease.release();
        lease.release();
        assert_eq!(arbiter.active(), 0);
        drop(lease);
        assert_eq!(arbiter.active(), 0);
    }

    #[test]
    fn a_panicking_worker_still_releases_its_permit() {
        let arbiter = LocalWorkloadArbiter::single();
        let handle = {
            let arbiter = arbiter.clone();
            std::thread::spawn(move || {
                let _lease = arbiter.try_acquire(Backend::Cpu).expect("lease");
                panic!("simulated worker failure");
            })
        };
        assert!(handle.join().is_err());
        assert_eq!(arbiter.active(), 0, "a panic leaked the permit");
        assert!(arbiter.try_acquire(Backend::Cpu).is_ok());
    }

    #[test]
    fn concurrent_acquirers_never_exceed_the_limit() {
        let arbiter = LocalWorkloadArbiter::with_limit(3);
        let peak = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..32)
            .map(|_| {
                let arbiter = arbiter.clone();
                let peak = Arc::clone(&peak);
                std::thread::spawn(move || {
                    if let Ok(lease) = arbiter.try_acquire(Backend::Cpu) {
                        let active = arbiter.active();
                        peak.fetch_max(active, Ordering::SeqCst);
                        std::thread::yield_now();
                        drop(lease);
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("join");
        }
        assert!(peak.load(Ordering::SeqCst) <= 3);
        assert_eq!(arbiter.active(), 0);
    }

    #[test]
    fn clones_share_one_permit_pool() {
        let arbiter = LocalWorkloadArbiter::single();
        let clone = arbiter.clone();
        let _lease = arbiter.try_acquire(Backend::Cpu).expect("lease");
        assert!(clone.try_acquire(Backend::Cpu).is_err());
    }
}
