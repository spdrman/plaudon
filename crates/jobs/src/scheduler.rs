//! Bounded resource scheduler (SPEC v1.3 §14.4, §14.7).
//!
//! CPU, GPU, local disk, BLE and cloud dispatch each have an independently
//! configurable concurrency limit. Admission is FIFO-fair (Tokio's semaphore
//! guarantees this), cancellable while queued, and never exceeds a configured
//! limit for any interleaving.
//!
//! Before a heavy stage starts the scheduler runs a preflight check against
//! observed disk/RAM, battery and power-saver policy, and model-asset presence
//! (§14.4, §14.7). A job that fails preflight is *parked* in a visible waiting
//! state rather than failed.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::error::JobError;
use crate::state::JobState;

/// An independently limited resource pool (§14.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceClass {
    /// Local CPU-bound inference and decoding.
    Cpu,
    /// Local GPU/Metal inference.
    Gpu,
    /// Local disk I/O: imports, staging writes, archive moves.
    LocalDisk,
    /// BLE transport sessions (single-owner actors, §6.3A).
    Ble,
    /// Outbound cloud inference dispatch.
    CloudDispatch,
}

impl ResourceClass {
    /// All classes.
    pub const ALL: &'static [ResourceClass] = &[
        ResourceClass::Cpu,
        ResourceClass::Gpu,
        ResourceClass::LocalDisk,
        ResourceClass::Ble,
        ResourceClass::CloudDispatch,
    ];

    /// Stable persisted name.
    pub const fn as_str(self) -> &'static str {
        match self {
            ResourceClass::Cpu => "cpu",
            ResourceClass::Gpu => "gpu",
            ResourceClass::LocalDisk => "local_disk",
            ResourceClass::Ble => "ble",
            ResourceClass::CloudDispatch => "cloud_dispatch",
        }
    }

    /// Parse a persisted name.
    pub fn parse(raw: &str) -> Option<ResourceClass> {
        ResourceClass::ALL.iter().copied().find(|c| c.as_str() == raw)
    }

    /// `true` for classes that constitute a "heavy local model workload"
    /// (§14.4: at most one by default on memory-constrained systems).
    pub const fn is_heavy_local(self) -> bool {
        matches!(self, ResourceClass::Cpu | ResourceClass::Gpu)
    }
}

/// Per-class concurrency limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulerLimits {
    limits: BTreeMap<ResourceClass, u32>,
}

impl Default for SchedulerLimits {
    /// §14.4 default posture: one active local model workload, a small bounded
    /// number of concurrent I/O imports, one BLE session, and a modest cloud
    /// dispatch bound.
    fn default() -> Self {
        let mut limits = BTreeMap::new();
        limits.insert(ResourceClass::Cpu, 1);
        limits.insert(ResourceClass::Gpu, 1);
        limits.insert(ResourceClass::LocalDisk, 2);
        limits.insert(ResourceClass::Ble, 1);
        limits.insert(ResourceClass::CloudDispatch, 2);
        Self { limits }
    }
}

impl SchedulerLimits {
    /// Configured limit for `class`.
    pub fn get(&self, class: ResourceClass) -> u32 {
        self.limits.get(&class).copied().unwrap_or(0)
    }

    /// Override one class. A limit of zero parks all work of that class.
    #[must_use]
    pub fn with(mut self, class: ResourceClass, limit: u32) -> Self {
        self.limits.insert(class, limit);
        self
    }

    /// Total number of heavy local workloads admitted simultaneously.
    pub fn heavy_local_total(&self) -> u32 {
        ResourceClass::ALL
            .iter()
            .filter(|c| c.is_heavy_local())
            .map(|c| self.get(*c))
            .sum()
    }
}

struct ClassState {
    semaphore: Arc<Semaphore>,
    limit: u32,
    in_flight: AtomicU32,
    high_water: AtomicU32,
}

/// An admitted slot. Holding it is the right to run; dropping it releases the
/// slot. The permit is owned so a worker can move it across await points.
pub struct Admission {
    class: ResourceClass,
    _permit: OwnedSemaphorePermit,
    state: Arc<ClassState>,
}

