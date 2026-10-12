//! Interaction tests for the appearance, privacy, AI-key, consent,
//! notification, storage and navigation sections of the Settings screen
//! (matrix rows C1–C4, A9, J16).
//!
//! Every assertion is made the way a screen reader or a keyboard user would
//! make it: find a control by its accessible name, act on it, and then look at
//! what the section asked the host to do. No test reaches into a section's
//! private state to decide what happened, and no test asserts a sentence the
//! application owns by retyping it — it compares against the constant.

use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    Appearance, Capabilities, ConsentSummary, Contrast, HardwareInfo, LocalOnlyModeResponse,
    NotificationCategory, OsInfo, OsNotificationPermission, PermissionState, Platform, Preferences,
    StatusResponse, StorageCapability,
};
use plaudon_app::settings_presentation::{
    format_bytes, local_only_status, notification_permission_copy, openrouter_key_status,
    storage_facts, CONSENT_EMPTY, LOCAL_ONLY_TOGGLE, OPENROUTER_REMOVE_DISCLOSURE,
    STORAGE_LOCATION_FIXED, STORAGE_UNAVAILABLE,
};
use plaudon_domain::error::{AppErrorCode, ErrorEnvelope};
use plaudon_domain::ids::DiagnosticId;
use plaudon_ui::settings::ai_key::AiKeySection;
use plaudon_ui::settings::appearance::AppearanceSection;
use plaudon_ui::settings::consent::ConsentSection;
use plaudon_ui::settings::links::LinksSection;
use plaudon_ui::settings::notifications::NotificationsSection;
use plaudon_ui::settings::privacy::PrivacySection;
use plaudon_ui::settings::storage::StorageSection;
use plaudon_ui::settings::{
    Section, SettingsData, SettingsDestination, SettingsIntent, SettingsOutput, VisualsTokens,
};
use plaudon_ui_testkit::a11y;

/* -------------------------------------------------------------- harness */

/// One section under test, with the cache it renders from and everything it
/// has asked the host to do.
struct SectionApp<S: Section> {
    section: S,
    data: SettingsData,
    out: SettingsOutput,
}

impl<S: Section> SectionApp<S> {
    fn view(&mut self, ui: &mut egui::Ui) {
        let tokens = VisualsTokens::from_ui(ui);
        self.section.ui(ui, &tokens, &self.data, &mut self.out);
    }

    /// Forget what has been asked for so far, so the next assertion sees only
    /// what the next interaction produced.
    fn forget(&mut self) {
        let _ = self.out.drain();
    }

    /// Fold a command outcome in exactly as `SettingsScreen` would.
    fn respond(&mut self, command: &'static str, result: &Result<AppResponse, ErrorEnvelope>) {
        self.data.absorb(result);
        let mut out = SettingsOutput::new();
        self.section.on_response(command, result, &mut out);
        self.out.intents.extend(out.drain());
    }

    /// Every command raised so far, as `(name, command)`.
    fn commands(&self) -> Vec<&AppCommand> {
        self.out
            .intents
            .iter()
            .filter_map(|intent| match intent {
                SettingsIntent::Command(command) => Some(command),
                _ => None,
            })
            .collect()
    }
}

fn harness<S: Section + 'static>(
    section: S,
    data: SettingsData,
) -> Harness<'static, SectionApp<S>> {
    plaudon_ui_testkit::builder().build_ui_state(
        |ui, app: &mut SectionApp<S>| app.view(ui),
        SectionApp {
            section,
            data,
            out: SettingsOutput::new(),
        },
    )
}

/// Whether anything in the frame announces `text`, as a label or as the text
/// of a static label.
fn announces<S: Section>(harness: &Harness<'_, SectionApp<S>>, text: &str) -> bool {
    a11y::nodes(harness)
        .iter()
        .any(|node| node.announced() == Some(text))
}

/// Whether anything in the frame announces something containing `fragment`.
fn announces_containing<S: Section>(harness: &Harness<'_, SectionApp<S>>, fragment: &str) -> bool {
    a11y::nodes(harness)
        .iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .any(|text| text.contains(fragment))
}

/* ------------------------------------------------------------- fixtures */

fn storage() -> StorageCapability {
    StorageCapability {
        library_path: "/Users/tester/Plaudon".to_owned(),
        free_bytes: 42 * 1024 * 1024 * 1024,
        total_bytes: 512 * 1024 * 1024 * 1024,
    }
}

