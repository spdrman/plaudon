//! Locating the vendor CLI (spec 4.2.1, 7.1).
//!
//! Spec 7.1 requires "a hardcoded executable name/path selected during
//! discovery" and forbids accepting an arbitrary user-supplied executable path
//! as auto-approved. Three rules follow, and this module is where they live:
//!
//! 1. **The leaf name is a constant.** We look for exactly `plaud`
//!    (`plaud.exe` on Windows) and nothing else. `plaud-next`, `plaud.sh` or a
//!    directory called `plaud` are not candidates.
//! 2. **The search space is a fixed list.** Well-known package-manager install
//!    directories first, then `PATH` entries, each joined with the constant
//!    leaf name. A `PATH` entry can only ever contribute a file with our leaf
//!    name, so a poisoned `PATH` cannot smuggle in a differently named binary.
//! 3. **A user-chosen path is never auto-approved.** It must arrive with an
//!    explicit [`UserApproval`], recorded at a known time, and it is still
//!    checked for absoluteness, canonical components and executability.
//!
//! On Windows we deliberately refuse the `plaud.cmd` / `plaud.ps1` shims that
//! npm installs alongside the real program. Launching either requires handing a
//! command string to `cmd.exe` or PowerShell, which spec 7.1 forbids outright,
//! so an installation that offers only a shim is reported as
//! [`DiscoveryError::ShimRequiresInterpreter`] with instructions rather than
//! silently downgraded.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use plaudon_recording_sources::source::SourceError;
use serde::{Deserialize, Serialize};

/// The only executable leaf name we will launch.
#[cfg(not(windows))]
pub const EXECUTABLE_LEAF: &str = "plaud";
/// The only executable leaf name we will launch.
#[cfg(windows)]
pub const EXECUTABLE_LEAF: &str = "plaud.exe";

/// Interpreter shims we recognize in order to explain them, never to run them.
pub const REFUSED_SHIM_LEAVES: &[&str] = &["plaud.cmd", "plaud.bat", "plaud.ps1"];

/// Where a candidate executable came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryOrigin {
    /// A well-known package-manager installation directory.
    WellKnownLocation,
    /// A directory listed in the `PATH` environment variable.
    PathEnvironment,
    /// A path the user selected and explicitly approved.
    UserSelected,
}

impl DiscoveryOrigin {
    /// Label for the diagnostics panel.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::WellKnownLocation => "standard installation directory",
            Self::PathEnvironment => "a directory on your PATH",
            Self::UserSelected => "a location you selected",
        }
    }
}

/// Evidence that the user chose a non-standard executable on purpose.
///
/// There is no `Implicit` variant: spec 7.1 has no notion of an auto-approved
/// user path, so an unapproved path is simply not representable as approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserApproval {
    /// Milliseconds since the Unix epoch at which the user confirmed.
    pub approved_at_ms: i64,
    /// The exact path string the user confirmed, for the audit record.
    pub approved_path: String,
}

/// A located executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredCli {
    /// Absolute, canonical path of the executable.
    pub program: PathBuf,
    /// How it was found.
    pub origin: DiscoveryOrigin,
    /// Approval record, present only for a user-selected path.
    pub approval: Option<UserApproval>,
}

/// Why discovery failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    /// No candidate location held the executable.
    #[error("the PLAUD command line tool is not installed")]
    NotFound,
    /// Only an interpreter shim was found.
    #[error("only a script shim for the PLAUD command line tool was found")]
    ShimRequiresInterpreter {
        /// Path of the shim, for the diagnostics panel.
        shim: PathBuf,
    },
    /// A user-selected path arrived without an approval record.
    #[error("a non-standard tool location must be confirmed before it is used")]
    NotApproved,
    /// A user-selected path is relative.
    #[error("the tool location must be an absolute path")]
    PathNotAbsolute,
    /// A user-selected path contains `.` or `..`.
    #[error("the tool location contains an unsafe path component")]
    PathNotCanonical,
    /// The candidate exists but is not an executable file.
    #[error("the selected file is not an executable program")]
    NotExecutable,
}

impl From<DiscoveryError> for SourceError {
    fn from(error: DiscoveryError) -> Self {
        match error {
            DiscoveryError::NotFound => Self::CliMissing,
            // A shim, an unapproved path or a non-executable file are all
            // "we found something we will not drive", which is the same user
            // decision as an unsupported version: install a supported tool.
            DiscoveryError::ShimRequiresInterpreter { .. }
            | DiscoveryError::NotApproved
            | DiscoveryError::PathNotAbsolute
            | DiscoveryError::PathNotCanonical
            | DiscoveryError::NotExecutable => Self::CliUnsupported,
        }
    }
}

