//! Installing what an engine needs, from first-run step 3 and from Settings
//! (issues #303 and #305).
//!
//! One panel, rendered by both screens, so the two can never offer different
//! actions. It renders data and raises commands; it fetches nothing itself.
//! Three rules shape it:
//!
//! * **Say what would be contacted before the click.** Every catalogue model
//!   shows the one host and the one file a download would fetch, and the
//!   domain that host may hand the transfer to. The PLAUD CLI install names
//!   the registry and the exact package.
//! * **Approve the exact action.** The first click on a download or an
//!   install opens an approval naming that host and file; only the second
//!   click raises the command, and the command carries what was approved, so
//!   the backend can refuse anything else.
//! * **Local-only Mode is never worked around.** While it is on, the
//!   download and install controls are off and say why, and the offline
//!   import stays available, because it reaches no network.

use egui::{ProgressBar, RichText, Ui};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    CancelModelInstallRequest, CatalogueOfferDto, EngineInventoryEntry, ImportModelFileRequest,
    InstallModelDownloadRequest, InstallPlaudCliRequest, PlaudConnectRequest, PlaudToolchainStatus,
    ToolPresence, PLAUD_CONNECT_INTENT,
};
use plaudon_app::settings_presentation::format_bytes;
use plaudon_domain::error::ErrorEnvelope;
use std::fmt::Write as _;

use super::intent::{FilePurpose, SettingsIntent};
use super::section;
use super::theme::{SettingsTokens, Tone};
use super::{PickedPath, SettingsOutput};

/// The import control's name, which is also what it does.
pub const IMPORT_LABEL: &str = "Import a model file I already have";

/// What the import does, said beside it.
const IMPORT_NOTE: &str = "Works offline: the file is copied in only if its size and SHA-256 \
match a model in the catalogue, and your own file is left untouched.";

/// Why the download controls are off.
const LOCAL_ONLY_DOWNLOADS: &str = "Local-only mode is on, so Plaudon will not download \
anything. Turn Local-only mode off to download a model, or import a model file you already have.";

/// Why the install control is off.
const LOCAL_ONLY_INSTALL: &str = "Local-only mode is on, so Plaudon will not run the installer \
or the sign-in. You can still copy the commands and run them yourself.";

/// Heading of the PLAUD CLI block.
const CLI_HEADING: &str = "PLAUD Cloud CLI";

/// Shown until detection answers.
const CLI_CHECKING: &str = "Checking this machine for Node.js, npm and the PLAUD CLI…";

/// The registry the guided install names, which is the only host npm is told
/// to use.
const NPM_REGISTRY_HOST: &str = "registry.npmjs.org";

/// The package the guided install approves.
const PLAUD_CLI_PACKAGE: &str = "@plaud-ai/cli@0.3.15";

/// Engine setup state shared by step 3 and Settings › Engines.
#[derive(Debug, Default)]
pub struct EngineSetup {
    /// The last detection answer.
    toolchain: Option<PlaudToolchainStatus>,
    /// Whether detection has been asked for.
    toolchain_requested: bool,
    /// Model whose download is awaiting the user's approval.
    confirm_download: Option<String>,
    /// Whether the guided CLI install is awaiting approval.
    confirm_install: bool,
    /// The download running now: model id and display name.
    downloading: Option<(String, String)>,
    /// Bytes received and expected, when the host has reported them.
    progress: Option<(u64, u64)>,
    /// A command without measurable progress that is running now.
    running: Option<Running>,
    /// The last outcome, in the backend's words where it has any.
    message: Option<(Tone, String)>,
}

/// A command the panel is waiting on that reports no progress of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Running {
    /// An import is being checked and copied.
    Import,
    /// The guided PLAUD CLI install is running.
    CliInstall,
}

/// Glyph for a tone, so an outcome is never colour alone.
const fn glyph(tone: Tone) -> &'static str {
    match tone {
        Tone::Neutral => "•",
        Tone::Success => "✓",
        Tone::Warning => "⚠",
        Tone::Danger => "✕",
    }
}

/// A button that is either live or visibly off.
fn button(ui: &mut Ui, tokens: &dyn SettingsTokens, label: &str, enabled: bool) -> bool {
    let mut clicked = false;
    ui.add_enabled_ui(enabled, |ui| {
        clicked = section::primary_button(ui, tokens, label).clicked();
    });
    clicked && enabled
}

/// "Node.js: found at …" / "Node.js: not found".
fn presence_line(name: &str, presence: &ToolPresence) -> String {
    match &presence.path {
        Some(path) => format!("{name}: found at {path}"),
        None => format!("{name}: not found"),
    }
}

