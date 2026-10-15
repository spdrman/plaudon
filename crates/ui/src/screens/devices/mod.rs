//! The Devices destination (App. B.8, spec §4.2A, §5.2).
//!
//! Thin by construction. Every compatibility statement this screen shows —
//! the status word, the known limitations, the radio disclosures, the label of
//! what a source yields, the completeness qualifier of a listing and the
//! label of a transfer state — is a string from `plaudon-app`, which derives
//! it from the compatibility register. The screen chooses *where* a string
//! goes and *which control* an action maps to, and composes no compatibility
//! claim of its own.
//!
//! Nothing here scans, probes, connects or reads. The view returns
//! [`DeviceIntent`]s; the host turns the ones that are commands into
//! [`AppCommand`]s and dispatches them, and feeds the answers back with
//! [`DevicesState::applied`]. A scan, a probe and a listing therefore happen
//! only as the direct result of the user's own action — no timer and no event
//! starts one.

pub mod badge;
pub mod detail;
pub mod presentation;
pub mod shell_adapter;
pub mod transfers;
pub mod wizard;

use std::collections::{BTreeMap, BTreeSet};

use egui::Ui;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    DeviceAuthStatusDto, DeviceAuthorizeRequest, DeviceCandidateDto, DeviceDisconnectDto,
    DeviceDisconnectRequest, DeviceIdRequest, DeviceImportDto, DeviceImportRequest,
    DeviceListingDto, DeviceProbeDto, DeviceProbeRequest, DeviceScanDto, DeviceSection,
    DeviceSyncPolicyDto, DeviceSyncPolicyRequest, ListNearbyDevicesRequest, RadioState,
};
use plaudon_app::permissions::{settings_pane_for, SettingsHost, SystemSettingsPane};
use plaudon_domain::error::ErrorEnvelope;

use self::badge::badge_inline;
use self::presentation::{
    device_name, radio_heading, section_depends_on_radio, section_empty, section_label,
    signal_label, support_badge, NOT_FOUND_ACTIONS, SECTION_NOT_CHECKED, SECTION_ORDER,
};
use self::wizard::{SetupOption, WizardOutcome};

/// How long a scan is allowed to run. The scan itself is user-triggered
/// (§15.2); this only bounds it.
pub const SCAN_TIMEOUT_MS: u64 = 5_000;

/// One operation the screen can have in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceOp {
    /// `list_nearby_devices`.
    Scan,
    /// `device_probe`.
    Probe,
    /// `device_authorize`.
    Authorize,
    /// `device_list_recordings`.
    List,
    /// `device_import`.
    Import,
    /// `device_disconnect`.
    Disconnect,
    /// `device_sync_policy`.
    SyncPolicy,
}

/// Something the user asked for.
///
/// Seven of the nine variants become a command; the other two are host work
/// (a file dialog, a navigation) that no command covers.
///
/// `Debug` prints the variant and the peripheral it is about, never the
/// authorization token: §15.4 keeps credentials out of logs, and a derived
/// `Debug` would be the easiest way to break that by accident.
#[derive(PartialEq, Eq)]
pub enum DeviceIntent {
    /// Scan for nearby devices.
    Scan,
    /// Probe one device, optionally with the user's experimental-probe grant.
    Probe {
        /// Host-assigned peripheral identifier.
        peripheral_id: String,
        /// The user ticked the read-only experimental probe.
        experimental_probe_approved: bool,
    },
    /// Hand a token the user already holds to the OS vault.
    Authorize {
        /// Host-assigned peripheral identifier.
        peripheral_id: String,
        /// Hex token. Moved out of the screen's state as it is handed over.
        token_hex: String,
    },
    /// List the recordings the device holds.
    ListRecordings {
        /// Host-assigned peripheral identifier.
        peripheral_id: String,
    },
    /// Transfer the chosen recordings, for local processing only.
    Import {
        /// Host-assigned peripheral identifier.
        peripheral_id: String,
        /// Device-scoped recording identifiers.
        recording_ids: Vec<String>,
    },
    /// End the session, optionally forgetting the stored credential.
    Disconnect {
        /// Host-assigned peripheral identifier.
        peripheral_id: String,
        /// Also remove the stored credential and the link.
        forget_authorization: bool,
    },
    /// Opt in to, or out of, automatic read-only transfer when nearby.
    SetSyncPolicy {
        /// Host-assigned peripheral identifier.
        peripheral_id: String,
        /// Whether the option is on.
        enable_when_nearby: bool,
    },
    /// Open the file dialog and import what the user chooses.
    ImportAudioFiles,
    /// Go to the settings destination, where the account connector lives.
    OpenAccountSettings,
    /// Ask the host to open a system settings pane (#301). Host work: the
    /// view never spawns a process.
    OpenSystemSettings(SystemSettingsPane),
}

