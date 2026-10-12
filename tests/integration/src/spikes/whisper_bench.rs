//! A1.1: the whisper.cpp benchmark harness, backend probe mapping and model
//! manifest verification.
//!
//! The harness is deliberately separate from the production adapter. Its job
//! is to turn a machine into a row of numbers that spec 17.5 can be written
//! from, and to make the two policy decisions testable without a GPU:
//!
//! * a backend that is not present yields a typed reason, never a silent
//!   downgrade to CPU that is then reported as the user's choice;
//! * a model file whose digest does not match its manifest is refused before
//!   it is loaded, because model weights are untrusted input.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Lowercase hex encoding without an extra dependency.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// A compute backend whisper.cpp can be built against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// Portable CPU path.
    Cpu,
    /// Apple Metal.
    Metal,
    /// NVIDIA CUDA.
    Cuda,
    /// Vulkan, used on Windows where CUDA is absent.
    Vulkan,
}

/// What a runtime probe found for one backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BackendProbe {
    /// The backend is compiled in and a device answered.
    Available {
        /// Device name as the platform reports it.
        device: String,
    },
    /// The backend is not compiled into this build.
    NotCompiledIn,
    /// The backend is compiled in but no device answered.
    NoDevice,
    /// The platform cannot provide this backend at all.
    PlatformUnsupported {
        /// Platform identifier observed at runtime.
        platform: String,
    },
    /// The probe itself failed.
    ProbeFailed {
        /// Short machine-readable discriminator.
        detail: String,
    },
}

/// Why a capability is unavailable, in the vocabulary of spec 8.3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AvailabilityReason {
    /// Ready to use.
    Available,
    /// The host lacks the hardware.
    HardwareUnsupported {
        /// Short machine-readable discriminator.
        detail: String,
    },
    /// The platform does not offer the backend.
    PlatformUnsupported {
        /// Platform identifier observed at runtime.
        platform: String,
    },
    /// The build does not contain the backend.
    NotBuilt,
    /// The probe failed.
    ProbeFailed {
        /// Short machine-readable discriminator.
        detail: String,
    },
}

/// Map a probe result onto the capability vocabulary.
///
/// There is no arm that turns an unavailable accelerator into "CPU is fine":
/// the caller chooses a different backend explicitly, and the reason travels
/// with the decision.
#[must_use]
pub fn availability_reason(probe: &BackendProbe) -> AvailabilityReason {
    match probe {
        BackendProbe::Available { .. } => AvailabilityReason::Available,
        BackendProbe::NotCompiledIn => AvailabilityReason::NotBuilt,
        BackendProbe::NoDevice => AvailabilityReason::HardwareUnsupported {
            detail: "no_device".to_owned(),
        },
        BackendProbe::PlatformUnsupported { platform } => AvailabilityReason::PlatformUnsupported {
            platform: platform.clone(),
        },
        BackendProbe::ProbeFailed { detail } => AvailabilityReason::ProbeFailed {
            detail: detail.clone(),
        },
    }
}

/// Choose a backend from a probe table and a preference order.
///
/// Returns the chosen backend together with the reason every rejected
/// candidate was rejected, so the interface can explain the choice rather
/// than presenting it as magic.
#[must_use]
pub fn select_backend(
    probes: &BTreeMap<Backend, BackendProbe>,
    preference: &[Backend],
) -> (Option<Backend>, Vec<(Backend, AvailabilityReason)>) {
    let mut rejected = Vec::new();
    for candidate in preference {
        match probes.get(candidate) {
            Some(BackendProbe::Available { .. }) => return (Some(*candidate), rejected),
            Some(probe) => rejected.push((*candidate, availability_reason(probe))),
            None => rejected.push((*candidate, AvailabilityReason::NotBuilt)),
        }
    }
    (None, rejected)
}

/// One entry of the model manifest (spec 12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelManifestEntry {
    /// Identifier used in the user data directory path.
    pub model_id: String,
    /// Human-readable size.
    pub display_size: String,
    /// Exact byte length.
    pub byte_length: u64,
    /// Lowercase hex SHA-256 of the weights.
    pub sha256: String,
    /// Licence of the weights.
    pub license: String,
    /// Whether we are permitted to ship the bytes.
    pub redistributable: bool,
    /// Where the user obtains the weights.
    pub source_url: String,
    /// Languages the weights cover.
    pub languages: Vec<String>,
    /// Minimum free RAM the model needs.
    pub min_ram_bytes: u64,
}

