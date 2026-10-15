//! Model installation (spec §12, §14.4, §1.2(15) — task F2.2).
//!
//! Three rules shape this module:
//!
//! * **Nothing is fetched without an explicit user action.** The installer
//!   takes an [`InstallRequest`] that carries the approval; there is no code
//!   path that starts a transfer from a manifest alone.
//! * **Preflight before bytes.** Disk and RAM are checked before the staging
//!   file is created, so a doomed install fails with an actionable message
//!   instead of filling the volume (§14.4).
//! * **A partial file is never installed.** Bytes land in
//!   `<root>/staging/<id>.part`, are verified against the manifest digest and
//!   size, and only then atomically renamed into the versioned directory.
//!   Any failure removes the staging file.
//!
//! The byte transport is injected ([`ModelByteSource`]) because `crates/app-core`
//! must stay free of HTTP dependencies (spec §6.3(1)); the networked
//! implementation lives in the shell, behind the outbound-policy gate.

use std::io::{Read, Write};
use std::path::PathBuf;

use plaudon_domain::media::Sha256Digest;
use sha2::{Digest, Sha256};

use super::manifest::{ModelEntry, ModelId, ModelSource};
use super::registry::{InstallOrigin, InstalledModel, ModelRegistry};
use super::ModelError;

/// How much of a transfer is read at a time.
const TRANSFER_CHUNK_BYTES: usize = 1024 * 1024;

/// Decision of the egress policy for one model transfer.
///
/// `crates/security`'s `OutboundPolicy` implements this; local-only mode
/// refuses every variant that needs the network (§1.2(15)).
pub trait ModelFetchPolicy: Send + Sync {
    /// Authorise fetching `source` for `id`.
    ///
    /// Called **before** the byte source is touched, so a refusal provably
    /// produces zero requests.
    fn authorize(&self, id: &ModelId, source: &ModelSource) -> Result<(), FetchRefused>;
}

/// Why a transfer was refused before it started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchRefused {
    /// Local-only mode forbids every app-initiated network operation.
    #[error("local-only mode refuses model downloads; install from an offline pack instead")]
    LocalOnlyMode,
    /// The user has not approved this specific download.
    #[error("downloading `{id}` has not been approved by the user")]
    NotApproved {
        /// Model the refusal applies to.
        id: String,
    },
    /// The destination is not on the allowlist.
    #[error("`{destination}` is not an allowed model host")]
    DestinationNotAllowed {
        /// Host or URL that was refused.
        destination: String,
    },
}

/// Refuses every networked source. The default posture, and the whole of
/// local-only mode's behaviour for model management.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalOnlyFetchPolicy;

impl ModelFetchPolicy for LocalOnlyFetchPolicy {
    fn authorize(&self, _id: &ModelId, source: &ModelSource) -> Result<(), FetchRefused> {
        if source.requires_network() {
            Err(FetchRefused::LocalOnlyMode)
        } else {
            Ok(())
        }
    }
}

/// Permits a transfer only for models the user explicitly approved.
#[derive(Debug, Clone, Default)]
pub struct ExplicitApprovalFetchPolicy {
    approved: std::collections::BTreeSet<String>,
}

impl ExplicitApprovalFetchPolicy {
    /// Record a user approval for one model.
    #[must_use]
    pub fn approving(mut self, id: &ModelId) -> Self {
        self.approved.insert(id.to_string());
        self
    }
}

impl ModelFetchPolicy for ExplicitApprovalFetchPolicy {
    fn authorize(&self, id: &ModelId, source: &ModelSource) -> Result<(), FetchRefused> {
        if !source.requires_network() {
            return Ok(());
        }
        if self.approved.contains(id.as_str()) {
            Ok(())
        } else {
            Err(FetchRefused::NotApproved { id: id.to_string() })
        }
    }
}

