//! Backup and restore (matrix row C5).
//!
//! The React component this replaces (`features/settings/BackupRestoreSection.tsx`)
//! was written and tested but never mounted. It is mounted here, with the
//! three obligations that made it worth keeping intact:
//!
//! * **The protection disclosure follows the password field.** With no
//!   password the modal says, before anything is written, that the archive
//!   will not be encrypted and that anyone holding the file can read the
//!   recordings. With a password it says the archive will be encrypted, offers
//!   the slower Argon2id profile, and warns that a lost password cannot be
//!   recovered.
//! * **Nothing is restored before it is inspected.** `inspect_restore_archive`
//!   is a dry run that writes nothing; the restore controls do not exist until
//!   it has answered, and the answer — conflicts, verification, what is not in
//!   the archive — is rendered in full before the user chooses a mode.
//! * **The backend owns the words.** `protection_disclosure`, `integrity_note`
//!   and every error message are rendered verbatim; this module composes no
//!   sentence the application already has one for.
//!
//! The two passwords live in the section only between being typed and being
//! handed to a command: the backup password is cleared the instant
//! `create_portable_backup` is raised, the archive password the instant
//! `restore_portable_archive` is raised (it has to survive the inspection,
//! which needs the same secret to read the archive at all), and both are
//! cleared when the modal closes. Neither appears in [`Debug`].

use std::fmt;

use egui::{Align, Layout, RichText, Ui};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    ConflictPolicyDto, CreateBackupRequest, CreateBackupResponse, InspectArchiveRequest,
    InspectArchiveResponse, RestoreArchiveRequest, RestoreArchiveResponse, RestoreModeDto,
};
use plaudon_app::settings_presentation::format_bytes;
use plaudon_domain::error::ErrorEnvelope;

use super::intent::{FilePurpose, FolderPurpose, SettingsIntent};
use super::section;
use super::theme::{SettingsTokens, Tone};
use super::{PickedPath, Section, SettingsData, SettingsOutput};

/// Standing prose of the section.
const SECTION_PROSE: &str = "Create portable backups of your library or restore from an archive.";

/// What the modal says before a backup is written without a password.
const PLAINTEXT_WARNING: &str =
    "This backup will NOT be encrypted. Anyone with the file can read your recordings.";

/// What the modal says once a password has been typed.
const ENCRYPTED_STATEMENT: &str = "This backup will be encrypted with your password.";

/// The heavier key-derivation profile.
const SENSITIVE_PROFILE_LABEL: &str =
    "Use sensitive profile (slower but more secure; recommended for highly sensitive data)";

/// The consequence of losing the password, stated before the archive exists.
const PASSWORD_LOSS_WARNING: &str = "We cannot recover a lost password. Store it securely.";

/// Introduces the conflict list of an inspection.
const CONFLICT_PROSE: &str = "The following records already exist in this library:";

/// Why `new_library` is refused when the library is not empty.
const NEW_LIBRARY_UNAVAILABLE: &str =
    "New library is not available: this library already contains recordings.";

/// What restoring a tombstoned recording overrides.
const TOMBSTONE_WARNING: &str =
    "Recordings you deleted on this machine will be restored, overriding that deletion.";

/// Which part of the restore flow is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreStage {
    /// Choose an archive and, if it is encrypted, a password.
    Choose,
    /// The dry run answered; review it and choose how to restore.
    Review,
    /// The restore ran; here is what it did.
    Done,
}

/// Backup and restore.
#[allow(
    clippy::struct_excessive_bools,
    reason = "two modals' worth of independent switches; grouping them into sub-structs would \
     buy nothing but indirection"
)]
pub struct BackupSection {
    backup_open: bool,
    backup_password: String,
    sensitive_profile: bool,
    backup_pending: bool,
    backup_result: Option<CreateBackupResponse>,
    backup_error: Option<ErrorEnvelope>,

    restore_open: bool,
    archive_path: Option<String>,
    archive_password: String,
    inspect_pending: bool,
    restore_pending: bool,
    inspection: Option<InspectArchiveResponse>,
    restore_result: Option<RestoreArchiveResponse>,
    restore_error: Option<ErrorEnvelope>,
    restore_mode: RestoreModeDto,
    conflict_policy: ConflictPolicyDto,
    restore_tombstoned: bool,
}

