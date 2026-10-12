//! Runtime compute-backend selection (spec §12, §14.4, §18.4 — task F2.5).
//!
//! > We will evaluate performance on laptop CPUs and Apple Metal and Windows
//! > GPU backends **without making GPU support a universal requirement**.
//!
//! So CPU is never a failure mode here: it is the baseline that always works,
//! and a GPU backend is an accelerator we take only when a runtime probe says
//! it is really there. The probe is three gated questions, in order:
//!
//! 1. Was the backend compiled into this binary? (`cfg!(feature = …)`)
//! 2. Does ggml report it at runtime? (`whisper_rs::print_system_info`)
//! 3. Does the platform actually expose the device?
//!
//! Every "no" is recorded with a reason, so the UI and the revision metadata
//! can say *which* backend ran and *why* the others did not.

use std::fmt::Write as _;

/// A compute backend whisper.cpp can run on.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// Always available. The baseline every supported configuration uses.
    Cpu,
    /// Apple Metal, macOS only.
    Metal,
    /// Vulkan, the portable Windows/Linux GPU runtime.
    Vulkan,
    /// NVIDIA CUDA.
    Cuda,
}

impl Backend {
    /// Stable label recorded in revision metadata and benchmark documents.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Backend::Cpu => "cpu",
            Backend::Metal => "metal",
            Backend::Vulkan => "vulkan",
            Backend::Cuda => "cuda",
        }
    }

    /// Parse a stable label.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cpu" => Some(Backend::Cpu),
            "metal" => Some(Backend::Metal),
            "vulkan" => Some(Backend::Vulkan),
            "cuda" => Some(Backend::Cuda),
            _ => None,
        }
    }

    /// Whether this backend offloads to a GPU.
    #[inline]
    #[must_use]
    pub const fn is_gpu(self) -> bool {
        !matches!(self, Backend::Cpu)
    }

    /// Label used in `SttCapabilities::hardware_requirements`
    /// (§8.3 `accelerator_backends`).
    #[must_use]
    pub const fn accelerator_label(self) -> &'static str {
        self.as_str()
    }

    /// Token ggml prints in its system-info string for this backend.
    const fn system_info_token(self) -> Option<&'static str> {
        match self {
            Backend::Cpu => None,
            Backend::Metal => Some("METAL"),
            Backend::Vulkan => Some("VULKAN"),
            Backend::Cuda => Some("CUDA"),
        }
    }

    /// Whether this build linked the backend in.
    ///
    /// The crate manifest enables `whisper-rs/metal` for macOS targets and
    /// `whisper-rs/vulkan` for Windows targets, so the platform determines
    /// what was linked. Nothing is inferred beyond that: whether the linked
    /// backend actually *works* is a separate runtime question answered by
    /// ggml's own report.
    #[must_use]
    pub const fn is_compiled_in(self) -> bool {
        match self {
            Backend::Cpu => true,
            Backend::Metal => cfg!(target_os = "macos"),
            Backend::Vulkan => cfg!(target_os = "windows"),
            Backend::Cuda => false,
        }
    }

    /// Whether this backend can exist on the current platform at all.
    #[must_use]
    pub const fn is_platform_possible(self) -> bool {
        match self {
            Backend::Cpu => true,
            Backend::Metal => cfg!(target_os = "macos"),
            Backend::Vulkan | Backend::Cuda => {
                cfg!(any(target_os = "windows", target_os = "linux"))
            }
        }
    }
}

/// Why a backend is not usable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "code", content = "detail")]
pub enum BackendUnavailable {
    /// The backend was not compiled into this binary.
    NotCompiledIn,
    /// The backend cannot exist on this operating system.
    UnsupportedPlatform(String),
    /// ggml reports the backend as absent at runtime.
    NotReportedByRuntime(String),
    /// The platform device or driver is missing.
    DeviceMissing(String),
    /// A previous attempt to use this backend failed in this process.
    PreviouslyFailed(String),
}

