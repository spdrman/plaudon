//! How the Devices destination plugs into the shell.
//!
//! [`devices_screen`] renders [`DevicesState`] and returns [`DeviceIntent`]s;
//! it reaches no port, opens no dialog and knows no route. This module is the
//! one place that knows both halves: it turns the seven command intents into
//! [`AppCommand`]s, carries out the two host intents, and feeds every answer
//! back with [`DevicesState::applied`].
//!
//! Nothing here scans, probes or lists on its own. A command is sent because
//! the user asked for it, because an answer arrived, or because a §15.3 device
//! event invalidated the listing on screen.

use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{
    DeviceCandidateDto, DuplicatePolicy, EventKind, ImportFilesRequest, UiEvent,
};
use plaudon_app::event::ResyncReason;
use plaudon_domain::error::ErrorEnvelope;

use crate::shell::route::{Destination, Route};
use crate::shell::screen::{Screen, ScreenCtx};
use crate::widgets::toast::ToastTone;

use super::{devices_screen, DeviceIntent, DeviceOp, DeviceOutcome, DevicesState};

/// The peripheral a route names, if it names one.
fn routed_peripheral(route: &Route) -> Option<String> {
    match route {
        Route::DevicesDevice { peripheral_id } => Some(peripheral_id.clone()),
        _ => None,
    }
}

/// The route a selection means.
fn route_for(peripheral_id: Option<String>) -> Route {
    match peripheral_id {
        Some(peripheral_id) => Route::DevicesDevice { peripheral_id },
        None => Route::Devices,
    }
}

/// The Devices destination, as the shell sees it.
///
/// # Route and selection
///
/// `Route::DevicesDevice { peripheral_id }` and the screen's own selection are
/// kept in step in both directions, within the limit of what [`DevicesState`]
/// can honestly do:
///
/// * selection → route: whenever the selection moves — the user opened a
///   device, or a disconnect closed one — the shell is navigated to match, so
///   a deep link, the header title and what is on screen agree.
/// * route → selection: [`DevicesState::open`] takes the
///   [`DeviceCandidateDto`] the last scan returned, because opening a device
///   also decides whether it can be probed. **A route naming a device that is
///   not in the last scan cannot be honoured**, and is not faked: the
///   selection is left alone, the link is retried on every frame, and it is
///   honoured as soon as a scan containing that device arrives. Nothing scans
///   by itself to satisfy a link — a scan asks for Bluetooth permission and is
///   the user's own action (§15.2).
#[derive(Debug, Default)]
pub struct DevicesShellScreen {
    state: DevicesState,
    /// Which operation each in-flight command belongs to, so an envelope is
    /// reported against the control that caused it. At most one entry per
    /// concurrently dispatched device command.
    in_flight: Vec<(CommandId, DeviceOp)>,
    /// `import_files` commands this screen started. Not a [`DeviceOp`]: the
    /// manual import has no slot in [`DevicesState`].
    imports: Vec<CommandId>,
    /// The peripheral the route and the selection were last agreed on.
    linked: Option<String>,
}

impl DevicesShellScreen {
    /// A fresh Devices screen.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The state underneath, for a test that wants to seed it.
    pub fn state_mut(&mut self) -> &mut DevicesState {
        &mut self.state
    }

    /// Send the command an intent stands for, remembering which operation it
    /// was so its answer can be routed back.
    fn send_intent(&mut self, intent: DeviceIntent, cx: &mut ScreenCtx<'_>) {
        let op = intent.op();
        let Some(command) = intent.into_command() else {
            return;
        };
        let id = cx.send(command);
        if let Some(op) = op {
            self.in_flight.push((id, op));
        }
    }

    /// Carry out one intent: a command, a dialog, or a navigation.
    fn dispatch(&mut self, intent: DeviceIntent, cx: &mut ScreenCtx<'_>) {
        match intent {
            DeviceIntent::ImportAudioFiles => {
                // The dialog blocks on the operating system, so it is opened
                // after the frame the control was clicked in. The view never
                // touches `rfd`: the binary supplies the hook.
                let paths: Vec<String> = (cx.hooks().pick_audio_files)()
                    .into_iter()
                    .filter_map(|path| path.to_str().map(str::to_owned))
                    .collect();
                if paths.is_empty() {
                    return;
                }
                let id = cx.send(AppCommand::ImportFiles(ImportFilesRequest {
                    paths,
                    // The user chose these files by hand and has said nothing
                    // about duplicates, so the application is asked rather
                    // than told what to do with one.
                    duplicate_policy: DuplicatePolicy::Ask,
                }));
                self.imports.push(id);
            }
            DeviceIntent::OpenAccountSettings => cx.navigate(Route::SettingsSources),
            DeviceIntent::OpenSystemSettings(pane) => {
                if let Err(reason) = (cx.hooks().open_system_settings)(pane) {
                    self.state.settings_not_opened(&reason);
                }
            }
            command_intent => self.send_intent(command_intent, cx),
        }
    }

    /// Which operation `id` belonged to, removing it from the ledger.
    fn take_in_flight(&mut self, id: CommandId) -> Option<DeviceOp> {
        let index = self.in_flight.iter().position(|(known, _)| *known == id)?;
        Some(self.in_flight.swap_remove(index).1)
    }

