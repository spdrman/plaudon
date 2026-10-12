//! What the updater can say without making a request (task N2.2).
//!
//! There is no check button here, and that is the design rather than an
//! omission: this build publishes no update-check command, so a button would
//! either do nothing or pretend to. The section says so, states the policy in
//! [`UpdateStatus::check_policy_copy`]'s words — which is also where the
//! Local-only suppression sentence comes from — and shows the pinned manifest
//! endpoint and the signing key the release is verified against, so the user
//! can see what *would* be contacted and what *would* be trusted.
//!
//! The only control is a copy button for the version string, which makes no
//! request at all.

use egui::Ui;
use plaudon_app::settings_presentation::{UpdateReadiness, UpdateStatus};

use super::intent::SettingsIntent;
use super::section;
use super::theme::{SettingsTokens, Tone};
use super::{Section, SettingsData, SettingsOutput};

/// Shown while `get_update_status` has not answered.
const NOT_READ_YET: &str = "Update status has not been read yet.";

/// Why there is no button that checks for an update.
const NO_CHECK_CONTROL: &str = "This build exposes no update-check command, so this section makes \
no network request and offers no control that would.";

/// The update state.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UpdateSection;

/// Badge for the installation root's readiness.
const fn readiness_badge(readiness: &UpdateReadiness) -> (Tone, &'static str, &'static str) {
    match readiness {
        UpdateReadiness::Ready { .. } => (Tone::Success, "✓", "Managed by the updater"),
        UpdateReadiness::NotInstalledByUpdater { .. } => {
            (Tone::Warning, "⚠", "Not installed by the updater")
        }
    }
}

impl Section for UpdateSection {
    fn title(&self) -> &'static str {
        "Updates"
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        let Some(status): Option<&UpdateStatus> = data.update.as_ref() else {
            section::prose(ui, tokens, NOT_READ_YET);
            return;
        };

        let (tone, glyph, label) = readiness_badge(&status.readiness);
        section::status_badge(ui, tokens, tone, glyph, label);
        let version = status.installed_version_copy();
        section::fact_row(ui, tokens, "Installed version", &version);
        if let UpdateReadiness::NotInstalledByUpdater { detail } = &status.readiness {
            // The updater's own explanation, verbatim.
            section::prose(ui, tokens, detail);
        }

        // Both branches — "only when you ask" and the Local-only suppression —
        // are the application's sentence, not one composed here.
        section::prose(ui, tokens, status.check_policy_copy());
        section::note(ui, tokens, NO_CHECK_CONTROL);

        section::fact_row(ui, tokens, "Update manifest", &status.manifest_endpoint);
        section::fact_row(ui, tokens, "Release signing key", &status.signing_key_id);

        if ui.button("Copy the version").clicked() {
            out.push(SettingsIntent::CopyText(version));
        }
    }
}
