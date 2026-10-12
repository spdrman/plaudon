//! Interaction tests for the Backup and restore section (matrix row C5).
//!
//! Everything is asserted the way a screen reader or a keyboard user would:
//! find a control by its accessible name, act on it, then look at the intents
//! the section raised. No test reaches into the section's private state, and
//! the only way the secret is checked is the way an attacker would look for
//! it — in the accessibility tree and in `Debug`.

use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    ConflictPolicyDto, CreateBackupResponse, DeletionCategoryDto, InspectArchiveResponse,
    RestoreArchiveResponse, RestoreConflictDto, RestoreModeDto,
};
use plaudon_app::settings_presentation::format_bytes;
use plaudon_domain::error::{AppErrorCode, ErrorEnvelope};
use plaudon_domain::ids::DiagnosticId;
use plaudon_ui::settings::backup::BackupSection;
use plaudon_ui::settings::{
    FilePurpose, FolderPurpose, PickedPath, Section as _, SettingsData, SettingsIntent,
    SettingsOutput, VisualsTokens,
};
use plaudon_ui_testkit::a11y;

const PLAINTEXT_WARNING: &str =
    "⚠ This backup will NOT be encrypted. Anyone with the file can read your recordings.";
const ENCRYPTED_STATEMENT: &str = "✓ This backup will be encrypted with your password.";
const SENSITIVE_PROFILE: &str =
    "Use sensitive profile (slower but more secure; recommended for highly sensitive data)";
const PASSWORD_LOSS_WARNING: &str = "⚠ We cannot recover a lost password. Store it securely.";
const NEW_LIBRARY: &str = "New library (requires an empty target)";
const NEW_LIBRARY_UNAVAILABLE: &str =
    "– New library is not available: this library already contains recordings.";
const TOMBSTONE_CHECKBOX: &str = "Restore previously deleted recordings (overrides deletion)";
const ARCHIVE: &str = "/Users/someone/Backups/plaudon-2026-10-12.plaudon";
const PASSWORD: &str = "correct horse battery";

/// The section under test, with the cache it renders from and the intents it
/// raised this frame.
struct App {
    section: BackupSection,
    data: SettingsData,
    out: SettingsOutput,
}

impl App {
    fn new() -> Self {
        Self {
            section: BackupSection::default(),
            data: SettingsData::default(),
            out: SettingsOutput::new(),
        }
    }

    fn view(&mut self, ui: &mut egui::Ui) {
        let tokens = VisualsTokens::from_ui(ui);
        self.section.ui(ui, &tokens, &self.data, &mut self.out);
    }
}

fn harness() -> Harness<'static, App> {
    plaudon_ui_testkit::builder().build_ui_state(|ui, app: &mut App| app.view(ui), App::new())
}

/// Forget the intents of previous frames so an assertion describes one action.
fn clear(harness: &mut Harness<'_, App>) {
    harness.state_mut().out = SettingsOutput::new();
}