/// Why a model file was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rejection", rename_all = "snake_case")]
pub enum ManifestRejection {
    /// The byte length does not match the manifest.
    LengthMismatch {
        /// Length the manifest declares.
        expected: u64,
        /// Length actually present.
        actual: u64,
    },
    /// The digest does not match the manifest.
    DigestMismatch {
        /// Digest the manifest declares.
        expected: String,
        /// Digest actually computed.
        actual: String,
    },
    /// The manifest entry itself is unusable.
    MalformedEntry {
        /// Field at fault.
        field: &'static str,
    },
}

impl ModelManifestEntry {
    /// Validate the entry itself before it is used to judge any bytes.
    pub fn validate(&self) -> Result<(), ManifestRejection> {
        if self.model_id.trim().is_empty() {
            return Err(ManifestRejection::MalformedEntry { field: "model_id" });
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ManifestRejection::MalformedEntry { field: "sha256" });
        }
        if self.sha256.chars().any(|c| c.is_ascii_uppercase()) {
            return Err(ManifestRejection::MalformedEntry { field: "sha256" });
        }
        if self.license.trim().is_empty() {
            return Err(ManifestRejection::MalformedEntry { field: "license" });
        }
        if self.byte_length == 0 {
            return Err(ManifestRejection::MalformedEntry {
                field: "byte_length",
            });
        }
        Ok(())
    }

    /// Verify candidate weights against this entry.
    pub fn verify(&self, bytes: &[u8]) -> Result<(), ManifestRejection> {
        self.validate()?;
        let actual_length = bytes.len() as u64;
        if actual_length != self.byte_length {
            return Err(ManifestRejection::LengthMismatch {
                expected: self.byte_length,
                actual: actual_length,
            });
        }
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let actual = hex_encode(&hasher.finalize());
        if actual != self.sha256 {
            return Err(ManifestRejection::DigestMismatch {
                expected: self.sha256.clone(),
                actual,
            });
        }
        Ok(())
    }

    /// Whether these weights may be shipped inside our installer.
    ///
    /// Spec 12 and ADR-0009: weights whose redistribution rights are not
    /// clear are user-downloaded only.
    #[must_use]
    pub const fn may_bundle(&self) -> bool {
        self.redistributable
    }
}

/// One machine-readable benchmark result (spec 17.5 input).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkResult {
    /// Schema version of this document.
    pub schema_version: u32,
    /// Operating system name and version, verbatim.
    pub host_os: String,
    /// CPU model, verbatim.
    pub host_cpu: String,
    /// Physical RAM in bytes.
    pub host_ram_bytes: u64,
    /// Backend the run used.
    pub backend: Backend,
    /// Model identifier.
    pub model_id: String,
    /// Duration of the input audio.
    pub audio_duration_ms: u64,
    /// Wall-clock duration of the run.
    pub wall_clock_ms: u64,
    /// Peak resident set size observed.
    pub peak_rss_bytes: u64,
    /// Milliseconds between a cancellation request and the run stopping.
    pub cancellation_latency_ms: Option<u64>,
    /// Whether the engine reported segment boundaries.
    pub segments_reported: bool,
    /// Whether the engine reported word timings.
    pub word_timestamps_reported: bool,
    /// Date of the run.
    pub run_on: String,
}

impl BenchmarkResult {
    /// Schema version this build writes and reads.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Real-time factor: wall-clock time divided by audio duration.
    ///
    /// Returns `None` for zero-length audio rather than dividing by zero or
    /// reporting a fabricated ratio.
    #[must_use]
    pub fn real_time_factor(&self) -> Option<f64> {
        if self.audio_duration_ms == 0 {
            return None;
        }
        // Both values are millisecond counts well under 2^53, so the
        // conversion is exact for any run a human would wait for.
        #[allow(clippy::cast_precision_loss)]
        let ratio = self.wall_clock_ms as f64 / self.audio_duration_ms as f64;
        Some(ratio)
    }

    /// Whether this result may be cited as a performance baseline.
    ///
    /// Spec 17.5 sets targets only from measured hardware, so a result
    /// missing its host identification is evidence of nothing.
    #[must_use]
    pub fn is_citable(&self) -> bool {
        self.schema_version == Self::SCHEMA_VERSION
            && !self.host_os.trim().is_empty()
            && !self.host_cpu.trim().is_empty()
            && self.host_ram_bytes > 0
            && self.audio_duration_ms > 0
            && self.wall_clock_ms > 0
    }
}