fn capabilities() -> Capabilities {
    Capabilities {
        os: OsInfo {
            platform: Platform::Macos,
            version: "26.0".to_owned(),
        },
        hardware: HardwareInfo {
            arch: "aarch64".to_owned(),
            logical_cores: 10,
            total_memory_bytes: 32 * 1024 * 1024 * 1024,
        },
        engines: Vec::new(),
        storage: storage(),
        integrations: std::collections::BTreeMap::new(),
        permissions: std::collections::BTreeMap::new(),
        preferences: preferences(),
        openrouter_key_present: false,
        cloud_consents: Vec::new(),
    }
}

fn preferences() -> Preferences {
    plaudon_app::backend::default_preferences()
}

fn data_with(capabilities: Capabilities) -> SettingsData {
    SettingsData {
        capabilities: Some(capabilities),
        ..SettingsData::default()
    }
}

fn grant(consent_id: &str, model_id: &str, revoked_at: Option<&str>) -> ConsentSummary {
    ConsentSummary {
        consent_id: consent_id.to_owned(),
        purpose: "transcription".to_owned(),
        content_type: "audio_bytes".to_owned(),
        scope_kind: "model_policy".to_owned(),
        recording_id: None,
        model_id: model_id.to_owned(),
        downstream_provider: Some("OpenAI".to_owned()),
        required_retention: Some("zero_retention".to_owned()),
        disclosure_version: "2026-01-01".to_owned(),
        granted_at: "2026-02-01T10:00:00Z".to_owned(),
        expires_at: None,
        revoked_at: revoked_at.map(str::to_owned),
    }
}

fn envelope(message: &str) -> ErrorEnvelope {
    ErrorEnvelope {
        code: AppErrorCode::InvalidArgument,
        message: message.to_owned(),
        retryable: true,
        action: None,
        diagnostic_id: DiagnosticId::new(),
    }
}

/* ----------------------------------------------------------- appearance */

#[test]
fn choosing_dark_raises_exactly_one_set_appearance() {
    let mut harness = harness(AppearanceSection, data_with(capabilities()));
    harness.run();

    harness.get_by_label("Dark").click();
    harness.run();

    assert_eq!(harness.state().out.command_names(), ["set_appearance"]);
    let commands = harness.state().commands();
    match commands.first() {
        Some(AppCommand::SetAppearance(request)) => {
            assert_eq!(request.appearance, Appearance::Dark);
            // The contrast travels unchanged; the command carries both.
            assert_eq!(request.contrast, Contrast::Standard);
        }
        other => panic!("expected set_appearance, got {other:?}"),
    }
}

#[test]
fn appearance_selects_nothing_before_capabilities_arrive() {
    let mut harness = harness(AppearanceSection, SettingsData::default());
    harness.run();

    let selected: Vec<String> = a11y::nodes(&harness)
        .into_iter()
        .filter(|node| node.role == "RadioButton")
        .filter_map(|node| node.label)
        .collect();
    assert_eq!(selected.len(), 5, "all five choices are offered");

    harness.get_by_label("Dark").click();
    harness.run();
    assert!(
        harness.state().out.command_names().is_empty(),
        "a theme cannot be chosen before the stored preferences are known"
    );
}

#[test]
fn appearance_controls_are_all_keyboard_reachable() {
    let mut harness = harness(AppearanceSection, data_with(capabilities()));
    harness.run();

    let order = a11y::focus_order(&mut harness, 12);
    for label in [
        "Follow the system",
        "Light",
        "Dark",
        "Standard",
        "High contrast",
    ] {
        assert!(
            order.contains(&label.to_owned()),
            "{label} not in {order:?}"
        );
    }
}

/* -------------------------------------------------------------- privacy */

#[test]
fn local_only_toggle_raises_a_confirmed_change() {
    let mut harness = harness(PrivacySection::default(), data_with(capabilities()));
    harness.run();

    assert!(
        !announces_containing(&harness, "Enforced policy:"),
        "the enforced policy is not known until the gate answers"
    );
    let off = local_only_status(false);
    assert!(announces(&harness, &format!("{} {}", off.glyph, off.label)));

    harness.get_by_label(LOCAL_ONLY_TOGGLE).click();
    harness.run();

    assert_eq!(harness.state().out.command_names(), ["set_local_only_mode"]);
    match harness.state().commands().first() {
        Some(AppCommand::SetLocalOnlyMode(request)) => {
            assert!(request.enabled);
            assert!(request.confirmed, "§15.2 requires explicit confirmation");
        }
        other => panic!("expected set_local_only_mode, got {other:?}"),
    }
}

