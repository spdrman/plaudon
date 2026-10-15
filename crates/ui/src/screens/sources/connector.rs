//! The connector diagnostics panel (K1.5, §4.2, §7.6).
//!
//! Rendered from one `plaud_probe` snapshot. The headline, the detail, the
//! warnings and the disconnect wording are the application's strings and are
//! rendered verbatim; this panel composes no sentence, and there is no vendor
//! text anywhere in the payload to forward.

use egui::Ui;
use plaudon_app::dto::PlaudConnectorSnapshot;

use crate::screens::devices::badge::badge_inline;

use super::presentation::{
    compatibility_label, next_action_control_in, offers_check_again, offers_disconnect,
    state_badge, state_label, usable_versions, ControlRun, NextActionControl,
};

/// What the panel asked the screen to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectorAction {
    /// Run one of the §5.4 corrective actions.
    Run(ControlRun),
    /// Open vendor documentation.
    OpenUrl(&'static str),
    /// Drop the association, with the wording the snapshot carries.
    Disconnect,
}

/// Render the panel.
pub fn connector_panel(
    ui: &mut Ui,
    snapshot: &PlaudConnectorSnapshot,
    busy: bool,
) -> Vec<ConnectorAction> {
    let mut actions = Vec::new();
    let diagnostics = &snapshot.diagnostics;

    ui.separator();
    ui.heading(diagnostics.headline.clone());
    ui.label(diagnostics.detail.clone());

    ui.horizontal(|ui| {
        ui.label("State:");
        badge_inline(
            ui,
            state_badge(diagnostics.state),
            state_label(diagnostics.state),
        );
    });
    ui.label(format!(
        "Tool version: {}",
        diagnostics
            .detected_version
            .as_deref()
            .unwrap_or("Not detected")
    ));
    ui.label(format!(
        "Compatibility: {}",
        compatibility_label(diagnostics.compatibility)
    ));
    ui.label(format!(
        "Found in: {}",
        diagnostics.program_origin.as_deref().unwrap_or("Not found")
    ));
    if let Some(code) = snapshot.status.last_error {
        ui.label(format!("Last error code: {code:?}"));
    }
    if snapshot.status.listing_possibly_incomplete {
        ui.label("The last listing may not include every recording in the account.");
    }
    if snapshot.status.scraping_warning {
        ui.label("The listing is read from the tool's on-screen table, so it can change with any release of that tool.");
    }

    for warning in &diagnostics.warnings {
        ui.label(format!("! {warning}"));
    }

    let versions = usable_versions(&diagnostics.next_action);
    if !versions.is_empty() {
        ui.label(format!("Tested versions: {}", versions.join(", ")));
    }

    ui.horizontal(|ui| {
        match next_action_control_in(diagnostics.state, &diagnostics.next_action) {
            Some(NextActionControl::Link { label, href })
                if ui.add_enabled(!busy, egui::Button::new(label)).clicked() =>
            {
                actions.push(ConnectorAction::OpenUrl(href));
            }
            Some(NextActionControl::Action { label, run })
                if ui.add_enabled(!busy, egui::Button::new(label)).clicked() =>
            {
                actions.push(ConnectorAction::Run(run));
            }
            Some(_) | None => {}
        }
        if offers_check_again(diagnostics.state)
            && ui
                .add_enabled(!busy, egui::Button::new("Check again"))
                .clicked()
        {
            actions.push(ConnectorAction::Run(ControlRun::Retry));
        }
    });

    if offers_disconnect(diagnostics.state) {
        ui.separator();
        ui.label(snapshot.disconnect.body.clone());
        if ui
            .add_enabled(!busy, egui::Button::new(snapshot.disconnect.label.clone()))
            .clicked()
        {
            actions.push(ConnectorAction::Disconnect);
        }
    }

    actions
}