impl BackendUnavailable {
    /// Short, non-secret explanation for the UI and revision metadata.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            BackendUnavailable::NotCompiledIn => "not compiled into this build".to_string(),
            BackendUnavailable::UnsupportedPlatform(detail) => {
                format!("not available on this platform: {detail}")
            }
            BackendUnavailable::NotReportedByRuntime(detail) => {
                format!("not reported by the runtime: {detail}")
            }
            BackendUnavailable::DeviceMissing(detail) => format!("device missing: {detail}"),
            BackendUnavailable::PreviouslyFailed(detail) => {
                format!("a previous attempt failed: {detail}")
            }
        }
    }
}

/// One backend's availability verdict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BackendAvailability {
    /// The backend this verdict is about.
    pub backend: Backend,
    /// `None` when usable.
    pub unavailable: Option<BackendUnavailable>,
}

impl BackendAvailability {
    /// Usable right now.
    #[must_use]
    pub const fn available(backend: Backend) -> Self {
        Self {
            backend,
            unavailable: None,
        }
    }

    /// Not usable, with a reason.
    #[must_use]
    pub const fn unavailable(backend: Backend, reason: BackendUnavailable) -> Self {
        Self {
            backend,
            unavailable: Some(reason),
        }
    }

    /// Whether this backend may be selected.
    #[inline]
    #[must_use]
    pub const fn is_available(&self) -> bool {
        self.unavailable.is_none()
    }
}

/// Runtime probe of the available backends.
pub trait BackendProbe: Send + Sync + std::fmt::Debug {
    /// Verdict for every backend this build knows about, in a stable order.
    fn probe(&self) -> Vec<BackendAvailability>;

    /// Free-form runtime description recorded in benchmark documents.
    fn runtime_description(&self) -> String;
}

/// The real probe, backed by ggml's own reporting.
#[derive(Debug, Clone, Copy, Default)]
pub struct RuntimeBackendProbe;

impl RuntimeBackendProbe {
    /// ggml's system-info string, e.g. `… | METAL = 1 | …`.
    #[must_use]
    pub fn system_info() -> &'static str {
        whisper_rs::print_system_info()
    }

    /// Whether ggml's system-info string reports `token = 1`.
    fn runtime_reports(system_info: &str, token: &str) -> bool {
        // ggml prints `NAME = 1` / `NAME = 0` pairs separated by `|`.
        system_info.split('|').any(|field| {
            let mut parts = field.split('=');
            let name = parts.next().unwrap_or_default().trim();
            let value = parts.next().unwrap_or_default().trim();
            name.eq_ignore_ascii_case(token) && value == "1"
        })
    }

    fn evaluate(backend: Backend, system_info: &str) -> BackendAvailability {
        if !backend.is_compiled_in() {
            return BackendAvailability::unavailable(backend, BackendUnavailable::NotCompiledIn);
        }
        if !backend.is_platform_possible() {
            return BackendAvailability::unavailable(
                backend,
                BackendUnavailable::UnsupportedPlatform(std::env::consts::OS.to_string()),
            );
        }
        if let Some(token) = backend.system_info_token() {
            if !Self::runtime_reports(system_info, token) {
                return BackendAvailability::unavailable(
                    backend,
                    BackendUnavailable::NotReportedByRuntime(format!(
                        "ggml does not report {token} = 1"
                    )),
                );
            }
        }
        if let Some(reason) = platform_device_missing(backend) {
            return BackendAvailability::unavailable(
                backend,
                BackendUnavailable::DeviceMissing(reason),
            );
        }
        BackendAvailability::available(backend)
    }
}

/// Platform device check. `None` means the device is present.
fn platform_device_missing(backend: Backend) -> Option<String> {
    match backend {
        Backend::Cpu => None,
        Backend::Metal => {
            // Metal ships inside the OS; its absence means a stripped or
            // virtualised environment where GPU offload cannot work.
            let framework = std::path::Path::new("/System/Library/Frameworks/Metal.framework");
            if cfg!(target_os = "macos") && !framework.exists() {
                Some("Metal.framework is not present".to_string())
            } else {
                None
            }
        }
        Backend::Vulkan | Backend::Cuda => None,
    }
}

