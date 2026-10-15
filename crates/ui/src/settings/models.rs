//! Local models: what is installed, and the approval gate before one more is
//! (matrix row A13).
//!
//! Three obligations shape this section.
//!
//! * **Nothing is installed without the two disclosures.** The licence
//!   identifier and the footprint acknowledgement are fields of
//!   `install_model_from_pack`, and the command refuses without them, so the
//!   gate here is the same gate the installer enforces — not a decoration in
//!   front of it. Every reason an install cannot proceed is listed at once,
//!   in [`InstallBlocker::explanation`]'s words.
//! * **An unmeasured quantity is never a zero.** Free disk and total memory
//!   arrive as `Option`; [`format_bytes`] renders `None` as "unknown", and an
//!   unmeasured figure never *refuses* an install either — the installer's own
//!   preflight is the authority, and the section says that is what will check
//!   it.
//! * **Progress is never invented.** An install with no byte figure shows an
//!   indeterminate bar. It becomes determinate only when the host has called
//!   [`LocalModelsSection::install_progress`] with real numbers.

use std::collections::BTreeMap;

use egui::{ProgressBar, RichText, Ui};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    InspectModelPackRequest, InspectModelPackResponse, InstallModelFromPackRequest, LocalModelDto,
    ModelOriginDto, RemoveLocalModelRequest, VerifyLocalModelRequest,
};
use plaudon_app::settings_presentation::format_bytes;
use plaudon_domain::error::ErrorEnvelope;

use super::intent::{FolderPurpose, SettingsIntent};
use super::section::{self, Confirmation};
use super::theme::{SettingsTokens, Tone};
use super::{PickedPath, Section, SettingsData, SettingsOutput};

/// Shown while `list_local_models` has not answered.
const NOT_READ_YET: &str = "The model inventory has not been read yet.";

/// Shown when the model root holds nothing.
const NO_MODELS: &str = "No models are installed on this machine.";

/// What removing a model costs, shown in the confirmation.
const REMOVE_DISCLOSURE: &str = "The weights are deleted from the model root. Anything that needs \
this model stops working until you install it again. Recordings, transcripts and summaries are \
not touched.";

/// Heading of the install flow.
const INSTALL_HEADING: &str = "Install a model from an offline pack";

/// Said when the machine is under the model's recommended memory.
const BELOW_RECOMMENDED: &str = "This machine is below the memory this model recommends. The \
install is not refused for that reason; expect it to run slowly.";

/// Said when disk or memory could not be measured.
const UNMEASURED: &str = "A figure this machine did not report is shown as unknown and is not \
checked here. The installer re-checks disk and memory before it writes anything.";

/// Whether the two disclosures of one offer have been accepted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Approval {
    /// The licence was accepted, by SPDX identifier.
    licence: bool,
    /// The disk and memory footprint was acknowledged.
    footprint: bool,
}

/// The local model manager.
#[derive(Debug, Default)]
pub struct LocalModelsSection {
    /// The last `inspect_model_pack` answer, which installs nothing by itself.
    pack: Option<InspectModelPackResponse>,
    /// Per-offer approvals, keyed by model id.
    approvals: BTreeMap<String, Approval>,
    /// Model whose removal is awaiting confirmation.
    confirm_remove: Option<String>,
    /// Display name of the model currently being installed.
    installing: Option<String>,
    /// Bytes received and expected, when the host has reported them.
    progress: Option<(u64, u64)>,
    /// The last outcome, in the backend's words where it has any.
    message: Option<(Tone, String)>,
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

impl LocalModelsSection {
    /// Report install progress from a `model.install_progress` event.
    ///
    /// Until this is called the bar is indeterminate, because a percentage
    /// nobody measured is a lie. `total_bytes == 0` is not a measurement, so
    /// it leaves the bar as it was.
    pub fn install_progress(&mut self, received_bytes: u64, total_bytes: u64) {
        if total_bytes == 0 {
            return;
        }
        self.progress = Some((received_bytes.min(total_bytes), total_bytes));
    }

    /// The pack the user is deciding about, when one has been inspected.
    #[must_use]
    pub const fn pack(&self) -> Option<&InspectModelPackResponse> {
        self.pack.as_ref()
    }

