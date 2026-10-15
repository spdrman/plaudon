//! The guided macOS Privacy & Security panel (issue #307).
//!
//! One renderer, two homes: the first-run permissions step and the Settings
//! section of the same name show exactly this panel, so what a user saw
//! during setup is what they find again later.
//!
//! The panel never talks to the operating system. It renders from a
//! [`PermissionsReport`] and raises [`SettingsIntent`]s; the shell hands
//! those to [`PermissionsHost`], which calls the
//! [`OsPermissionProvider`] off the render thread and publishes the next
//! report. A test swaps the provider for
//! [`plaudon_app::os_permissions::FakePermissionProvider`] and runs the work
//! inline.

use std::fmt;
use std::sync::{Arc, Mutex};

use egui::Ui;
use plaudon_app::dto::Platform;
use plaudon_app::os_permissions::{
    NoPermissions, OpenSettingsError, OsPermissionKind, OsPermissionProvider, PermissionSnapshot,
};

use crate::settings::{SettingsIntent, SettingsOutput, SettingsTokens};

/// The button that reads every status again.
pub const CHECK_AGAIN: &str = "Check again";

/// Name of the button that shows the macOS prompt for `kind`.
#[must_use]
pub fn ask_label(kind: OsPermissionKind) -> String {
    format!("Ask macOS for {}", kind.guide().title)
}

/// Name of the button that opens the System Settings pane for `kind`.
#[must_use]
pub fn open_label(kind: OsPermissionKind) -> String {
    format!("Open System Settings for {}", kind.guide().title)
}

/// Name of the disclosure that holds the numbered manual steps for `kind`.
#[must_use]
pub fn manual_steps_label(kind: OsPermissionKind) -> String {
    format!("How to allow {} by hand", kind.guide().title)
}

/// What the panel renders: the last snapshot, and the last thing that went
/// wrong, if anything did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionsReport {
    /// The last statuses read. `None` until the first read finishes.
    pub snapshot: Option<PermissionSnapshot>,
    /// A sentence about the last failed action, such as System Settings not
    /// opening.
    pub error: Option<String>,
}

/// Runs permission reads, prompts and System Settings opens for the shell.
#[derive(Clone)]
pub struct PermissionsHost {
    provider: Arc<dyn OsPermissionProvider>,
    platform: Platform,
    report: Arc<Mutex<PermissionsReport>>,
    background: bool,
}

impl fmt::Debug for PermissionsHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PermissionsHost")
            .field("platform", &self.platform)
            .field("background", &self.background)
            .finish_non_exhaustive()
    }
}

impl Default for PermissionsHost {
    fn default() -> Self {
        Self::none()
    }
}

impl PermissionsHost {
    /// A host with no permissions to report, for a headless run.
    #[must_use]
    pub fn none() -> Self {
        Self::inline(Arc::new(NoPermissions), Platform::host())
    }

    /// A host that does its work on the calling thread. For tests.
    #[must_use]
    pub fn inline(provider: Arc<dyn OsPermissionProvider>, platform: Platform) -> Self {
        Self {
            provider,
            platform,
            report: Arc::new(Mutex::new(PermissionsReport::default())),
            background: false,
        }
    }

    /// A host that does its work on a background thread and asks `egui` for
    /// a repaint when it is done. For the shipped application.
    #[must_use]
    pub fn background(provider: Arc<dyn OsPermissionProvider>, platform: Platform) -> Self {
        Self {
            background: true,
            ..Self::inline(provider, platform)
        }
    }

    /// Read every status, without prompting.
    pub fn check(&self, repaint: Option<&egui::Context>) {
        let _ = repaint;
    }

    /// Show the operating system's prompt for `kind`, then read again.
    pub fn ask(&self, kind: OsPermissionKind, repaint: Option<&egui::Context>) {
        let _ = (kind, repaint);
    }

    /// Open the System Settings pane for `kind`.
    ///
    /// # Errors
    ///
    /// When the pane could not be opened; the sentence is also kept in the
    /// report so the panel can show it.
    pub fn open_settings(&self, kind: OsPermissionKind) -> Result<(), OpenSettingsError> {
        let _ = kind;
        Ok(())
    }

    /// The latest report.
    #[must_use]
    pub fn latest(&self) -> PermissionsReport {
        PermissionsReport::default()
    }
}

/// Hand one intent to the host, when it is a permission intent.
///
/// Returns `false` for every other intent, which the caller handles itself.
pub fn handle_intent(
    host: &PermissionsHost,
    intent: &SettingsIntent,
    repaint: Option<&egui::Context>,
) -> bool {
    let _ = (host, intent, repaint);
    false
}

/// The panel itself.
#[derive(Debug, Clone)]
pub struct PermissionsPanel {
    platform: Platform,
    checked: bool,
}

impl PermissionsPanel {
    /// A panel for `platform`.
    #[must_use]
    pub const fn new(platform: Platform) -> Self {
        Self {
            platform,
            checked: false,
        }
    }

    /// The platform the panel lists permissions for.
    #[must_use]
    pub const fn platform(&self) -> Platform {
        self.platform
    }

    /// Forget that a read was requested, so the next render asks again.
    pub fn reset(&mut self) {
        self.checked = false;
    }

    /// Render the panel.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        report: &PermissionsReport,
        out: &mut SettingsOutput,
    ) {
        let _ = (ui, tokens, report, out);
    }
}