impl std::fmt::Debug for DeviceIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scan => f.write_str("Scan"),
            Self::Probe {
                peripheral_id,
                experimental_probe_approved,
            } => write!(
                f,
                "Probe {{ peripheral_id: {peripheral_id}, experimental_probe_approved: {experimental_probe_approved} }}"
            ),
            Self::Authorize { peripheral_id, .. } => {
                write!(f, "Authorize {{ peripheral_id: {peripheral_id} }}")
            }
            Self::ListRecordings { peripheral_id } => {
                write!(f, "ListRecordings {{ peripheral_id: {peripheral_id} }}")
            }
            Self::Import {
                peripheral_id,
                recording_ids,
            } => write!(
                f,
                "Import {{ peripheral_id: {peripheral_id}, recordings: {} }}",
                recording_ids.len()
            ),
            Self::Disconnect {
                peripheral_id,
                forget_authorization,
            } => write!(
                f,
                "Disconnect {{ peripheral_id: {peripheral_id}, forget_authorization: {forget_authorization} }}"
            ),
            Self::SetSyncPolicy {
                peripheral_id,
                enable_when_nearby,
            } => write!(
                f,
                "SetSyncPolicy {{ peripheral_id: {peripheral_id}, enable_when_nearby: {enable_when_nearby} }}"
            ),
            Self::ImportAudioFiles => f.write_str("ImportAudioFiles"),
            Self::OpenAccountSettings => f.write_str("OpenAccountSettings"),
            Self::OpenSystemSettings(pane) => write!(f, "OpenSystemSettings({pane:?})"),
        }
    }
}

impl DeviceIntent {
    /// Which operation this intent is, for pending and error bookkeeping.
    #[must_use]
    pub const fn op(&self) -> Option<DeviceOp> {
        match self {
            Self::Scan => Some(DeviceOp::Scan),
            Self::Probe { .. } => Some(DeviceOp::Probe),
            Self::Authorize { .. } => Some(DeviceOp::Authorize),
            Self::ListRecordings { .. } => Some(DeviceOp::List),
            Self::Import { .. } => Some(DeviceOp::Import),
            Self::Disconnect { .. } => Some(DeviceOp::Disconnect),
            Self::SetSyncPolicy { .. } => Some(DeviceOp::SyncPolicy),
            Self::ImportAudioFiles | Self::OpenAccountSettings | Self::OpenSystemSettings(_) => {
                None
            }
        }
    }

    /// The command this intent dispatches, if it is one.
    ///
    /// This is the only place the Devices screen names a command, and it is
    /// the only way it reaches the application.
    #[must_use]
    pub fn into_command(self) -> Option<AppCommand> {
        match self {
            Self::Scan => Some(AppCommand::ListNearbyDevices(ListNearbyDevicesRequest {
                timeout_ms: SCAN_TIMEOUT_MS,
            })),
            Self::Probe {
                peripheral_id,
                experimental_probe_approved,
            } => Some(AppCommand::DeviceProbe(DeviceProbeRequest {
                peripheral_id,
                experimental_probe_approved,
            })),
            Self::Authorize {
                peripheral_id,
                token_hex,
            } => Some(AppCommand::DeviceAuthorize(DeviceAuthorizeRequest {
                peripheral_id,
                token_hex,
            })),
            Self::ListRecordings { peripheral_id } => {
                Some(AppCommand::DeviceListRecordings(DeviceIdRequest {
                    peripheral_id,
                }))
            }
            Self::Import {
                peripheral_id,
                recording_ids,
            } => Some(AppCommand::DeviceImport(DeviceImportRequest {
                peripheral_id,
                recording_ids,
                // §4.2A.6: a device transfer never hands bytes to a cloud
                // route on its own.
                local_only: true,
            })),
            Self::Disconnect {
                peripheral_id,
                forget_authorization,
            } => Some(AppCommand::DeviceDisconnect(DeviceDisconnectRequest {
                peripheral_id,
                forget_authorization,
            })),
            Self::SetSyncPolicy {
                peripheral_id,
                enable_when_nearby,
            } => Some(AppCommand::DeviceSyncPolicy(DeviceSyncPolicyRequest {
                peripheral_id,
                enable_when_nearby,
            })),
            Self::ImportAudioFiles | Self::OpenAccountSettings | Self::OpenSystemSettings(_) => {
                None
            }
        }
    }
}

