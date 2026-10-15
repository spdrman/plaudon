//! The real operating-system privacy permissions (#307).
//!
//! [`native`] is the [`OsPermissionProvider`] the shipped binary hands to the
//! permissions panel. On macOS it reads and asks through the system APIs:
//!
//! * **Bluetooth**: `+[CBManager authorization]` reads the grant without a
//!   prompt. Core Bluetooth has no "request" call; macOS shows its prompt the
//!   first time the app creates a `CBCentralManager`, so asking means making
//!   one and waiting until the answer is in.
//! * **Notifications**: the `UNUserNotificationCenter` calls that
//!   `plaudon-notifications` already makes for delivery, so the panel and the
//!   delivery gate never disagree about the grant.
//! * **Removable Volumes** and **Files and Folders**: macOS has no public API
//!   that reports either without trying a read, so the honest status is
//!   [`OsPermissionStatus::NotReported`] and there is nothing to ask.
//!
//! Every pane opens through `open` with a fixed, compiled-in
//! `x-apple.systempreferences:` URI ([`OsPermissionKind::settings_url`]), with
//! an argument vector and never a shell.
//!
//! Everywhere else [`native`] is [`NoPermissions`]: Windows and Linux gate none
//! of this behind a per-app privacy grant.
//!
//! None of the macOS half runs in the Linux test container, and a real TCC
//! prompt needs a person to click it. The panel's behaviour is pinned through
//! the fake provider in `plaudon-app`; this adapter is glue whose answers have
//! to be checked by hand on a Mac.

use std::sync::Arc;

use plaudon_app::os_permissions::OsPermissionProvider;