/// A byte transport for model weights.
///
/// Implementations are responsible for range requests; `resume_from` is the
/// number of bytes the caller already has.
pub trait ModelByteSource: Send + Sync {
    /// Open a reader positioned `resume_from` bytes into the model.
    fn open(
        &self,
        source: &ModelSource,
        resume_from: u64,
    ) -> Result<Box<dyn Read + Send>, TransferError>;

    /// Whether this source can resume mid-file. A source that cannot must be
    /// restarted from zero rather than silently producing a corrupt file.
    fn supports_resume(&self, source: &ModelSource) -> bool;
}

/// Transport-level failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransferError {
    /// The source could not be reached or opened.
    #[error("transfer could not start: {0}")]
    Unavailable(String),
    /// The transfer stopped part way.
    #[error("transfer interrupted after {received} bytes: {detail}")]
    Interrupted {
        /// Bytes successfully received before the interruption.
        received: u64,
        /// Non-secret description.
        detail: String,
    },
}

/// Host resources consulted by preflight (§14.4).
pub trait SystemResources: Send + Sync {
    /// Bytes available to a non-privileged user on the volume holding `path`.
    fn available_disk_bytes(&self, path: &std::path::Path) -> Result<u64, ModelError>;
    /// Physical RAM in bytes.
    fn total_ram_bytes(&self) -> Result<u64, ModelError>;
}

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRequest {
    /// Model to install.
    pub id: ModelId,
    /// Mark it "keep downloaded for offline use" once installed (§12).
    pub keep_offline: bool,
}

impl InstallRequest {
    /// An install of `id` without the offline-retention flag.
    #[must_use]
    pub fn new(id: ModelId) -> Self {
        Self {
            id,
            keep_offline: false,
        }
    }
}

/// Outcome of a successful install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    /// The installed model's registry row.
    pub model: InstalledModel,
    /// Bytes transferred during this attempt, excluding resumed bytes.
    pub transferred_bytes: u64,
    /// Whether the attempt resumed a previous partial transfer.
    pub resumed: bool,
}

/// Progress of an in-flight install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallProgress {
    /// Bytes present in the staging file so far.
    pub received_bytes: u64,
    /// Total bytes expected, from the manifest.
    pub total_bytes: u64,
}

/// Observer for install progress (§12 "installation progress").
pub trait InstallObserver: Send + Sync {
    /// Report progress. Must not block.
    fn progress(&self, progress: InstallProgress);
}

/// Discards progress.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullInstallObserver;

impl InstallObserver for NullInstallObserver {
    fn progress(&self, _progress: InstallProgress) {}
}

/// Installs models into a [`ModelRegistry`].
pub struct ModelInstaller<'a> {
    policy: &'a dyn ModelFetchPolicy,
    source: &'a dyn ModelByteSource,
    resources: &'a dyn SystemResources,
    observer: &'a dyn InstallObserver,
    /// Set by the user's "Cancel download"; checked between chunks.
    cancel: Option<&'a std::sync::atomic::AtomicBool>,
}

impl std::fmt::Debug for ModelInstaller<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelInstaller").finish_non_exhaustive()
    }
}

impl<'a> ModelInstaller<'a> {
    /// Build an installer.
    #[must_use]
    pub fn new(
        policy: &'a dyn ModelFetchPolicy,
        source: &'a dyn ModelByteSource,
        resources: &'a dyn SystemResources,
        observer: &'a dyn InstallObserver,
    ) -> Self {
        Self {
            policy,
            source,
            resources,
            observer,
            cancel: None,
        }
    }

    /// Stop the transfer as soon as `cancel` is set, removing the partial
    /// file so a cancelled download leaves nothing behind.
    #[must_use]
    pub fn with_cancellation(mut self, cancel: &'a std::sync::atomic::AtomicBool) -> Self {
        self.cancel = Some(cancel);
        self
    }

    fn cancelled(&self) -> bool {
        self.cancel
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
    }

