//! Storage facts and the operating-system permission inventory (row C4).
//!
//! Read-only on purpose: there is no command that moves the library, so the
//! section states that ([`STORAGE_LOCATION_FIXED`]) instead of offering a
//! control that would do nothing. The only action is revealing the folder.
//!
//! Every figure comes from [`storage_facts`], which renders an unmeasured
//! quantity as "unknown" and never as a zero; when `get_capabilities` has not
//! answered there are no figures at all and the section says
//! [`STORAGE_UNAVAILABLE`] rather than printing an empty list that would read
//! as "nothing is stored".

use egui::{RichText, Ui};
use plaudon_app::dto::PermissionState;
use plaudon_app::settings_presentation::{
    storage_facts, STORAGE_LOCATION_FIXED, STORAGE_UNAVAILABLE,
};

use super::{section, Section, SettingsData, SettingsIntent, SettingsOutput, SettingsTokens};

/// The only action the section offers.
const REVEAL: &str = "Show in file manager";

/// Heading of the permission inventory.
const PERMISSIONS_LEGEND: &str = "Operating-system permissions";

/// Shown when the capability answer carries no permissions at all.
const PERMISSIONS_EMPTY: &str = "This platform reports no permissions to grant.";

/// Glyph and word for a permission state, so it is never colour alone and a
/// permission that was never asked for is not reported as a refusal.
const fn permission_status(state: PermissionState) -> (&'static str, &'static str) {
    match state {
        PermissionState::Granted => ("✓", "Granted"),
        PermissionState::Denied => ("✕", "Denied"),
        PermissionState::NotRequested => ("–", "Not requested"),
    }
}

/// Read-only storage facts and permissions (matrix row C4).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StorageSection;

impl Section for StorageSection {
    fn title(&self) -> &'static str {
        "Storage and permissions"
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        let Some(capabilities) = data.capabilities.as_ref() else {
            section::note(ui, tokens, STORAGE_UNAVAILABLE);
            return;
        };

        section::facts(ui, tokens, &storage_facts(Some(&capabilities.storage)));
        section::note(ui, tokens, STORAGE_LOCATION_FIXED);
        if section::primary_button(ui, tokens, REVEAL).clicked() {
            out.push(SettingsIntent::RevealInFolder(
                capabilities.storage.library_path.clone(),
            ));
        }

        ui.add_space(8.0);
        ui.label(
            RichText::new(PERMISSIONS_LEGEND)
                .strong()
                .color(tokens.text()),
        );
        if capabilities.permissions.is_empty() {
            section::note(ui, tokens, PERMISSIONS_EMPTY);
        }
        for (permission, state) in &capabilities.permissions {
            let (glyph, word) = permission_status(*state);
            section::fact_row(ui, tokens, permission, &format!("{glyph} {word}"));
        }
    }
}
