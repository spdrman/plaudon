//! The recordings a device lists, each with its own transfer state (§4.2A.8).
//!
//! Labels are the DTO's `state_label`, which `plaudon-app` takes from
//! `TransferState::ui_label`. Bandwidth is observed and says so when it has not
//! been measured yet; the time remaining is labelled an estimate and is omitted
//! rather than invented. The size a bar is a fraction *of* is the length the
//! device declared, which is a claim and is named as one.

use std::collections::BTreeSet;

use egui::Ui;
use plaudon_app::dto::{DeviceListingDto, DeviceTransferState as TransferState};

use super::badge::badge_inline;
use super::presentation::{
    bandwidth_label, bytes_label, cloud_label, completeness_badge, eta_label, is_cancellable,
    is_retryable, progress_fraction, transfer_badge,
};

/// What the user did in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferAction {
    /// Select or deselect one recording for import.
    Toggle(String),
    /// Stop everything that is in flight by ending the session.
    Cancel,
    /// Ask for one stopped recording again.
    Retry(String),
}

/// Render the listing. `chosen` is the user's current selection.
pub fn transfer_list(
    ui: &mut Ui,
    listing: &DeviceListingDto,
    selectable: bool,
    chosen: &BTreeSet<String>,
    cancel_pending: bool,
) -> Vec<TransferAction> {
    let mut actions = Vec::new();

    ui.horizontal(|ui| {
        badge_inline(
            ui,
            completeness_badge(listing.completeness),
            &listing.completeness_label,
        );
    });

    if listing.items.is_empty() {
        ui.label("The device lists no recordings right now.");
        return actions;
    }

    if listing
        .items
        .iter()
        .any(|item| is_cancellable(item.transfer.state))
    {
        ui.label(
            "Cancelling ends this session with the device. Recordings already in your library are kept, and a recording that was only part-way through is not added.",
        );
    }

    for item in &listing.items {
        let transfer = &item.transfer;
        ui.group(|ui| {
            ui.horizontal(|ui| {
                if selectable && transfer.state == TransferState::Discovered {
                    let mut selected = chosen.contains(&item.recording_id);
                    if ui.checkbox(&mut selected, item.display_name.clone()).changed() {
                        actions.push(TransferAction::Toggle(item.recording_id.clone()));
                    }
                } else {
                    ui.label(item.display_name.clone());
                }
                badge_inline(
                    ui,
                    transfer_badge(transfer.state),
                    &transfer.state_label,
                );
            });
            ui.label(format!(
                "Declared by the device: {}",
                bytes_label(item.declared_byte_length)
            ));
            if let Some(captured) = item.captured_at.as_deref() {
                ui.label(format!("Captured {captured}"));
            }
            ui.label(format!(
                "Source: {} · {}",
                transfer.source_label,
                cloud_label(transfer.cloud_route)
            ));
            if transfer.verified {
                ui.label("Bytes verified before they were kept.");
            }

            if transfer.state == TransferState::Receiving {
                match progress_fraction(transfer.bytes_received, item.declared_byte_length) {
                    Some(fraction) => {
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .text(format!(
                                    "{} of the size the device declared",
                                    bytes_label(transfer.bytes_received.unwrap_or(0))
                                ))
                                .desired_width(320.0),
                        );
                    }
                    None => {
                        ui.label("Transferring. How much of the recording has arrived is not known yet.");
                    }
                }
                ui.label(format!(
                    "Observed bandwidth: {}",
                    bandwidth_label(transfer.observed_bytes_per_sec)
                ));
                ui.label(format!(
                    "Estimated time remaining (an estimate, not a promise): {}",
                    eta_label(transfer.eta_seconds_estimate)
                ));
            }

            if transfer.state == TransferState::Interrupted {
                ui.label("The link dropped before this recording finished. Nothing partial was added to your library; the bytes that did arrive are staged on this computer and are verified before anything is kept.");
            }
            if transfer.state == TransferState::WaitingForDevice {
                ui.label("The device is out of range or busy. Bring it closer; nothing is retried behind your back.");
            }
            if transfer.state == TransferState::Unsupported {
                ui.label("This recording's profile, codec or authentication mode is not one this build can read, so there is nothing to retry.");
            }

            ui.horizontal(|ui| {
                if is_cancellable(transfer.state)
                    && ui
                        .add_enabled(
                            !cancel_pending,
                            egui::Button::new(format!("Cancel transfer of {}", item.display_name)),
                        )
                        .clicked()
                {
                    actions.push(TransferAction::Cancel);
                }
                if selectable
                    && is_retryable(transfer.state)
                    && ui
                        .button(format!("Try {} again", item.display_name))
                        .clicked()
                {
                    actions.push(TransferAction::Retry(item.recording_id.clone()));
                }
            });
        });
    }

    actions
}