/// The provider for the platform this binary was built for.
///
/// `runtime` drives the notification calls, which `plaudon-notifications`
/// exposes as async. The panel calls the provider from its own background
/// thread, never from a runtime worker, so blocking on the handle there is
/// safe.
#[must_use]
pub fn native(runtime: &tokio::runtime::Handle) -> Arc<dyn OsPermissionProvider> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(macos::MacPermissions::new(runtime.clone()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = runtime;
        Arc::new(plaudon_app::os_permissions::NoPermissions)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::process::Command;
    use std::time::{Duration, Instant};

    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use plaudon_app::os_permissions::{
        OpenSettingsError, OsPermissionKind, OsPermissionProvider, OsPermissionStatus,
    };
    use plaudon_notifications::macos::MacNotifier;
    use plaudon_notifications::{Notifier as _, OsPermission};

    // Core Bluetooth is not linked by anything else in a build without the
    // `hardware-scan` radio, so the framework is named here.
    #[link(name = "CoreBluetooth", kind = "framework")]
    extern "C" {}

    /// How long "Ask macOS" waits for the user to answer the Bluetooth prompt
    /// before reporting whatever macOS says at that point.
    const PROMPT_WAIT: Duration = Duration::from_secs(120);

    /// How often the Bluetooth grant is read while the prompt is up.
    const PROMPT_POLL: Duration = Duration::from_millis(250);

    /// The macOS provider.
    pub(super) struct MacPermissions {
        runtime: tokio::runtime::Handle,
        notifier: MacNotifier,
    }

    impl MacPermissions {
        pub(super) fn new(runtime: tokio::runtime::Handle) -> Self {
            Self {
                runtime,
                notifier: MacNotifier::new(),
            }
        }
    }

    /// `CBManagerAuthorization`, as `+[CBManager authorization]` returns it
    /// (macOS 10.15 and later).
    fn bluetooth_status() -> OsPermissionStatus {
        let Some(class) = AnyClass::get(c"CBManager") else {
            return OsPermissionStatus::NotReported;
        };
        // Safety: `authorization` is a class property of `CBManager` that
        // returns an `NSInteger` and has no side effect, prompt included.
        let raw: isize = unsafe { msg_send![class, authorization] };
        map_bluetooth(raw)
    }

    const fn map_bluetooth(raw: isize) -> OsPermissionStatus {
        match raw {
            0 => OsPermissionStatus::NotDetermined,
            1 => OsPermissionStatus::Restricted,
            2 => OsPermissionStatus::Denied,
            3 => OsPermissionStatus::Granted,
            _ => OsPermissionStatus::NotReported,
        }
    }

    /// Make macOS show its Bluetooth prompt, and wait for the answer.
    fn ask_bluetooth() -> OsPermissionStatus {
        if bluetooth_status() != OsPermissionStatus::NotDetermined {
            return bluetooth_status();
        }
        let Some(class) = AnyClass::get(c"CBCentralManager") else {
            return OsPermissionStatus::NotReported;
        };
        // Safety: `+new` on `CBCentralManager` returns a retained manager
        // with no delegate, on the main queue. Creating it is what shows the
        // prompt; it is never asked to scan and is dropped once answered.
        let manager: Option<Retained<AnyObject>> = unsafe { msg_send![class, new] };
        let started = Instant::now();
        let mut status = bluetooth_status();
        while status == OsPermissionStatus::NotDetermined && started.elapsed() < PROMPT_WAIT {
            std::thread::sleep(PROMPT_POLL);
            status = bluetooth_status();
        }
        drop(manager);
        status
    }

    const fn map_notifications(permission: OsPermission) -> OsPermissionStatus {
        match permission {
            OsPermission::Granted => OsPermissionStatus::Granted,
            OsPermission::Denied => OsPermissionStatus::Denied,
            OsPermission::NotRequested => OsPermissionStatus::NotDetermined,
            // An unbundled process (`cargo run`) may not use the notification
            // centre at all, so macOS has said nothing either way.
            OsPermission::Unsupported => OsPermissionStatus::NotReported,
        }
    }

    impl OsPermissionProvider for MacPermissions {
        fn status(&self, kind: OsPermissionKind) -> OsPermissionStatus {
            match kind {
                OsPermissionKind::Bluetooth => bluetooth_status(),
                OsPermissionKind::Notifications => {
                    map_notifications(self.runtime.block_on(self.notifier.permission()))
                }
                OsPermissionKind::RemovableVolumes | OsPermissionKind::FilesAndFolders => {
                    OsPermissionStatus::NotReported
                }
            }
        }

        fn request(&self, kind: OsPermissionKind) -> OsPermissionStatus {
            match kind {
                OsPermissionKind::Bluetooth => ask_bluetooth(),
                OsPermissionKind::Notifications => {
                    if self.status(kind) != OsPermissionStatus::NotDetermined {
                        return self.status(kind);
                    }
                    map_notifications(self.runtime.block_on(self.notifier.request_permission()))
                }
                OsPermissionKind::RemovableVolumes | OsPermissionKind::FilesAndFolders => {
                    self.status(kind)
                }
            }
        }

        fn open_settings(&self, kind: OsPermissionKind) -> Result<(), OpenSettingsError> {
            let opened = Command::new("/usr/bin/open")
                .arg(kind.settings_url())
                .status();
            match opened {
                Ok(status) if status.success() => Ok(()),
                Ok(_) | Err(_) => {
                    tracing::debug!(kind = kind.as_str(), "System Settings did not open");
                    Err(OpenSettingsError(format!(
                        "System Settings could not be opened. Open it from the Apple menu and go \
to Privacy & Security, then {}.",
                        kind.guide().title
                    )))
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn issue_307_core_bluetooth_authorization_values_map_onto_the_status_vocabulary() {
            assert_eq!(map_bluetooth(0), OsPermissionStatus::NotDetermined);
            assert_eq!(map_bluetooth(1), OsPermissionStatus::Restricted);
            assert_eq!(map_bluetooth(2), OsPermissionStatus::Denied);
            assert_eq!(map_bluetooth(3), OsPermissionStatus::Granted);
            assert_eq!(map_bluetooth(42), OsPermissionStatus::NotReported);
        }

        #[test]
        fn issue_307_notification_permissions_map_onto_the_status_vocabulary() {
            assert_eq!(
                map_notifications(OsPermission::NotRequested),
                OsPermissionStatus::NotDetermined
            );
            assert_eq!(
                map_notifications(OsPermission::Unsupported),
                OsPermissionStatus::NotReported
            );
        }
    }
}
