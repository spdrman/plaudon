//! The templates manager (matrix row A12).
//!
//! A template decides what each summary stage asks the model for, which
//! extraction fields an output carries, and how those fields are rendered. Six
//! commands publish that — `list_templates`, `get_template`,
//! `create_template_copy`, `save_template`, `validate_template` and
//! `list_template_versions` — and this panel is the whole of their user
//! interface.
//!
//! Three rules shape it:
//!
//! * **A built-in is never edited in place.** It ships with the application,
//!   and changing it under an output that was already produced with it would
//!   make that output's recorded `template_version` a lie. The panel says so
//!   and offers a copy, rather than disabling a control and leaving the user to
//!   guess why.
//! * **The backend judges a document, not the panel.** Validate asks
//!   `validate_template` and lists the problems it returned, one per row with
//!   its code; Save sends the document and the version the editor started from
//!   and renders whatever envelope comes back. Nothing here decides in advance
//!   that a save would fail.
//! * **The user's text is never thrown away.** A refused save leaves the
//!   editor exactly as it was and offers the two moves that help — reload the
//!   current version, or copy the edits out first.
//!
//! Like the other panels in this module it owns no data: the host dispatches
//! the commands it returns and hands the responses back, borrowed for
//! rendering ([`TemplateManager::ui`]) and fed through
//! [`TemplateManager::on_response`] so the editor knows which document and
//! which version it is working on.

use egui::{TextEdit, Ui};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    CreateTemplateCopyRequest, GetTemplateRequest, ListTemplateVersionsRequest,
    ListTemplateVersionsResponse, ListTemplatesResponse, SaveTemplateRequest, TemplateDocumentDto,
    TemplateFieldDto, TemplateSummaryDto, TemplateVersionDto, ValidateTemplateRequest,
    ValidateTemplateResponse,
};
use plaudon_app::tone::Tone;
use plaudon_domain::error::ErrorEnvelope;

use super::{fact, AiOutput, Badge};

/// Why a built-in template offers no editor.
///
/// Shown in full where the editor would otherwise be, because "the control is
/// missing" is not an explanation.
pub const BUILT_IN_IS_READ_ONLY: &str =
    "This template ships with Plaudon and is never edited in place: outputs already produced with \
     it record its version, and rewriting it would make those records wrong. Create a copy — the \
     copy is yours to edit and keeps its own version history.";

/// What a save produced, once the backend accepted it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SaveOutcome {
    /// Version the backend assigned to the saved document.
    version: String,
    /// Version it superseded, when the template already had one.
    previous: Option<String>,
}

/// The templates catalogue, the document editor and the version history.
///
/// Holds the user's own working state — which template is selected, the text
/// being edited, the version that text started from, the last validation and
/// the last refusal — and nothing that the application owns.
#[derive(Debug, Default)]
pub struct TemplateManager {
    selected: Option<String>,
    document_text: String,
    expected_version: Option<String>,
    pinned_version: Option<String>,
    copy_id: String,
    copy_name: String,
    validation: Option<ValidateTemplateResponse>,
    saved: Option<SaveOutcome>,
    refusal: Option<(&'static str, ErrorEnvelope)>,
}

impl TemplateManager {
    /// The template the user is looking at, if any.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// The text currently in the editor.
    ///
    /// Public because a refused save must demonstrably keep it, and because
    /// the host may want to warn before navigating away from unsaved edits.
    #[must_use]
    pub fn document_text(&self) -> &str {
        &self.document_text
    }

    /// The version the editor started from, which a save sends as
    /// `expected_version`. `None` while a document is still being loaded, and
    /// while a stored version is being read.
    #[must_use]
    pub fn expected_version(&self) -> Option<&str> {
        self.expected_version.as_deref()
    }

    /// The stored version being read, when the user loaded an older one.
    #[must_use]
    pub fn pinned_version(&self) -> Option<&str> {
        self.pinned_version.as_deref()
    }