fn commands(harness: &Harness<'_, App>) -> Vec<&'static str> {
    harness.state().out.command_names()
}

/// The one command of `name` raised this frame.
fn command<'a>(harness: &'a Harness<'_, App>, name: &str) -> &'a AppCommand {
    harness
        .state()
        .out
        .intents
        .iter()
        .find_map(|intent| match intent {
            SettingsIntent::Command(command) if command.name() == name => Some(command),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {name} command was raised"))
}

fn focus_and_type(harness: &mut Harness<'_, App>, label: &str, text: &str) {
    harness.get_by_label(label).focus();
    harness.run();
    harness.get_by_label(label).type_text(text);
    harness.run();
}

/// Operate a control the way an assistive technology does: an AccessKit
/// `Click` action aimed at that node.
///
/// A modal body scrolls, so a pointer at a widget's layout position would
/// sometimes land on the backdrop instead; the action request cannot miss.
/// egui honours it only for an enabled widget, so a disabled control stays
/// unoperable — which is what the `new_library` case depends on.
fn click(harness: &mut Harness<'_, App>, label: &str) {
    harness.get_by_label(label).click_accesskit();
    harness.run();
}

fn has(harness: &Harness<'_, App>, label: &str) -> bool {
    harness.query_all_by_label(label).next().is_some()
}

fn open_backup_modal(harness: &mut Harness<'_, App>) {
    harness.run();
    click(harness, "Create backup…");
}

fn open_restore_modal(harness: &mut Harness<'_, App>) {
    harness.run();
    click(harness, "Restore from archive…");
}

/// Hand the section the archive the user chose in the native dialog.
fn pick_archive(harness: &mut Harness<'_, App>) {
    {
        let app = harness.state_mut();
        let mut out = SettingsOutput::new();
        app.section.on_path_picked(
            PickedPath::File(FilePurpose::RestoreArchive),
            ARCHIVE,
            &mut out,
        );
    }
    harness.run();
}

fn backup_response() -> CreateBackupResponse {
    CreateBackupResponse {
        file_name: "plaudon-2026-10-12.plaudon".to_owned(),
        encrypted: true,
        protection_disclosure: "This archive is encrypted with the password you chose. Plaudon \
cannot read it without that password and cannot recover it."
            .to_owned(),
        entry_count: 128,
        total_byte_length: 2_097_152,
        file_byte_length: 1_572_864,
        integrity_digest: "sha256:4f1c0a9e2b".to_owned(),
        integrity_note: "The digest covers the manifest, which covers every entry. It proves the \
archive is intact; it does not prove who made it."
            .to_owned(),
        manifest_schema_version: "archive/v1".to_owned(),
        nonportable_settings: vec!["OpenRouter key (stays in the system keychain)".to_owned()],
    }
}

fn inspection(target_is_empty: bool, conflicts: Vec<RestoreConflictDto>) -> InspectArchiveResponse {
    InspectArchiveResponse {
        encrypted: true,
        manifest_schema_version: "archive/v1".to_owned(),
        format_version: 1,
        created_at: "2026-10-01T09:30:00Z".to_owned(),
        source_os: "macOS 27.0".to_owned(),
        entries_by_namespace: vec![DeletionCategoryDto {
            label: "Recordings".to_owned(),
            count: 12,
        }],
        total_byte_length: 4_194_304,
        payloads_verified: true,
        integrity_digest: "sha256:0b77ac".to_owned(),
        integrity_note: "The digest covers the manifest, which covers every entry.".to_owned(),
        nonportable_settings: Vec::new(),
        conflicts,
        target_is_empty,
    }
}

fn conflict() -> RestoreConflictDto {
    RestoreConflictDto {
        entry_path: "recordings/0199a0e2.json".to_owned(),
        reason: "A recording with this identifier already exists.".to_owned(),
    }
}

fn envelope(message: &str) -> ErrorEnvelope {
    ErrorEnvelope {
        code: AppErrorCode::InternalError,
        message: message.to_owned(),
        retryable: false,
        action: None,
        diagnostic_id: DiagnosticId::new(),
    }
}

/// Deliver a command outcome to the section, the way the screen does.
fn respond(
    harness: &mut Harness<'_, App>,
    command: &'static str,
    result: &Result<AppResponse, ErrorEnvelope>,
) {
    let app = harness.state_mut();
    let mut out = SettingsOutput::new();
    app.section.on_response(command, result, &mut out);
    harness.run();
}

#[test]
fn the_protection_disclosure_follows_the_password_field() {
    let mut harness = harness();
    open_backup_modal(&mut harness);

    // No password: the archive is plaintext, and the modal says so before
    // anything is written.
    harness.get_by_label(PLAINTEXT_WARNING);
    assert!(
        !has(&harness, SENSITIVE_PROFILE),
        "the Argon2id profile only applies to an encrypted archive"
    );
    assert!(!has(&harness, PASSWORD_LOSS_WARNING));

    focus_and_type(&mut harness, "Password (optional)", PASSWORD);

    harness.get_by_label(ENCRYPTED_STATEMENT);
    harness.get_by_label(PASSWORD_LOSS_WARNING);
    harness.get_by_label(SENSITIVE_PROFILE);
    assert!(
        !has(&harness, PLAINTEXT_WARNING),
        "the plaintext warning must not survive a password being typed"
    );
}

#[test]
fn creating_a_backup_picks_a_folder_then_raises_the_command_and_forgets_the_password() {
    let mut harness = harness();
    open_backup_modal(&mut harness);
    focus_and_type(&mut harness, "Password (optional)", PASSWORD);
    click(&mut harness, SENSITIVE_PROFILE);

    clear(&mut harness);
    click(&mut harness, "Create backup now");

    assert!(
        commands(&harness).is_empty(),
        "nothing is written before a destination exists"
    );
    assert!(matches!(
        harness.state().out.intents.as_slice(),
        [SettingsIntent::PickFolder(FolderPurpose::BackupDestination)]
    ));

    clear(&mut harness);
    {
        let app = harness.state_mut();
        let mut out = SettingsOutput::new();
        app.section.on_path_picked(
            PickedPath::Folder(FolderPurpose::BackupDestination),
            "/tmp/backups",
            &mut out,
        );
        app.out = out;
    }
    harness.run();

    assert_eq!(commands(&harness), ["create_portable_backup"]);
    let AppCommand::CreatePortableBackup(request) = command(&harness, "create_portable_backup")
    else {
        panic!("wrong command variant");
    };
    assert_eq!(request.destination_directory, "/tmp/backups");
    assert_eq!(request.password.as_deref(), Some(PASSWORD));
    assert!(request.sensitive_profile);

    // The draft is gone: the modal has fallen back to the plaintext warning,
    // nothing in the accessibility tree announces the secret, and `Debug`
    // does not carry it either.
    harness.get_by_label(PLAINTEXT_WARNING);
    for node in a11y::nodes(&harness) {
        if let Some(text) = node.announced() {
            assert!(!text.contains(PASSWORD), "{text:?} announces the password");
        }
    }
    let rendered = format!("{:?}", harness.state().section);
    assert!(!rendered.contains(PASSWORD), "{rendered}");
}

#[test]
fn the_result_panel_renders_the_backends_own_sentences() {
    let mut harness = harness();
    open_backup_modal(&mut harness);
    let response = backup_response();
    respond(
        &mut harness,
        "create_portable_backup",
        &Ok(AppResponse::CreatePortableBackup(response.clone())),
    );

    harness.get_by_label("✓ Backup created successfully");
    harness.get_by_label(&response.file_name);
    harness.get_by_label("128");
    harness.get_by_label(&format_bytes(Some(response.total_byte_length)));
    harness.get_by_label(&format_bytes(Some(response.file_byte_length)));
    // Verbatim, both of them.
    harness.get_by_label(&response.protection_disclosure);
    harness.get_by_label(&response.integrity_note);
    harness.get_by_label(&response.integrity_digest);
    harness.get_by_label(&response.manifest_schema_version);
    harness.get_by_label("Settings not included");
    harness.get_by_label(&format!("• {}", response.nonportable_settings[0]));

    clear(&mut harness);
    click(&mut harness, "Copy integrity digest");
    assert!(harness
        .state()
        .out
        .intents
        .iter()
        .any(|intent| matches!(intent, SettingsIntent::CopyText(text) if *text == response.integrity_digest)));
}

#[test]
fn a_failed_backup_shows_the_envelopes_own_message() {
    let mut harness = harness();
    open_backup_modal(&mut harness);
    let failure = envelope("The destination folder is not writable.");
    respond(
        &mut harness,
        "create_portable_backup",
        &Err(failure.clone()),
    );

    harness.get_by_label(&format!("✕ {}", failure.message));
}

#[test]
fn nothing_can_be_restored_before_the_archive_has_been_inspected() {
    let mut harness = harness();
    open_restore_modal(&mut harness);

    harness.get_by_label("Inspect archive");
    assert!(
        !has(&harness, "Restore"),
        "a restore must not be offered before a dry run"
    );

    clear(&mut harness);
    click(&mut harness, "Choose archive");
    assert!(matches!(
        harness.state().out.intents.as_slice(),
        [SettingsIntent::PickFile(FilePurpose::RestoreArchive)]
    ));

    pick_archive(&mut harness);
    harness.get_by_label(ARCHIVE);
    assert!(
        !has(&harness, "Restore"),
        "choosing a file is not inspecting it"
    );

    focus_and_type(&mut harness, "Password (if encrypted)", PASSWORD);
    clear(&mut harness);
    click(&mut harness, "Inspect archive");

    assert_eq!(commands(&harness), ["inspect_restore_archive"]);
    let AppCommand::InspectRestoreArchive(request) = command(&harness, "inspect_restore_archive")
    else {
        panic!("wrong command variant");
    };
    assert_eq!(request.archive_path, ARCHIVE);
    assert_eq!(request.password.as_deref(), Some(PASSWORD));

    respond(
        &mut harness,
        "inspect_restore_archive",
        &Ok(AppResponse::InspectRestoreArchive(inspection(
            true,
            Vec::new(),
        ))),
    );
    harness.get_by_label("Restore");
}

#[test]
fn the_inspection_is_rendered_in_full_and_new_library_says_why_it_is_unavailable() {
    let mut harness = harness();
    open_restore_modal(&mut harness);
    pick_archive(&mut harness);
    let dry_run = inspection(false, vec![conflict()]);
    respond(
        &mut harness,
        "inspect_restore_archive",
        &Ok(AppResponse::InspectRestoreArchive(dry_run.clone())),
    );

    harness.get_by_label(&dry_run.created_at);
    harness.get_by_label(&dry_run.source_os);
    harness.get_by_label("Yes");
    harness.get_by_label(&dry_run.manifest_schema_version);
    harness.get_by_label(&format_bytes(Some(dry_run.total_byte_length)));
    harness.get_by_label("✓ Payload digests verified: yes");
    harness.get_by_label(&dry_run.integrity_note);
    // Namespace counts, as counts and not as a single total.
    harness.get_by_label("12");
    harness.get_by_label("⚠ The following records already exist in this library:");
    harness.get_by_label(&format!(
        "{}: {}",
        dry_run.conflicts[0].entry_path, dry_run.conflicts[0].reason
    ));

    // `new_library` is refused, and the refusal says why.
    harness.get_by_label(NEW_LIBRARY_UNAVAILABLE);
    assert!(
        harness
            .get_by_label(NEW_LIBRARY)
            .accesskit_node()
            .is_disabled(),
        "an unavailable mode must not be operable"
    );
    click(&mut harness, NEW_LIBRARY);
    clear(&mut harness);
    click(&mut harness, "Restore");

    let AppCommand::RestorePortableArchive(request) = command(&harness, "restore_portable_archive")
    else {
        panic!("wrong command variant");
    };
    assert_eq!(
        request.mode,
        RestoreModeDto::Merge,
        "an unavailable mode must not become the chosen one"
    );
}

#[test]
fn restoring_carries_the_chosen_mode_policy_and_tombstone_decision() {
    let mut harness = harness();
    open_restore_modal(&mut harness);
    pick_archive(&mut harness);
    focus_and_type(&mut harness, "Password (if encrypted)", PASSWORD);
    respond(
        &mut harness,
        "inspect_restore_archive",
        &Ok(AppResponse::InspectRestoreArchive(inspection(
            true,
            Vec::new(),
        ))),
    );

    click(&mut harness, NEW_LIBRARY);
    click(&mut harness, "Abort on any conflict");
    click(&mut harness, TOMBSTONE_CHECKBOX);
    harness.get_by_label(
        "⚠ Recordings you deleted on this machine will be restored, overriding that deletion.",
    );

    clear(&mut harness);
    click(&mut harness, "Restore");

    assert_eq!(commands(&harness), ["restore_portable_archive"]);
    let AppCommand::RestorePortableArchive(request) = command(&harness, "restore_portable_archive")
    else {
        panic!("wrong command variant");
    };
    assert_eq!(request.archive_path, ARCHIVE);
    assert_eq!(request.password.as_deref(), Some(PASSWORD));
    assert_eq!(request.mode, RestoreModeDto::NewLibrary);
    assert_eq!(request.on_conflict, ConflictPolicyDto::Fail);
    assert!(request.restore_tombstoned);

    respond(
        &mut harness,
        "restore_portable_archive",
        &Ok(AppResponse::RestorePortableArchive(
            RestoreArchiveResponse {
                recordings_restored: 12,
                transcripts_restored: 9,
                outputs_restored: 3,
                sources_restored: 1,
                media_committed: 12,
                media_reused: 0,
                conflicts: vec![conflict()],
            },
        )),
    );
    harness.get_by_label("✓ Archive restored successfully");
    harness.get_by_label("Recordings restored:");
    harness.get_by_label("Records that were not applied");
}

#[test]
fn every_control_of_the_backup_modal_is_reachable_by_keyboard() {
    let mut harness = harness();
    open_backup_modal(&mut harness);
    focus_and_type(&mut harness, "Password (optional)", PASSWORD);

    let order = a11y::focus_order(&mut harness, 24);
    for label in [
        "Password (optional)",
        SENSITIVE_PROFILE,
        "Create backup now",
        "Cancel",
    ] {
        assert!(
            order.iter().any(|reached| reached == label),
            "{label:?} is unreachable by keyboard; Tab reached {order:?}"
        );
    }
}

#[test]
fn every_control_of_the_restore_modal_is_reachable_by_keyboard() {
    let mut harness = harness();
    open_restore_modal(&mut harness);
    pick_archive(&mut harness);

    let order = a11y::focus_order(&mut harness, 24);
    for label in [
        "Choose different archive",
        "Password (if encrypted)",
        "Inspect archive",
        "Cancel",
    ] {
        assert!(
            order.iter().any(|reached| reached == label),
            "{label:?} is unreachable by keyboard; Tab reached {order:?}"
        );
    }

    respond(
        &mut harness,
        "inspect_restore_archive",
        &Ok(AppResponse::InspectRestoreArchive(inspection(
            true,
            Vec::new(),
        ))),
    );
    let order = a11y::focus_order(&mut harness, 32);
    for label in [
        NEW_LIBRARY,
        "Merge into existing library",
        "Skip conflicting records",
        "Abort on any conflict",
        TOMBSTONE_CHECKBOX,
        "Restore",
        "Back",
    ] {
        assert!(
            order.iter().any(|reached| reached == label),
            "{label:?} is unreachable by keyboard; Tab reached {order:?}"
        );
    }
}