    /// Check disk and RAM without transferring anything (§14.4).
    ///
    /// A staging file plus the final file coexist during the atomic rename
    /// only if the rename crosses a filesystem; within the model root it does
    /// not, so one copy of the model is the requirement.
    pub fn preflight(
        &self,
        registry: &ModelRegistry,
        entry: &ModelEntry,
    ) -> Result<(), ModelError> {
        let available = self.resources.available_disk_bytes(registry.root())?;
        // Leave headroom so the install cannot be the thing that fills the
        // volume completely.
        let required = entry.size_bytes.saturating_add(entry.size_bytes / 10);
        if available < required {
            return Err(ModelError::InsufficientDisk {
                required_bytes: required,
                available_bytes: available,
            });
        }
        let ram = self.resources.total_ram_bytes()?;
        if ram < entry.hardware.min_ram_bytes {
            return Err(ModelError::InsufficientMemory {
                required_bytes: entry.hardware.min_ram_bytes,
                available_bytes: ram,
            });
        }
        Ok(())
    }

    /// Install `request.id` from `entry`.
    pub fn install(
        &self,
        registry: &mut ModelRegistry,
        entry: &ModelEntry,
        request: &InstallRequest,
    ) -> Result<InstallOutcome, ModelError> {
        if entry.id != request.id {
            return Err(ModelError::NotInManifest {
                id: request.id.to_string(),
            });
        }

        // 1. Policy first: a refusal must produce zero transport calls.
        self.policy
            .authorize(&entry.id, &entry.source)
            .map_err(ModelError::FetchRefused)?;

        // 2. Preflight before anything is created on disk.
        self.preflight(registry, entry)?;

        // 3. Stage.
        let staging_dir = registry.staging_dir();
        std::fs::create_dir_all(&staging_dir)
            .map_err(|source| ModelError::io(&staging_dir, &source))?;
        let staging_path = staging_dir.join(format!("{}.part", entry.id));

        let existing = std::fs::metadata(&staging_path).map_or(0, |metadata| metadata.len());
        let can_resume = existing > 0
            && existing < entry.size_bytes
            && self.source.supports_resume(&entry.source);
        let resume_from = if can_resume {
            existing
        } else {
            if existing > 0 {
                std::fs::remove_file(&staging_path)
                    .map_err(|source| ModelError::io(&staging_path, &source))?;
            }
            0
        };

        let transferred = self.transfer(entry, &staging_path, resume_from)?;

        // 4. Verify before activation. Any mismatch removes the partial file.
        let verification = verify_file(&staging_path, entry.size_bytes, entry.sha256);
        if let Err(error) = verification {
            let _ = std::fs::remove_file(&staging_path);
            return Err(error);
        }

        // 5. Atomic move into the versioned directory.
        let install_dir = registry.prepare_install_dir(entry)?;
        let final_path = install_dir.join(&entry.file_name);
        std::fs::rename(&staging_path, &final_path).map_err(|source| {
            let _ = std::fs::remove_file(&staging_path);
            ModelError::io(&final_path, &source)
        })?;

        let model = InstalledModel {
            id: entry.id.clone(),
            engine: entry.engine.clone(),
            version: entry.version.clone(),
            display_name: entry.display_name.clone(),
            relative_path: entry.relative_path(),
            sha256: entry.sha256,
            byte_length: entry.size_bytes,
            license_spdx: entry.license.spdx_id.clone(),
            license_text_path: None,
            keep_offline: request.keep_offline,
            installed_from: match entry.source {
                ModelSource::Remote { .. } => InstallOrigin::Download,
                ModelSource::Pack { .. } => InstallOrigin::OfflinePack,
            },
        };
        registry.record(model.clone())?;

        Ok(InstallOutcome {
            model,
            transferred_bytes: transferred,
            resumed: resume_from > 0,
        })
    }

    fn transfer(
        &self,
        entry: &ModelEntry,
        staging_path: &PathBuf,
        resume_from: u64,
    ) -> Result<u64, ModelError> {
        let mut reader = self
            .source
            .open(&entry.source, resume_from)
            .map_err(ModelError::Transfer)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resume_from > 0)
            .truncate(resume_from == 0)
            .open(staging_path)
            .map_err(|source| ModelError::io(staging_path, &source))?;