impl BackendProbe for RuntimeBackendProbe {
    fn probe(&self) -> Vec<BackendAvailability> {
        let system_info = Self::system_info();
        ALL_BACKENDS
            .iter()
            .map(|backend| Self::evaluate(*backend, system_info))
            .collect()
    }

    fn runtime_description(&self) -> String {
        let mut description = String::new();
        let _ = write!(
            description,
            "whisper.cpp {} on {} {}; ggml: {}",
            whisper_rs::get_whisper_version(),
            std::env::consts::OS,
            std::env::consts::ARCH,
            Self::system_info().trim()
        );
        description
    }
}

/// Backends in preference order: fastest first, CPU last.
pub const ALL_BACKENDS: [Backend; 4] =
    [Backend::Metal, Backend::Cuda, Backend::Vulkan, Backend::Cpu];

/// What the caller wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendPreference {
    /// Use the fastest available backend, falling back to CPU.
    #[default]
    PreferGpu,
    /// Use the CPU regardless of what is available. Used by hermetic tests and
    /// by the "forced-unavailable GPU" parity fixture.
    ForceCpu,
    /// Use exactly this backend, or fail. Used by benchmarks that must not
    /// silently measure something else.
    Require(Backend),
}

/// The chosen backend plus the full evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSelection {
    /// Backend to run on.
    pub chosen: Backend,
    /// Every backend with its verdict, in preference order.
    pub evaluated: Vec<BackendAvailability>,
    /// Set when a GPU backend was wanted but could not be used.
    pub fallback_reason: Option<String>,
}

impl BackendSelection {
    /// Human-readable summary for logs and revision metadata.
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.fallback_reason {
            None => format!("{} selected", self.chosen.as_str()),
            Some(reason) => format!("{} selected after fallback: {reason}", self.chosen.as_str()),
        }
    }
}

/// Why a required backend could not be provided.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("backend `{backend}` was required but is unavailable: {reason}")]
pub struct RequiredBackendUnavailable {
    /// The backend that was demanded.
    pub backend: &'static str,
    /// Why it cannot be used.
    pub reason: String,
}

/// Choose a backend from a probe result.
///
/// CPU is always available, so [`BackendPreference::PreferGpu`] can never
/// fail: that is the point of the spec's "without making GPU support a
/// universal requirement".
pub fn select_backend(
    probe: &dyn BackendProbe,
    preference: BackendPreference,
) -> Result<BackendSelection, RequiredBackendUnavailable> {
    let evaluated = probe.probe();

    match preference {
        BackendPreference::ForceCpu => Ok(BackendSelection {
            chosen: Backend::Cpu,
            evaluated,
            fallback_reason: None,
        }),
        BackendPreference::Require(required) => {
            let verdict = evaluated
                .iter()
                .find(|availability| availability.backend == required);
            match verdict.and_then(|availability| availability.unavailable.as_ref()) {
                None if verdict.is_some() => Ok(BackendSelection {
                    chosen: required,
                    evaluated,
                    fallback_reason: None,
                }),
                reason => Err(RequiredBackendUnavailable {
                    backend: required.as_str(),
                    reason: reason.map_or_else(
                        || "backend was not evaluated".to_string(),
                        BackendUnavailable::message,
                    ),
                }),
            }
        }
        BackendPreference::PreferGpu => {
            let gpu = evaluated
                .iter()
                .find(|availability| availability.backend.is_gpu() && availability.is_available());
            if let Some(gpu) = gpu {
                return Ok(BackendSelection {
                    chosen: gpu.backend,
                    evaluated,
                    fallback_reason: None,
                });
            }
            let reason = evaluated
                .iter()
                .filter(|availability| availability.backend.is_gpu())
                .filter_map(|availability| {
                    availability.unavailable.as_ref().map(|reason| {
                        format!("{}: {}", availability.backend.as_str(), reason.message())
                    })
                })
                .collect::<Vec<_>>()
                .join("; ");
            Ok(BackendSelection {
                chosen: Backend::Cpu,
                evaluated,
                fallback_reason: Some(if reason.is_empty() {
                    "no GPU backend was evaluated".to_string()
                } else {
                    reason
                }),
            })
        }
    }
}