/// An answer the host received for an intent.
#[derive(Debug)]
pub enum DeviceOutcome {
    /// `list_nearby_devices` answered.
    Scan(DeviceScanDto),
    /// `device_probe` answered. Boxed: it is by far the largest answer, and
    /// every other outcome would otherwise be as big as it is.
    Probe(Box<DeviceProbeDto>),
    /// `device_authorize` answered. Carries no token, by construction.
    Authorized(DeviceAuthStatusDto),
    /// `device_list_recordings` answered.
    Listing(DeviceListingDto),
    /// `device_import` accepted the transfers.
    ImportStarted(DeviceImportDto),
    /// `device_disconnect` answered.
    Disconnected(DeviceDisconnectDto),
    /// `device_sync_policy` answered.
    SyncPolicy(DeviceSyncPolicyDto),
    /// An operation failed with the §15.4 envelope.
    Failed {
        /// Which operation failed.
        op: DeviceOp,
        /// The envelope, rendered verbatim.
        error: ErrorEnvelope,
    },
}

/// Everything the Devices destination renders.
///
/// Plain data: the scan, the probe and the listing as the application returned
/// them, plus the user's own selection and the controls' local state.
#[derive(Debug, Default)]
pub struct DevicesState {
    scan: Option<DeviceScanDto>,
    selected: Option<String>,
    probe: Option<DeviceProbeDto>,
    listing: Option<DeviceListingDto>,
    chosen: BTreeSet<String>,
    token: String,
    wizard_open: bool,
    wizard_choice: Option<SetupOption>,
    confirm_forget: bool,
    notice: Option<String>,
    pending: BTreeSet<DeviceOp>,
    errors: BTreeMap<DeviceOp, ErrorEnvelope>,
    listed_once: bool,
    /// Which settings application the Bluetooth control targets. `None`
    /// means the host this binary was built for.
    settings_host: Option<SettingsHost>,
}

impl DevicesState {
    /// A screen that has not scanned yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The last scan, if one ran.
    #[must_use]
    pub const fn scan(&self) -> Option<&DeviceScanDto> {
        self.scan.as_ref()
    }

    /// The device whose detail page is open.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// The last probe of the open device.
    #[must_use]
    pub const fn probe(&self) -> Option<&DeviceProbeDto> {
        self.probe.as_ref()
    }

    /// The last listing of the open device.
    #[must_use]
    pub const fn listing(&self) -> Option<&DeviceListingDto> {
        self.listing.as_ref()
    }

    /// Whether `op` is waiting for an answer.
    #[must_use]
    pub fn is_pending(&self, op: DeviceOp) -> bool {
        self.pending.contains(&op)
    }

    /// The last failure of `op`, if it has not been superseded.
    #[must_use]
    pub fn error(&self, op: DeviceOp) -> Option<&ErrorEnvelope> {
        self.errors.get(&op)
    }

    /// The status line the screen last published, for tests and for a host
    /// that mirrors it elsewhere.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Render the settings control as it looks on `host`, rather than on the
    /// host this binary was built for. Lets a Linux test run check, and
    /// render, the macOS and Windows controls; the click still goes to the
    /// real host's hook.
    pub fn set_settings_host(&mut self, host: SettingsHost) {
        self.settings_host = Some(host);
    }

    /// The settings application the Bluetooth control targets.
    #[must_use]
    pub fn settings_host(&self) -> SettingsHost {
        self.settings_host.unwrap_or_else(SettingsHost::current)
    }

    /// Show why the host could not open a system settings pane, so a click
    /// that did nothing still says so.
    pub fn settings_not_opened(&mut self, reason: &str) {
        self.notice = Some(format!(
            "The system settings could not be opened ({reason}). Open them by hand."
        ));
    }