#[test]
fn the_enforced_policy_appears_only_after_the_gate_answers() {
    let mut harness = harness(PrivacySection::default(), data_with(capabilities()));
    harness.run();
    assert!(!announces_containing(&harness, "Enforced policy:"));

    let response = Ok(AppResponse::SetLocalOnlyMode(LocalOnlyModeResponse {
        enabled: false,
        description: "Outbound requests are limited to the hosts below.".to_owned(),
        allowed_hosts: vec!["openrouter.ai".to_owned(), "api.plaud.ai".to_owned()],
    }));
    harness
        .state_mut()
        .respond("set_local_only_mode", &response);
    harness.run();

    assert!(announces(
        &harness,
        "Enforced policy: Outbound requests are limited to the hosts below."
    ));
    assert!(announces(&harness, "openrouter.ai"));
    assert!(announces(&harness, "api.plaud.ai"));
    let on = local_only_status(false);
    assert!(announces(&harness, &format!("{} {}", on.glyph, on.label)));
}

#[test]
fn an_empty_host_list_says_no_hosts_are_reachable() {
    let mut harness = harness(PrivacySection::default(), data_with(capabilities()));
    harness.run();

    let response = Ok(AppResponse::SetLocalOnlyMode(LocalOnlyModeResponse {
        enabled: true,
        description: "All outbound network access is refused.".to_owned(),
        allowed_hosts: Vec::new(),
    }));
    harness
        .state_mut()
        .respond("set_local_only_mode", &response);
    harness.run();

    assert!(announces(
        &harness,
        "Enforced policy: All outbound network access is refused."
    ));
    assert!(announces(&harness, "No hosts are reachable."));
}

#[test]
fn a_rejected_change_keeps_the_envelope_and_offers_a_retry() {
    let mut harness = harness(PrivacySection::default(), data_with(capabilities()));
    harness.run();

    harness.get_by_label(LOCAL_ONLY_TOGGLE).click();
    harness.run();
    harness.state_mut().forget();

    let rejected = Err(envelope("The egress gate refused the change."));
    harness
        .state_mut()
        .respond("set_local_only_mode", &rejected);
    harness.run();

    assert!(
        announces(&harness, "✕ The egress gate refused the change."),
        "the backend's own message is rendered, with a glyph"
    );

    harness
        .get_by_label("Try changing local-only mode again")
        .click();
    harness.run();

    assert_eq!(harness.state().out.command_names(), ["set_local_only_mode"]);
    match harness.state().commands().first() {
        Some(AppCommand::SetLocalOnlyMode(request)) => {
            assert!(request.enabled, "the retry asks for what was asked before");
            assert!(request.confirmed);
        }
        other => panic!("expected set_local_only_mode, got {other:?}"),
    }
}

#[test]
fn the_privacy_toggle_is_keyboard_reachable() {
    let mut harness = harness(PrivacySection::default(), data_with(capabilities()));
    harness.run();

    let order = a11y::focus_order(&mut harness, 8);
    assert!(order.contains(&LOCAL_ONLY_TOGGLE.to_owned()), "{order:?}");
}

/* --------------------------------------------------------------- AI key */

/// Not a real credential; it only has to be recognisable if it leaks.
const KEY: &str = "sk-or-v1-test-secret-value";

#[test]
fn saving_a_key_clears_the_field_and_leaks_it_nowhere() {
    let mut harness = harness(AiKeySection::default(), data_with(capabilities()));
    harness.run();

    let absent = openrouter_key_status(false);
    assert!(announces(
        &harness,
        &format!("{} {}", absent.glyph, absent.label)
    ));

    harness.get_by_label("Set OpenRouter API key").click();
    harness.run();
    harness
        .get_by_label("Set OpenRouter API key")
        .type_text(KEY);
    harness.run();

    assert!(
        !format!("{:?}", harness.state().section).contains(KEY),
        "the draft must not print itself"
    );

    harness.get_by_label("Set key").click();
    harness.run();

    assert_eq!(harness.state().out.command_names(), ["set_openrouter_key"]);

    let field = harness.get_by_label("Set OpenRouter API key");
    assert_eq!(
        field.value().unwrap_or_default(),
        String::new(),
        "the draft is emptied in the same breath as the command"
    );
    assert!(!format!("{:?}", harness.state().section).contains(KEY));
    assert!(
        !a11y::nodes(&harness).iter().any(|node| {
            node.label.as_deref().is_some_and(|text| text.contains(KEY))
                || node.value.as_deref().is_some_and(|text| text.contains(KEY))
        }),
        "the key appears nowhere in the accessibility tree"
    );
    assert!(
        !format!("{:?}", harness.state().out.intents).contains(KEY),
        "nor in the intent the host will drain"
    );
}

