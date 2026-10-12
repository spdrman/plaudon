//! Resource preflight for a local inference workload (spec §14.4 — F2.5).
//!
//! > We will monitor available disk and estimated RAM before downloading
//! > models or creating PCM intermediates.
//!
//! The rule this module enforces is "refuse with a typed reason instead of
//! OOM". It deliberately refuses to guess: when a figure cannot be measured
//! (GPU memory on most desktop stacks) it is reported as
//! [`Known::Unknown`] and **no** refusal is derived from it. An unknown
//! ceiling can block a *capability* (§8.3) but it must not manufacture a
//! resource failure, because the honest consequence of "we cannot measure
//! your VRAM" is "we did not check", not "you do not have enough".

use plaudon_ai_contracts::Known;

use crate::backend::Backend;

/// Measured host resources relevant to one inference workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCapacity {
    /// Physical RAM.
    pub total_ram_bytes: u64,
    /// Bytes free on the volume holding the scratch directory.
    pub available_disk_bytes: u64,
    /// GPU memory, when the platform lets us measure it.
    pub gpu_memory_bytes: Known<u64>,
}

/// Probes host capacity.
pub trait CapacityProbe: Send + Sync + std::fmt::Debug {
    /// Measure capacity for a workload whose scratch files live in `scratch`.
    fn capacity(&self, scratch: &std::path::Path) -> Result<HostCapacity, PreflightError>;
}

/// What a workload is estimated to need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkloadEstimate {
    /// Weights that must be resident.
    pub model_bytes: u64,
    /// Canonical PCM that must be resident.
    pub audio_bytes: u64,
    /// Decoder state, KV cache and allocator slack.
    pub overhead_bytes: u64,
    /// Scratch bytes written during the run.
    pub scratch_bytes: u64,
}

impl WorkloadEstimate {
    /// Fixed allocator and KV-cache slack.
    ///
    /// whisper.cpp's state allocation scales with the audio context, not with
    /// the recording length, because audio is processed in 30 s windows. A
    /// flat allowance is therefore the honest model; it is deliberately
    /// generous rather than tuned to one model size.
    pub const BASE_OVERHEAD_BYTES: u64 = 256 * 1024 * 1024;

    /// Estimate for a whisper.cpp run.
    ///
    /// `model_bytes` is the weight file size; whisper.cpp memory-maps it but
    /// touches essentially all of it, so it counts in full. The half-again
    /// allowance covers the decoder state and the mel spectrogram.
    #[must_use]
    pub const fn for_whisper(model_bytes: u64, audio_bytes: u64) -> Self {
        Self {
            model_bytes,
            audio_bytes,
            overhead_bytes: Self::BASE_OVERHEAD_BYTES + model_bytes / 2,
            scratch_bytes: audio_bytes,
        }
    }

    /// Total resident bytes the workload is expected to need.
    #[must_use]
    pub const fn resident_bytes(&self) -> u64 {
        self.model_bytes
            .saturating_add(self.audio_bytes)
            .saturating_add(self.overhead_bytes)
    }
}

/// Why a workload was refused before it started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PreflightError {
    /// The machine does not have enough RAM.
    #[error(
        "workload needs about {required_bytes} bytes of memory; this machine has {available_bytes}"
    )]
    InsufficientMemory {
        /// Estimated requirement.
        required_bytes: u64,
        /// Measured capacity.
        available_bytes: u64,
    },
    /// The volume does not have enough free space for the intermediates.
    #[error("workload needs {required_bytes} bytes of scratch space; {available_bytes} available")]
    InsufficientDisk {
        /// Estimated requirement.
        required_bytes: u64,
        /// Measured capacity.
        available_bytes: u64,
    },
    /// The GPU does not have enough memory. Only produced when GPU memory was
    /// actually measured.
    #[error("backend `{backend}` needs about {required_bytes} bytes of GPU memory; {available_bytes} available")]
    InsufficientGpuMemory {
        /// Backend that was requested.
        backend: &'static str,
        /// Estimated requirement.
        required_bytes: u64,
        /// Measured capacity.
        available_bytes: u64,
    },
    /// The probe itself failed.
    #[error("could not measure host capacity: {0}")]
    ProbeFailed(String),
}

