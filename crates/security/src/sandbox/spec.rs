//! The declarative restriction set a helper process runs under (§6.3A).
//!
//! A [`SandboxSpec`] is *what we asked the operating system for*. It is not a
//! statement about what the operating system actually enforced — that is
//! [`crate::sandbox::IsolationReport`], which is only produced after a
//! primitive has been applied successfully.
//!
//! # Path handling
//!
//! Granted paths are canonicalized at build time. This matters on macOS, where
//! `/tmp` and `/var` are symlinks into `/private`: a Seatbelt profile matches
//! the *resolved* path, so an unresolved grant would silently fail to allow the
//! file the caller meant. For a path that does not exist yet (a helper output
//! file) the deepest existing ancestor is canonicalized and the remaining
//! components are re-appended; `..` is rejected anywhere in the remainder.
//!
//! The builder accepts `impl AsRef<Path>`, so once `crate::fsguard` lands a
//! `ValidatedPath` that implements `AsRef<Path>` it can be passed directly with
//! no change here. At the time this module was written `fsguard` was still a
//! placeholder, so absoluteness and traversal are validated locally; a
//! follow-up should route grants through `fsguard` so that one module owns
//! containment policy.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::error::{self, ErrorEnvelopeParts, SecurityError};

/// Smallest memory budget a helper may be given. Below this a process cannot
/// reliably start, so accepting it would mean promising a cap we would have to
/// break.
pub const MIN_MEMORY_BYTES: u64 = 64 * 1024 * 1024;
/// Largest number of path grants in one spec. A decoder needs one input and one
/// output; an inference helper needs a model directory as well.
pub const MAX_GRANTED_PATHS: usize = 32;
/// Shortest wall-clock budget. Shorter budgets expire during process start-up.
pub const MIN_WALL_CLOCK: Duration = Duration::from_millis(100);
/// Longest wall-clock budget: a local transcription of a long recording.
pub const MAX_WALL_CLOCK: Duration = Duration::from_secs(24 * 60 * 60);

/// Default memory budget for a decoder or inference helper.
pub const DEFAULT_MEMORY_BYTES: u64 = 1024 * 1024 * 1024;
/// Default CPU-seconds budget.
pub const DEFAULT_CPU_SECONDS: u64 = 120;
/// Default wall-clock budget.
pub const DEFAULT_WALL_CLOCK: Duration = Duration::from_secs(180);

/// Network access granted to a helper.
///
/// V1 has exactly one variant on purpose: *permitting* network access from a
/// decoder or inference helper is not expressible in this API. §16.2A requires
/// every network initiation to go through [`crate::outbound`], which lives in
/// the main process and never in a helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NetworkAccess {
    /// No network access is requested for the helper.
    #[default]
    Denied,
}

/// Process-execution rights granted to a helper.
///
/// As with [`NetworkAccess`] there is one variant: a helper may be `exec`ed by
/// us, and may not `exec` anything else. Granting it is not expressible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ExecAccess {
    /// The helper itself may be executed; it may not execute anything further.
    #[default]
    DeniedBeyondHelper,
}

/// Why a [`SandboxSpec`] could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecError {
    /// A granted path was not absolute.
    #[error("sandbox grant must be an absolute path, got a relative path with {components} component(s)")]
    RelativePath {
        /// Number of components in the rejected path (the path itself is not
        /// echoed: §15.4 forbids user content in envelopes).
        components: usize,
    },
    /// A granted path contained `..` that could not be resolved away.
    #[error("sandbox grant contains an unresolved parent-directory component")]
    TraversalComponent,
    /// Neither the path nor any ancestor of it exists, so it cannot be resolved.
    #[error("sandbox grant cannot be resolved: no existing ancestor directory")]
    Unresolvable,
    /// Canonicalization failed at the filesystem level.
    #[error("sandbox grant could not be canonicalized: {kind}")]
    NotCanonicalizable {
        /// `std::io::ErrorKind` rendered as a short static label.
        kind: String,
    },
    /// Too many grants in one spec.
    #[error("sandbox spec has {count} path grants, the maximum is {max}")]
    TooManyPaths {
        /// Requested grant count.
        count: usize,
        /// [`MAX_GRANTED_PATHS`].
        max: usize,
    },
    /// Memory budget below [`MIN_MEMORY_BYTES`].
    #[error("memory budget {requested} is below the {minimum} byte minimum")]
    MemoryTooSmall {
        /// Requested budget.
        requested: u64,
        /// [`MIN_MEMORY_BYTES`].
        minimum: u64,
    },
    /// CPU budget of zero seconds.
    #[error("cpu budget must be at least one second")]
    CpuBudgetZero,
    /// Wall-clock budget outside [`MIN_WALL_CLOCK`]..=[`MAX_WALL_CLOCK`].
    #[error("wall-clock budget {requested_ms}ms is outside {min_ms}ms..={max_ms}ms")]
    WallClockOutOfRange {
        /// Requested budget in milliseconds.
        requested_ms: u128,
        /// [`MIN_WALL_CLOCK`] in milliseconds.
        min_ms: u128,
        /// [`MAX_WALL_CLOCK`] in milliseconds.
        max_ms: u128,
    },
}