impl Default for BackupSection {
    fn default() -> Self {
        Self {
            backup_open: false,
            backup_password: String::new(),
            sensitive_profile: false,
            backup_pending: false,
            backup_result: None,
            backup_error: None,
            restore_open: false,
            archive_path: None,
            archive_password: String::new(),
            inspect_pending: false,
            restore_pending: false,
            inspection: None,
            restore_result: None,
            restore_error: None,
            // Merging is the only mode that is always legal, so it is where an
            // unanswered question rests.
            restore_mode: RestoreModeDto::Merge,
            // Spec §13.6: the default is to keep what the library already has.
            conflict_policy: ConflictPolicyDto::Skip,
            restore_tombstoned: false,
        }
    }
}

impl fmt::Debug for BackupSection {
    /// Written by hand so that neither password can reach a log line.
    ///
    /// Whether a password has been typed is a fact the user can already see;
    /// the characters are not.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BackupSection")
            .field("backup_open", &self.backup_open)
            .field("backup_password", &"[redacted]")
            .field("sensitive_profile", &self.sensitive_profile)
            .field("backup_pending", &self.backup_pending)
            .field("backup_result", &self.backup_result)
            .field("backup_error", &self.backup_error)
            .field("restore_open", &self.restore_open)
            .field("archive_path", &self.archive_path)
            .field("archive_password", &"[redacted]")
            .field("inspect_pending", &self.inspect_pending)
            .field("restore_pending", &self.restore_pending)
            .field("inspection", &self.inspection)
            .field("restore_result", &self.restore_result)
            .field("restore_error", &self.restore_error)
            .field("restore_mode", &self.restore_mode)
            .field("conflict_policy", &self.conflict_policy)
            .field("restore_tombstoned", &self.restore_tombstoned)
            .finish()
    }
}

impl BackupSection {
    /// Close the backup modal and forget everything it held.
    fn close_backup(&mut self) {
        self.backup_open = false;
        self.backup_password.clear();
        self.sensitive_profile = false;
        self.backup_pending = false;
        self.backup_result = None;
        self.backup_error = None;
    }

    /// Close the restore modal and forget everything it held.
    fn close_restore(&mut self) {
        self.restore_open = false;
        self.archive_path = None;
        self.archive_password.clear();
        self.inspect_pending = false;
        self.restore_pending = false;
        self.inspection = None;
        self.restore_result = None;
        self.restore_error = None;
        self.restore_mode = RestoreModeDto::Merge;
        self.conflict_policy = ConflictPolicyDto::Skip;
        self.restore_tombstoned = false;
    }

    /// Which stage of the restore flow to render.
    fn restore_stage(&self) -> RestoreStage {
        if self.restore_result.is_some() {
            RestoreStage::Done
        } else if self.inspection.is_some() {
            RestoreStage::Review
        } else {
            RestoreStage::Choose
        }
    }