impl Admission {
    /// The class this admission belongs to.
    pub fn class(&self) -> ResourceClass {
        self.class
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        self.state.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Bounded per-class scheduler.
#[derive(Clone)]
pub struct ResourceScheduler {
    classes: Arc<BTreeMap<ResourceClass, Arc<ClassState>>>,
    limits: SchedulerLimits,
}

impl ResourceScheduler {
    /// Build a scheduler with the given limits.
    pub fn new(limits: SchedulerLimits) -> Self {
        let mut classes = BTreeMap::new();
        for class in ResourceClass::ALL.iter().copied() {
            let limit = limits.get(class);
            classes.insert(
                class,
                Arc::new(ClassState {
                    semaphore: Arc::new(Semaphore::new(limit as usize)),
                    limit,
                    in_flight: AtomicU32::new(0),
                    high_water: AtomicU32::new(0),
                }),
            );
        }
        Self {
            classes: Arc::new(classes),
            limits,
        }
    }

    /// The configured limits.
    pub fn limits(&self) -> &SchedulerLimits {
        &self.limits
    }

    /// Highest simultaneous in-flight count observed for `class`.
    pub fn high_water_mark(&self, class: ResourceClass) -> u32 {
        self.classes
            .get(&class)
            .map(|s| s.high_water.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    /// Currently admitted count for `class`.
    pub fn in_flight(&self, class: ResourceClass) -> u32 {
        self.classes
            .get(&class)
            .map(|s| s.in_flight.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    /// Wait for a slot in `class`, honouring cancellation while queued.
    ///
    /// Returns `Err(JobError::Cancelled)` if `cancel` fires before admission —
    /// a job sitting in the queue can always be cancelled (§14.4).
    pub async fn admit(
        &self,
        class: ResourceClass,
        job: plaudon_domain::ids::JobId,
        cancel: &CancellationToken,
    ) -> Result<Admission, JobError> {
        let state = self
            .classes
            .get(&class)
            .ok_or_else(|| JobError::UnknownResourceClass {
                class: class.as_str().to_string(),
            })?
            .clone();

        if state.limit == 0 {
            // A zero limit means "parked"; never silently run anyway.
            return Err(JobError::InsufficientResources {
                resource: "configured concurrency",
                needed: 1,
                available: 0,
            });
        }

        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(JobError::Cancelled(job)),
            permit = Arc::clone(&state.semaphore).acquire_owned() => {
                permit.map_err(|_| JobError::UnknownResourceClass {
                    class: class.as_str().to_string(),
                })?
            }
        };

        let now = state.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        state.high_water.fetch_max(now, Ordering::SeqCst);
        Ok(Admission {
            class,
            _permit: permit,
            state,
        })
    }

    /// Try to admit without waiting.
    pub fn try_admit(&self, class: ResourceClass) -> Option<Admission> {
        let state = self.classes.get(&class)?.clone();
        if state.limit == 0 {
            return None;
        }
        let permit = Arc::clone(&state.semaphore).try_acquire_owned().ok()?;
        let now = state.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        state.high_water.fetch_max(now, Ordering::SeqCst);
        Some(Admission {
            class,
            _permit: permit,
            state,
        })
    }
}

/// Where the machine's power is coming from (§14.7 battery/power-saver policy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerSource {
    Ac,
    Battery,
    Unknown,
}

/// Observed machine resources at preflight time. Supplied by the caller so the
/// decision function stays pure and testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemResources {
    pub available_disk_bytes: u64,
    pub available_ram_bytes: u64,
    pub power_source: PowerSource,
    pub power_saver_enabled: bool,
    /// Whether the model asset the job needs is present on disk.
    pub model_asset_present: bool,
    /// Whether the job's source (device, volume, folder) is reachable.
    pub source_available: bool,
}

/// What a job needs before it may start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResourceRequest {
    /// Disk needed for staging files and PCM intermediates.
    pub required_disk_bytes: u64,
    /// Peak resident memory estimated for the stage.
    pub estimated_ram_bytes: u64,
    /// `true` for stages that need a model asset on disk.
    pub needs_model_asset: bool,
    /// `true` for stages that read from a source.
    pub needs_source: bool,
    /// `true` for heavy local inference that should defer on power saver.
    pub heavy_local_compute: bool,
}

/// Outcome of the preflight check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum PreflightDecision {
    /// Start the job.
    Admit,
    /// Park the job in `state` with `reason` until the condition clears.
    /// Parking is visible and resumable; it is never a failure (§14.4).
    Park {
        state: JobState,
        reason: WaitReason,
    },
}

/// Why a job is parked. Persisted on `job_engine_state.wait_reason` and shown
/// verbatim in the Jobs view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    /// Not enough free disk for staging/intermediates.
    InsufficientDisk,
    /// Estimated peak RAM exceeds what is available.
    InsufficientMemory,
    /// A required model asset is absent or was deleted (§14.7 "visibly pause
    /// when model assets disappear").
    ModelAssetMissing,
    /// The source device/volume/folder is unreachable.
    SourceUnavailable,
    /// On battery with power saver enabled; heavy compute deferred.
    PowerSaver,
    /// Network unavailable for a cloud stage.
    NetworkUnavailable,
    /// Credential store locked.
    KeychainLocked,
    /// The user must decide something.
    UserDecisionRequired,
    /// Consent is missing, expired or revoked.
    ConsentRequired,
    /// The machine is suspending / the user session is ending.
    SystemSuspend,
}