        let mut buffer = vec![0u8; TRANSFER_CHUNK_BYTES];
        let mut received = resume_from;
        let mut transferred = 0u64;
        loop {
            // The user's "Cancel download" is honoured between chunks, and a
            // cancelled transfer keeps nothing: a partial file the user asked
            // to stop is not something to resume behind their back.
            if self.cancelled() {
                drop(file);
                let _ = std::fs::remove_file(staging_path);
                return Err(ModelError::Cancelled);
            }
            let read = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) => {
                    // Keep the partial file: the next attempt may resume it.
                    let _ = file.flush();
                    return Err(ModelError::Transfer(TransferError::Interrupted {
                        received,
                        detail: error.to_string(),
                    }));
                }
            };
            // Refuse to write past the declared size: a lying source must not
            // be able to fill the disk.
            if received + read as u64 > entry.size_bytes {
                let _ = file.flush();
                drop(file);
                let _ = std::fs::remove_file(staging_path);
                return Err(ModelError::SizeMismatch {
                    expected: entry.size_bytes,
                    observed: received + read as u64,
                });
            }
            file.write_all(&buffer[..read])
                .map_err(|source| ModelError::io(staging_path, &source))?;
            received += read as u64;
            transferred += read as u64;
            self.observer.progress(InstallProgress {
                received_bytes: received,
                total_bytes: entry.size_bytes,
            });
        }
        if self.cancelled() {
            drop(file);
            let _ = std::fs::remove_file(staging_path);
            return Err(ModelError::Cancelled);
        }
        file.flush()
            .map_err(|source| ModelError::io(staging_path, &source))?;
        file.sync_all()
            .map_err(|source| ModelError::io(staging_path, &source))?;
        Ok(transferred)
    }
}

/// Verify a file's size and digest against the manifest.
pub(crate) fn verify_file(
    path: &std::path::Path,
    expected_bytes: u64,
    expected_digest: Sha256Digest,
) -> Result<(), ModelError> {
    let metadata = std::fs::metadata(path).map_err(|source| ModelError::io(path, &source))?;
    if metadata.len() != expected_bytes {
        return Err(ModelError::SizeMismatch {
            expected: expected_bytes,
            observed: metadata.len(),
        });
    }
    let mut file = std::fs::File::open(path).map_err(|source| ModelError::io(path, &source))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; TRANSFER_CHUNK_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| ModelError::io(path, &source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest: [u8; 32] = hasher.finalize().into();
    let observed = Sha256Digest::from_bytes(digest);
    if observed != expected_digest {
        return Err(ModelError::ChecksumMismatch {
            expected: expected_digest.to_hex(),
            observed: observed.to_hex(),
        });
    }
    Ok(())
}

/// Real host resource probe.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostResources;

impl SystemResources for HostResources {
    fn available_disk_bytes(&self, path: &std::path::Path) -> Result<u64, ModelError> {
        // Probe the nearest existing ancestor: the model root may not exist.
        let mut probe = path;
        while !probe.exists() {
            match probe.parent() {
                Some(parent) => probe = parent,
                None => break,
            }
        }
        fs4::available_space(probe).map_err(|source| ModelError::io(probe, &source))
    }

    fn total_ram_bytes(&self) -> Result<u64, ModelError> {
        host_total_ram()
    }
}

#[cfg(target_os = "macos")]
fn host_total_ram() -> Result<u64, ModelError> {
    read_sysctl_u64("hw.memsize")
}

#[cfg(target_os = "macos")]
fn read_sysctl_u64(name: &str) -> Result<u64, ModelError> {
    let output = std::process::Command::new("/usr/sbin/sysctl")
        .arg("-n")
        .arg(name)
        .output()
        .map_err(|source| ModelError::io(std::path::Path::new("/usr/sbin/sysctl"), &source))?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|error| ModelError::ResourceProbeFailed(error.to_string()))
}