#[test]
fn an_empty_field_submits_nothing() {
    let mut harness = harness(AiKeySection::default(), data_with(capabilities()));
    harness.run();

    harness.get_by_label("Set key").click();
    harness.run();

    assert!(harness.state().out.command_names().is_empty());
}

#[test]
fn removing_the_key_needs_the_disclosed_confirmation() {
    let mut capabilities = capabilities();
    capabilities.openrouter_key_present = true;
    let mut harness = harness(AiKeySection::default(), data_with(capabilities));
    harness.run();

    let present = openrouter_key_status(true);
    assert!(announces(
        &harness,
        &format!("{} {}", present.glyph, present.label)
    ));
    assert!(
        a11y::has_label(&harness, "Replace OpenRouter API key"),
        "the field says replace once a key is stored"
    );

    harness.get_by_label("Remove key").click();
    harness.run();

    assert!(
        harness.state().out.command_names().is_empty(),
        "the trigger only opens the confirmation"
    );
    assert!(announces(&harness, OPENROUTER_REMOVE_DISCLOSURE));
    assert!(announces(&harness, "Remove the OpenRouter key?"));

    harness.get_by_label("Remove key").click();
    harness.run();

    assert_eq!(
        harness.state().out.command_names(),
        ["remove_openrouter_key"]
    );
}

#[test]
fn cancelling_the_removal_raises_nothing() {
    let mut capabilities = capabilities();
    capabilities.openrouter_key_present = true;
    let mut harness = harness(AiKeySection::default(), data_with(capabilities));
    harness.run();

    harness.get_by_label("Remove key").click();
    harness.run();
    harness.get_by_label("Cancel").click();
    harness.run();

    assert!(harness.state().out.command_names().is_empty());
    assert!(!announces(&harness, OPENROUTER_REMOVE_DISCLOSURE));
}

#[test]
fn a_saved_key_asks_for_fresh_capabilities() {
    let mut harness = harness(AiKeySection::default(), data_with(capabilities()));
    harness.run();

    let saved = Ok(AppResponse::SetOpenRouterKey(StatusResponse::ok()));
    harness.state_mut().respond("set_openrouter_key", &saved);

    assert!(harness
        .state()
        .out
        .intents
        .iter()
        .any(|intent| matches!(intent, SettingsIntent::RefreshCapabilities)));
}

#[test]
fn every_ai_key_control_is_keyboard_reachable() {
    let mut capabilities = capabilities();
    capabilities.openrouter_key_present = true;
    let mut harness = harness(AiKeySection::default(), data_with(capabilities));
    harness.run();

    let order = a11y::focus_order(&mut harness, 10);
    for label in ["Replace OpenRouter API key", "Replace key", "Remove key"] {
        assert!(
            order.contains(&label.to_owned()),
            "{label} not in {order:?}"
        );
    }
}

/* -------------------------------------------------------------- consent */

#[test]
fn a_revoked_grant_stays_listed_and_an_active_one_can_be_revoked() {
    let mut capabilities = capabilities();
    capabilities.cloud_consents = vec![
        grant("consent-active", "anthropic/claude-3-5-sonnet", None),
        grant(
            "consent-gone",
            "openai/gpt-4o-mini",
            Some("2026-03-04T09:00:00Z"),
        ),
    ];
    let mut harness = harness(ConsentSection::default(), data_with(capabilities));
    harness.run();

    assert!(
        announces(&harness, "openai/gpt-4o-mini"),
        "revoked, still listed"
    );
    assert!(announces(&harness, "✕ Revoked"));
    assert!(announces(&harness, "✓ Active"));
    assert!(announces(&harness, "Revoked:"), "with the time it happened");
    assert!(
        harness
            .query_by_label("Revoke consent for openai/gpt-4o-mini")
            .is_none(),
        "a revoked grant offers no revoke button"
    );

    harness
        .get_by_label("Revoke consent for anthropic/claude-3-5-sonnet")
        .click();
    harness.run();

    assert_eq!(
        harness.state().out.command_names(),
        ["revoke_cloud_consent"]
    );
    match harness.state().commands().first() {
        Some(AppCommand::RevokeCloudConsent(request)) => {
            assert_eq!(request.consent_id, "consent-active");
        }
        other => panic!("expected revoke_cloud_consent, got {other:?}"),
    }
}