/// A passed preflight, carrying what was measured so it can be logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReport {
    /// What the workload is expected to need.
    pub estimate: WorkloadEstimate,
    /// What the host reported.
    pub capacity: HostCapacity,
    /// Backend the check was made against.
    pub backend: Backend,
    /// True when GPU memory could not be measured, so no VRAM check ran.
    pub gpu_memory_unchecked: bool,
}

/// Check a workload against measured capacity.
pub fn preflight(
    estimate: WorkloadEstimate,
    capacity: HostCapacity,
    backend: Backend,
) -> Result<PreflightReport, PreflightError> {
    let resident = estimate.resident_bytes();
    if capacity.total_ram_bytes < resident {
        return Err(PreflightError::InsufficientMemory {
            required_bytes: resident,
            available_bytes: capacity.total_ram_bytes,
        });
    }
    if capacity.available_disk_bytes < estimate.scratch_bytes {
        return Err(PreflightError::InsufficientDisk {
            required_bytes: estimate.scratch_bytes,
            available_bytes: capacity.available_disk_bytes,
        });
    }

    let mut gpu_memory_unchecked = false;
    if backend.is_gpu() {
        match capacity.gpu_memory_bytes.get() {
            Some(&available) => {
                // Only the weights live on the GPU; audio buffers stay host-side.
                let required = estimate
                    .model_bytes
                    .saturating_add(estimate.overhead_bytes / 2);
                if available < required {
                    return Err(PreflightError::InsufficientGpuMemory {
                        backend: backend.as_str(),
                        required_bytes: required,
                        available_bytes: available,
                    });
                }
            }
            None => gpu_memory_unchecked = true,
        }
    }

    Ok(PreflightReport {
        estimate,
        capacity,
        backend,
        gpu_memory_unchecked,
    })
}

/// Real host capacity probe.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostCapacityProbe;

impl CapacityProbe for HostCapacityProbe {
    fn capacity(&self, scratch: &std::path::Path) -> Result<HostCapacity, PreflightError> {
        let mut probe_path = scratch;
        while !probe_path.exists() {
            match probe_path.parent() {
                Some(parent) => probe_path = parent,
                None => break,
            }
        }
        let available_disk_bytes = fs4::available_space(probe_path)
            .map_err(|source| PreflightError::ProbeFailed(source.to_string()))?;
        let total_ram_bytes = total_ram_bytes()?;
        Ok(HostCapacity {
            total_ram_bytes,
            available_disk_bytes,
            gpu_memory_bytes: gpu_memory_bytes(total_ram_bytes),
        })
    }
}

/// GPU memory, when the platform makes it knowable.
///
/// Apple Silicon has unified memory, so the GPU's budget *is* system RAM and
/// reporting it is honest. Everywhere else we would need a vendor SDK to get a
/// real number, and a guess is worse than `Unknown`.
fn gpu_memory_bytes(total_ram_bytes: u64) -> Known<u64> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Known::Known(total_ram_bytes)
    } else {
        Known::Unknown
    }
}

#[cfg(target_os = "macos")]
fn total_ram_bytes() -> Result<u64, PreflightError> {
    let output = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .map_err(|source| PreflightError::ProbeFailed(source.to_string()))?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|error| PreflightError::ProbeFailed(error.to_string()))
}

#[cfg(target_os = "linux")]
fn total_ram_bytes() -> Result<u64, PreflightError> {
    let text = std::fs::read_to_string("/proc/meminfo")
        .map_err(|source| PreflightError::ProbeFailed(source.to_string()))?;
    text.lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| {
            rest.trim()
                .trim_end_matches(" kB")
                .trim()
                .parse::<u64>()
                .ok()
        })
        .map(|kilobytes| kilobytes * 1024)
        .ok_or_else(|| PreflightError::ProbeFailed("MemTotal missing".into()))
}