    /// Whether `id` was one of this screen's manual imports.
    fn take_import(&mut self, id: CommandId) -> bool {
        let Some(index) = self.imports.iter().position(|known| *known == id) else {
            return false;
        };
        self.imports.swap_remove(index);
        true
    }

    /// Move the selection to where the route points, when that is possible.
    fn follow_route(&mut self, cx: &mut ScreenCtx<'_>) {
        let routed = routed_peripheral(cx.route());
        if routed == self.linked {
            return;
        }
        let Some(peripheral_id) = routed.clone() else {
            self.state.close_detail();
            self.linked = None;
            return;
        };
        let candidate: Option<DeviceCandidateDto> = self.state.scan().and_then(|scan| {
            scan.devices
                .iter()
                .find(|device| device.peripheral_id == peripheral_id)
                .cloned()
        });
        let Some(candidate) = candidate else {
            // Not in the last scan: the link stays unhonoured rather than
            // opening a device this process knows nothing about. Retried on
            // the next frame, so a scan that arrives later completes it.
            return;
        };
        if let Some(intent) = self.state.open(&candidate) {
            self.send_intent(intent, cx);
        }
        self.linked = routed;
    }

    /// Navigate when the selection no longer matches the agreed route.
    fn publish_selection(&mut self, cx: &mut ScreenCtx<'_>) {
        let selected = self.state.selected().map(str::to_owned);
        if selected == self.linked {
            return;
        }
        cx.navigate(route_for(selected.clone()));
        self.linked = selected;
    }
}

impl Screen for DevicesShellScreen {
    fn destination(&self) -> Destination {
        Destination::Devices
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        self.follow_route(cx);
        for intent in devices_screen(ui, &mut self.state) {
            self.dispatch(intent, cx);
        }
        self.publish_selection(cx);
    }

    fn on_response(
        &mut self,
        id: CommandId,
        _command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        cx: &mut ScreenCtx<'_>,
    ) {
        let op = self.take_in_flight(id);
        let was_import = self.take_import(id);
        match result {
            Ok(response) => {
                let outcome = match response {
                    AppResponse::ListNearbyDevices(scan) => DeviceOutcome::Scan(scan.clone()),
                    AppResponse::DeviceProbe(probe) => {
                        DeviceOutcome::Probe(Box::new(probe.clone()))
                    }
                    AppResponse::DeviceAuthorize(status) => {
                        DeviceOutcome::Authorized(status.clone())
                    }
                    AppResponse::DeviceListRecordings(listing) => {
                        DeviceOutcome::Listing(listing.clone())
                    }
                    AppResponse::DeviceImport(started) => {
                        DeviceOutcome::ImportStarted(started.clone())
                    }
                    AppResponse::DeviceDisconnect(dto) => DeviceOutcome::Disconnected(dto.clone()),
                    AppResponse::DeviceSyncPolicy(policy) => {
                        DeviceOutcome::SyncPolicy(policy.clone())
                    }
                    // Not this screen's answer. `import_files` is reported by
                    // the library it lands in, and the rest belongs to the
                    // other screens that share the shell's fan-out.
                    _ => return,
                };
                self.state.applied(outcome);
            }
            Err(error) => {
                if let Some(op) = op {
                    self.state.applied(DeviceOutcome::Failed {
                        op,
                        error: error.clone(),
                    });
                } else if was_import {
                    // The manual import has no slot in `DevicesState`, so the
                    // failure is announced instead — in the application's own
                    // words, with the diagnostic id the §15.4 envelope
                    // carries.
                    cx.toast(
                        ToastTone::Danger,
                        error.message.clone(),
                        Some(format!("Diagnostic id: {}", error.diagnostic_id)),
                    );
                }
            }
        }
    }

    fn on_ui_event(&mut self, event: &UiEvent, cx: &mut ScreenCtx<'_>) {
        if !matches!(
            event.kind,
            EventKind::DeviceTransferProgress
                | EventKind::DeviceTransferVerified
                | EventKind::DeviceDisconnected
                | EventKind::DeviceConnected
                | EventKind::DeviceCompatibilityChanged
                | EventKind::DeviceDiscovered
        ) {
            return;
        }
        // The peripheral is only ever in the hint payload: `UiEvent` addresses
        // jobs and recordings by id, never devices. An event without one names
        // no device this screen could refresh, so nothing happens — guessing
        // would refresh the wrong listing.
        let Some(hint) = event.payload.get("peripheral_id") else {
            return;
        };
        let Some(peripheral_id) = hint.as_str() else {
            return;
        };
        if let Some(intent) = self.state.device_event(peripheral_id) {
            self.send_intent(intent, cx);
        }
    }

    fn on_resync(&mut self, _reason: ResyncReason, _cx: &mut ScreenCtx<'_>) {
        // A scan touches the radio and asks for Bluetooth permission (§15.2),
        // so nothing but the user ever starts one. A resync therefore only
        // drops what may be stale: the screen then says it has not scanned,
        // which is true, rather than showing devices that may be gone.
        self.state = DevicesState::new();
        self.in_flight.clear();
        self.linked = None;
    }
}
