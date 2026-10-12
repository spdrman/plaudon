//! Detail page of one device (App. B.8).
//!
//! Model, firmware, transport, advertised name, signal, battery, identity
//! assurance, last contact, the number of files on the device with its
//! completeness qualifier, the register's status and verbatim limitations, the
//! radio disclosures and whether automatic sync may be advertised at all.
//! Anything the application could not establish is `Unknown`; none of it is
//! guessed and none of it is a zero.

use egui::{TextEdit, Ui};
use plaudon_app::dto::{DeviceCandidateDto, DeviceCapability, DeviceSection};

use super::badge::badge_inline;
use super::presentation::{
    battery_label, capability_label, cloud_label, device_name, signal_label, support_badge,
    transport_label,
};
use super::transfers::{transfer_list, TransferAction};
use super::{DeviceIntent, DeviceOp, DevicesState};

/// Render the detail page of `device`, pushing what the user asked for onto
/// `intents`.
#[allow(clippy::too_many_lines)]
pub fn device_detail(
    ui: &mut Ui,
    state: &mut DevicesState,
    device: &DeviceCandidateDto,
    intents: &mut Vec<DeviceIntent>,
) {
    let peripheral_id = device.peripheral_id.clone();
    if ui.button("All devices").clicked() {
        state.close_detail();
        return;
    }

    let probe = state.probe.clone();
    let name = device_name(
        probe
            .as_ref()
            .and_then(|probe| probe.advertised_name.as_deref())
            .or(device.advertised_name.as_deref()),
    )
    .to_owned();
    let status_label = probe.as_ref().map_or(device.status_label.clone(), |probe| {
        probe.status_label.clone()
    });
    let support = support_badge(
        probe
            .as_ref()
            .map_or(device.support_status, |probe| probe.support_status),
    );
    let identity_label = state
        .listing
        .as_ref()
        .map(|listing| listing.identity_label.clone())
        .or_else(|| probe.as_ref().map(|probe| probe.identity_label.clone()))
        .unwrap_or_else(|| device.identity_label.clone());
    let authorized = probe
        .as_ref()
        .map_or(device.authorized, |probe| probe.authorized);

    ui.heading(name.clone());
    ui.horizontal(|ui| {
        badge_inline(ui, support, &status_label);
        ui.label(identity_label.clone());
    });

    if state.is_pending(DeviceOp::Probe) {
        ui.label("Checking the device…");
    }
    if device.section == DeviceSection::PreviouslyLinked && probe.is_none() {
        ui.label(
            "This device is not advertising right now. Bring it within range and scan again to check it.",
        );
    }
    if let Some(error) = state.error(DeviceOp::Probe) {
        ui.label(error.message.clone());
        if error.retryable && ui.button("Check the device again").clicked() {
            intents.push(DeviceIntent::Probe {
                peripheral_id: peripheral_id.clone(),
                experimental_probe_approved: false,
            });
        }
    }

    /* ------------------------------------------------------------- facts */

    ui.separator();
    ui.heading("Device facts");
    ui.label(format!(
        "Model: {}",
        probe
            .as_ref()
            .and_then(|probe| probe.model.as_deref())
            .or(device.model.as_deref())
            .unwrap_or("Unknown")
    ));
    ui.label(format!(
        "Firmware: {}",
        probe
            .as_ref()
            .and_then(|probe| probe.firmware.as_deref())
            .or(device.firmware.as_deref())
            .unwrap_or("Unknown")
    ));
    ui.label(format!(
        "Transport: {}",
        transport_label(
            probe
                .as_ref()
                .map_or(device.transport.as_str(), |probe| probe.transport.as_str())
        )
    ));
    ui.label(format!(
        "Advertised name: {}",
        probe
            .as_ref()
            .and_then(|probe| probe.advertised_name.as_deref())
            .or(device.advertised_name.as_deref())
            .unwrap_or("Not advertised")
    ));
    ui.label(format!(
        "Signal strength: {}",
        signal_label(
            probe
                .as_ref()
                .and_then(|probe| probe.rssi_dbm)
                .or(device.rssi_dbm)
        )
    ));
    ui.label(format!(
        "Battery: {}",
        battery_label(probe.as_ref().and_then(|probe| probe.battery_percent))
    ));
    ui.label(format!(
        "Authentication mode: {}",
        probe
            .as_ref()
            .and_then(|probe| probe.auth_mode.as_deref())
            .unwrap_or("Unknown")
    ));
    ui.label(format!("Identity: {identity_label}"));
    ui.label(format!(
        "Last contact: {}",
        probe
            .as_ref()
            .and_then(|probe| probe.last_contact_at.as_deref())
            .or(device.last_contact_at.as_deref())
            .unwrap_or("Never")
    ));
    ui.label(format!(
        "Files on the device: {}",
        state.listing.as_ref().map_or_else(
            || "Unknown until the recordings are listed".to_owned(),
            |listing| listing.file_count.to_string()
        )
    ));
    if let Some(probe) = probe.as_ref() {
        ui.label(format!("Source: {}", probe.source_label));
        ui.label(cloud_label(probe.cloud_route));
    }
    if !state
        .probe
        .as_ref()
        .is_some_and(|probe| probe.identity.is_authenticated())
    {
        ui.label(
            "An advertisement can be copied, so a name or a signal does not identify this device. It stays unverified until the device accepts its authorization.",
        );
    }

    /* ------------------------------------------------------- disclosures */

    if let Some(probe) = probe.as_ref() {
        if !probe.disclosures.is_empty() {
            ui.separator();
            ui.heading("Radio disclosures");
            for disclosure in &probe.disclosures {
                ui.label(disclosure.clone());
            }
        }

        ui.separator();
        ui.heading("Capabilities");
        for capability in &probe.capabilities {
            let availability = if capability.available {
                "Available"
            } else {
                "Not available"
            };
            ui.label(format!(
                "{}: {availability}{}",
                capability_label(capability.capability),
                capability
                    .reason
                    .as_deref()
                    .map_or_else(String::new, |reason| format!(" — {reason}"))
            ));
        }

        if !probe.known_limitations.is_empty() {
            ui.separator();
            ui.heading("Known limitations");
            for limitation in &probe.known_limitations {
                ui.label(limitation.clone());
            }
        }

        /* ------------------------------------------------------ sync */

        ui.separator();
        ui.heading("Automatic sync");
        if probe.sync.may_advertise {
            let mut enabled = probe.sync.enabled;
            if ui
                .add_enabled_ui(!state.is_pending(DeviceOp::SyncPolicy), |ui| {
                    ui.checkbox(
                        &mut enabled,
                        "Transfer recordings automatically when this device is nearby (read-only)",
                    )
                })
                .inner
                .changed()
            {
                intents.push(DeviceIntent::SetSyncPolicy {
                    peripheral_id: peripheral_id.clone(),
                    enable_when_nearby: enabled,
                });
            }
        } else {
            ui.label(format!("Not offered. {}", probe.sync.explanation));
        }
        if let Some(error) = state.error(DeviceOp::SyncPolicy) {
            ui.label(error.message.clone());
        }

        /* ---------------------------------------------- experimental */

        if probe.gate.experimental_probe_available {
            ui.separator();
            ui.heading("Experimental probe");
            if let Some(code) = probe.gate.refusal_code.as_deref() {
                ui.label(format!("Gate decision: {code}"));
            }
            let mut approved = probe.gate.experimental_probe_approved;
            if ui
                .add_enabled_ui(!state.is_pending(DeviceOp::Probe), |ui| {
                    ui.checkbox(
                        &mut approved,
                        "Allow a read-only experimental probe of this device (listing only, no transfer)",
                    )
                })
                .inner
                .changed()
            {
                intents.push(DeviceIntent::Probe {
                    peripheral_id: peripheral_id.clone(),
                    experimental_probe_approved: approved,
                });
            }
        }

        /* ----------------------------------------------- authorization */

        let needs_credential = !probe.authorized
            && matches!(
                probe.section,
                DeviceSection::NeedsAuthorization | DeviceSection::Nearby
            );
        if needs_credential {
            ui.separator();
            ui.heading("Authorize this device");
            if let Some(note) = probe.provisioning_note.as_deref() {
                ui.label(note);
            }
            let label = ui.label("Authorization token (hexadecimal)");
            let field = ui.add(
                TextEdit::singleline(&mut state.token)
                    .password(true)
                    .hint_text("Token you already hold for your own device"),
            );
            field.labelled_by(label.id);
            ui.label(
                "Stored in the system keychain for this device only. It is never shown again and never leaves this computer.",
            );
            let ready = !state.token.trim().is_empty() && !state.is_pending(DeviceOp::Authorize);
            if ui
                .add_enabled(ready, egui::Button::new("Store in system keychain"))
                .clicked()
            {
                // The value leaves the screen's state as it is handed over, so
                // nothing renders it again and nothing can re-send it.
                let token_hex = std::mem::take(&mut state.token).trim().to_owned();
                intents.push(DeviceIntent::Authorize {
                    peripheral_id: peripheral_id.clone(),
                    token_hex,
                });
            }
            if let Some(error) = state.error(DeviceOp::Authorize) {
                ui.label(error.message.clone());
            }
        }

        /* --------------------------------------------------- listing */

        if authorized {
            ui.separator();
            ui.heading("Recordings on the device");
            let can_list = probe.capabilities.iter().any(|capability| {
                capability.capability == DeviceCapability::ReadOnlyListing && capability.available
            });
            let can_transfer = probe.capabilities.iter().any(|capability| {
                capability.capability == DeviceCapability::ReadOnlyTransfer && capability.available
            });
            let list_label = if state.listing.is_some() {
                "Refresh the list"
            } else {
                "List recordings on the device"
            };
            if ui
                .add_enabled(
                    can_list && !state.is_pending(DeviceOp::List),
                    egui::Button::new(list_label),
                )
                .clicked()
            {
                intents.push(DeviceIntent::ListRecordings {
                    peripheral_id: peripheral_id.clone(),
                });
            }
            if !can_list {
                ui.label(
                    "Listing is not available for this device yet; see the capabilities above.",
                );
            }
            if let Some(error) = state.error(DeviceOp::List) {
                ui.label(error.message.clone());
            }

            if let Some(listing) = state.listing.clone() {
                let actions = transfer_list(
                    ui,
                    &listing,
                    can_transfer,
                    &state.chosen,
                    state.is_pending(DeviceOp::Disconnect),
                );
                for action in actions {
                    match action {
                        TransferAction::Toggle(id) => {
                            if !state.chosen.remove(&id) {
                                state.chosen.insert(id);
                            }
                        }
                        TransferAction::Cancel => intents.push(DeviceIntent::Disconnect {
                            peripheral_id: peripheral_id.clone(),
                            forget_authorization: false,
                        }),
                        TransferAction::Retry(id) => intents.push(DeviceIntent::Import {
                            peripheral_id: peripheral_id.clone(),
                            recording_ids: vec![id],
                        }),
                    }
                }

                if can_transfer {
                    let in_flight = listing
                        .items
                        .iter()
                        .any(|item| super::presentation::is_in_flight(item.transfer.state));
                    let ready = !state.chosen.is_empty()
                        && !in_flight
                        && !state.is_pending(DeviceOp::Import);
                    if ui
                        .add_enabled(
                            ready,
                            egui::Button::new("Import selected for local processing"),
                        )
                        .clicked()
                    {
                        intents.push(DeviceIntent::Import {
                            peripheral_id: peripheral_id.clone(),
                            recording_ids: state.chosen.iter().cloned().collect(),
                        });
                    }
                } else {
                    ui.label(format!(
                        "Transfer is not enabled for this device profile ({status_label})."
                    ));
                }
            }
            if let Some(error) = state.error(DeviceOp::Import) {
                ui.label(error.message.clone());
            }
        }
    }

    /* -------------------------------------------------------- session */

    ui.separator();
    if (state.listing.is_some() || authorized)
        && ui
            .add_enabled(
                !state.is_pending(DeviceOp::Disconnect),
                egui::Button::new("Disconnect"),
            )
            .clicked()
    {
        intents.push(DeviceIntent::Disconnect {
            peripheral_id: peripheral_id.clone(),
            forget_authorization: false,
        });
    }
    if authorized {
        if state.confirm_forget {
            ui.label(
                "Forget this device's authorization? The stored token is removed from the system keychain. Recordings already in your library are kept. You will need to authorize the device again to transfer from it.",
            );
            ui.horizontal(|ui| {
                if ui.button("Keep authorization").clicked() {
                    state.confirm_forget = false;
                }
                if ui.button("Forget authorization").clicked() {
                    intents.push(DeviceIntent::Disconnect {
                        peripheral_id: peripheral_id.clone(),
                        forget_authorization: true,
                    });
                }
            });
        } else if ui.button("Forget authorization…").clicked() {
            state.confirm_forget = true;
        }
    }
    if let Some(error) = state.error(DeviceOp::Disconnect) {
        ui.label(error.message.clone());
    }
}