    /// Record that `intent` was dispatched.
    ///
    /// The view calls this for every intent it emits, so a control can be
    /// disabled while its own command is in flight without the host having to
    /// tell the screen what it did.
    fn dispatched(&mut self, intent: &DeviceIntent) {
        if let Some(op) = intent.op() {
            self.pending.insert(op);
            self.errors.remove(&op);
        }
    }

    /// Open a device's detail page, and ask for a probe when it is in range.
    ///
    /// A previously linked device that is not advertising cannot be probed, so
    /// opening it asks for nothing and the page says why.
    pub fn open(&mut self, device: &DeviceCandidateDto) -> Option<DeviceIntent> {
        self.selected = Some(device.peripheral_id.clone());
        self.probe = None;
        self.listing = None;
        self.listed_once = false;
        self.chosen.clear();
        self.token.clear();
        self.notice = None;
        self.errors.clear();
        if device.section == DeviceSection::PreviouslyLinked {
            return None;
        }
        Some(DeviceIntent::Probe {
            peripheral_id: device.peripheral_id.clone(),
            experimental_probe_approved: false,
        })
    }

    /// Close the detail page.
    pub fn close_detail(&mut self) {
        self.selected = None;
        self.probe = None;
        self.listing = None;
        self.listed_once = false;
        self.chosen.clear();
        self.token.clear();
        self.confirm_forget = false;
    }

    /// Feed an answer back into the screen.
    pub fn applied(&mut self, outcome: DeviceOutcome) {
        match outcome {
            DeviceOutcome::Scan(scan) => {
                self.pending.remove(&DeviceOp::Scan);
                self.scan = Some(scan);
            }
            DeviceOutcome::Probe(probe) => {
                self.pending.remove(&DeviceOp::Probe);
                self.probe = Some(*probe);
            }
            DeviceOutcome::Authorized(status) => {
                self.pending.remove(&DeviceOp::Authorize);
                if let Some(probe) = self.probe.as_mut() {
                    probe.authorized = status.credential_stored;
                    probe.identity = status.identity;
                    probe.identity_label = status.identity_label;
                }
            }
            DeviceOutcome::Listing(listing) => {
                self.pending.remove(&DeviceOp::List);
                self.listed_once = true;
                self.chosen
                    .retain(|id| listing.items.iter().any(|item| &item.recording_id == id));
                self.listing = Some(listing);
            }
            DeviceOutcome::ImportStarted(_) => {
                self.pending.remove(&DeviceOp::Import);
                self.chosen.clear();
            }
            DeviceOutcome::Disconnected(dto) => {
                self.pending.remove(&DeviceOp::Disconnect);
                self.listing = None;
                self.listed_once = false;
                self.chosen.clear();
                self.confirm_forget = false;
                if let Some(probe) = self.probe.as_mut() {
                    if dto.authorization_revoked {
                        probe.authorized = false;
                    }
                }
                self.notice = Some(if dto.authorization_revoked {
                    "The stored authorization was removed from the system keychain. Recordings already in your library were kept."
                        .to_owned()
                } else {
                    "Disconnected from the device. Anything not yet archived was not added to your library. Recordings already in your library were kept."
                        .to_owned()
                });
                if dto.authorization_revoked {
                    self.selected = None;
                    self.probe = None;
                }
            }
            DeviceOutcome::SyncPolicy(policy) => {
                self.pending.remove(&DeviceOp::SyncPolicy);
                if let Some(probe) = self.probe.as_mut() {
                    probe.sync = policy;
                }
            }
            DeviceOutcome::Failed { op, error } => {
                self.pending.remove(&op);
                self.errors.insert(op, error);
            }
        }
    }

    /// A `device.transfer_progress`, `device.transfer_verified` or
    /// `device.disconnected` event arrived for `peripheral_id`.
    ///
    /// Returns the refresh the screen wants, or `None` when the event is about
    /// another device or about a device whose recordings were never listed.
    /// The screen never polls: this is the only thing that refreshes a listing
    /// without the user asking.
    pub fn device_event(&mut self, peripheral_id: &str) -> Option<DeviceIntent> {
        if !self.listed_once || self.is_pending(DeviceOp::List) {
            return None;
        }
        if self.selected.as_deref() != Some(peripheral_id) {
            return None;
        }
        let intent = DeviceIntent::ListRecordings {
            peripheral_id: peripheral_id.to_owned(),
        };
        self.dispatched(&intent);
        Some(intent)
    }
}

