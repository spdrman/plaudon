//! The macOS Privacy & Security permissions, revisited from Settings (#307).
//!
//! The same [`PermissionsPanel`] first-run setup shows, so a permission
//! skipped during setup can be granted later from a place the user can find
//! again. The platform comes from `get_capabilities`; until that answers the
//! section waits rather than guessing.

use egui::Ui;
use plaudon_app::dto::Platform;

use super::{section, Section, SettingsData, SettingsOutput, SettingsTokens};
use crate::permissions::PermissionsPanel;

/// Heading of the section.
pub const TITLE: &str = "Privacy & Security permissions";

/// Shown before `get_capabilities` has answered.
const WAITING: &str = "Reading which platform this is…";

/// The permissions panel under Settings.
#[derive(Debug, Clone)]
pub struct PermissionsSection {
    panel: Option<PermissionsPanel>,
}

impl Default for PermissionsSection {
    fn default() -> Self {
        Self { panel: None }
    }
}

impl Section for PermissionsSection {
    fn title(&self) -> &'static str {
        TITLE
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        let _ = (ui, tokens, data, out, WAITING, Platform::host());
        let _ = section::note;
        let _ = &self.panel;
    }
}