    /// Render one installed model, and its removal confirmation when it is the
    /// one awaiting confirmation.
    fn installed_row(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        model: &LocalModelDto,
        out: &mut SettingsOutput,
    ) {
        ui.add_space(6.0);
        ui.label(
            RichText::new(&model.display_name)
                .strong()
                .color(tokens.text()),
        );
        section::fact_row(ui, tokens, "Engine", &model.engine);
        section::fact_row(ui, tokens, "Version", &model.version);
        section::fact_row(ui, tokens, "Size", &format_bytes(Some(model.size_bytes)));
        section::fact_row(ui, tokens, "Licence", &model.licence_spdx_id);
        section::fact_row(
            ui,
            tokens,
            "Origin",
            match model.origin {
                ModelOriginDto::Pack => "Offline pack",
                ModelOriginDto::Download => "Download",
                ModelOriginDto::ImportedFile => "Imported file",
            },
        );
        section::fact_row(
            ui,
            tokens,
            "Kept for offline use",
            if model.keep_offline { "Yes" } else { "No" },
        );
        ui.horizontal_wrapped(|ui| {
            if ui
                .button(format!("Verify {}", model.display_name))
                .clicked()
            {
                out.command(AppCommand::VerifyLocalModel(VerifyLocalModelRequest {
                    model_id: model.model_id.clone(),
                }));
            }
            if section::danger_button(ui, tokens, &format!("Remove {}", model.display_name))
                .clicked()
            {
                self.confirm_remove = Some(model.model_id.clone());
            }
        });

        if self.confirm_remove.as_deref() == Some(model.model_id.as_str()) {
            match section::confirm_modal(
                ui,
                tokens,
                &format!("Remove {}?", model.display_name),
                REMOVE_DISCLOSURE,
                "Remove this model",
            ) {
                Confirmation::Confirmed => {
                    self.confirm_remove = None;
                    out.command(AppCommand::RemoveLocalModel(RemoveLocalModelRequest {
                        model_id: model.model_id.clone(),
                        confirmed: true,
                    }));
                }
                Confirmation::Cancelled => self.confirm_remove = None,
                Confirmation::Pending => {}
            }
        }
    }
}

/// Render the offers of an inspected pack.
///
/// Returns the display name of an install the user just started, so the caller
/// can record it without this function borrowing the whole section.
fn offers_ui(
    ui: &mut Ui,
    tokens: &dyn SettingsTokens,
    pack: &InspectModelPackResponse,
    approvals: &mut BTreeMap<String, Approval>,
    out: &mut SettingsOutput,
) -> Option<String> {
    let mut started = None;
    section::fact_row(ui, tokens, "Pack", &pack.pack_path);
    section::fact_row(ui, tokens, "Manifest digest", &pack.artifact_digest);
    if pack.available_disk_bytes.is_none() || pack.total_ram_bytes.is_none() {
        section::note(ui, tokens, UNMEASURED);
    }
    section::fact_row(
        ui,
        tokens,
        "Free disk on this volume",
        &format_bytes(pack.available_disk_bytes),
    );
    section::fact_row(
        ui,
        tokens,
        "Memory on this machine",
        &format_bytes(pack.total_ram_bytes),
    );

    if pack.offers.is_empty() {
        section::prose(
            ui,
            tokens,
            "This pack offers no model this build can install.",
        );
        return None;
    }

    for offer in &pack.offers {
        let gate = &offer.gate;
        ui.add_space(8.0);
        ui.label(
            RichText::new(&gate.display_name)
                .strong()
                .color(tokens.text()),
        );
        section::fact_row(ui, tokens, "Engine", &offer.engine);
        section::fact_row(ui, tokens, "Version", &offer.version);
        section::fact_row(
            ui,
            tokens,
            "Languages",
            &if offer.languages.is_empty() {
                "not stated".to_owned()
            } else {
                offer.languages.join(", ")
            },
        );

        section::fact_row(
            ui,
            tokens,
            "Licence",
            &format!("{} ({})", gate.licence.name, gate.licence.spdx_id),
        );
        // The manifest's own sentence about redistribution.
        section::note(ui, tokens, &gate.licence.redistribution_note);
        section::fact_row(ui, tokens, "Licence text", &gate.licence.text_file);

        section::fact_row(
            ui,
            tokens,
            "Size on disk",
            &format_bytes(Some(gate.footprint.size_bytes)),
        );
        section::fact_row(
            ui,
            tokens,
            "Needs, including headroom",
            &format_bytes(Some(gate.footprint.required_bytes)),
        );
        section::fact_row(
            ui,
            tokens,
            "Minimum memory",
            &format_bytes(Some(gate.footprint.min_ram_bytes)),
        );
        section::fact_row(
            ui,
            tokens,
            "Recommended memory",
            &format_bytes(Some(gate.footprint.recommended_ram_bytes)),
        );
        // The manifest's own hardware note.
        section::note(ui, tokens, &gate.footprint.notes);
        if gate.below_recommended_memory {
            section::status_badge(ui, tokens, Tone::Warning, "⚠", BELOW_RECOMMENDED);
        }

        let approval = approvals.entry(gate.model_id.clone()).or_default();
        let mut licence = approval.licence;
        if ui
            .checkbox(
                &mut licence,
                format!("I accept the {} licence", gate.licence.spdx_id),
            )
            .changed()
        {
            approval.licence = licence;
        }
        let mut footprint = approval.footprint;
        if ui
            .checkbox(
                &mut footprint,
                format!(
                    "I understand this needs {} of disk",
                    format_bytes(Some(gate.footprint.required_bytes))
                ),
            )
            .changed()
        {
            approval.footprint = footprint;
        }
        let approval = *approval;

        // An unmeasured figure cannot refuse an install: `u64::MAX` stands for
        // "not checked here", and the note above says the installer checks it.
        let blockers = gate.blockers(
            approval.licence,
            pack.available_disk_bytes.unwrap_or(u64::MAX),
            pack.total_ram_bytes.unwrap_or(u64::MAX),
            offer.already_installed,
        );
        for blocker in &blockers {
            section::status_badge(ui, tokens, Tone::Danger, "✕", &blocker.explanation());
        }

        let enabled = blockers.is_empty() && approval.footprint;
        if !enabled && blockers.is_empty() {
            section::note(
                ui,
                tokens,
                "Acknowledge the disk and memory footprint before installing.",
            );
        }
        let label = format!("Install {}", gate.display_name);
        ui.add_enabled_ui(enabled, |ui| {
            if section::primary_button(ui, tokens, &label).clicked() {
                started = Some(gate.display_name.clone());
                out.command(AppCommand::InstallModelFromPack(
                    InstallModelFromPackRequest {
                        pack_path: pack.pack_path.clone(),
                        model_id: gate.model_id.clone(),
                        licence_accepted_spdx_id: gate.licence.spdx_id.clone(),
                        footprint_acknowledged: true,
                        keep_offline: false,
                    },
                ));
            }
        });
    }
    started
}

impl Section for LocalModelsSection {
    fn title(&self) -> &'static str {
        "Local models"
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        let Some(models) = data.local_models.as_ref() else {
            section::prose(ui, tokens, NOT_READ_YET);
            return;
        };