/// The sentence naming every host a set of offers would contact.
fn host_disclosure(offers: &[CatalogueOfferDto]) -> Option<String> {
    let first = offers.first()?;
    let mut sentence = format!(
        "Downloading contacts {} only, for the one file you choose",
        first.host
    );
    if let Some(suffix) = &first.delivery_host_suffix {
        let _ = write!(
            sentence,
            ", and {} may hand the transfer to a server under {suffix}",
            first.host
        );
    }
    sentence.push_str(
        ". Nothing about you or your recordings is sent. Every file is checked against its \
         pinned SHA-256 before it is installed, and removed if it does not match.",
    );
    Some(sentence)
}

impl EngineSetup {
    /// Report download progress from a `model.install_progress` event.
    ///
    /// Until this is called a running download shows an indeterminate bar:
    /// a percentage nobody measured is a lie.
    pub fn install_progress(&mut self, received_bytes: u64, total_bytes: u64) {
        if total_bytes == 0 || self.downloading.is_none() {
            return;
        }
        self.progress = Some((received_bytes.min(total_bytes), total_bytes));
    }

    /// The install and import controls for one engine.
    ///
    /// An engine with nothing in the catalogue gets none: an engine that is
    /// genuinely unavailable keeps its honest explanation and no control
    /// that would pretend otherwise.
    pub fn engine_actions(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        engine: &EngineInventoryEntry,
        local_only: bool,
        out: &mut SettingsOutput,
    ) {
        if engine.catalogue.is_empty() {
            return;
        }
        let busy = self.downloading.is_some() || self.running == Some(Running::Import);
        for offer in &engine.catalogue {
            self.offer_row(ui, tokens, offer, local_only, busy, out);
        }
        if let Some(disclosure) = host_disclosure(&engine.catalogue) {
            section::note(ui, tokens, &disclosure);
        }
        if local_only {
            section::note(ui, tokens, LOCAL_ONLY_DOWNLOADS);
        }

        if let Some((model_id, name)) = self.downloading.clone() {
            if engine
                .catalogue
                .iter()
                .any(|offer| offer.model_id == model_id)
            {
                self.download_progress(ui, tokens, &model_id, &name, out);
            }
        }

        ui.add_space(4.0);
        if ui
            .add_enabled(!busy, egui::Button::new(IMPORT_LABEL))
            .clicked()
            && !busy
        {
            out.push(SettingsIntent::PickFile(FilePurpose::ModelFile));
        }
        section::note(ui, tokens, IMPORT_NOTE);
        if self.running == Some(Running::Import) {
            section::note(ui, tokens, "Checking and copying the model file…");
        }
        if let Some((tone, text)) = &self.message {
            section::status_badge(ui, tokens, *tone, glyph(*tone), text);
        }
        ui.add_space(8.0);
    }

    fn offer_row(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        offer: &CatalogueOfferDto,
        local_only: bool,
        busy: bool,
        out: &mut SettingsOutput,
    ) {
        ui.label(
            RichText::new(format!(
                "{} — {} · {}",
                offer.display_name,
                format_bytes(Some(offer.size_bytes)),
                offer.licence_name
            ))
            .color(tokens.text()),
        );
        section::note(
            ui,
            tokens,
            &format!("Fetched from {}{}", offer.host, offer.file),
        );
        if offer.installed {
            section::status_badge(ui, tokens, Tone::Success, "✓", "Installed");
            return;
        }
        let label = format!("Download {}", offer.display_name);
        if button(ui, tokens, &label, !local_only && !busy) {
            self.confirm_download = Some(offer.model_id.clone());
            self.message = None;
        }
        if self.confirm_download.as_deref() == Some(offer.model_id.as_str()) {
            self.download_approval(ui, tokens, offer, local_only, out);
        }
    }