    /// Fold one command outcome into the editor's state.
    ///
    /// The catalogue and the version history stay with the host — they are
    /// rendered from the borrowed responses — but the document text, the
    /// version it started from, the validation result and the last refusal are
    /// the panel's own, because they are what the user is in the middle of.
    pub fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
    ) {
        let response = match result {
            Err(envelope) => {
                self.refusal = Some((command, envelope.clone()));
                return;
            }
            Ok(response) => response,
        };
        match response {
            AppResponse::GetTemplate(document) => {
                self.refusal = None;
                self.adopt(document);
            }
            AppResponse::CreateTemplateCopy(document) => {
                self.refusal = None;
                self.validation = None;
                self.saved = None;
                self.pinned_version = None;
                self.selected = Some(document.template_id.clone());
                self.copy_id.clear();
                self.copy_name.clear();
                self.adopt(document);
            }
            AppResponse::SaveTemplate(saved) => {
                self.refusal = None;
                self.validation = None;
                self.pinned_version = None;
                self.saved = Some(SaveOutcome {
                    version: saved.template.version.clone(),
                    previous: saved.previous_version.clone(),
                });
                self.adopt(&saved.template);
            }
            AppResponse::ValidateTemplate(validation) => {
                self.refusal = None;
                self.validation = Some(validation.clone());
            }
            // `list_templates` and `list_template_versions` are the host's to
            // keep: the panel renders them borrowed and stores no copy.
            _ => {}
        }
    }