        section::fact_row(ui, tokens, "Model root", &models.model_root);
        if ui
            .button("Show the model folder in the file manager")
            .clicked()
        {
            out.push(SettingsIntent::RevealInFolder(models.model_root.clone()));
        }
        section::fact_row(
            ui,
            tokens,
            "Installed weights",
            &format_bytes(Some(models.disk.installed_bytes)),
        );
        section::fact_row(
            ui,
            tokens,
            "Left in staging",
            &format_bytes(Some(models.disk.staging_bytes)),
        );
        section::fact_row(
            ui,
            tokens,
            "Free disk on this volume",
            &format_bytes(models.available_disk_bytes),
        );
        section::fact_row(
            ui,
            tokens,
            "Memory on this machine",
            &format_bytes(models.total_ram_bytes),
        );
        // The backend's own sentence about where models come from
        // (`settings_presentation::MODEL_SOURCE_NOTE`).
        section::note(ui, tokens, &models.source_note);

        if let Some((tone, message)) = self.message.clone() {
            section::status_badge(ui, tokens, tone, glyph(tone), &message);
        }

        ui.add_space(8.0);
        if models.models.is_empty() {
            section::prose(ui, tokens, NO_MODELS);
        } else {
            for model in &models.models {
                self.installed_row(ui, tokens, model, out);
            }
        }