impl WaitReason {
    /// Stable persisted name.
    pub const fn as_str(self) -> &'static str {
        match self {
            WaitReason::InsufficientDisk => "insufficient_disk",
            WaitReason::InsufficientMemory => "insufficient_memory",
            WaitReason::ModelAssetMissing => "model_asset_missing",
            WaitReason::SourceUnavailable => "source_unavailable",
            WaitReason::PowerSaver => "power_saver",
            WaitReason::NetworkUnavailable => "network_unavailable",
            WaitReason::KeychainLocked => "keychain_locked",
            WaitReason::UserDecisionRequired => "user_decision_required",
            WaitReason::ConsentRequired => "consent_required",
            WaitReason::SystemSuspend => "system_suspend",
        }
    }

    /// Parse a persisted name.
    pub fn parse(raw: &str) -> Option<WaitReason> {
        const ALL: &[WaitReason] = &[
            WaitReason::InsufficientDisk,
            WaitReason::InsufficientMemory,
            WaitReason::ModelAssetMissing,
            WaitReason::SourceUnavailable,
            WaitReason::PowerSaver,
            WaitReason::NetworkUnavailable,
            WaitReason::KeychainLocked,
            WaitReason::UserDecisionRequired,
            WaitReason::ConsentRequired,
            WaitReason::SystemSuspend,
        ];
        ALL.iter().copied().find(|r| r.as_str() == raw)
    }

    /// The persisted wait state this reason parks a job in.
    pub const fn wait_state(self) -> JobState {
        match self {
            WaitReason::InsufficientDisk | WaitReason::InsufficientMemory => {
                JobState::WaitingForUser
            }
            WaitReason::ModelAssetMissing => JobState::WaitingForModel,
            WaitReason::SourceUnavailable => JobState::WaitingForSource,
            WaitReason::PowerSaver | WaitReason::SystemSuspend => JobState::WaitingForUser,
            WaitReason::NetworkUnavailable => JobState::WaitingForNetwork,
            WaitReason::KeychainLocked | WaitReason::ConsentRequired => {
                JobState::WaitingForAuthorization
            }
            WaitReason::UserDecisionRequired => JobState::WaitingForUser,
        }
    }
}