    /// Draw the manager.
    ///
    /// `templates` is the last `list_templates` response, `document` the last
    /// `get_template` (or the document a copy or a save returned) and
    /// `versions` the last `list_template_versions`; each is `None` while its
    /// read is still in flight.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        templates: Option<&ListTemplatesResponse>,
        document: Option<&TemplateDocumentDto>,
        versions: Option<&ListTemplateVersionsResponse>,
    ) -> AiOutput {
        let mut output = AiOutput::default();
        ui.label(egui::RichText::new("Templates").heading());
        ui.label(
            "A template decides what each summary stage asks for, which fields an output carries \
             and how they are rendered. Built-ins ship with the application; a copy is yours to \
             edit and carries its own version history.",
        );

        self.catalogue(ui, templates, &mut output);

        match document {
            Some(document) => {
                ui.separator();
                Self::details(ui, document);
                ui.separator();
                if document.built_in {
                    self.copy_form(ui, document, &mut output);
                } else {
                    self.editor(ui, document, &mut output);
                }
            }
            None if self.selected.is_some() => {
                ui.separator();
                ui.label("Reading the selected template…");
            }
            None => {}
        }

        if self.selected.is_some() {
            ui.separator();
            self.history(ui, versions, &mut output);
        }
        output
    }

    /* --------------------------------------------------------- catalogue */

    fn catalogue(
        &mut self,
        ui: &mut Ui,
        templates: Option<&ListTemplatesResponse>,
        output: &mut AiOutput,
    ) {
        let Some(response) = templates else {
            ui.label("Templates have not been read yet.");
            return;
        };
        if response.templates.is_empty() {
            ui.label("No template is installed in this library.");
            return;
        }
        for summary in &response.templates {
            let selected = self.selected.as_deref() == Some(summary.template_id.as_str());
            let chosen = ui.radio(selected, summary.display_name.as_str()).clicked();
            ui.indent(summary.template_id.as_str(), |ui| {
                Self::summary_row(ui, summary);
            });
            if chosen && !selected {
                self.select(&summary.template_id, output);
            }
        }
    }

    /// One catalogue row under its selector.
    ///
    /// The origin of a template carries a glyph *and* a word, so "built-in"
    /// survives a monochrome screen and a screen reader (§5.3).
    fn summary_row(ui: &mut Ui, summary: &TemplateSummaryDto) {
        if summary.built_in {
            Badge::new("◆", "Built-in", Tone::Neutral).ui(ui);
        } else {
            Badge::new("✎", "Your copy", Tone::Running).ui(ui);
        }
        fact(ui, "Version", &summary.version);
        fact(ui, "Purpose", &summary.purpose);
        fact(ui, "Extraction fields", &summary.field_count.to_string());
        fact(ui, "Stored versions", &summary.version_count.to_string());
        if let Some(base_id) = summary.base_id.as_deref() {
            match summary.base_version.as_deref() {
                Some(base_version) => fact(
                    ui,
                    "Copied from",
                    &format!("{base_id} at version {base_version}"),
                ),
                None => fact(ui, "Copied from", base_id),
            }
        }
    }

    /// Select a template and ask for everything that describes it.
    ///
    /// Called from a click, never from a frame: the two reads happen once per
    /// selection, and the editor state of the template being left behind goes
    /// with it.
    fn select(&mut self, template_id: &str, output: &mut AiOutput) {
        self.selected = Some(template_id.to_owned());
        self.copy_id.clear();
        self.copy_name.clear();
        self.forget_document();
        self.pinned_version = None;
        output.command(AppCommand::GetTemplate(GetTemplateRequest {
            template_id: template_id.to_owned(),
            version: None,
        }));
        output.command(AppCommand::ListTemplateVersions(
            ListTemplateVersionsRequest {
                template_id: template_id.to_owned(),
            },
        ));
    }

    /* ----------------------------------------------------------- details */

    /// The selected template in full.
    ///
    /// The heading carries the identity as well as the name: two templates may
    /// be called the same thing, and the identity is what every stored output
    /// records.
    fn details(ui: &mut Ui, document: &TemplateDocumentDto) {
        ui.label(
            egui::RichText::new(format!(
                "{} — {}",
                document.display_name, document.template_id
            ))
            .heading(),
        );
        fact(ui, "Version", &document.version);
        fact(ui, "Purpose", &document.purpose);
        fact(ui, "Prompt version", &document.prompt_version);
        fact(ui, "Output schema version", &document.output_schema_version);
        if let Some(base_id) = document.base_id.as_deref() {
            match document.base_version.as_deref() {
                Some(base_version) => fact(
                    ui,
                    "Copied from",
                    &format!("{base_id} at version {base_version}"),
                ),
                None => fact(ui, "Copied from", base_id),
            }
        }

        ui.label(egui::RichText::new("Stage instructions").strong());
        fact(ui, "Role and refusal rules", &document.instructions.system);
        fact(
            ui,
            "Section summary",
            &document.instructions.section_summary,
        );
        fact(ui, "Synthesis", &document.instructions.synthesis);
        fact(ui, "Extraction", &document.instructions.extraction);

        ui.label(egui::RichText::new("Extraction fields").strong());
        if document.fields.is_empty() {
            ui.label("This template declares no extraction field.");
        }
        for field in &document.fields {
            Self::field_row(ui, field);
        }

        ui.label(egui::RichText::new("Rendering").strong());
        if document.rendering.order.is_empty() {
            ui.label("No presentation order is declared, so fields render in declaration order.");
        } else {
            fact(ui, "Order", &document.rendering.order.join(" → "));
        }
        fact(
            ui,
            "An empty field shows",
            &document.rendering.empty_placeholder,
        );
        fact(
            ui,
            "Anchor chips",
            if document.rendering.show_anchors {
                "shown on every item"
            } else {
                "not shown"
            },
        );
    }

    fn field_row(ui: &mut Ui, field: &TemplateFieldDto) {
        ui.label(egui::RichText::new(format!("{} — {}", field.heading, field.key)).strong());
        ui.indent(field.key.as_str(), |ui| {
            fact(ui, "Claim kind", &field.kind);
            fact(ui, "Anchors", &anchor_rule(field));
            fact(
                ui,
                "Ceilings",
                &format!(
                    "at most {} items, {} characters per string",
                    field.max_items, field.max_text_chars
                ),
            );
        });
    }

    /* -------------------------------------------------------- copy a built-in */

    fn copy_form(&mut self, ui: &mut Ui, document: &TemplateDocumentDto, output: &mut AiOutput) {
        ui.label(egui::RichText::new("Copy this template").heading());
        ui.label(BUILT_IN_IS_READ_ONLY);

        let id_label = ui.label("New template id");
        ui.add(
            TextEdit::singleline(&mut self.copy_id)
                .hint_text("an identifier of your own")
                .desired_width(240.0),
        )
        .labelled_by(id_label.id);
        let name_label = ui.label("Display name (optional)");
        ui.add(
            TextEdit::singleline(&mut self.copy_name)
                .hint_text("the base name with a suffix when left empty")
                .desired_width(280.0),
        )
        .labelled_by(name_label.id);

        let new_id = self.copy_id.trim().to_owned();
        let named = self.copy_name.trim().to_owned();
        let has_id = !new_id.is_empty();
        if ui
            .add_enabled(has_id, egui::Button::new("Create a copy"))
            .clicked()
        {
            output.command(AppCommand::CreateTemplateCopy(CreateTemplateCopyRequest {
                base_id: document.template_id.clone(),
                new_id,
                display_name: (!named.is_empty()).then_some(named),
            }));
        }
        if !has_id {
            ui.label("A copy needs an identifier of its own before it can be created.");
        }
        self.refusal_row(ui, "create_template_copy");
    }

    /* ------------------------------------------------------------ editor */

    fn editor(&mut self, ui: &mut Ui, document: &TemplateDocumentDto, output: &mut AiOutput) {
        if self.pinned_version.is_some() {
            self.stored_version_view(ui, document, output);
            return;
        }

        ui.label(egui::RichText::new("Edit this template").heading());
        let Some(expected) = self.expected_version.clone() else {
            ui.label("Reading the current document…");
            return;
        };
        ui.label(format!(
            "Editing version {expected}. Saving stores a new version and leaves this one in the \
             history."
        ));

        let label = ui.label("Template document");
        ui.add(
            // `code_editor` locks focus so Tab indents; that would trap the
            // keyboard in the one control on the screen it is hardest to leave,
            // which §7 of the design system forbids. The monospace face stays,
            // the trap does not.
            TextEdit::multiline(&mut self.document_text)
                .code_editor()
                .lock_focus(false)
                .desired_rows(12)
                .desired_width(f32::INFINITY),
        )
        .labelled_by(label.id);

        let mut validate = false;
        let mut save = false;
        ui.horizontal_wrapped(|ui| {
            validate = ui.button("Validate").clicked();
            save = ui.button("Save").clicked();
        });
        if validate {
            output.command(AppCommand::ValidateTemplate(ValidateTemplateRequest {
                document: self.document_text.clone(),
            }));
        }
        if save {
            output.command(AppCommand::SaveTemplate(SaveTemplateRequest {
                template_id: document.template_id.clone(),
                document: self.document_text.clone(),
                expected_version: Some(expected),
            }));
        }

        self.validation_rows(ui);
        self.saved_row(ui);
        self.save_refusal(ui, document, output);
    }

    /// The read-only view of a stored version.
    ///
    /// The document is shown as it was stored and cannot be typed into: a save
    /// always applies to the current version, and an editor that accepted
    /// keystrokes here would be promising otherwise.
    fn stored_version_view(
        &mut self,
        ui: &mut Ui,
        document: &TemplateDocumentDto,
        output: &mut AiOutput,
    ) {
        let Some(heading) = self
            .pinned_version
            .as_deref()
            .map(|pinned| format!("Version {pinned}, read only"))
        else {
            return;
        };
        ui.label(egui::RichText::new(heading).heading());
        ui.label("This is a stored version. Editing and saving apply to the current version only.");
        let label = ui.label("Template document (read only)");
        ui.add(
            TextEdit::multiline(&mut self.document_text)
                .code_editor()
                .lock_focus(false)
                .interactive(false)
                .desired_rows(12)
                .desired_width(f32::INFINITY),
        )
        .labelled_by(label.id);
        if ui.button("Show the current version").clicked() {
            let template_id = document.template_id.clone();
            self.pinned_version = None;
            self.forget_document();
            output.command(AppCommand::GetTemplate(GetTemplateRequest {
                template_id,
                version: None,
            }));
        }
        self.refusal_row(ui, "get_template");
    }

    fn validation_rows(&self, ui: &mut Ui) {
        let Some(validation) = self.validation.as_ref() else {
            return;
        };
        if validation.valid {
            Badge::new("✓", "Valid", Tone::Success)
                .description("This document would be accepted by Save.")
                .ui(ui);
            return;
        }
        Badge::new("!", "Not valid", Tone::Warning)
            .description("Save refuses a document with problems. Each one is listed below.")
            .ui(ui);
        for problem in &validation.problems {
            ui.label(format!("{}: {}", problem.code, problem.message));
        }
    }

    fn saved_row(&self, ui: &mut Ui) {
        let Some(saved) = self.saved.as_ref() else {
            return;
        };
        let description = match saved.previous.as_deref() {
            Some(previous) => format!(
                "Stored as version {}, superseding version {previous}.",
                saved.version
            ),
            None => format!(
                "Stored as version {}, the first stored version of this template.",
                saved.version
            ),
        };
        Badge::new("✓", "Saved", Tone::Success)
            .description(&description)
            .ui(ui);
    }

    /// A refused save, with the two moves that recover from it.
    ///
    /// Reloading replaces the editor's text with the current document, so the
    /// copy-out control sits next to it: the user decides which of the two
    /// versions survives, and neither happens on its own.
    fn save_refusal(&mut self, ui: &mut Ui, document: &TemplateDocumentDto, output: &mut AiOutput) {
        let mut reload = false;
        if let Some(envelope) = self.refusal_for("save_template") {
            Badge::new("✕", "Save refused", Tone::Danger)
                .description(&envelope.message)
                .ui(ui);
            ui.label(format!("Code: {}", envelope.code.as_str()));
            ui.label("Your edits are still in the editor above.");
            ui.horizontal_wrapped(|ui| {
                reload = ui.button("Reload the current version").clicked();
                if ui.button("Copy my changes").clicked() {
                    ui.ctx().copy_text(self.document_text.clone());
                }
            });
        }
        if reload {
            let template_id = document.template_id.clone();
            self.pinned_version = None;
            self.forget_document();
            output.command(AppCommand::GetTemplate(GetTemplateRequest {
                template_id,
                version: None,
            }));
        }
    }

    /// A refusal of a command that has no recovery control of its own.
    fn refusal_row(&self, ui: &mut Ui, command: &str) {
        let Some(envelope) = self.refusal_for(command) else {
            return;
        };
        Badge::new("✕", refusal_title(command), Tone::Danger)
            .description(&envelope.message)
            .ui(ui);
        ui.label(format!("Code: {}", envelope.code.as_str()));
    }

    fn refusal_for(&self, command: &str) -> Option<&ErrorEnvelope> {
        self.refusal
            .as_ref()
            .filter(|(refused, _)| *refused == command)
            .map(|(_, envelope)| envelope)
    }

    /* ----------------------------------------------------------- history */

    /// The stored versions, in the order the backend listed them.
    ///
    /// `list_template_versions` returns them newest first; the panel does not
    /// re-sort, because the order is the application's statement about the
    /// history, not a presentation choice.
    fn history(
        &mut self,
        ui: &mut Ui,
        versions: Option<&ListTemplateVersionsResponse>,
        output: &mut AiOutput,
    ) {
        ui.label(egui::RichText::new("Version history").heading());
        let Some(response) = versions else {
            ui.label("Versions have not been read yet.");
            return;
        };
        if response.versions.is_empty() {
            ui.label("No stored version was reported for this template.");
            return;
        }
        let Some(template_id) = self.selected.clone() else {
            return;
        };

        let mut load = None;
        for version in &response.versions {
            ui.horizontal_wrapped(|ui| {
                ui.label(version_line(version));
                if version.is_current {
                    Badge::new("●", "Current version", Tone::Success).ui(ui);
                } else if ui
                    .button(format!("Load version {}", version.version))
                    .clicked()
                {
                    load = Some(version.version.clone());
                }
            });
        }
        if let Some(version) = load {
            self.forget_document();
            self.pinned_version = Some(version.clone());
            output.command(AppCommand::GetTemplate(GetTemplateRequest {
                template_id,
                version: Some(version),
            }));
        }
    }

    /* ------------------------------------------------------------ shared */

    /// Take a document as the one being worked on.
    ///
    /// A stored version is adopted for reading only, so it leaves
    /// `expected_version` empty: nothing may be saved against a version the
    /// editor is not editing.
    fn adopt(&mut self, document: &TemplateDocumentDto) {
        self.document_text.clear();
        self.document_text.push_str(&document.document);
        self.expected_version = if self.pinned_version.is_some() {
            None
        } else {
            Some(document.version.clone())
        };
    }

    /// Drop everything that belonged to the document being replaced.
    fn forget_document(&mut self) {
        self.document_text.clear();
        self.expected_version = None;
        self.validation = None;
        self.saved = None;
        self.refusal = None;
    }
}

/// How many anchors an item of this field must and may carry.
///
/// A minimum of zero is "optional", not "at least 0": the second reads as a
/// constraint where there is none.
fn anchor_rule(field: &TemplateFieldDto) -> String {
    if field.min_anchors == 0 {
        format!("optional, at most {}", field.max_anchors)
    } else {
        format!(
            "at least {}, at most {}",
            field.min_anchors, field.max_anchors
        )
    }
}

/// One row of the version history.
///
/// A built-in version is compiled into the binary: there is no stored file and
/// therefore no time to report. The row says so instead of borrowing a time
/// from somewhere else, because a fabricated date is worse than no date.
fn version_line(version: &TemplateVersionDto) -> String {
    match version.created_at.as_deref() {
        Some(created_at) => format!("Version {} · stored {created_at}", version.version),
        None => format!("Version {} · ships with the application", version.version),
    }
}

/// The headline a refusal gets, in the user's terms rather than the command's.
fn refusal_title(command: &str) -> &'static str {
    match command {
        "create_template_copy" => "Copy refused",
        "get_template" => "Could not read that version",
        "validate_template" => "Check refused",
        _ => "The application refused the request",
    }
}