        ui.add_space(10.0);
        ui.label(RichText::new(INSTALL_HEADING).strong().color(tokens.text()));
        if section::primary_button(ui, tokens, "Choose a model pack").clicked() {
            out.push(SettingsIntent::PickFolder(FolderPurpose::ModelPack));
        }

        if let Some(name) = self.installing.clone() {
            section::prose(ui, tokens, &format!("Installing {name}…"));
            let bar = match self.progress {
                Some((received, total)) => {
                    #[allow(clippy::cast_precision_loss)]
                    let fraction = received as f32 / total as f32;
                    ProgressBar::new(fraction).text(format!(
                        "{} of {}",
                        format_bytes(Some(received)),
                        format_bytes(Some(total))
                    ))
                }
                // No figure has been reported, so the bar states that it does
                // not know rather than showing a number nobody measured.
                None => ProgressBar::new(0.0)
                    .animate(true)
                    .text("Size not reported yet"),
            };
            ui.add(bar);
        }

        if let Some(pack) = self.pack.as_ref() {
            if let Some(started) = offers_ui(ui, tokens, pack, &mut self.approvals, out) {
                self.installing = Some(started);
                self.progress = None;
            }
        }
    }

    fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        out: &mut SettingsOutput,
    ) {
        match command {
            "inspect_model_pack" => match result {
                Ok(AppResponse::InspectModelPack(response)) => {
                    self.pack = Some(response.clone());
                    self.approvals.clear();
                    self.message = None;
                }
                Err(envelope) => {
                    self.pack = None;
                    self.message = Some((Tone::Danger, envelope.message.clone()));
                }
                Ok(_) => {}
            },
            "install_model_from_pack" => {
                self.installing = None;
                self.progress = None;
                match result {
                    Ok(AppResponse::InstallModelFromPack(response)) => {
                        self.message = Some((
                            Tone::Success,
                            format!(
                                "Installed {} — {} written.",
                                response.model.display_name,
                                format_bytes(Some(response.transferred_bytes))
                            ),
                        ));
                    }
                    Err(envelope) => {
                        self.message = Some((Tone::Danger, envelope.message.clone()));
                    }
                    Ok(_) => {}
                }
                // Even a failed install can leave staging bytes behind, and
                // the listing is what reports them.
                out.command(AppCommand::ListLocalModels);
            }
            "remove_local_model" => {
                match result {
                    Ok(AppResponse::RemoveLocalModel(response)) => {
                        self.message = Some((
                            Tone::Success,
                            format!(
                                "Removed — {} freed.",
                                format_bytes(Some(response.bytes_freed))
                            ),
                        ));
                    }
                    Err(envelope) => {
                        self.message = Some((Tone::Danger, envelope.message.clone()));
                    }
                    Ok(_) => {}
                }
                out.command(AppCommand::ListLocalModels);
            }
            "verify_local_model" => {
                match result {
                    // The verifier's own sentence, verbatim.
                    Ok(AppResponse::VerifyLocalModel(response)) => {
                        let tone = if response.intact {
                            Tone::Success
                        } else {
                            Tone::Danger
                        };
                        self.message = Some((tone, response.detail.clone()));
                    }
                    Err(envelope) => {
                        self.message = Some((Tone::Danger, envelope.message.clone()));
                    }
                    Ok(_) => {}
                }
                out.command(AppCommand::ListLocalModels);
            }
            _ => {}
        }
    }

    fn on_path_picked(&mut self, picked: PickedPath, path: &str, out: &mut SettingsOutput) {
        if picked != PickedPath::Folder(FolderPurpose::ModelPack) {
            return;
        }
        out.command(AppCommand::InspectModelPack(InspectModelPackRequest {
            pack_path: path.to_owned(),
        }));
    }
}