    /// The password as a command argument: absent when nothing was typed.
    fn password_argument(value: &str) -> Option<String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    }

    /// How tall a modal body may be before it scrolls.
    ///
    /// Half the viewport, so that the heading above it and the buttons below
    /// it are on screen on a laptop display as well as on a large one: a
    /// confirmation whose confirm button is off screen is not one.
    fn body_height(ui: &Ui) -> f32 {
        (ui.ctx().content_rect().height() * 0.5).max(240.0)
    }

    /// The "Create backup" modal.
    fn backup_modal(&mut self, ui: &mut Ui, tokens: &dyn SettingsTokens, out: &mut SettingsOutput) {
        let mut close = false;
        let response =
            egui::Modal::new(egui::Id::new("settings-backup-create")).show(ui.ctx(), |ui| {
                ui.set_min_width(460.0);
                ui.label(
                    RichText::new("Create backup")
                        .heading()
                        .strong()
                        .color(tokens.text()),
                );
                ui.add_space(8.0);
                let max_height = Self::body_height(ui);
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if self.backup_result.is_some() {
                            self.backup_result_body(ui, tokens, out);
                        } else {
                            self.backup_form_body(ui, tokens);
                        }
                    });
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.backup_result.is_some() {
                        if section::primary_button(ui, tokens, "Done").clicked() {
                            close = true;
                        }
                        return;
                    }
                    let mut create = false;
                    ui.scope(|ui| {
                        if self.backup_pending {
                            ui.disable();
                        }
                        create = section::primary_button(ui, tokens, "Create backup now").clicked();
                    });
                    if create {
                        // The folder comes back through `on_path_picked`,
                        // which is where the command is raised.
                        self.backup_pending = true;
                        self.backup_error = None;
                        out.push(SettingsIntent::PickFolder(FolderPurpose::BackupDestination));
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        if close || response.should_close() {
            self.close_backup();
        }
    }

    /// The password field and the disclosure that follows it.
    fn backup_form_body(&mut self, ui: &mut Ui, tokens: &dyn SettingsTokens) {
        section::prose(
            ui,
            tokens,
            "Create a portable backup of your entire library.",
        );
        ui.add_space(8.0);
        section::secret_field(ui, "Password (optional)", &mut self.backup_password);
        ui.add_space(8.0);
        if self.backup_password.trim().is_empty() {
            section::status_badge(ui, tokens, Tone::Warning, "⚠", PLAINTEXT_WARNING);
        } else {
            section::status_badge(ui, tokens, Tone::Success, "✓", ENCRYPTED_STATEMENT);
            ui.add_space(4.0);
            ui.checkbox(&mut self.sensitive_profile, SENSITIVE_PROFILE_LABEL);
            ui.add_space(4.0);
            section::status_badge(ui, tokens, Tone::Danger, "⚠", PASSWORD_LOSS_WARNING);
        }
        if self.backup_pending {
            ui.add_space(8.0);
            section::note(ui, tokens, "Creating the backup…");
        }
        if let Some(envelope) = &self.backup_error {
            ui.add_space(8.0);
            error_line(ui, tokens, envelope);
        }
    }

    /// What `create_portable_backup` reported, rendered from its own fields.
    fn backup_result_body(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        out: &mut SettingsOutput,
    ) {
        let Some(result) = self.backup_result.as_ref() else {
            return;
        };
        section::status_badge(
            ui,
            tokens,
            Tone::Success,
            "✓",
            "Backup created successfully",
        );
        ui.add_space(8.0);
        section::fact_row(ui, tokens, "File", &result.file_name);
        section::fact_row(ui, tokens, "Entries", &result.entry_count.to_string());
        section::fact_row(
            ui,
            tokens,
            "Content size",
            &format_bytes(Some(result.total_byte_length)),
        );
        section::fact_row(
            ui,
            tokens,
            "File size",
            &format_bytes(Some(result.file_byte_length)),
        );
        // Verbatim: the disclosure of what the archive protects is the
        // backend's sentence, not ours.
        section::fact_row(ui, tokens, "Protection", &result.protection_disclosure);
        section::fact_row(ui, tokens, "Integrity digest", &result.integrity_digest);
        if ui.button("Copy integrity digest").clicked() {
            out.push(SettingsIntent::CopyText(result.integrity_digest.clone()));
        }
        section::fact_row(ui, tokens, "Note", &result.integrity_note);
        section::fact_row(
            ui,
            tokens,
            "Manifest schema version",
            &result.manifest_schema_version,
        );
        if !result.nonportable_settings.is_empty() {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Settings not included")
                    .strong()
                    .color(tokens.text()),
            );
            for setting in &result.nonportable_settings {
                section::prose(ui, tokens, &format!("• {setting}"));
            }
        }
    }

    /// The "Restore from archive" modal.
    fn restore_modal(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        out: &mut SettingsOutput,
    ) {
        let mut close = false;
        let response =
            egui::Modal::new(egui::Id::new("settings-backup-restore")).show(ui.ctx(), |ui| {
                ui.set_min_width(520.0);
                ui.label(
                    RichText::new("Restore from archive")
                        .heading()
                        .strong()
                        .color(tokens.text()),
                );
                ui.add_space(8.0);
                let stage = self.restore_stage();
                let max_height = Self::body_height(ui);
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| match stage {
                        RestoreStage::Choose => self.restore_choose_body(ui, tokens, out),
                        RestoreStage::Review => self.restore_review_body(ui, tokens),
                        RestoreStage::Done => self.restore_done_body(ui, tokens),
                    });
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| match stage {
                    RestoreStage::Choose => {
                        let mut inspect = false;
                        ui.scope(|ui| {
                            if self.archive_path.is_none() || self.inspect_pending {
                                ui.disable();
                            }
                            inspect =
                                section::primary_button(ui, tokens, "Inspect archive").clicked();
                        });
                        if inspect {
                            if let Some(path) = self.archive_path.clone() {
                                self.inspect_pending = true;
                                self.restore_error = None;
                                out.command(AppCommand::InspectRestoreArchive(
                                    InspectArchiveRequest {
                                        archive_path: path,
                                        password: Self::password_argument(&self.archive_password),
                                    },
                                ));
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    }
                    RestoreStage::Review => {
                        let mut restore = false;
                        ui.scope(|ui| {
                            if self.restore_pending {
                                ui.disable();
                            }
                            restore = section::primary_button(ui, tokens, "Restore").clicked();
                        });
                        if restore {
                            if let Some(path) = self.archive_path.clone() {
                                self.restore_pending = true;
                                self.restore_error = None;
                                // The archive password has done its work: the
                                // restore is the last command that needs it.
                                let password = std::mem::take(&mut self.archive_password);
                                out.command(AppCommand::RestorePortableArchive(
                                    RestoreArchiveRequest {
                                        archive_path: path,
                                        password: Self::password_argument(&password),
                                        mode: self.restore_mode,
                                        on_conflict: self.conflict_policy,
                                        restore_tombstoned: self.restore_tombstoned,
                                    },
                                ));
                            }
                        }
                        if ui.button("Back").clicked() {
                            self.inspection = None;
                            self.restore_error = None;
                        }
                    }
                    RestoreStage::Done => {
                        if section::primary_button(ui, tokens, "Done").clicked() {
                            close = true;
                        }
                    }
                });
            });
        if close || response.should_close() {
            self.close_restore();
        }
    }

    /// Stage one: choose the archive, and its password when it has one.
    fn restore_choose_body(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        out: &mut SettingsOutput,
    ) {
        let label = if self.archive_path.is_some() {
            "Choose different archive"
        } else {
            "Choose archive"
        };
        if ui.button(label).clicked() {
            out.push(SettingsIntent::PickFile(FilePurpose::RestoreArchive));
        }
        ui.add_space(8.0);
        if let Some(path) = self.archive_path.clone() {
            section::fact_row(ui, tokens, "Selected", &path);
            ui.add_space(8.0);
            section::secret_field(ui, "Password (if encrypted)", &mut self.archive_password);
        } else {
            section::note(ui, tokens, "No archive chosen yet.");
        }
        ui.add_space(8.0);
        section::note(
            ui,
            tokens,
            "Inspecting an archive is a dry run: it reads the archive and changes nothing.",
        );
        if self.inspect_pending {
            ui.add_space(8.0);
            section::note(ui, tokens, "Inspecting the archive…");
        }
        if let Some(envelope) = &self.restore_error {
            ui.add_space(8.0);
            error_line(ui, tokens, envelope);
        }
    }

    /// Stage two: the dry run in full, then the restore options.
    fn restore_review_body(&mut self, ui: &mut Ui, tokens: &dyn SettingsTokens) {
        let Some(inspection) = self.inspection.clone() else {
            return;
        };
        ui.label(
            RichText::new("Archive information")
                .strong()
                .color(tokens.text()),
        );
        section::fact_row(ui, tokens, "Created", &inspection.created_at);
        section::fact_row(ui, tokens, "Source OS", &inspection.source_os);
        section::fact_row(
            ui,
            tokens,
            "Encrypted",
            if inspection.encrypted { "Yes" } else { "No" },
        );
        section::fact_row(
            ui,
            tokens,
            "Format version",
            &inspection.format_version.to_string(),
        );
        section::fact_row(
            ui,
            tokens,
            "Schema version",
            &inspection.manifest_schema_version,
        );
        section::fact_row(
            ui,
            tokens,
            "Content size",
            &format_bytes(Some(inspection.total_byte_length)),
        );
        if inspection.payloads_verified {
            section::status_badge(
                ui,
                tokens,
                Tone::Success,
                "✓",
                "Payload digests verified: yes",
            );
        } else {
            section::status_badge(
                ui,
                tokens,
                Tone::Danger,
                "✕",
                "Payload digests verified: no",
            );
        }
        // Verbatim: what the digest proves, and what it does not.
        section::fact_row(ui, tokens, "Note", &inspection.integrity_note);

        ui.add_space(8.0);
        ui.label(RichText::new("Contents").strong().color(tokens.text()));
        for namespace in &inspection.entries_by_namespace {
            section::fact_row(ui, tokens, &namespace.label, &namespace.count.to_string());
        }

        if !inspection.nonportable_settings.is_empty() {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Settings not in archive")
                    .strong()
                    .color(tokens.text()),
            );
            for setting in &inspection.nonportable_settings {
                section::prose(ui, tokens, &format!("• {setting}"));
            }
        }

        if !inspection.conflicts.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Conflicts").strong().color(tokens.text()));
            section::status_badge(ui, tokens, Tone::Warning, "⚠", CONFLICT_PROSE);
            for conflict in &inspection.conflicts {
                section::prose(
                    ui,
                    tokens,
                    &format!("{}: {}", conflict.entry_path, conflict.reason),
                );
            }
        }

        ui.add_space(12.0);
        ui.label(
            RichText::new("Restore options")
                .strong()
                .color(tokens.text()),
        );
        ui.label(RichText::new("Mode").strong().color(tokens.muted_text()));
        let new_library_allowed = inspection.target_is_empty;
        let new_library = ui.add_enabled(
            new_library_allowed,
            egui::RadioButton::new(
                self.restore_mode == RestoreModeDto::NewLibrary,
                "New library (requires an empty target)",
            ),
        );
        if new_library.clicked() && new_library_allowed {
            self.restore_mode = RestoreModeDto::NewLibrary;
        }
        if !new_library_allowed {
            section::status_badge(ui, tokens, Tone::Warning, "–", NEW_LIBRARY_UNAVAILABLE);
        }
        if ui
            .radio(
                self.restore_mode == RestoreModeDto::Merge,
                "Merge into existing library",
            )
            .clicked()
        {
            self.restore_mode = RestoreModeDto::Merge;
        }

        ui.add_space(8.0);
        ui.label(
            RichText::new("On conflict")
                .strong()
                .color(tokens.muted_text()),
        );
        if ui
            .radio(
                self.conflict_policy == ConflictPolicyDto::Skip,
                "Skip conflicting records",
            )
            .clicked()
        {
            self.conflict_policy = ConflictPolicyDto::Skip;
        }
        if ui
            .radio(
                self.conflict_policy == ConflictPolicyDto::Fail,
                "Abort on any conflict",
            )
            .clicked()
        {
            self.conflict_policy = ConflictPolicyDto::Fail;
        }

        ui.add_space(8.0);
        ui.checkbox(
            &mut self.restore_tombstoned,
            "Restore previously deleted recordings (overrides deletion)",
        );
        if self.restore_tombstoned {
            section::status_badge(ui, tokens, Tone::Warning, "⚠", TOMBSTONE_WARNING);
        }

        if self.restore_pending {
            ui.add_space(8.0);
            section::note(ui, tokens, "Restoring…");
        }
        if let Some(envelope) = &self.restore_error {
            ui.add_space(8.0);
            error_line(ui, tokens, envelope);
        }
    }

    /// Stage three: what the restore actually did.
    fn restore_done_body(&mut self, ui: &mut Ui, tokens: &dyn SettingsTokens) {
        let Some(result) = self.restore_result.as_ref() else {
            return;
        };
        section::status_badge(
            ui,
            tokens,
            Tone::Success,
            "✓",
            "Archive restored successfully",
        );
        ui.add_space(8.0);
        section::fact_row(
            ui,
            tokens,
            "Recordings restored",
            &result.recordings_restored.to_string(),
        );
        section::fact_row(
            ui,
            tokens,
            "Transcripts restored",
            &result.transcripts_restored.to_string(),
        );
        section::fact_row(
            ui,
            tokens,
            "Outputs restored",
            &result.outputs_restored.to_string(),
        );
        section::fact_row(
            ui,
            tokens,
            "Sources restored",
            &result.sources_restored.to_string(),
        );
        section::fact_row(
            ui,
            tokens,
            "Media committed",
            &result.media_committed.to_string(),
        );
        section::fact_row(ui, tokens, "Media reused", &result.media_reused.to_string());
        if !result.conflicts.is_empty() {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Records that were not applied")
                    .strong()
                    .color(tokens.text()),
            );
            for conflict in &result.conflicts {
                section::prose(
                    ui,
                    tokens,
                    &format!("{}: {}", conflict.entry_path, conflict.reason),
                );
            }
        }
    }
}