/// Re-select after a GPU backend failed at run time.
///
/// Returns a CPU selection carrying the failure as its stated reason, so the
/// transcript records *why* it ran on the CPU rather than silently appearing
/// to have been a CPU job all along (F2.5 acceptance).
#[must_use]
pub fn fall_back_to_cpu(previous: &BackendSelection, failure: &str) -> BackendSelection {
    let mut evaluated = previous.evaluated.clone();
    for availability in &mut evaluated {
        if availability.backend == previous.chosen {
            availability.unavailable =
                Some(BackendUnavailable::PreviouslyFailed(failure.to_string()));
        }
    }
    BackendSelection {
        chosen: Backend::Cpu,
        evaluated,
        fallback_reason: Some(format!(
            "{} failed at run time: {failure}",
            previous.chosen.as_str()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A probe whose answers the test dictates, so backend-selection policy is
    /// testable on any machine.
    #[derive(Debug, Clone)]
    struct ScriptedProbe(Vec<BackendAvailability>);

    impl BackendProbe for ScriptedProbe {
        fn probe(&self) -> Vec<BackendAvailability> {
            self.0.clone()
        }

        fn runtime_description(&self) -> String {
            "scripted".to_string()
        }
    }

    fn all_unavailable() -> ScriptedProbe {
        ScriptedProbe(vec![
            BackendAvailability::unavailable(Backend::Metal, BackendUnavailable::NotCompiledIn),
            BackendAvailability::unavailable(Backend::Cuda, BackendUnavailable::NotCompiledIn),
            BackendAvailability::unavailable(Backend::Vulkan, BackendUnavailable::NotCompiledIn),
            BackendAvailability::available(Backend::Cpu),
        ])
    }

    fn metal_available() -> ScriptedProbe {
        ScriptedProbe(vec![
            BackendAvailability::available(Backend::Metal),
            BackendAvailability::unavailable(Backend::Cuda, BackendUnavailable::NotCompiledIn),
            BackendAvailability::unavailable(Backend::Vulkan, BackendUnavailable::NotCompiledIn),
            BackendAvailability::available(Backend::Cpu),
        ])
    }

    #[test]
    fn an_unavailable_gpu_falls_back_to_cpu_with_a_stated_reason() {
        let selection =
            select_backend(&all_unavailable(), BackendPreference::PreferGpu).expect("cpu baseline");
        assert_eq!(selection.chosen, Backend::Cpu);
        let reason = selection.fallback_reason.expect("a reason must be stated");
        assert!(reason.contains("metal"), "{reason}");
        assert!(reason.contains("not compiled"), "{reason}");
    }

    #[test]
    fn an_available_gpu_is_preferred() {
        let selection =
            select_backend(&metal_available(), BackendPreference::PreferGpu).expect("select");
        assert_eq!(selection.chosen, Backend::Metal);
        assert_eq!(selection.fallback_reason, None);
    }

    #[test]
    fn forcing_cpu_ignores_an_available_gpu() {
        let selection =
            select_backend(&metal_available(), BackendPreference::ForceCpu).expect("select");
        assert_eq!(selection.chosen, Backend::Cpu);
        assert_eq!(selection.fallback_reason, None);
    }

    #[test]
    fn requiring_an_unavailable_backend_fails_loudly() {
        let error = select_backend(
            &all_unavailable(),
            BackendPreference::Require(Backend::Metal),
        )
        .expect_err("a required backend must not silently become CPU");
        assert_eq!(error.backend, "metal");
        assert!(error.reason.contains("not compiled"));
    }

    #[test]
    fn requiring_an_available_backend_succeeds() {
        let selection = select_backend(
            &metal_available(),
            BackendPreference::Require(Backend::Metal),
        )
        .expect("select");
        assert_eq!(selection.chosen, Backend::Metal);
    }

    #[test]
    fn cpu_is_always_selectable() {
        let selection =
            select_backend(&all_unavailable(), BackendPreference::Require(Backend::Cpu))
                .expect("cpu is the baseline");
        assert_eq!(selection.chosen, Backend::Cpu);
    }

    #[test]
    fn a_mid_run_failure_records_why_the_cpu_ran() {
        let gpu = select_backend(&metal_available(), BackendPreference::PreferGpu).expect("select");
        let fallback = fall_back_to_cpu(&gpu, "command buffer aborted");
        assert_eq!(fallback.chosen, Backend::Cpu);
        let description = fallback.describe();
        assert!(
            description.contains("metal failed at run time"),
            "{description}"
        );
        assert!(
            description.contains("command buffer aborted"),
            "{description}"
        );
        assert!(fallback.evaluated.iter().any(|availability| matches!(
            availability.unavailable,
            Some(BackendUnavailable::PreviouslyFailed(_))
        )));
    }

    #[test]
    fn selection_is_deterministic() {
        let probe = metal_available();
        let first = select_backend(&probe, BackendPreference::PreferGpu).expect("select");
        let second = select_backend(&probe, BackendPreference::PreferGpu).expect("select");
        assert_eq!(first, second);
    }

    #[test]
    fn backend_labels_round_trip() {
        for backend in ALL_BACKENDS {
            assert_eq!(Backend::parse(backend.as_str()), Some(backend));
        }
        assert_eq!(Backend::parse("quantum"), None);
    }

    #[test]
    fn only_the_cpu_backend_is_not_a_gpu() {
        assert!(!Backend::Cpu.is_gpu());
        assert!(Backend::Metal.is_gpu());
        assert!(Backend::Vulkan.is_gpu());
        assert!(Backend::Cuda.is_gpu());
        assert_eq!(Backend::Cpu.accelerator_label(), "cpu");
        assert_eq!(Backend::Metal.accelerator_label(), "metal");
    }

    #[test]
    fn ggml_system_info_parsing_distinguishes_one_from_zero() {
        let info = "AVX = 0 | NEON = 1 | METAL = 1 | CUDA = 0 | VULKAN = 0 |";
        assert!(RuntimeBackendProbe::runtime_reports(info, "METAL"));
        assert!(!RuntimeBackendProbe::runtime_reports(info, "CUDA"));
        assert!(!RuntimeBackendProbe::runtime_reports(info, "VULKAN"));
        assert!(!RuntimeBackendProbe::runtime_reports(info, "ABSENT"));
    }

    #[test]
    fn the_real_probe_always_offers_cpu_and_explains_the_rest() {
        let probe = RuntimeBackendProbe;
        let verdicts = probe.probe();
        assert_eq!(verdicts.len(), ALL_BACKENDS.len());
        let cpu = verdicts
            .iter()
            .find(|availability| availability.backend == Backend::Cpu)
            .expect("cpu verdict");
        assert!(cpu.is_available(), "cpu must always be available");
        for verdict in &verdicts {
            if !verdict.is_available() {
                assert_ne!(
                    verdict
                        .unavailable
                        .as_ref()
                        .expect("reason")
                        .message()
                        .len(),
                    0,
                    "{:?} was rejected without a reason",
                    verdict.backend
                );
            }
        }
        assert!(probe.runtime_description().contains("whisper.cpp"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn metal_is_compiled_in_on_macos() {
        // The crate enables the `metal` feature for macOS targets, so the
        // runtime probe has something real to select.
        assert!(
            Backend::Metal.is_compiled_in(),
            "macOS builds must compile Metal in so the probe is meaningful"
        );
        assert!(Backend::Metal.is_platform_possible());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn metal_is_impossible_off_macos() {
        assert!(!Backend::Metal.is_platform_possible());
    }
}