#[test]
fn an_empty_register_says_so_only_once_it_has_been_read() {
    let mut unread = harness(ConsentSection::default(), SettingsData::default());
    unread.run();
    assert!(!announces(&unread, CONSENT_EMPTY));

    let mut read = harness(ConsentSection::default(), data_with(capabilities()));
    read.run();
    assert!(announces(&read, CONSENT_EMPTY));
}

#[test]
fn the_revoke_button_is_keyboard_reachable() {
    let mut capabilities = capabilities();
    capabilities.cloud_consents = vec![grant("consent-active", "anthropic/claude", None)];
    let mut harness = harness(ConsentSection::default(), data_with(capabilities));
    harness.run();

    let order = a11y::focus_order(&mut harness, 8);
    assert!(
        order.contains(&"Revoke consent for anthropic/claude".to_owned()),
        "{order:?}"
    );
}

/* -------------------------------------------------------- notifications */

#[test]
fn the_master_switch_sends_every_category() {
    let mut harness = harness(NotificationsSection, data_with(capabilities()));
    harness.run();

    harness.get_by_label("Show desktop notifications").click();
    harness.run();

    assert_eq!(
        harness.state().out.command_names(),
        ["set_notification_settings"]
    );
    match harness.state().commands().first() {
        Some(AppCommand::SetNotificationSettings(request)) => {
            assert!(request.settings.enabled);
            for category in NotificationCategory::ALL {
                assert!(
                    request.settings.categories.contains_key(category),
                    "{category:?} must be present; the command rejects a partial map"
                );
            }
        }
        other => panic!("expected set_notification_settings, got {other:?}"),
    }
}

#[test]
fn a_category_change_carries_the_whole_map() {
    let mut capabilities = capabilities();
    capabilities.preferences.notifications.enabled = true;
    let mut harness = harness(NotificationsSection, data_with(capabilities));
    harness.run();

    harness.get_by_label("A transcription finished").click();
    harness.run();

    match harness.state().commands().first() {
        Some(AppCommand::SetNotificationSettings(request)) => {
            assert!(request.settings.enabled);
            assert_eq!(request.settings.categories.len(), 4);
            assert_eq!(
                request
                    .settings
                    .categories
                    .get(&NotificationCategory::TranscriptionFinished),
                Some(&true)
            );
            assert_eq!(
                request
                    .settings
                    .categories
                    .get(&NotificationCategory::ImportFinished),
                Some(&false)
            );
        }
        other => panic!("expected set_notification_settings, got {other:?}"),
    }
}

#[test]
fn the_categories_are_inert_while_the_master_switch_is_off() {
    let mut harness = harness(NotificationsSection, data_with(capabilities()));
    harness.run();

    harness.get_by_label("An import finished").click();
    harness.run();

    assert!(harness.state().out.command_names().is_empty());
}

#[test]
fn a_denied_permission_says_the_history_is_still_complete() {
    let mut capabilities = capabilities();
    capabilities
        .permissions
        .insert("notifications".to_owned(), PermissionState::Denied);
    let mut harness = harness(NotificationsSection, data_with(capabilities));
    harness.run();

    let copy = notification_permission_copy(Some(OsNotificationPermission::Denied));
    assert!(announces(&harness, copy));
    assert!(
        copy.contains("activity history still records every event"),
        "the backend copy is what carries the promise"
    );
}

#[test]
fn the_permission_the_backend_last_reported_wins() {
    let mut capabilities = capabilities();
    capabilities
        .permissions
        .insert("notifications".to_owned(), PermissionState::NotRequested);
    let mut data = data_with(capabilities);
    data.notification_permission = Some(OsNotificationPermission::Unsupported);
    let mut harness = harness(NotificationsSection, data);
    harness.run();

    assert!(announces(
        &harness,
        notification_permission_copy(Some(OsNotificationPermission::Unsupported))
    ));
}