    /// The approval for one download, naming exactly what it contacts.
    fn download_approval(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        offer: &CatalogueOfferDto,
        local_only: bool,
        out: &mut SettingsOutput,
    ) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(
                RichText::new("Approve this download?")
                    .strong()
                    .color(tokens.text()),
            );
            let mut text = format!(
                "Plaudon will contact {} and fetch {} ({}).",
                offer.host,
                offer.file,
                format_bytes(Some(offer.size_bytes))
            );
            if let Some(suffix) = &offer.delivery_host_suffix {
                let _ = write!(
                    text,
                    " {} may redirect the transfer to a server under {suffix}; nowhere else is \
                     followed.",
                    offer.host
                );
            }
            let _ = write!(
                text,
                " Licence: {} ({}). It needs about {} of disk, and {} of memory is recommended. \
                 This approval covers this one file and ends when the download does.",
                offer.licence_name,
                offer.licence_spdx_id,
                format_bytes(Some(offer.size_bytes + offer.size_bytes / 10)),
                format_bytes(Some(offer.recommended_ram_bytes))
            );
            section::prose(ui, tokens, &text);
            ui.horizontal_wrapped(|ui| {
                if button(ui, tokens, "Approve this download", !local_only) {
                    self.confirm_download = None;
                    self.downloading = Some((offer.model_id.clone(), offer.display_name.clone()));
                    self.progress = None;
                    out.command(AppCommand::InstallModelDownload(
                        InstallModelDownloadRequest {
                            model_id: offer.model_id.clone(),
                            licence_accepted_spdx_id: offer.licence_spdx_id.clone(),
                            footprint_acknowledged: true,
                            approved_host: offer.host.clone(),
                            approved_file: offer.file.clone(),
                            keep_offline: false,
                        },
                    ));
                }
                if ui.button("Don't download").clicked() {
                    self.confirm_download = None;
                }
            });
        });
    }

    fn download_progress(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        model_id: &str,
        name: &str,
        out: &mut SettingsOutput,
    ) {
        section::prose(ui, tokens, &format!("Downloading {name}…"));
        if let Some((received, total)) = self.progress {
            let fraction = received as f32 / total as f32;
            ui.add(ProgressBar::new(fraction).show_percentage());
            section::note(
                ui,
                tokens,
                &format!(
                    "{} of {}",
                    format_bytes(Some(received)),
                    format_bytes(Some(total))
                ),
            );
        } else {
            // Nothing measured yet: an empty bar and the words, never an
            // invented percentage, and no animation that would repaint
            // forever.
            ui.add(ProgressBar::new(0.0));
            section::note(ui, tokens, "Waiting for the first bytes…");
        }
        if ui.button("Cancel download").clicked() {
            out.command(AppCommand::CancelModelInstall(CancelModelInstallRequest {
                model_id: model_id.to_owned(),
            }));
        }
    }

    /// The PLAUD Cloud CLI block: what is missing, the copyable commands, the
    /// guided install and the sign-in.
    pub fn plaud_cli(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        local_only: bool,
        out: &mut SettingsOutput,
    ) {
        if !self.toolchain_requested {
            self.toolchain_requested = true;
            out.command(AppCommand::DetectPlaudToolchain);
        }
        ui.label(RichText::new(CLI_HEADING).strong().color(tokens.text()));
        let Some(status) = self.toolchain.clone() else {
            section::note(ui, tokens, CLI_CHECKING);
            return;
        };
        section::prose(ui, tokens, &status.summary);
        for (name, presence) in [
            ("Node.js", &status.node),
            ("npm", &status.npm),
            ("PLAUD CLI", &status.plaud),
        ] {
            section::note(ui, tokens, &presence_line(name, presence));
        }
        section::note(
            ui,
            tokens,
            &format!(
                "This build has exercised @plaud-ai/cli {}.",
                status.exercised_release
            ),
        );

        ui.add_space(4.0);
        section::note(
            ui,
            tokens,
            "Install command, to copy and run yourself if you prefer:",
        );
        ui.label(
            RichText::new(&status.install_command)
                .monospace()
                .color(tokens.text()),
        );
        if ui.button("Copy install command").clicked() {
            out.push(SettingsIntent::CopyText(status.install_command.clone()));
        }

        let plaud_missing = status.plaud.path.is_none();
        if plaud_missing {
            section::note(
                ui,
                tokens,
                &format!(
                    "The guided install runs npm, which contacts {NPM_REGISTRY_HOST} for \
                     {PLAUD_CLI_PACKAGE} and the packages it depends on, and nothing else."
                ),
            );
            let can_install =
                !local_only && status.guided_install_available && self.running.is_none();
            if button(ui, tokens, "Install the PLAUD CLI", can_install) {
                self.confirm_install = true;
            }
            if !status.guided_install_available {
                section::note(
                    ui,
                    tokens,
                    "The guided install needs npm, which was not found. Install Node.js (it \
                     includes npm), then check again.",
                );
            }
            if self.confirm_install && can_install {
                self.install_approval(ui, tokens, out);
            }
            if self.running == Some(Running::CliInstall) {
                section::note(ui, tokens, "Installing the PLAUD CLI with npm…");
            }
        }

        ui.add_space(4.0);
        section::note(
            ui,
            tokens,
            "Sign-in command, to copy and run yourself if you prefer:",
        );
        ui.label(
            RichText::new(&status.login_command)
                .monospace()
                .color(tokens.text()),
        );
        if ui.button("Copy sign-in command").clicked() {
            out.push(SettingsIntent::CopyText(status.login_command.clone()));
        }
        // The existing hardened launcher: `plaud_connect` discovers the CLI
        // and hands authentication to it, with no shell in between.
        if button(
            ui,
            tokens,
            "Sign in to PLAUD",
            !local_only && !plaud_missing,
        ) {
            out.command(AppCommand::PlaudConnect(PlaudConnectRequest {
                user_intent: PLAUD_CONNECT_INTENT.to_owned(),
            }));
        }
        if local_only {
            section::note(ui, tokens, LOCAL_ONLY_INSTALL);
        }
        if ui.button("Check again").clicked() {
            out.command(AppCommand::DetectPlaudToolchain);
        }
    }

    fn install_approval(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        out: &mut SettingsOutput,
    ) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(
                RichText::new("Approve this install?")
                    .strong()
                    .color(tokens.text()),
            );
            section::prose(
                ui,
                tokens,
                &format!(
                    "Plaudon will run npm to install {PLAUD_CLI_PACKAGE} globally from \
                     {NPM_REGISTRY_HOST}. This approval covers this one package and ends when \
                     the install does."
                ),
            );
            ui.horizontal_wrapped(|ui| {
                if section::primary_button(ui, tokens, "Approve this install").clicked() {
                    self.confirm_install = false;
                    self.running = Some(Running::CliInstall);
                    self.message = None;
                    out.command(AppCommand::InstallPlaudCli(InstallPlaudCliRequest {
                        approved_host: NPM_REGISTRY_HOST.to_owned(),
                        approved_package: PLAUD_CLI_PACKAGE.to_owned(),
                    }));
                }
                if ui.button("Don't install").clicked() {
                    self.confirm_install = false;
                }
            });
        });
    }

    /// Receive the path a file dialog this panel opened returned.
    pub fn path_picked(&mut self, picked: PickedPath, path: &str, out: &mut SettingsOutput) {
        if picked != PickedPath::File(FilePurpose::ModelFile) {
            return;
        }
        self.running = Some(Running::Import);
        self.message = None;
        out.command(AppCommand::ImportModelFile(ImportModelFileRequest {
            path: path.to_owned(),
            model_id: None,
        }));
    }

    /// Fold a command outcome into the panel. Returns whether a model was
    /// installed, so the caller re-reads the engine inventory.
    pub fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
    ) -> bool {
        match (command, result) {
            ("detect_plaud_toolchain", Ok(AppResponse::DetectPlaudToolchain(status)))
            | ("install_plaud_cli", Ok(AppResponse::InstallPlaudCli(status))) => {
                if command == "install_plaud_cli" {
                    self.running = None;
                    self.message = Some((Tone::Success, "The PLAUD CLI is installed.".to_owned()));
                }
                self.toolchain = Some(status.clone());
                false
            }
            ("install_plaud_cli", Err(envelope)) => {
                self.running = None;
                self.message = Some((Tone::Danger, envelope.message.clone()));
                false
            }
            ("install_model_download", Ok(AppResponse::InstallModelDownload(response)))
            | ("import_model_file", Ok(AppResponse::ImportModelFile(response))) => {
                self.downloading = None;
                self.progress = None;
                self.running = None;
                self.message = Some((
                    Tone::Success,
                    format!("{} is installed.", response.model.display_name),
                ));
                true
            }
            ("install_model_download" | "import_model_file", Err(envelope)) => {
                self.downloading = None;
                self.progress = None;
                self.running = None;
                self.message = Some((Tone::Danger, envelope.message.clone()));
                false
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_ignored_when_nothing_is_downloading() {
        let mut setup = EngineSetup::default();
        setup.install_progress(10, 100);
        assert_eq!(setup.progress, None);
    }

    #[test]
    fn the_host_disclosure_names_the_host_and_the_delivery_domain() {
        let offer = CatalogueOfferDto {
            model_id: "m".into(),
            display_name: "M".into(),
            size_bytes: 1,
            licence_spdx_id: "MIT".into(),
            licence_name: "MIT License".into(),
            host: "huggingface.co".into(),
            file: "/f.bin".into(),
            delivery_host_suffix: Some("hf.co".into()),
            recommended_ram_bytes: 1,
            installed: false,
        };
        let text = host_disclosure(&[offer]).expect("a disclosure");
        assert!(text.contains("contacts huggingface.co only"), "{text}");
        assert!(text.contains("under hf.co"), "{text}");
        assert!(host_disclosure(&[]).is_none());
    }
}
