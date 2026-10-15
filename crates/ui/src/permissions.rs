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

use egui::{RichText, Ui};
use plaudon_app::dto::Platform;
use plaudon_app::os_permissions::{
    NoPermissions, OpenSettingsError, OsPermissionKind, OsPermissionProvider, OsPermissionStatus,
    PermissionSnapshot, NOTHING_TO_SET_UP, SIGNING_NOTE,
};

use crate::settings::{section, SettingsIntent, SettingsOutput, SettingsTokens, Tone};

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
        self.run(repaint, PermissionSnapshot::read);
    }

    /// Show the operating system's prompt for `kind`, then read again.
    pub fn ask(&self, kind: OsPermissionKind, repaint: Option<&egui::Context>) {
        if OsPermissionKind::for_platform(self.platform).contains(&kind) {
            self.run(repaint, move |provider, platform| {
                provider.request(kind);
                PermissionSnapshot::read(provider, platform)
            });
        }
    }

    /// Open the System Settings pane for `kind`.
    ///
    /// Opening is quick (it hands a URL to Launch Services), so it happens on
    /// the calling thread and the answer is known straight away.
    ///
    /// # Errors
    ///
    /// When the pane could not be opened; the sentence is also kept in the
    /// report so the panel can show it.
    pub fn open_settings(&self, kind: OsPermissionKind) -> Result<(), OpenSettingsError> {
        let result = if OsPermissionKind::for_platform(self.platform).contains(&kind) {
            self.provider.open_settings(kind)
        } else {
            Err(OpenSettingsError(NOTHING_TO_SET_UP.to_owned()))
        };
        self.lock().error = result.as_ref().err().map(|error| error.0.clone());
        result
    }

    /// The latest report.
    #[must_use]
    pub fn latest(&self) -> PermissionsReport {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PermissionsReport> {
        self.report
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Do `work` against the provider and publish what it read, inline or on
    /// a background thread. A read that succeeds clears the last error.
    fn run<F>(&self, repaint: Option<&egui::Context>, work: F)
    where
        F: FnOnce(&dyn OsPermissionProvider, Platform) -> PermissionSnapshot + Send + 'static,
    {
        let provider = Arc::clone(&self.provider);
        let platform = self.platform;
        let report = Arc::clone(&self.report);
        let repaint = repaint.cloned();
        let job = move || {
            let snapshot = work(provider.as_ref(), platform);
            {
                let mut report = report
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                report.snapshot = Some(snapshot);
                report.error = None;
            }
            if let Some(ctx) = repaint {
                ctx.request_repaint();
            }
        };
        if self.background {
            let spawned = std::thread::Builder::new()
                .name("plaudon-os-permissions".to_owned())
                .spawn(job);
            if let Err(error) = spawned {
                tracing::warn!(%error, "the permission check could not start");
            }
        } else {
            job();
        }
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
    match intent {
        SettingsIntent::CheckOsPermissions => host.check(repaint),
        SettingsIntent::AskOsPermission(kind) => host.ask(*kind, repaint),
        SettingsIntent::OpenOsPermissionSettings(kind) => {
            if let Err(error) = host.open_settings(*kind) {
                tracing::debug!(kind = kind.as_str(), detail = %error.0, "System Settings did not open");
            }
        }
        _ => return false,
    }
    true
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
    ///
    /// The first frame asks the host for a read, and so does every return to
    /// the application window: the usual way back from System Settings.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        report: &PermissionsReport,
        out: &mut SettingsOutput,
    ) {
        let kinds = OsPermissionKind::for_platform(self.platform);
        if kinds.is_empty() {
            section::note(ui, tokens, NOTHING_TO_SET_UP);
            return;
        }

        let refocused = ui.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::WindowFocused(true)))
        });
        if !self.checked || refocused {
            self.checked = true;
            out.push(SettingsIntent::CheckOsPermissions);
        }

        section::prose(ui, tokens, INTRO);
        ui.add_space(4.0);
        section::status_badge(ui, tokens, Tone::Warning, "⚠", SIGNING_NOTE);
        if let Some(error) = &report.error {
            ui.add_space(4.0);
            section::status_badge(ui, tokens, Tone::Danger, "✖", error);
        }
        ui.add_space(4.0);
        if ui.button(CHECK_AGAIN).clicked() {
            out.push(SettingsIntent::CheckOsPermissions);
        }

        for kind in kinds {
            ui.add_space(12.0);
            let status = report
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.status(*kind));
            Self::permission(ui, tokens, *kind, status, out);
        }
    }

    fn permission(
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        kind: OsPermissionKind,
        status: Option<OsPermissionStatus>,
        out: &mut SettingsOutput,
    ) {
        let guide = kind.guide();
        ui.label(RichText::new(guide.title).strong().color(tokens.text()));
        match status {
            Some(status) => {
                let (glyph, word) = status.label();
                section::status_badge(ui, tokens, tone(status), glyph, word);
            }
            None => section::note(ui, tokens, CHECKING),
        }
        section::prose(ui, tokens, &format!("Needed for: {}", guide.needed_for));
        section::prose(ui, tokens, &format!("Without it: {}", guide.without_it));
        if guide.optional {
            section::note(ui, tokens, OPTIONAL);
        }
        section::note(ui, tokens, guide.ask_note);
        ui.horizontal_wrapped(|ui| {
            // macOS shows its prompt once: after any answer, a button that
            // promises one would do nothing.
            if guide.can_ask
                && status == Some(OsPermissionStatus::NotDetermined)
                && ui.button(ask_label(kind)).clicked()
            {
                out.push(SettingsIntent::AskOsPermission(kind));
            }
            if ui.button(open_label(kind)).clicked() {
                out.push(SettingsIntent::OpenOsPermissionSettings(kind));
            }
        });
        egui::CollapsingHeader::new(manual_steps_label(kind))
            .id_salt(("os-permission-steps", kind.as_str()))
            .show(ui, |ui| {
                for (index, step) in guide.steps.iter().enumerate() {
                    section::prose(ui, tokens, &format!("{}. {step}", index + 1));
                }
            });
    }
}

/// What the panel says first.
const INTRO: &str = "Plaudon works without any of these. Each one switches on one feature, and \
you can come back here from Settings whenever you like.";

/// Shown under a permission whose status has not been read yet.
const CHECKING: &str = "Checking with macOS…";

/// Shown under every permission, because every one of them is.
const OPTIONAL: &str = "Optional.";

/// The tone a status is drawn in. The glyph and the word carry the meaning;
/// the colour only repeats it.
const fn tone(status: OsPermissionStatus) -> Tone {
    match status {
        OsPermissionStatus::Granted => Tone::Success,
        OsPermissionStatus::Denied => Tone::Danger,
        OsPermissionStatus::Restricted => Tone::Warning,
        OsPermissionStatus::NotDetermined
        | OsPermissionStatus::NotApplicable
        | OsPermissionStatus::NotReported => Tone::Neutral,
    }
}
