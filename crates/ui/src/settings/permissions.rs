//! The macOS Privacy & Security permissions, revisited from Settings (#307).
//!
//! The same [`PermissionsPanel`] first-run setup shows, so a permission
//! skipped during setup can be granted later from a place the user can find
//! again. The platform comes from `get_capabilities`; until that answers the
//! section waits rather than guessing.

use egui::Ui;

use super::{section, Section, SettingsData, SettingsOutput, SettingsTokens};
use crate::permissions::PermissionsPanel;

/// Heading of the section.
pub const TITLE: &str = "Privacy & Security permissions";

/// Shown before `get_capabilities` has answered.
const WAITING: &str = "Reading which platform this is…";

/// The permissions panel under Settings.
#[derive(Debug, Clone, Default)]
pub struct PermissionsSection {
    panel: Option<PermissionsPanel>,
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
        let Some(capabilities) = &data.capabilities else {
            section::note(ui, tokens, WAITING);
            return;
        };
        let platform = capabilities.os.platform;
        let panel = match &mut self.panel {
            Some(panel) if panel.platform() == platform => panel,
            slot => slot.insert(PermissionsPanel::new(platform)),
        };
        panel.ui(ui, tokens, &data.os_permissions, out);
    }
}
