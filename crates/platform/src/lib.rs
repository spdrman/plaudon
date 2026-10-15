//! Native desktop integration for the Plaudon shell.
//!
//! EPIC O moves the desktop frontend onto egui, which means the host
//! integration Tauri used to provide — file dialogs, the clipboard, "reveal in
//! Finder", the OS colour scheme, file associations, deep links, drag-and-drop
//! and the tray — has to be written in Rust. This crate owns all of it
//! (migration plan §4.8 rows C10 and C11, §4.5 rows J14 and J15).
//!
//! Three rules shape every module:
//!
//! 1. **Every host capability is a trait with a real adapter and a fake.** The
//!    view layer depends on the trait, so a test never opens a window, never
//!    touches the pasteboard and never spawns a process.
//! 2. **A capability that does not exist says so.** There is no silent no-op
//!    anywhere in this crate: a clipboard that cannot be opened returns
//!    [`clipboard::ClipboardError`], a platform with no tray backend reports
//!    [`tray::TraySupport::UnavailableInBuild`] with a reason, and a refused
//!    URL scheme is an [`opener::OpenError`] the caller can show.
//! 3. **User data is input, never a command line and never a log field.**
//!    Processes are spawned with argument vectors through
//!    [`std::process::Command`]; no module in this crate ever runs `sh -c`.
//!    Rejections are logged as stable reason codes, never as the path, URL or
//!    text that caused them.
//!
//! The crate deliberately has no `egui`, `eframe` or `plaudon-ui` dependency:
//! it is linked by the shell binary, which owns the window, and by the
//! application layer's tests.
//!
//! # Modules
//!
//! | Module | Port | Real adapter |
//! | --- | --- | --- |
//! | [`dialogs`] | [`dialogs::FileDialogs`] | [`dialogs::NativeFileDialogs`] (`rfd`) |
//! | [`clipboard`] | [`clipboard::Clipboard`] | [`clipboard::NativeClipboard`] (`arboard`) |
//! | [`theme`] | [`theme::OsThemeSource`] | [`theme::NativeOsTheme`] |
//! | [`opener`] | [`opener::Opener`] | [`opener::NativeOpener`] |
//! | [`notifications`] | [`notifications::NotificationPort`] | [`notifications::DispatcherNotifications`] |
//! | [`os_permissions`] | `plaudon_app::os_permissions::OsPermissionProvider` | [`os_permissions::native`] (Core Bluetooth, `UserNotifications`, System Settings deep links) |
//! | [`tray`] | [`tray::Tray`] | [`tray::NativeTray`] (reports honestly; see the module docs) |
//! | [`associations`] | — (pure generators plus committed artifacts) | — |
//! | [`dragdrop`] | — (pure classification) | — |
//! | [`activation`] | — (pure parsing) | — |

#![deny(missing_docs)]

use std::sync::{Mutex, MutexGuard, PoisonError};

pub mod activation;
pub mod associations;
pub mod clipboard;
pub mod dialogs;
pub mod dragdrop;
pub mod notifications;
pub mod opener;
pub mod os_permissions;
pub mod theme;
pub mod tray;

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-platform";

/// The desktop platforms Plaudon targets.
///
/// Several modules need to *name* a platform rather than be compiled for it:
/// the opener's argument vectors and the theme parsers are pure functions of
/// the platform, so they are written once, compiled on every host and tested
/// for all three platforms everywhere. Only the choice of which variant the
/// current host is uses `cfg!`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// macOS.
    MacOs,
    /// Windows.
    Windows,
    /// Linux and other freedesktop.org systems.
    Linux,
}

impl Platform {
    /// The platform this binary was compiled for, or `None` on a host Plaudon
    /// does not target.
    #[must_use]
    pub const fn host() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::MacOs)
        } else if cfg!(target_os = "windows") {
            Some(Self::Windows)
        } else if cfg!(target_os = "linux") {
            Some(Self::Linux)
        } else {
            None
        }
    }

    /// Stable lowercase name, safe to put in a log line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MacOs => "macos",
            Self::Windows => "windows",
            Self::Linux => "linux",
        }
    }
}

/// Lock a mutex without ever panicking on a poisoned lock.
///
/// Nothing in this crate holds a lock across a panic-prone operation, and a
/// poisoned clipboard handle or call log is still perfectly usable; turning a
/// poisoned lock into a panic would only convert someone else's failure into
/// a crash of the UI thread.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_matches_manifest() {
        assert_eq!(PACKAGE_NAME, env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn the_host_platform_is_one_of_the_targets_we_support() {
        let host = Platform::host();
        if cfg!(any(
            target_os = "macos",
            target_os = "windows",
            target_os = "linux"
        )) {
            assert!(host.is_some());
        } else {
            assert!(host.is_none());
        }
    }

    #[test]
    fn platform_names_are_stable() {
        assert_eq!(Platform::MacOs.as_str(), "macos");
        assert_eq!(Platform::Windows.as_str(), "windows");
        assert_eq!(Platform::Linux.as_str(), "linux");
    }

    #[test]
    fn a_poisoned_lock_is_still_usable() {
        let mutex = std::sync::Arc::new(Mutex::new(7_u32));
        let poisoner = std::sync::Arc::clone(&mutex);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().expect("lock");
            panic!("poison the lock");
        })
        .join();
        assert!(mutex.is_poisoned());
        assert_eq!(*lock(&mutex), 7);
    }
}