#[test]
fn every_notification_switch_is_keyboard_reachable() {
    let mut capabilities = capabilities();
    capabilities.preferences.notifications.enabled = true;
    let mut harness = harness(NotificationsSection, data_with(capabilities));
    harness.run();

    let order = a11y::focus_order(&mut harness, 12);
    for label in [
        "Show desktop notifications",
        "An import finished",
        "A transcription finished",
        "Something needs your attention",
        "The cloud budget was exhausted",
    ] {
        assert!(
            order.contains(&label.to_owned()),
            "{label} not in {order:?}"
        );
    }
}

/* -------------------------------------------------------------- storage */

#[test]
fn storage_says_unknown_rather_than_zero() {
    // The honest-unknown rule, as the presentation layer states it.
    assert_eq!(format_bytes(None), "unknown");

    let mut unread = harness(StorageSection, SettingsData::default());
    unread.run();
    assert!(announces(&unread, STORAGE_UNAVAILABLE));
    assert!(
        !announces_containing(&unread, "Free space"),
        "no figure is invented before the capability answer"
    );
    assert!(!announces_containing(&unread, "0 bytes"));

    let mut read = harness(StorageSection, data_with(capabilities()));
    read.run();
    for fact in storage_facts(Some(&storage())) {
        assert!(
            announces(&read, &format!("{}:", fact.label)),
            "missing fact {}",
            fact.label
        );
        assert!(
            announces(&read, &fact.value),
            "missing value {}",
            fact.value
        );
    }
    assert!(announces(&read, STORAGE_LOCATION_FIXED));
}

#[test]
fn showing_the_library_folder_asks_the_host_to_reveal_it() {
    let mut harness = harness(StorageSection, data_with(capabilities()));
    harness.run();

    harness.get_by_label("Show in file manager").click();
    harness.run();

    let revealed = harness.state().out.intents.iter().any(|intent| {
        matches!(intent, SettingsIntent::RevealInFolder(path) if path == &storage().library_path)
    });
    assert!(revealed, "{:?}", harness.state().out.intents);
}

#[test]
fn a_permission_never_requested_is_not_reported_as_refused() {
    let mut capabilities = capabilities();
    capabilities
        .permissions
        .insert("microphone".to_owned(), PermissionState::NotRequested);
    capabilities
        .permissions
        .insert("notifications".to_owned(), PermissionState::Granted);
    let mut harness = harness(StorageSection, data_with(capabilities));
    harness.run();

    assert!(announces(&harness, "microphone:"));
    assert!(announces(&harness, "– Not requested"));
    assert!(announces(&harness, "✓ Granted"));
    assert!(!announces_containing(&harness, "✕ Denied"));
}

#[test]
fn the_storage_action_is_keyboard_reachable() {
    let mut harness = harness(StorageSection, data_with(capabilities()));
    harness.run();

    let order = a11y::focus_order(&mut harness, 8);
    assert!(
        order.contains(&"Show in file manager".to_owned()),
        "{order:?}"
    );
}

/* ---------------------------------------------------------------- links */

#[test]
fn the_navigation_entries_route_where_the_react_screen_did() {
    let mut harness = harness(LinksSection, SettingsData::default());
    harness.run();

    harness.get_by_label("Open PLAUD and sources").click();
    harness.run();
    harness.get_by_label("Open activity history").click();
    harness.run();

    let destinations: Vec<SettingsDestination> = harness
        .state()
        .out
        .intents
        .iter()
        .filter_map(|intent| match intent {
            SettingsIntent::Navigate(destination) => Some(*destination),
            _ => None,
        })
        .collect();
    assert_eq!(
        destinations,
        [SettingsDestination::Sources, SettingsDestination::Activity]
    );
}

#[test]
fn both_navigation_buttons_are_keyboard_reachable() {
    let mut harness = harness(LinksSection, SettingsData::default());
    harness.run();

    let order = a11y::focus_order(&mut harness, 8);
    for label in ["Open PLAUD and sources", "Open activity history"] {
        assert!(
            order.contains(&label.to_owned()),
            "{label} not in {order:?}"
        );
    }
}