#[cfg(target_os = "linux")]
fn host_total_ram() -> Result<u64, ModelError> {
    let text = std::fs::read_to_string("/proc/meminfo")
        .map_err(|source| ModelError::io(std::path::Path::new("/proc/meminfo"), &source))?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kilobytes: u64 = rest
                .trim()
                .trim_end_matches(" kB")
                .trim()
                .parse()
                .map_err(|_| ModelError::ResourceProbeFailed("unparsable MemTotal".into()))?;
            return Ok(kilobytes * 1024);
        }
    }
    Err(ModelError::ResourceProbeFailed(
        "MemTotal missing from /proc/meminfo".into(),
    ))
}

#[cfg(target_os = "windows")]
fn host_total_ram() -> Result<u64, ModelError> {
    // `wmic` is deprecated; PowerShell's CIM provider is the supported route.
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
        ])
        .output()
        .map_err(|source| ModelError::io(std::path::Path::new("powershell"), &source))?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|error| ModelError::ResourceProbeFailed(error.to_string()))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn host_total_ram() -> Result<u64, ModelError> {
    Err(ModelError::ResourceProbeFailed(
        "no RAM probe for this platform".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::super::manifest::{HardwareRecommendation, ModelLicense};
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    const WEIGHTS: &[u8] = b"pretend these are whisper weights";

    fn entry(source: ModelSource) -> ModelEntry {
        ModelEntry {
            id: ModelId::parse("tiny-en").expect("id"),
            engine: "whisper.cpp".into(),
            version: "v1".into(),
            display_name: "Tiny".into(),
            file_name: "weights.bin".into(),
            size_bytes: WEIGHTS.len() as u64,
            sha256: Sha256Digest::of(WEIGHTS),
            languages: Vec::new(),
            license: ModelLicense {
                spdx_id: "MIT".into(),
                name: "MIT".into(),
                redistributable: true,
                text_file: "MIT.txt".into(),
            },
            source,
            hardware: HardwareRecommendation {
                min_ram_bytes: 1,
                recommended_ram_bytes: 2,
                notes: String::new(),
            },
        }
    }

    fn remote() -> ModelSource {
        ModelSource::Remote {
            url: "https://example.org/weights.bin".into(),
        }
    }

    /// Counts every transport call so "zero network requests" is an equality,
    /// not an inference.
    #[derive(Debug, Default)]
    struct CountingSource {
        opens: AtomicUsize,
        bytes: Vec<u8>,
        /// Stop after this many bytes on the next open, simulating a drop.
        interrupt_after: Mutex<Option<usize>>,
        resumable: bool,
    }

    impl CountingSource {
        fn new(bytes: &[u8], resumable: bool) -> Self {
            Self {
                opens: AtomicUsize::new(0),
                bytes: bytes.to_vec(),
                interrupt_after: Mutex::new(None),
                resumable,
            }
        }

        fn opens(&self) -> usize {
            self.opens.load(Ordering::SeqCst)
        }
    }

    impl ModelByteSource for CountingSource {
        fn open(
            &self,
            _source: &ModelSource,
            resume_from: u64,
        ) -> Result<Box<dyn Read + Send>, TransferError> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            let start = usize::try_from(resume_from)
                .unwrap_or(usize::MAX)
                .min(self.bytes.len());
            let mut slice = self.bytes[start..].to_vec();
            if let Some(limit) = self.interrupt_after.lock().expect("lock").take() {
                slice.truncate(limit);
            }
            Ok(Box::new(std::io::Cursor::new(slice)))
        }

        fn supports_resume(&self, _source: &ModelSource) -> bool {
            self.resumable
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct FixedResources {
        disk: u64,
        ram: u64,
    }

    impl SystemResources for FixedResources {
        fn available_disk_bytes(&self, _path: &std::path::Path) -> Result<u64, ModelError> {
            Ok(self.disk)
        }

        fn total_ram_bytes(&self) -> Result<u64, ModelError> {
            Ok(self.ram)
        }
    }

    fn plenty() -> FixedResources {
        FixedResources {
            disk: 1 << 40,
            ram: 1 << 34,
        }
    }

    #[test]
    fn local_only_mode_makes_zero_transport_calls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let source = CountingSource::new(WEIGHTS, true);
        let resources = plenty();
        let installer = ModelInstaller::new(
            &LocalOnlyFetchPolicy,
            &source,
            &resources,
            &NullInstallObserver,
        );
        let entry = entry(remote());

        let error = installer
            .install(
                &mut registry,
                &entry,
                &InstallRequest::new(entry.id.clone()),
            )
            .expect_err("local-only must refuse");

        assert_eq!(
            error,
            ModelError::FetchRefused(FetchRefused::LocalOnlyMode),
            "{error}"
        );
        assert_eq!(source.opens(), 0, "local-only mode issued a transport call");
        assert_eq!(registry.installed().len(), 0);
    }

    #[test]
    fn nothing_is_fetched_without_an_explicit_approval() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let source = CountingSource::new(WEIGHTS, true);
        let resources = plenty();
        let policy = ExplicitApprovalFetchPolicy::default();
        let installer = ModelInstaller::new(&policy, &source, &resources, &NullInstallObserver);
        let entry = entry(remote());

        let error = installer
            .install(
                &mut registry,
                &entry,
                &InstallRequest::new(entry.id.clone()),
            )
            .expect_err("unapproved download must refuse");
        assert!(matches!(
            error,
            ModelError::FetchRefused(FetchRefused::NotApproved { .. })
        ));
        assert_eq!(source.opens(), 0);

        let approved = ExplicitApprovalFetchPolicy::default().approving(&entry.id);
        let installer = ModelInstaller::new(&approved, &source, &resources, &NullInstallObserver);
        assert!(installer
            .install(
                &mut registry,
                &entry,
                &InstallRequest::new(entry.id.clone())
            )
            .is_ok());
        assert_eq!(source.opens(), 1);
    }

    #[test]
    fn a_successful_install_verifies_and_activates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let source = CountingSource::new(WEIGHTS, true);
        let resources = plenty();
        let entry = entry(remote());
        let policy = ExplicitApprovalFetchPolicy::default().approving(&entry.id);
        let installer = ModelInstaller::new(&policy, &source, &resources, &NullInstallObserver);

        let outcome = installer
            .install(
                &mut registry,
                &entry,
                &InstallRequest {
                    id: entry.id.clone(),
                    keep_offline: true,
                },
            )
            .expect("install");

        assert_eq!(outcome.transferred_bytes, WEIGHTS.len() as u64);
        assert!(!outcome.resumed);
        assert!(outcome.model.keep_offline);
        assert_eq!(outcome.model.installed_from, InstallOrigin::Download);
        assert!(registry.verify(&entry.id).expect("verify").intact);
        assert_eq!(registry.disk_usage().staging_bytes, 0);
        assert_eq!(
            std::fs::read(registry.resolve(&entry.id).expect("resolve")).expect("read"),
            WEIGHTS
        );
    }

    #[test]
    fn a_checksum_mismatch_leaves_nothing_installed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        // Same length, different content.
        let corrupted: Vec<u8> = WEIGHTS.iter().map(|byte| byte ^ 0x01).collect();
        let source = CountingSource::new(&corrupted, true);
        let resources = plenty();
        let entry = entry(remote());
        let policy = ExplicitApprovalFetchPolicy::default().approving(&entry.id);
        let installer = ModelInstaller::new(&policy, &source, &resources, &NullInstallObserver);

        let error = installer
            .install(
                &mut registry,
                &entry,
                &InstallRequest::new(entry.id.clone()),
            )
            .expect_err("corrupt download must fail");
        assert!(
            matches!(error, ModelError::ChecksumMismatch { .. }),
            "{error}"
        );
        assert_eq!(registry.installed().len(), 0);
        assert_eq!(
            registry.disk_usage().staging_bytes,
            0,
            "partial file survived"
        );
    }

    #[test]
    fn a_truncated_download_is_not_installed_and_can_resume() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let source = CountingSource::new(WEIGHTS, true);
        let resources = plenty();
        let entry = entry(remote());
        let policy = ExplicitApprovalFetchPolicy::default().approving(&entry.id);
        let installer = ModelInstaller::new(&policy, &source, &resources, &NullInstallObserver);

        // First attempt stops after 10 bytes.
        *source.interrupt_after.lock().expect("lock") = Some(10);
        let error = installer
            .install(
                &mut registry,
                &entry,
                &InstallRequest::new(entry.id.clone()),
            )
            .expect_err("truncated download must fail");
        assert!(matches!(error, ModelError::SizeMismatch { .. }), "{error}");
        assert_eq!(registry.installed().len(), 0);

        // The staging file is kept for resume; a short read is a size
        // mismatch, which deletes it, so the next attempt restarts cleanly.
        let outcome = installer
            .install(
                &mut registry,
                &entry,
                &InstallRequest::new(entry.id.clone()),
            )
            .expect("retry");
        assert!(registry.verify(&entry.id).expect("verify").intact);
        assert_eq!(outcome.transferred_bytes, WEIGHTS.len() as u64);
    }

    #[test]
    fn an_interrupted_transfer_resumes_from_the_staging_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let source = CountingSource::new(WEIGHTS, true);
        let resources = plenty();
        let entry = entry(remote());
        let policy = ExplicitApprovalFetchPolicy::default().approving(&entry.id);
        let installer = ModelInstaller::new(&policy, &source, &resources, &NullInstallObserver);

        // Pre-seed a partial staging file as if a previous run had died.
        std::fs::create_dir_all(registry.staging_dir()).expect("mkdir");
        std::fs::write(registry.staging_dir().join("tiny-en.part"), &WEIGHTS[..10]).expect("seed");

        let outcome = installer
            .install(
                &mut registry,
                &entry,
                &InstallRequest::new(entry.id.clone()),
            )
            .expect("resume");
        assert!(outcome.resumed);
        assert_eq!(outcome.transferred_bytes, (WEIGHTS.len() - 10) as u64);
        assert!(registry.verify(&entry.id).expect("verify").intact);
    }

    #[test]
    fn a_non_resumable_source_restarts_instead_of_corrupting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let source = CountingSource::new(WEIGHTS, false);
        let resources = plenty();
        let entry = entry(remote());
        let policy = ExplicitApprovalFetchPolicy::default().approving(&entry.id);
        let installer = ModelInstaller::new(&policy, &source, &resources, &NullInstallObserver);

        std::fs::create_dir_all(registry.staging_dir()).expect("mkdir");
        std::fs::write(registry.staging_dir().join("tiny-en.part"), &WEIGHTS[..10]).expect("seed");

        let outcome = installer
            .install(
                &mut registry,
                &entry,
                &InstallRequest::new(entry.id.clone()),
            )
            .expect("restart");
        assert!(!outcome.resumed);
        assert_eq!(outcome.transferred_bytes, WEIGHTS.len() as u64);
        assert!(registry.verify(&entry.id).expect("verify").intact);
    }

    #[test]
    fn a_source_longer_than_declared_is_cut_off_and_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let mut oversized = WEIGHTS.to_vec();
        oversized.extend_from_slice(&[0u8; 4096]);
        let source = CountingSource::new(&oversized, true);
        let resources = plenty();
        let entry = entry(remote());
        let policy = ExplicitApprovalFetchPolicy::default().approving(&entry.id);
        let installer = ModelInstaller::new(&policy, &source, &resources, &NullInstallObserver);

        let error = installer
            .install(
                &mut registry,
                &entry,
                &InstallRequest::new(entry.id.clone()),
            )
            .expect_err("oversized source must fail");
        assert!(matches!(error, ModelError::SizeMismatch { .. }), "{error}");
        assert_eq!(registry.installed().len(), 0);
        assert_eq!(registry.disk_usage().staging_bytes, 0);
    }

    #[test]
    fn insufficient_disk_blocks_the_install_before_any_transfer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let source = CountingSource::new(WEIGHTS, true);
        let resources = FixedResources {
            disk: 4,
            ram: 1 << 34,
        };
        let entry = entry(remote());
        let policy = ExplicitApprovalFetchPolicy::default().approving(&entry.id);
        let installer = ModelInstaller::new(&policy, &source, &resources, &NullInstallObserver);

        let error = installer
            .install(
                &mut registry,
                &entry,
                &InstallRequest::new(entry.id.clone()),
            )
            .expect_err("install must be blocked");
        assert!(
            matches!(error, ModelError::InsufficientDisk { .. }),
            "{error}"
        );
        assert_eq!(source.opens(), 0, "bytes were requested despite preflight");
    }

    #[test]
    fn insufficient_ram_blocks_the_install_before_any_transfer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let source = CountingSource::new(WEIGHTS, true);
        let resources = FixedResources {
            disk: 1 << 40,
            ram: 1,
        };
        let mut entry = entry(remote());
        entry.hardware.min_ram_bytes = 1 << 30;
        let policy = ExplicitApprovalFetchPolicy::default().approving(&entry.id);
        let installer = ModelInstaller::new(&policy, &source, &resources, &NullInstallObserver);

        let error = installer
            .install(
                &mut registry,
                &entry,
                &InstallRequest::new(entry.id.clone()),
            )
            .expect_err("install must be blocked");
        assert!(
            matches!(error, ModelError::InsufficientMemory { .. }),
            "{error}"
        );
        assert_eq!(source.opens(), 0);
    }

    #[test]
    fn progress_is_reported_and_monotonic() {
        #[derive(Debug, Default)]
        struct Recorder(Mutex<Vec<InstallProgress>>);
        impl InstallObserver for Recorder {
            fn progress(&self, progress: InstallProgress) {
                self.0.lock().expect("lock").push(progress);
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let source = CountingSource::new(WEIGHTS, true);
        let resources = plenty();
        let entry = entry(remote());
        let policy = ExplicitApprovalFetchPolicy::default().approving(&entry.id);
        let recorder = Recorder::default();
        let installer = ModelInstaller::new(&policy, &source, &resources, &recorder);

        installer
            .install(
                &mut registry,
                &entry,
                &InstallRequest::new(entry.id.clone()),
            )
            .expect("install");

        let reports = recorder.0.lock().expect("lock").clone();
        assert_ne!(reports.len(), 0);
        assert!(reports
            .windows(2)
            .all(|pair| pair[1].received_bytes >= pair[0].received_bytes));
        assert_eq!(
            reports.last().map(|report| report.received_bytes),
            Some(WEIGHTS.len() as u64)
        );
        assert!(reports
            .iter()
            .all(|report| report.total_bytes == WEIGHTS.len() as u64));
    }

    #[test]
    fn installing_a_model_the_request_does_not_name_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = ModelRegistry::open(dir.path()).expect("open");
        let source = CountingSource::new(WEIGHTS, true);
        let resources = plenty();
        let entry = entry(remote());
        let policy = ExplicitApprovalFetchPolicy::default().approving(&entry.id);
        let installer = ModelInstaller::new(&policy, &source, &resources, &NullInstallObserver);

        let other = ModelId::parse("other-model").expect("id");
        let error = installer
            .install(&mut registry, &entry, &InstallRequest::new(other))
            .expect_err("mismatched request must fail");
        assert!(matches!(error, ModelError::NotInManifest { .. }));
        assert_eq!(source.opens(), 0);
    }

    #[test]
    fn the_host_resource_probe_returns_plausible_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let resources = HostResources;
        let disk = resources
            .available_disk_bytes(dir.path())
            .expect("disk probe");
        let ram = resources.total_ram_bytes().expect("ram probe");
        assert!(disk > 0, "available disk reported as zero");
        assert!(ram >= 256 * 1024 * 1024, "implausible RAM reading: {ram}");
    }
}