/// Render an error envelope's own message.
fn error_line(ui: &mut Ui, tokens: &dyn SettingsTokens, envelope: &ErrorEnvelope) {
    section::status_badge(ui, tokens, Tone::Danger, "✕", &envelope.message);
}

impl Section for BackupSection {
    fn title(&self) -> &'static str {
        "Backup and restore"
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        _data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        section::prose(ui, tokens, SECTION_PROSE);
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if section::primary_button(ui, tokens, "Create backup…").clicked() {
                self.close_restore();
                self.backup_open = true;
            }
            if ui.button("Restore from archive…").clicked() {
                self.close_backup();
                self.restore_open = true;
            }
        });

        if self.backup_open {
            self.backup_modal(ui, tokens, out);
        }
        if self.restore_open {
            self.restore_modal(ui, tokens, out);
        }
    }

    fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        _out: &mut SettingsOutput,
    ) {
        match command {
            "create_portable_backup" => {
                self.backup_pending = false;
                match result {
                    Ok(AppResponse::CreatePortableBackup(response)) => {
                        self.backup_error = None;
                        self.backup_result = Some(response.clone());
                    }
                    Err(envelope) => self.backup_error = Some(envelope.clone()),
                    Ok(_) => {}
                }
            }
            "inspect_restore_archive" => {
                self.inspect_pending = false;
                match result {
                    Ok(AppResponse::InspectRestoreArchive(response)) => {
                        self.restore_error = None;
                        // A new inspection supersedes the chosen mode when the
                        // mode it allowed is no longer allowed.
                        if !response.target_is_empty {
                            self.restore_mode = RestoreModeDto::Merge;
                        }
                        self.inspection = Some(response.clone());
                    }
                    Err(envelope) => self.restore_error = Some(envelope.clone()),
                    Ok(_) => {}
                }
            }
            "restore_portable_archive" => {
                self.restore_pending = false;
                match result {
                    Ok(AppResponse::RestorePortableArchive(response)) => {
                        self.restore_error = None;
                        self.restore_result = Some(response.clone());
                    }
                    Err(envelope) => self.restore_error = Some(envelope.clone()),
                    Ok(_) => {}
                }
            }
            _ => {}
        }
    }

    fn on_path_picked(&mut self, picked: PickedPath, path: &str, out: &mut SettingsOutput) {
        match picked {
            PickedPath::Folder(FolderPurpose::BackupDestination) => {
                if !self.backup_pending {
                    return;
                }
                // The secret leaves the section in the same statement that
                // raises the command.
                let password = std::mem::take(&mut self.backup_password);
                out.command(AppCommand::CreatePortableBackup(CreateBackupRequest {
                    destination_directory: path.to_owned(),
                    password: Self::password_argument(&password),
                    sensitive_profile: self.sensitive_profile,
                    // The "include voice profiles" control is the People/
                    // Settings work (VP-090); this screen never opts in, and
                    // the backend refuses the combination anyway unless the
                    // archive is encrypted (VP-078).
                    include_voice_profiles: false,
                }));
            }
            PickedPath::File(FilePurpose::RestoreArchive) => {
                if !self.restore_open {
                    return;
                }
                self.archive_path = Some(path.to_owned());
                self.inspection = None;
                self.restore_result = None;
                self.restore_error = None;
            }
            PickedPath::Folder(_) => {}
        }
    }

    fn on_open_archive(&mut self, path: &str) -> bool {
        // Opening the document opens the flow it belongs to, at its first
        // stage: the archive is chosen, and nothing has been read from it.
        // The dry run still has to be asked for, because an encrypted
        // archive needs a password and because no library is ever changed
        // without the review step in between.
        self.close_backup();
        self.close_restore();
        self.restore_open = true;
        self.archive_path = Some(path.to_owned());
        true
    }
}