/// Decide whether a job may start, given its needs and the observed machine.
///
/// Pure and total. Checks run in a fixed order so the decision is deterministic
/// for any input: missing asset, unavailable source, disk, memory, power.
pub fn preflight(request: ResourceRequest, system: SystemResources) -> PreflightDecision {
    let park = |reason: WaitReason| PreflightDecision::Park {
        state: reason.wait_state(),
        reason,
    };

    if request.needs_model_asset && !system.model_asset_present {
        return park(WaitReason::ModelAssetMissing);
    }
    if request.needs_source && !system.source_available {
        return park(WaitReason::SourceUnavailable);
    }
    if request.required_disk_bytes > system.available_disk_bytes {
        return park(WaitReason::InsufficientDisk);
    }
    if request.estimated_ram_bytes > system.available_ram_bytes {
        return park(WaitReason::InsufficientMemory);
    }
    if request.heavy_local_compute
        && system.power_saver_enabled
        && system.power_source == PowerSource::Battery
    {
        return park(WaitReason::PowerSaver);
    }
    PreflightDecision::Admit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roomy() -> SystemResources {
        SystemResources {
            available_disk_bytes: 100 * 1024 * 1024 * 1024,
            available_ram_bytes: 16 * 1024 * 1024 * 1024,
            power_source: PowerSource::Ac,
            power_saver_enabled: false,
            model_asset_present: true,
            source_available: true,
        }
    }

    #[test]
    fn default_limits_admit_one_local_model_workload_per_accelerator() {
        let limits = SchedulerLimits::default();
        assert_eq!(limits.get(ResourceClass::Cpu), 1);
        assert_eq!(limits.get(ResourceClass::Gpu), 1);
        assert_eq!(limits.get(ResourceClass::Ble), 1);
        assert!(limits.get(ResourceClass::LocalDisk) >= 1);
    }

    #[test]
    fn limits_are_independently_configurable() {
        let limits = SchedulerLimits::default()
            .with(ResourceClass::LocalDisk, 8)
            .with(ResourceClass::CloudDispatch, 4);
        assert_eq!(limits.get(ResourceClass::LocalDisk), 8);
        assert_eq!(limits.get(ResourceClass::CloudDispatch), 4);
        // Untouched classes keep their defaults.
        assert_eq!(limits.get(ResourceClass::Cpu), 1);
    }

    #[tokio::test]
    async fn scheduler_never_exceeds_a_class_limit() {
        let limits = SchedulerLimits::default().with(ResourceClass::LocalDisk, 3);
        let scheduler = ResourceScheduler::new(limits);
        let cancel = CancellationToken::new();
        let job = plaudon_domain::ids::JobId::new();

        let mut handles = Vec::new();
        for _ in 0..20 {
            let scheduler = scheduler.clone();
            let cancel = cancel.clone();
            handles.push(tokio::spawn(async move {
                let admission = scheduler
                    .admit(ResourceClass::LocalDisk, job, &cancel)
                    .await
                    .expect("admit");
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                drop(admission);
            }));
        }
        for handle in handles {
            handle.await.expect("task");
        }
        assert!(scheduler.high_water_mark(ResourceClass::LocalDisk) <= 3);
        assert_eq!(scheduler.in_flight(ResourceClass::LocalDisk), 0);
    }

    #[tokio::test]
    async fn classes_do_not_share_capacity() {
        let scheduler = ResourceScheduler::new(SchedulerLimits::default());
        let cpu = scheduler.try_admit(ResourceClass::Cpu).expect("cpu slot");
        // CPU is saturated at its default limit of 1 ...
        assert!(scheduler.try_admit(ResourceClass::Cpu).is_none());
        // ... but GPU and disk are unaffected.
        assert!(scheduler.try_admit(ResourceClass::Gpu).is_some());
        assert!(scheduler.try_admit(ResourceClass::LocalDisk).is_some());
        drop(cpu);
        assert!(scheduler.try_admit(ResourceClass::Cpu).is_some());
    }

    #[tokio::test]
    async fn cancellation_is_honoured_while_queued() {
        let scheduler = ResourceScheduler::new(SchedulerLimits::default().with(ResourceClass::Cpu, 1));
        let job = plaudon_domain::ids::JobId::new();
        let held = scheduler.try_admit(ResourceClass::Cpu).expect("slot");

        let cancel = CancellationToken::new();
        let waiter = {
            let scheduler = scheduler.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move { scheduler.admit(ResourceClass::Cpu, job, &cancel).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        cancel.cancel();
        let result = waiter.await.expect("task");
        assert!(matches!(result, Err(JobError::Cancelled(_))));
        drop(held);
    }

    #[tokio::test]
    async fn a_zero_limit_parks_rather_than_overrunning() {
        let scheduler =
            ResourceScheduler::new(SchedulerLimits::default().with(ResourceClass::Gpu, 0));
        assert!(scheduler.try_admit(ResourceClass::Gpu).is_none());
        let err = scheduler
            .admit(
                ResourceClass::Gpu,
                plaudon_domain::ids::JobId::new(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("must not admit");
        assert!(matches!(err, JobError::InsufficientResources { .. }));
    }

    #[test]
    fn preflight_admits_when_everything_is_available() {
        let request = ResourceRequest {
            required_disk_bytes: 1024,
            estimated_ram_bytes: 1024,
            needs_model_asset: true,
            needs_source: true,
            heavy_local_compute: true,
        };
        assert_eq!(preflight(request, roomy()), PreflightDecision::Admit);
    }

    #[test]
    fn preflight_decision_table() {
        let base = ResourceRequest {
            required_disk_bytes: 1024,
            estimated_ram_bytes: 1024,
            needs_model_asset: true,
            needs_source: true,
            heavy_local_compute: true,
        };

        let cases: Vec<(SystemResources, WaitReason)> = vec![
            (
                SystemResources {
                    model_asset_present: false,
                    ..roomy()
                },
                WaitReason::ModelAssetMissing,
            ),
            (
                SystemResources {
                    source_available: false,
                    ..roomy()
                },
                WaitReason::SourceUnavailable,
            ),
            (
                SystemResources {
                    available_disk_bytes: 0,
                    ..roomy()
                },
                WaitReason::InsufficientDisk,
            ),
            (
                SystemResources {
                    available_ram_bytes: 0,
                    ..roomy()
                },
                WaitReason::InsufficientMemory,
            ),
            (
                SystemResources {
                    power_source: PowerSource::Battery,
                    power_saver_enabled: true,
                    ..roomy()
                },
                WaitReason::PowerSaver,
            ),
        ];

        for (system, expected) in cases {
            match preflight(base, system) {
                PreflightDecision::Park { state, reason } => {
                    assert_eq!(reason, expected);
                    assert_eq!(state, expected.wait_state());
                    assert!(state.is_waiting(), "{state} must be a waiting state");
                }
                PreflightDecision::Admit => panic!("expected park for {expected:?}"),
            }
        }
    }

    #[test]
    fn a_missing_model_asset_parks_rather_than_fails() {
        let decision = preflight(
            ResourceRequest {
                needs_model_asset: true,
                ..Default::default()
            },
            SystemResources {
                model_asset_present: false,
                ..roomy()
            },
        );
        assert_eq!(
            decision,
            PreflightDecision::Park {
                state: JobState::WaitingForModel,
                reason: WaitReason::ModelAssetMissing,
            }
        );
    }

    #[test]
    fn every_wait_reason_maps_to_a_waiting_state_and_round_trips() {
        for reason in [
            WaitReason::InsufficientDisk,
            WaitReason::InsufficientMemory,
            WaitReason::ModelAssetMissing,
            WaitReason::SourceUnavailable,
            WaitReason::PowerSaver,
            WaitReason::NetworkUnavailable,
            WaitReason::KeychainLocked,
            WaitReason::UserDecisionRequired,
            WaitReason::ConsentRequired,
            WaitReason::SystemSuspend,
        ] {
            assert!(reason.wait_state().is_waiting(), "{reason:?}");
            assert_eq!(WaitReason::parse(reason.as_str()), Some(reason));
        }
    }

    #[test]
    fn resource_classes_round_trip() {
        for class in ResourceClass::ALL {
            assert_eq!(ResourceClass::parse(class.as_str()), Some(*class));
        }
    }
}
