//! The operating-system privacy permissions Plaudon can need (issue #307).
//!
//! This module is the plain-data half of the guided macOS Privacy & Security
//! setup: which permissions exist, what each one is for, what happens without
//! it, and how to grant it by hand. It performs no IO. Reading a live status,
//! asking macOS and opening System Settings go through
//! [`OsPermissionProvider`], whose real adapter lives in `plaudon-platform`
//! and whose fake ([`FakePermissionProvider`]) drives every test.
//!
//! The list is deliberately short, and it is derived from what the product
//! does rather than from what macOS offers:
//!
//! | Permission | Feature that needs it | Can Plaudon ask? |
//! | --- | --- | --- |
//! | Bluetooth | direct import from a PLAUD device (`plaud-ble`) | yes, `CBCentralManager` |
//! | Notifications | "import finished" style notices (`notifications`) | yes, `UNUserNotificationCenter` |
//! | Removable volumes | USB import from a PLAUD Note that mounts as a disk (`plaud-usb`) | no, macOS asks on first read |
//! | Files and folders | a watched folder inside Desktop, Documents, Downloads or a network share (`source-folder`) | no, macOS asks on first read |
//!
//! Two permissions are left out on purpose:
//!
//! * **Speech recognition.** Apple transcription runs through
//!   `SpeechAnalyzer`, which needs no grant for file analysis
//!   (`docs/spikes/A1.3-apple-availability.md`, defect 3). Nothing in the
//!   product calls `SFSpeechRecognizer`, so there is nothing to grant.
//! * **Full Disk Access.** No feature reads a location that only Full Disk
//!   Access unlocks (Mail, Messages, Safari data and the like). Every folder
//!   Plaudon reads is either one the user picked or covered by "Files and
//!   folders" above, so asking for it would be a request no feature needs.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::dto::Platform;

/// What the panel says about the ad-hoc signature, always, on macOS.
///
/// macOS ties a privacy grant to the code signature of the app that received
/// it. The shipped build is signed ad hoc (no Developer ID), so every build
/// carries a different signature and an update can look like a new app.
pub const SIGNING_NOTE: &str = "This build of Plaudon is signed ad hoc, not with an Apple \
Developer ID. macOS ties each permission to the exact build that received it, so after you \
install an update a permission can show as not asked yet, or stop working while System Settings \
still shows it switched on. If that happens, switch Plaudon off and on again in that list (or \
remove it with the minus button and add it back), then press Check again.";

/// What the panel says where there is nothing to set up.
pub const NOTHING_TO_SET_UP: &str = "This operating system does not ask for per-app privacy \
permissions for anything Plaudon does, so there is nothing to set up here.";

/// One operating-system permission a Plaudon feature can need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OsPermissionKind {
    /// The Bluetooth radio, for direct device import.
    Bluetooth,
    /// Notification banners.
    Notifications,
    /// Reading a USB device that mounts as a disk.
    RemovableVolumes,
    /// Reading a watched folder in a location macOS protects.
    FilesAndFolders,
}

impl OsPermissionKind {
    /// Every permission, in the order the panel lists them.
    pub const ALL: &'static [Self] = &[
        Self::Bluetooth,
        Self::Notifications,
        Self::RemovableVolumes,
        Self::FilesAndFolders,
    ];

    /// The permissions that exist on `platform`.
    ///
    /// Only macOS gates these behind a per-app privacy grant; on Windows and
    /// Linux the list is empty, which is what makes the first-run step skip.
    #[must_use]
    pub const fn for_platform(platform: Platform) -> &'static [Self] {
        let _ = platform;
        &[]
    }

    /// Stable identifier, for logs and test names.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bluetooth => "bluetooth",
            Self::Notifications => "notifications",
            Self::RemovableVolumes => "removable_volumes",
            Self::FilesAndFolders => "files_and_folders",
        }
    }

    /// Everything the panel says about this permission.
    #[must_use]
    pub const fn guide(self) -> PermissionGuide {
        PermissionGuide {
            title: "",
            needed_for: "",
            without_it: "",
            optional: false,
            can_ask: false,
            ask_note: "",
            steps: &[],
        }
    }
}

/// What macOS says about one permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OsPermissionStatus {
    /// The user allowed it.
    Granted,
    /// The user refused it. macOS will not ask again; only System Settings
    /// can change it.
    Denied,
    /// macOS has never asked.
    NotDetermined,
    /// A device-management profile or parental control decides; the user
    /// cannot change it here.
    Restricted,
    /// This permission does not exist on this platform.
    NotApplicable,
    /// macOS has no API that reports this permission without prompting, so
    /// the honest answer is that it is not known until Plaudon tries.
    NotReported,
}

impl OsPermissionStatus {
    /// Glyph and word, so a status is never colour alone and "never asked" is
    /// never shown as a refusal.
    #[must_use]
    pub const fn label(self) -> (&'static str, &'static str) {
        let _ = self;
        ("", "")
    }
}

/// The words the panel shows for one permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionGuide {
    /// Name, as macOS shows it in System Settings.
    pub title: &'static str,
    /// The feature that needs it, in plain language.
    pub needed_for: &'static str,
    /// What happens without it.
    pub without_it: &'static str,
    /// Whether Plaudon works without it (every permission here is optional).
    pub optional: bool,
    /// Whether macOS offers an API to ask for it ahead of time.
    pub can_ask: bool,
    /// What to expect about asking: how the prompt appears, or why there is
    /// none to show.
    pub ask_note: &'static str,
    /// Numbered, plain-language steps for granting it by hand.
    pub steps: &'static [&'static str],
}