impl SecurityError for SpecError {
    fn envelope_parts(&self) -> ErrorEnvelopeParts {
        match self {
            Self::RelativePath { .. }
            | Self::TraversalComponent
            | Self::Unresolvable
            | Self::NotCanonicalizable { .. } => {
                ErrorEnvelopeParts::new(error::PATH_REJECTED, self.to_string())
                    .action(error::CHOOSE_DIFFERENT_FILE)
            }
            Self::TooManyPaths { .. }
            | Self::MemoryTooSmall { .. }
            | Self::CpuBudgetZero
            | Self::WallClockOutOfRange { .. } => {
                ErrorEnvelopeParts::new(error::HELPER_FAILED, self.to_string())
            }
        }
    }
}

/// The restrictions requested for one helper process.
///
/// Build one with [`SandboxSpec::builder`]; the fields are read-only because a
/// spec is validated at construction and must stay valid afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSpec {
    read_paths: Vec<PathBuf>,
    write_paths: Vec<PathBuf>,
    memory_bytes: u64,
    cpu_seconds: u64,
    wall_clock: Duration,
    network: NetworkAccess,
    allow_exec: ExecAccess,
}

impl SandboxSpec {
    /// Start building a spec with the defaults documented on this module.
    #[must_use]
    pub fn builder() -> SandboxSpecBuilder {
        SandboxSpecBuilder {
            read_paths: Vec::new(),
            write_paths: Vec::new(),
            memory_bytes: DEFAULT_MEMORY_BYTES,
            cpu_seconds: DEFAULT_CPU_SECONDS,
            wall_clock: DEFAULT_WALL_CLOCK,
        }
    }

    /// Canonical absolute paths the helper may read.
    #[must_use]
    pub fn read_paths(&self) -> &[PathBuf] {
        &self.read_paths
    }

    /// Canonical absolute paths the helper may write.
    #[must_use]
    pub fn write_paths(&self) -> &[PathBuf] {
        &self.write_paths
    }

    /// Requested memory cap in bytes.
    #[must_use]
    pub const fn memory_bytes(&self) -> u64 {
        self.memory_bytes
    }

    /// Requested CPU budget in seconds.
    #[must_use]
    pub const fn cpu_seconds(&self) -> u64 {
        self.cpu_seconds
    }

    /// Requested wall-clock lifetime budget for the whole helper process.
    #[must_use]
    pub const fn wall_clock(&self) -> Duration {
        self.wall_clock
    }

    /// Always [`NetworkAccess::Denied`] in V1.
    #[must_use]
    pub const fn network(&self) -> NetworkAccess {
        self.network
    }

    /// Always [`ExecAccess::DeniedBeyondHelper`] in V1.
    #[must_use]
    pub const fn allow_exec(&self) -> ExecAccess {
        self.allow_exec
    }
}

/// Builder for [`SandboxSpec`].
#[derive(Debug, Clone)]
pub struct SandboxSpecBuilder {
    read_paths: Vec<PathBuf>,
    write_paths: Vec<PathBuf>,
    memory_bytes: u64,
    cpu_seconds: u64,
    wall_clock: Duration,
}

impl SandboxSpecBuilder {
    /// Grant read access to one absolute path.
    #[must_use]
    pub fn read_path(mut self, path: impl AsRef<Path>) -> Self {
        self.read_paths.push(path.as_ref().to_path_buf());
        self
    }