#[cfg(target_os = "windows")]
fn total_ram_bytes() -> Result<u64, PreflightError> {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
        ])
        .output()
        .map_err(|source| PreflightError::ProbeFailed(source.to_string()))?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|error| PreflightError::ProbeFailed(error.to_string()))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn total_ram_bytes() -> Result<u64, PreflightError> {
    Err(PreflightError::ProbeFailed(
        "no memory probe for this platform".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn capacity(ram: u64, disk: u64, gpu: Known<u64>) -> HostCapacity {
        HostCapacity {
            total_ram_bytes: ram,
            available_disk_bytes: disk,
            gpu_memory_bytes: gpu,
        }
    }

    #[test]
    fn a_comfortable_machine_passes() {
        let estimate = WorkloadEstimate::for_whisper(75 * 1024 * 1024, 16 * 1024 * 1024);
        let report = preflight(
            estimate,
            capacity(16 * GIB, 100 * GIB, Known::Known(16 * GIB)),
            Backend::Cpu,
        )
        .expect("should pass");
        assert_eq!(report.backend, Backend::Cpu);
        assert!(!report.gpu_memory_unchecked);
    }

    #[test]
    fn insufficient_ram_is_refused_with_both_numbers() {
        let estimate = WorkloadEstimate::for_whisper(3 * GIB, 100 * 1024 * 1024);
        let error = preflight(
            estimate,
            capacity(2 * GIB, 100 * GIB, Known::Unknown),
            Backend::Cpu,
        )
        .expect_err("must refuse");
        match error {
            PreflightError::InsufficientMemory {
                required_bytes,
                available_bytes,
            } => {
                assert!(required_bytes > available_bytes);
                assert_eq!(available_bytes, 2 * GIB);
            }
            other => panic!("expected a memory refusal, got {other}"),
        }
    }

    #[test]
    fn insufficient_disk_is_refused() {
        let estimate = WorkloadEstimate::for_whisper(75 * 1024 * 1024, 4 * GIB);
        let error = preflight(
            estimate,
            capacity(64 * GIB, 1024, Known::Unknown),
            Backend::Cpu,
        )
        .expect_err("must refuse");
        assert!(
            matches!(error, PreflightError::InsufficientDisk { .. }),
            "{error}"
        );
    }

    #[test]
    fn insufficient_gpu_memory_is_refused_only_when_it_was_measured() {
        let estimate = WorkloadEstimate::for_whisper(4 * GIB, 16 * 1024 * 1024);
        let measured = preflight(
            estimate,
            capacity(64 * GIB, 100 * GIB, Known::Known(GIB)),
            Backend::Metal,
        );
        assert!(matches!(
            measured,
            Err(PreflightError::InsufficientGpuMemory { .. })
        ));

        let unmeasured = preflight(
            estimate,
            capacity(64 * GIB, 100 * GIB, Known::Unknown),
            Backend::Metal,
        )
        .expect("unknown VRAM must not manufacture a refusal");
        assert!(unmeasured.gpu_memory_unchecked);
    }

    #[test]
    fn gpu_memory_is_never_checked_for_a_cpu_workload() {
        let estimate = WorkloadEstimate::for_whisper(4 * GIB, 16 * 1024 * 1024);
        let report = preflight(
            estimate,
            capacity(64 * GIB, 100 * GIB, Known::Known(1)),
            Backend::Cpu,
        )
        .expect("cpu workloads ignore VRAM");
        assert!(!report.gpu_memory_unchecked);
    }

    #[test]
    fn the_estimate_grows_with_the_model_and_the_audio() {
        let small = WorkloadEstimate::for_whisper(75 * 1024 * 1024, 1024);
        let large = WorkloadEstimate::for_whisper(3 * GIB, 1024);
        assert!(large.resident_bytes() > small.resident_bytes());

        let long_audio = WorkloadEstimate::for_whisper(75 * 1024 * 1024, 4 * GIB);
        assert!(long_audio.resident_bytes() > small.resident_bytes());
        assert_eq!(long_audio.scratch_bytes, 4 * GIB);
    }

    #[test]
    fn the_estimate_never_overflows() {
        let estimate = WorkloadEstimate::for_whisper(u64::MAX, u64::MAX);
        assert_eq!(estimate.resident_bytes(), u64::MAX);
    }

    #[test]
    fn the_real_probe_reports_plausible_capacity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let capacity = HostCapacityProbe
            .capacity(dir.path())
            .expect("probe should succeed on a supported platform");
        assert!(capacity.total_ram_bytes >= 256 * 1024 * 1024);
        assert!(capacity.available_disk_bytes > 0);
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            assert_eq!(
                capacity.gpu_memory_bytes,
                Known::Known(capacity.total_ram_bytes),
                "unified memory means GPU budget equals system RAM"
            );
        }
    }
}