/// The live status of every permission on this platform, at one moment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionSnapshot {
    statuses: BTreeMap<OsPermissionKind, OsPermissionStatus>,
}

impl PermissionSnapshot {
    /// A snapshot from explicit statuses.
    #[must_use]
    pub fn from_statuses(
        statuses: impl IntoIterator<Item = (OsPermissionKind, OsPermissionStatus)>,
    ) -> Self {
        Self {
            statuses: statuses.into_iter().collect(),
        }
    }

    /// Read every permission that exists on `platform`, without prompting.
    #[must_use]
    pub fn read(provider: &dyn OsPermissionProvider, platform: Platform) -> Self {
        let _ = (provider, platform);
        Self::default()
    }

    /// The status of `kind`; `None` when it was not read.
    #[must_use]
    pub fn status(&self, kind: OsPermissionKind) -> Option<OsPermissionStatus> {
        self.statuses.get(&kind).copied()
    }

    /// Every status read, in panel order.
    pub fn iter(&self) -> impl Iterator<Item = (OsPermissionKind, OsPermissionStatus)> + '_ {
        self.statuses.iter().map(|(kind, status)| (*kind, *status))
    }
}

/// Why System Settings could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSettingsError(pub String);

/// The port the real macOS adapter and the test fake implement.
///
/// Implementations may block (a notification-settings read is an XPC round
/// trip, and an "Ask macOS" call waits for the user to answer the prompt), so
/// the shell always calls them off the render thread.
pub trait OsPermissionProvider: Send + Sync {
    /// The current status, **without** showing any prompt.
    fn status(&self, kind: OsPermissionKind) -> OsPermissionStatus;

    /// Ask macOS to show its prompt, and return the status afterwards.
    ///
    /// Only meaningful where [`PermissionGuide::can_ask`] is true; for any
    /// other permission an implementation returns [`Self::status`] unchanged
    /// and shows nothing.
    fn request(&self, kind: OsPermissionKind) -> OsPermissionStatus;

    /// Open the System Settings pane where `kind` is granted.
    ///
    /// # Errors
    ///
    /// When the pane could not be opened, with a sentence safe to show.
    fn open_settings(&self, kind: OsPermissionKind) -> Result<(), OpenSettingsError>;
}

/// A provider that reports every permission as not applicable.
///
/// The honest default for a headless run and for hosts with no privacy
/// permissions: it never prompts and never opens anything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPermissions;

impl OsPermissionProvider for NoPermissions {
    fn status(&self, _kind: OsPermissionKind) -> OsPermissionStatus {
        OsPermissionStatus::NotApplicable
    }

    fn request(&self, _kind: OsPermissionKind) -> OsPermissionStatus {
        OsPermissionStatus::NotApplicable
    }

    fn open_settings(&self, _kind: OsPermissionKind) -> Result<(), OpenSettingsError> {
        Err(OpenSettingsError(
            "This platform has no privacy settings for Plaudon.".to_owned(),
        ))
    }
}

/// One call the fake recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeCall {
    /// [`OsPermissionProvider::status`].
    Status(OsPermissionKind),
    /// [`OsPermissionProvider::request`].
    Request(OsPermissionKind),
    /// [`OsPermissionProvider::open_settings`].
    OpenSettings(OsPermissionKind),
}

/// A scripted provider for tests: statuses are set by the test, an "Ask
/// macOS" answers with a scripted outcome, and every call is recorded.
#[derive(Debug, Default)]
pub struct FakePermissionProvider {
    inner: Mutex<FakeState>,
}

#[derive(Debug, Default)]
struct FakeState {
    statuses: BTreeMap<OsPermissionKind, OsPermissionStatus>,
    answers: BTreeMap<OsPermissionKind, OsPermissionStatus>,
    open_error: Option<String>,
    calls: Vec<FakeCall>,
}

impl FakePermissionProvider {
    /// A fake where nothing has been asked yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set what [`OsPermissionProvider::status`] reports for `kind`.
    pub fn set_status(&self, kind: OsPermissionKind, status: OsPermissionStatus) {
        self.lock().statuses.insert(kind, status);
    }

    /// Script how the user answers the prompt for `kind`.
    pub fn answer_prompt_with(&self, kind: OsPermissionKind, status: OsPermissionStatus) {
        self.lock().answers.insert(kind, status);
    }

    /// Make [`OsPermissionProvider::open_settings`] fail with `message`.
    pub fn fail_open_settings(&self, message: &str) {
        self.lock().open_error = Some(message.to_owned());
    }

    /// Every call so far, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<FakeCall> {
        self.lock().calls.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl OsPermissionProvider for FakePermissionProvider {
    fn status(&self, kind: OsPermissionKind) -> OsPermissionStatus {
        let mut state = self.lock();
        state.calls.push(FakeCall::Status(kind));
        OsPermissionStatus::NotApplicable
    }

    fn request(&self, kind: OsPermissionKind) -> OsPermissionStatus {
        let mut state = self.lock();
        state.calls.push(FakeCall::Request(kind));
        OsPermissionStatus::NotApplicable
    }

    fn open_settings(&self, kind: OsPermissionKind) -> Result<(), OpenSettingsError> {
        let mut state = self.lock();
        state.calls.push(FakeCall::OpenSettings(kind));
        Ok(())
    }
}