    /// Grant write access to one absolute path. The path need not exist yet.
    #[must_use]
    pub fn write_path(mut self, path: impl AsRef<Path>) -> Self {
        self.write_paths.push(path.as_ref().to_path_buf());
        self
    }

    /// Set the memory cap in bytes.
    #[must_use]
    pub const fn memory_bytes(mut self, bytes: u64) -> Self {
        self.memory_bytes = bytes;
        self
    }

    /// Set the CPU budget in seconds.
    #[must_use]
    pub const fn cpu_seconds(mut self, seconds: u64) -> Self {
        self.cpu_seconds = seconds;
        self
    }

    /// Set the wall-clock lifetime budget.
    #[must_use]
    pub const fn wall_clock(mut self, wall_clock: Duration) -> Self {
        self.wall_clock = wall_clock;
        self
    }

    /// Validate and canonicalize the spec.
    ///
    /// # Errors
    ///
    /// Returns [`SpecError`] if a grant is relative, contains unresolvable
    /// traversal, cannot be canonicalized, or if a budget is out of range.
    pub fn build(self) -> Result<SandboxSpec, SpecError> {
        let count = self.read_paths.len() + self.write_paths.len();
        if count > MAX_GRANTED_PATHS {
            return Err(SpecError::TooManyPaths {
                count,
                max: MAX_GRANTED_PATHS,
            });
        }
        if self.memory_bytes < MIN_MEMORY_BYTES {
            return Err(SpecError::MemoryTooSmall {
                requested: self.memory_bytes,
                minimum: MIN_MEMORY_BYTES,
            });
        }
        if self.cpu_seconds == 0 {
            return Err(SpecError::CpuBudgetZero);
        }
        if self.wall_clock < MIN_WALL_CLOCK || self.wall_clock > MAX_WALL_CLOCK {
            return Err(SpecError::WallClockOutOfRange {
                requested_ms: self.wall_clock.as_millis(),
                min_ms: MIN_WALL_CLOCK.as_millis(),
                max_ms: MAX_WALL_CLOCK.as_millis(),
            });
        }

        let read_paths = canonicalize_all(&self.read_paths)?;
        let write_paths = canonicalize_all(&self.write_paths)?;

        Ok(SandboxSpec {
            read_paths,
            write_paths,
            memory_bytes: self.memory_bytes,
            cpu_seconds: self.cpu_seconds,
            wall_clock: self.wall_clock,
            network: NetworkAccess::Denied,
            allow_exec: ExecAccess::DeniedBeyondHelper,
        })
    }
}

fn canonicalize_all(paths: &[PathBuf]) -> Result<Vec<PathBuf>, SpecError> {
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let resolved = canonicalize_grant(path)?;
        if !out.contains(&resolved) {
            out.push(resolved);
        }
    }
    Ok(out)
}

/// Resolve a grant to an absolute, symlink-free path.
///
/// At most **one** component may be missing: the leaf. A grant whose parent
/// directory does not exist is rejected rather than resolved, because the
/// unresolved middle of the path could later be created as a symlink pointing
/// somewhere else, which would silently widen the grant.
pub(crate) fn canonicalize_grant(path: &Path) -> Result<PathBuf, SpecError> {
    if !path.is_absolute() {
        return Err(SpecError::RelativePath {
            components: path.components().count(),
        });
    }

    let mut probe = path.to_path_buf();
    let mut leaf: Option<std::ffi::OsString> = None;
    if probe.symlink_metadata().is_err() {
        let Some(name) = probe.file_name().map(std::ffi::OsStr::to_os_string) else {
            return Err(SpecError::Unresolvable);
        };
        if name == *".." || name == *"." {
            return Err(SpecError::TraversalComponent);
        }
        if !probe.pop() {
            return Err(SpecError::Unresolvable);
        }
        if !probe.is_dir() {
            return Err(SpecError::Unresolvable);
        }
        leaf = Some(name);
    }

    let mut resolved = probe
        .canonicalize()
        .map_err(|err| SpecError::NotCanonicalizable {
            kind: io_kind(&err),
        })?;
    if let Some(name) = leaf {
        resolved.push(name);
    }
    if resolved
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(SpecError::TraversalComponent);
    }
    Ok(resolved)
}

fn io_kind(err: &std::io::Error) -> String {
    format!("{:?}", err.kind())
}