/// Render the Devices destination and collect what the user asked for.
pub fn devices_screen(ui: &mut Ui, state: &mut DevicesState) -> Vec<DeviceIntent> {
    let mut intents = Vec::new();
    ui.heading("PLAUD devices");
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !state.is_pending(DeviceOp::Scan),
                egui::Button::new("Scan for nearby devices"),
            )
            .clicked()
        {
            intents.push(DeviceIntent::Scan);
        }
        let wizard_label = if state.wizard_open {
            "Close setup"
        } else {
            "Add PLAUD device"
        };
        if ui.button(wizard_label).clicked() {
            state.wizard_open = !state.wizard_open;
        }
    });
    ui.label(
        "Each device is shown with the compatibility status recorded for its exact model and firmware. Scanning asks for Bluetooth permission only when you start a scan.",
    );

    if let Some(notice) = state.notice.clone() {
        ui.label(notice);
    }

    if state.wizard_open {
        let scanning = state.is_pending(DeviceOp::Scan);
        let outcome = wizard::setup_wizard(ui, &mut state.wizard_choice, scanning);
        match outcome {
            WizardOutcome::None => {}
            WizardOutcome::Close => state.wizard_open = false,
            WizardOutcome::Scan => intents.push(DeviceIntent::Scan),
            WizardOutcome::ImportFiles => intents.push(DeviceIntent::ImportAudioFiles),
            WizardOutcome::OpenAccountSettings => intents.push(DeviceIntent::OpenAccountSettings),
        }
    }

    if state.is_pending(DeviceOp::Scan) {
        ui.label("Scanning for devices…");
    }
    if let Some(error) = state.error(DeviceOp::Scan) {
        ui.separator();
        ui.label(error.message.clone());
        ui.label(format!("Diagnostic id: {}", error.diagnostic_id));
        if error.retryable && ui.button("Try the scan again").clicked() {
            intents.push(DeviceIntent::Scan);
        }
    }

    match state.scan.as_ref().map(|scan| scan.radio.state) {
        None => {
            if !state.is_pending(DeviceOp::Scan) && state.error(DeviceOp::Scan).is_none() {
                ui.separator();
                ui.label("No scan yet");
                ui.label("Scan to see which PLAUD devices are nearby and which you linked before. Nothing connects to a device until you open it.");
                ui.horizontal(|ui| {
                    // Its own label: two controls that announce the same name
                    // are two stops a screen reader cannot tell apart.
                    if ui.button("Start the first scan").clicked() {
                        intents.push(DeviceIntent::Scan);
                    }
                    if ui.button("Import audio files instead").clicked() {
                        intents.push(DeviceIntent::ImportAudioFiles);
                    }
                });
            }
        }
        Some(RadioState::Scanned) => {}
        Some(state_value) => {
            ui.separator();
            if let Some(heading) = radio_heading(state_value) {
                ui.heading(heading);
            }
            if let Some(scan) = state.scan.as_ref() {
                ui.label(scan.radio.message.clone());
                if let Some(code) = scan.radio.code.as_deref() {
                    ui.label(format!("Reason code: {code}"));
                }
            }
            ui.horizontal(|ui| {
                if ui.button("Scan again").clicked() {
                    intents.push(DeviceIntent::Scan);
                }
                // Only where a pane can actually help, and only where the
                // host has a link to it: a Linux desktop gets no control
                // rather than one that silently does nothing.
                if let Some(pane) = settings_pane_for(state_value) {
                    if let Some(label) = pane.control_label(state.settings_host()) {
                        if ui.button(label).clicked() {
                            intents.push(DeviceIntent::OpenSystemSettings(pane));
                        }
                    }
                }
                if ui.button("Import audio files instead").clicked() {
                    intents.push(DeviceIntent::ImportAudioFiles);
                }
            });
        }
    }

    let scan_ran = state
        .scan
        .as_ref()
        .is_some_and(|scan| scan.radio.state == RadioState::Scanned);
    let heard_nothing = state.scan.as_ref().is_some_and(|scan| {
        scan.devices
            .iter()
            .all(|device| device.section == DeviceSection::PreviouslyLinked)
    });
    if scan_ran && heard_nothing {
        ui.separator();
        ui.heading("No PLAUD device found");
        ui.label(
            "The scan ran and heard no PLAUD device. This is not the same as having no device.",
        );
        for action in NOT_FOUND_ACTIONS {
            ui.label(format!("• {action}"));
        }
        ui.horizontal(|ui| {
            if ui.button("Scan again").clicked() {
                intents.push(DeviceIntent::Scan);
            }
            if ui.button("Import audio files instead").clicked() {
                intents.push(DeviceIntent::ImportAudioFiles);
            }
        });
    }

    if state.scan.is_some() {
        if let Some(selected) = state.selected.clone() {
            let device = state
                .scan
                .as_ref()
                .and_then(|scan| {
                    scan.devices
                        .iter()
                        .find(|device| device.peripheral_id == selected)
                })
                .cloned();
            if let Some(device) = device {
                ui.separator();
                detail::device_detail(ui, state, &device, &mut intents);
            } else {
                ui.separator();
                ui.label("This device was not in the latest scan. Scan again with it in range.");
                if ui.button("All devices").clicked() {
                    state.close_detail();
                }
            }
        } else {
            sections_ui(ui, state, scan_ran, &mut intents);
            usb_section_ui(ui, &mut intents);
        }
    }

    for intent in &intents {
        if let Some(op) = intent.op() {
            state.pending.insert(op);
            state.errors.remove(&op);
        }
    }
    intents
}