/// Filesystem questions discovery needs answered.
///
/// Behind a trait so the search policy is testable without creating real
/// executables in well-known system directories.
pub trait ExecutableProbe: fmt::Debug + Send + Sync {
    /// Whether `path` is a regular file the current user may execute.
    fn is_executable_file(&self, path: &Path) -> bool;
    /// Whether `path` exists at all, executable or not.
    fn exists(&self, path: &Path) -> bool;
    /// Resolve symlinks and `.`/`..` to an absolute path.
    fn canonicalize(&self, path: &Path) -> Option<PathBuf>;
}

/// The real filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsProbe;

impl ExecutableProbe for OsProbe {
    fn is_executable_file(&self, path: &Path) -> bool {
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        if !meta.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    }

    fn exists(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path).ok()
    }
}

/// Searches a fixed candidate list for the vendor CLI.
#[derive(Debug, Clone)]
pub struct CliDiscovery {
    probe: Arc<dyn ExecutableProbe>,
    environment: BTreeMap<String, String>,
}

impl CliDiscovery {
    /// Discovery over the real filesystem and the current environment.
    #[must_use]
    pub fn new() -> Self {
        Self {
            probe: Arc::new(OsProbe),
            environment: std::env::vars().collect(),
        }
    }

    /// Discovery over an injected filesystem and environment.
    #[must_use]
    pub fn with_probe(
        probe: Arc<dyn ExecutableProbe>,
        environment: BTreeMap<String, String>,
    ) -> Self {
        Self { probe, environment }
    }

    /// Directories we search before consulting `PATH`.
    ///
    /// These are the documented install locations of the package managers that
    /// can install `@plaud-ai/cli`. The list is data, not a glob: we never walk
    /// a directory tree looking for something that resembles the tool.
    #[must_use]
    pub fn well_known_directories(&self) -> Vec<PathBuf> {
        let home = self
            .environment
            .get("HOME")
            .or_else(|| self.environment.get("USERPROFILE"))
            .map(PathBuf::from);
        let mut dirs: Vec<PathBuf> = Vec::new();

        #[cfg(not(windows))]
        {
            dirs.extend(
                [
                    "/opt/homebrew/bin",
                    "/usr/local/bin",
                    "/usr/bin",
                    "/opt/local/bin",
                ]
                .iter()
                .map(PathBuf::from),
            );
            if let Some(home) = &home {
                for relative in [
                    ".npm-global/bin",
                    ".local/bin",
                    ".volta/bin",
                    ".bun/bin",
                    ".yarn/bin",
                    "n/bin",
                ] {
                    dirs.push(home.join(relative));
                }
            }
        }
        #[cfg(windows)]
        {
            for key in ["APPDATA", "LOCALAPPDATA", "ProgramFiles"] {
                if let Some(base) = self.environment.get(key) {
                    dirs.push(PathBuf::from(base).join("npm"));
                }
            }
            if let Some(home) = &home {
                dirs.push(home.join(".volta").join("bin"));
                dirs.push(home.join(".bun").join("bin"));
            }
        }
        dirs
    }