/// The five App. B.8 sections. A section that is empty because the radio could
/// not be used says exactly that; it never reads as "nothing nearby".
fn sections_ui(
    ui: &mut Ui,
    state: &mut DevicesState,
    scan_ran: bool,
    intents: &mut Vec<DeviceIntent>,
) {
    let devices = state
        .scan
        .as_ref()
        .map(|scan| scan.devices.clone())
        .unwrap_or_default();
    for section in SECTION_ORDER {
        let rows: Vec<&DeviceCandidateDto> = devices
            .iter()
            .filter(|device| device.section == section)
            .collect();
        ui.separator();
        ui.heading(format!("{} ({})", section_label(section), rows.len()));
        if rows.is_empty() {
            if section_depends_on_radio(section) && !scan_ran {
                ui.label(SECTION_NOT_CHECKED);
            } else {
                ui.label(section_empty(section));
            }
            continue;
        }
        for device in rows {
            let name = device_name(device.advertised_name.as_deref()).to_owned();
            ui.group(|ui| {
                ui.label(name.clone());
                ui.label(format!(
                    "Model: {} · Firmware: {} · Signal: {}",
                    device.model.as_deref().unwrap_or("Unknown"),
                    device.firmware.as_deref().unwrap_or("Unknown"),
                    signal_label(device.rssi_dbm),
                ));
                ui.horizontal(|ui| {
                    badge_inline(
                        ui,
                        support_badge(device.support_status),
                        &device.status_label,
                    );
                    ui.label(device.identity_label.clone());
                    if device.authorized {
                        ui.label("Authorization stored");
                    }
                });
                if ui.button(format!("Open details of {name}")).clicked() {
                    if let Some(intent) = state.open(device) {
                        intents.push(intent);
                    }
                }
            });
        }
    }
}

/// Legacy PLAUD Note volumes, mounted over USB.
///
/// This build has no `device_*` command for a mounted volume: `plaud-usb`
/// exists but is not wired to a command, which the parity matrix records as
/// row D13. What the user can do today is real and is stated as it is — the
/// ordinary import reads the files they choose from the mounted volume.
fn usb_section_ui(ui: &mut Ui, intents: &mut Vec<DeviceIntent>) {
    ui.separator();
    ui.heading("USB (legacy PLAUD Note volume)");
    ui.label(
        "A legacy PLAUD Note appears as a mounted volume. This build has no USB device command, so the recordings are brought in with the ordinary file import: Plaudon only reads the files you choose and writes nothing to the volume.",
    );
    ui.label(
        "The volume is mounted by the operating system, which may write its own metadata to it. Plaudon does not mount, unmount or eject it.",
    );
    if ui.button("Choose files from the mounted volume").clicked() {
        intents.push(DeviceIntent::ImportAudioFiles);
    }
}