    /// Directories from `PATH`, in order.
    #[must_use]
    pub fn path_directories(&self) -> Vec<PathBuf> {
        let separator = if cfg!(windows) { ';' } else { ':' };
        self.environment
            .get("PATH")
            .map(|value| {
                value
                    .split(separator)
                    .filter(|entry| !entry.is_empty())
                    .map(PathBuf::from)
                    // A relative PATH entry resolves against the current
                    // directory, which an attacker may control. We only ever
                    // launch an absolute path.
                    .filter(|entry| entry.is_absolute())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Search the fixed candidate list.
    pub fn discover(&self) -> Result<DiscoveredCli, DiscoveryError> {
        let mut shim: Option<PathBuf> = None;
        let groups = [
            (
                DiscoveryOrigin::WellKnownLocation,
                self.well_known_directories(),
            ),
            (DiscoveryOrigin::PathEnvironment, self.path_directories()),
        ];
        for (origin, directories) in groups {
            for directory in directories {
                let candidate = directory.join(EXECUTABLE_LEAF);
                if self.probe.is_executable_file(&candidate) {
                    let program = self.resolved_candidate(&directory).unwrap_or(candidate);
                    return Ok(DiscoveredCli {
                        program,
                        origin,
                        approval: None,
                    });
                }
                if shim.is_none() {
                    shim = REFUSED_SHIM_LEAVES
                        .iter()
                        .map(|leaf| directory.join(leaf))
                        .find(|path| self.probe.exists(path));
                }
            }
        }
        shim.map_or(Err(DiscoveryError::NotFound), |shim| {
            Err(DiscoveryError::ShimRequiresInterpreter { shim })
        })
    }

    /// Resolve the *directory* of a candidate, never the executable itself.
    ///
    /// npm installs the CLI as a symlink from a `bin` directory into the
    /// package (on macOS 2026-10-12: `/opt/homebrew/bin/plaud` ->
    /// `/opt/homebrew/lib/node_modules/@plaud-ai/cli/dist/index.js`).
    /// Canonicalizing the whole path would hand the launcher a program whose
    /// leaf name is no longer the pinned `plaud`, and the egress allowlist
    /// refuses that — correctly, because the pinned name is the property being
    /// enforced. Resolving only the directory still strips `..` and symlinked
    /// directories from the path we launch, which is what canonicalization was
    /// there for.
    fn resolved_candidate(&self, directory: &Path) -> Option<PathBuf> {
        let resolved = self.probe.canonicalize(directory)?.join(EXECUTABLE_LEAF);
        self.probe.is_executable_file(&resolved).then_some(resolved)
    }

    /// Accept a path the user picked, which must carry an approval record.
    ///
    /// The leaf name is *not* required to be `plaud` here: a user who installed
    /// the tool under a different name has made an explicit, recorded decision.
    /// Everything else is still enforced.
    pub fn accept_user_selected(
        &self,
        path: &Path,
        approval: Option<UserApproval>,
    ) -> Result<DiscoveredCli, DiscoveryError> {
        let Some(approval) = approval else {
            return Err(DiscoveryError::NotApproved);
        };
        if approval.approved_path != path.to_string_lossy() {
            return Err(DiscoveryError::NotApproved);
        }
        if !path.is_absolute() {
            return Err(DiscoveryError::PathNotAbsolute);
        }
        for component in path.components() {
            match component {
                Component::Normal(_) | Component::RootDir | Component::Prefix(_) => {}
                Component::CurDir | Component::ParentDir => {
                    return Err(DiscoveryError::PathNotCanonical)
                }
            }
        }
        if REFUSED_SHIM_LEAVES
            .iter()
            .any(|leaf| path.file_name().is_some_and(|name| name == *leaf))
        {
            return Err(DiscoveryError::ShimRequiresInterpreter {
                shim: path.to_path_buf(),
            });
        }
        if !self.probe.is_executable_file(path) {
            return Err(DiscoveryError::NotExecutable);
        }
        Ok(DiscoveredCli {
            program: self
                .probe
                .canonicalize(path)
                .unwrap_or_else(|| path.to_path_buf()),
            origin: DiscoveryOrigin::UserSelected,
            approval: Some(approval),
        })
    }
}

impl Default for CliDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[derive(Debug, Default)]
    struct FakeProbe {
        executables: BTreeSet<PathBuf>,
        present: BTreeSet<PathBuf>,
    }

    impl FakeProbe {
        fn with_executable(mut self, path: &str) -> Self {
            self.executables.insert(PathBuf::from(path));
            self.present.insert(PathBuf::from(path));
            self
        }

        fn with_file(mut self, path: &str) -> Self {
            self.present.insert(PathBuf::from(path));
            self
        }
    }

    impl ExecutableProbe for FakeProbe {
        fn is_executable_file(&self, path: &Path) -> bool {
            self.executables.contains(path)
        }
        fn exists(&self, path: &Path) -> bool {
            self.present.contains(path)
        }
        fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
            self.present.contains(path).then(|| path.to_path_buf())
        }
    }

    fn environment(path: &str) -> BTreeMap<String, String> {
        [
            ("HOME".to_owned(), "/Users/example".to_owned()),
            ("USERPROFILE".to_owned(), "C:\\Users\\example".to_owned()),
            (
                "APPDATA".to_owned(),
                "C:\\Users\\example\\AppData\\Roaming".to_owned(),
            ),
            ("PATH".to_owned(), path.to_owned()),
        ]
        .into_iter()
        .collect()
    }

    fn discovery(probe: FakeProbe, path: &str) -> CliDiscovery {
        CliDiscovery::with_probe(Arc::new(probe), environment(path))
    }

    #[cfg(not(windows))]
    #[test]
    fn a_well_known_location_is_found() {
        let d = discovery(
            FakeProbe::default().with_executable("/opt/homebrew/bin/plaud"),
            "",
        );
        let found = d.discover().expect("discovered");
        assert_eq!(found.program, PathBuf::from("/opt/homebrew/bin/plaud"));
        assert_eq!(found.origin, DiscoveryOrigin::WellKnownLocation);
        assert!(found.approval.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn an_npm_symlink_is_launched_under_the_pinned_leaf_name() {
        // How npm actually installs the CLI: a `bin` symlink into the package
        // directory. Following that symlink all the way would produce
        // `.../dist/index.js`, which the egress allowlist refuses because its
        // leaf name is not `plaud`.
        let root = tempfile::tempdir().expect("tempdir");
        let bin = root.path().join("bin");
        let package = root.path().join("lib/node_modules/@plaud-ai/cli/dist");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::create_dir_all(&package).expect("package");
        let real = package.join("index.js");
        std::fs::write(&real, "#!/usr/bin/env node\n").expect("write");
        std::fs::set_permissions(&real, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        std::os::unix::fs::symlink(&real, bin.join(EXECUTABLE_LEAF)).expect("symlink");

        let discovery = CliDiscovery::with_probe(
            Arc::new(OsProbe),
            [("PATH".to_owned(), bin.to_string_lossy().into_owned())]
                .into_iter()
                .collect(),
        );
        let found = discovery.discover().expect("discovered");
        assert_eq!(
            found.program.file_name().and_then(|leaf| leaf.to_str()),
            Some(EXECUTABLE_LEAF)
        );
        assert!(found.program.is_absolute());
        assert!(!found.program.to_string_lossy().contains("index.js"));
    }

    #[cfg(not(windows))]
    #[test]
    fn a_path_entry_is_found_after_well_known_locations() {
        let d = discovery(
            FakeProbe::default().with_executable("/custom/tools/plaud"),
            "/custom/tools",
        );
        let found = d.discover().expect("discovered");
        assert_eq!(found.origin, DiscoveryOrigin::PathEnvironment);
    }

    #[cfg(not(windows))]
    #[test]
    fn a_well_known_location_wins_over_path() {
        let d = discovery(
            FakeProbe::default()
                .with_executable("/custom/tools/plaud")
                .with_executable("/usr/local/bin/plaud"),
            "/custom/tools",
        );
        assert_eq!(
            d.discover().expect("discovered").program,
            PathBuf::from("/usr/local/bin/plaud")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn a_similarly_named_binary_is_never_a_candidate() {
        // The search is for a constant leaf name, so a `PATH` full of
        // lookalikes yields CliMissing rather than a surprise executable.
        let d = discovery(
            FakeProbe::default()
                .with_executable("/custom/tools/plaud-next")
                .with_executable("/custom/tools/plaudx")
                .with_executable("/custom/tools/plaud.sh")
                .with_executable("/custom/tools/PLAUD"),
            "/custom/tools",
        );
        assert_eq!(d.discover().unwrap_err(), DiscoveryError::NotFound);
        assert_eq!(
            SourceError::from(DiscoveryError::NotFound),
            SourceError::CliMissing
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn a_relative_path_entry_is_ignored() {
        let d = discovery(
            FakeProbe::default().with_executable("tools/plaud"),
            "tools:.:..",
        );
        assert_eq!(d.discover().unwrap_err(), DiscoveryError::NotFound);
    }

    #[cfg(not(windows))]
    #[test]
    fn a_non_executable_file_is_not_a_candidate() {
        let d = discovery(FakeProbe::default().with_file("/usr/local/bin/plaud"), "");
        assert_eq!(d.discover().unwrap_err(), DiscoveryError::NotFound);
    }

    #[cfg(not(windows))]
    #[test]
    fn a_script_shim_is_reported_rather_than_launched() {
        let d = discovery(
            FakeProbe::default().with_file("/usr/local/bin/plaud.cmd"),
            "",
        );
        let error = d.discover().unwrap_err();
        assert_eq!(
            error,
            DiscoveryError::ShimRequiresInterpreter {
                shim: PathBuf::from("/usr/local/bin/plaud.cmd")
            }
        );
        assert_eq!(SourceError::from(error), SourceError::CliUnsupported);
    }

    #[test]
    fn nothing_installed_yields_cli_missing() {
        let d = discovery(FakeProbe::default(), "/empty");
        assert_eq!(d.discover().unwrap_err(), DiscoveryError::NotFound);
    }

    #[cfg(not(windows))]
    #[test]
    fn a_user_path_without_approval_is_refused() {
        let d = discovery(
            FakeProbe::default().with_executable("/opt/custom/plaud"),
            "",
        );
        assert_eq!(
            d.accept_user_selected(Path::new("/opt/custom/plaud"), None)
                .unwrap_err(),
            DiscoveryError::NotApproved
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn an_approval_for_a_different_path_does_not_transfer() {
        let d = discovery(
            FakeProbe::default().with_executable("/opt/custom/plaud"),
            "",
        );
        let approval = UserApproval {
            approved_at_ms: 1,
            approved_path: "/opt/other/plaud".to_owned(),
        };
        assert_eq!(
            d.accept_user_selected(Path::new("/opt/custom/plaud"), Some(approval))
                .unwrap_err(),
            DiscoveryError::NotApproved
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn an_approved_user_path_is_accepted_and_recorded() {
        let d = discovery(
            FakeProbe::default().with_executable("/opt/custom/plaud"),
            "",
        );
        let approval = UserApproval {
            approved_at_ms: 1_700_000_000_000,
            approved_path: "/opt/custom/plaud".to_owned(),
        };
        let found = d
            .accept_user_selected(Path::new("/opt/custom/plaud"), Some(approval.clone()))
            .expect("accepted");
        assert_eq!(found.origin, DiscoveryOrigin::UserSelected);
        assert_eq!(found.approval, Some(approval));
    }

    #[cfg(not(windows))]
    #[test]
    fn an_approved_traversal_path_is_still_refused() {
        let d = discovery(
            FakeProbe::default().with_executable("/opt/custom/../custom/plaud"),
            "",
        );
        let approval = UserApproval {
            approved_at_ms: 1,
            approved_path: "/opt/custom/../custom/plaud".to_owned(),
        };
        assert_eq!(
            d.accept_user_selected(Path::new("/opt/custom/../custom/plaud"), Some(approval))
                .unwrap_err(),
            DiscoveryError::PathNotCanonical
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn an_approved_relative_path_is_refused() {
        let d = discovery(FakeProbe::default().with_executable("tools/plaud"), "");
        let approval = UserApproval {
            approved_at_ms: 1,
            approved_path: "tools/plaud".to_owned(),
        };
        assert_eq!(
            d.accept_user_selected(Path::new("tools/plaud"), Some(approval))
                .unwrap_err(),
            DiscoveryError::PathNotAbsolute
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn an_approved_shim_is_refused() {
        let d = discovery(FakeProbe::default().with_file("/opt/custom/plaud.ps1"), "");
        let approval = UserApproval {
            approved_at_ms: 1,
            approved_path: "/opt/custom/plaud.ps1".to_owned(),
        };
        assert!(matches!(
            d.accept_user_selected(Path::new("/opt/custom/plaud.ps1"), Some(approval)),
            Err(DiscoveryError::ShimRequiresInterpreter { .. })
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn an_approved_directory_is_refused() {
        let d = discovery(FakeProbe::default(), "");
        let approval = UserApproval {
            approved_at_ms: 1,
            approved_path: "/opt/custom".to_owned(),
        };
        assert_eq!(
            d.accept_user_selected(Path::new("/opt/custom"), Some(approval))
                .unwrap_err(),
            DiscoveryError::NotExecutable
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn unicode_and_spaced_directories_are_handled_literally() {
        let path = "/Users/example/Dév Tools/plaud";
        let d = discovery(
            FakeProbe::default().with_executable(path),
            "/Users/example/Dév Tools",
        );
        assert_eq!(
            d.discover().expect("discovered").program,
            PathBuf::from(path)
        );
    }

    #[test]
    fn path_parsing_never_yields_an_empty_or_relative_directory() {
        let d = discovery(FakeProbe::default(), "::/a::/b:");
        let dirs = d.path_directories();
        assert!(dirs.iter().all(|d| d.is_absolute()));
        assert_ne!(dirs, Vec::<PathBuf>::new());
    }
}
