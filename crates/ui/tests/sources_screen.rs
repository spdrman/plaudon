//! Interaction tests for the Sources destination: the PLAUD Cloud connector,
//! the account listing, watched folders and manual import.

mod fixtures;

use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    FolderSyncCounts, JobState, PlaudConnectorState, PlaudDisconnectResponse,
    PlaudDisconnectVariant, PlaudImportOutcome, PlaudListingCompleteness, PlaudNextAction,
    ReconcileDecisionSummary, SourceAddFolderResponse, SourceReconcileResponse, SourceSyncResponse,
    SyncTrigger, SyncedSourceKind, WatchedFolderRule, PLAUD_CONNECT_INTENT,
};
use plaudon_domain::ids::JobId;
use plaudon_ui::screens::sources::presentation::PLAUD_CLI_DOCS_URL;
use plaudon_ui::screens::sources::{
    sources_screen, SourceIntent, SourceOp, SourceOutcome, SourcesState,
};
use plaudon_ui_testkit::a11y;

struct SourcesApp {
    state: SourcesState,
    intents: Vec<SourceIntent>,
}

impl SourcesApp {
    fn new() -> Self {
        Self {
            state: SourcesState::new(),
            intents: Vec::new(),
        }
    }
}

/// A tall window: the destination is a long page, and a control scrolled out of
/// view cannot be clicked by a pointer.
fn harness(app: SourcesApp) -> Harness<'static, SourcesApp> {
    plaudon_ui_testkit::builder()
        .with_size(egui::vec2(1280.0, 4000.0))
        .build_ui_state(
            |ui, app: &mut SourcesApp| {
                let intents = sources_screen(ui, &mut app.state);
                app.intents.extend(intents);
            },
            app,
        )
}

fn labels(harness: &Harness<'_, SourcesApp>) -> Vec<String> {
    a11y::nodes(harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect()
}

fn says(harness: &Harness<'_, SourcesApp>, needle: &str) -> bool {
    labels(harness).iter().any(|label| label.contains(needle))
}

fn folder_counts(inventory_complete: bool) -> FolderSyncCounts {
    FolderSyncCounts {
        scanned: 12,
        inventory_complete,
        imported: 3,
        already_imported: 8,
        deferred: 1,
        refused_tombstoned: 0,
        rejected: 0,
        vanished: 0,
    }
}

#[test]
fn the_connector_is_probed_before_anything_is_rendered() {
    assert_eq!(SourcesState::initial_intent(), SourceIntent::Probe);
    assert!(matches!(
        SourceIntent::Probe.into_command(),
        Some(AppCommand::PlaudProbe)
    ));
    let connect = SourceIntent::Connect.into_command().expect("a command");
    assert!(
        matches!(connect, AppCommand::PlaudConnect(request) if request.user_intent == PLAUD_CONNECT_INTENT),
        "every launch of the vendor tool carries the user action it is acting for"
    );
}

#[test]
fn an_untested_version_is_reported_honestly_and_never_as_tested() {
    let mut app = SourcesApp::new();
    app.state.applied(SourceOutcome::Snapshot(fixtures::connector(
        PlaudConnectorState::IncompatibleVersion,
        "The installed PLAUD tool has not been validated with this build.",
        "No version of the official tool is in the validated compatibility table, so nothing can be promised about it.",
        PlaudNextAction::InstallTestedVersion {
            usable_versions: Vec::new(),
        },
    )));
    let mut harness = harness(app);
    harness.run();

    assert!(says(
        &harness,
        "The installed PLAUD tool has not been validated with this build."
    ));
    assert!(says(&harness, "Untested version"));
    assert!(says(&harness, "Compatibility: Untested"));
    assert!(says(&harness, "Tool version: 0.9.1"));
    assert!(
        !says(&harness, "Compatibility: Tested"),
        "an empty compatibility table can never read as tested"
    );
    assert!(says(
        &harness,
        "No version of the official tool has been validated against this build."
    ));
    assert!(
        !says(&harness, "Tested versions:"),
        "no version may be named when none has been validated"
    );
    assert!(
        !says(&harness, "List my PLAUD recordings"),
        "an untested connector cannot list"
    );

    harness
        .get_by_label("Open PLAUD's installation guide")
        .click();
    harness.run();
    assert!(harness
        .state()
        .intents
        .contains(&SourceIntent::OpenUrl(PLAUD_CLI_DOCS_URL)));

    harness.get_by_label("Check again").click();
    harness.run();
    assert!(harness.state().intents.contains(&SourceIntent::Probe));
}

#[test]
fn an_expired_session_asks_for_a_sign_in_in_the_applications_own_words() {
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::connector(
            PlaudConnectorState::ExpiredAuth,
            "The PLAUD session has expired.",
            "The tool reported that its stored session is no longer valid.",
            PlaudNextAction::SignIn,
        )));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "Session expired"));
    assert!(says(&harness, "The PLAUD session has expired."));
    assert!(says(
        &harness,
        "The tool reported that its stored session is no longer valid."
    ));
    harness.get_by_label("Sign in to PLAUD").click();
    harness.run();
    assert!(harness.state().intents.contains(&SourceIntent::Connect));
}

#[test]
fn a_stopped_connector_renders_the_error_state_and_its_corrective_action() {
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::connector(
            PlaudConnectorState::Error,
            "The PLAUD tool stopped with an error.",
            "The tool exited before it answered.",
            PlaudNextAction::WaitAndRetry,
        )));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "Stopped"));
    assert!(says(&harness, "The tool exited before it answered."));
    assert!(says(
        &harness,
        "The listing is read from the tool's on-screen table"
    ));
    harness.get_by_label("Try again").click();
    harness.run();
    assert!(harness.state().intents.contains(&SourceIntent::Probe));
}

#[test]
fn local_only_mode_is_reviewed_in_settings_not_worked_around() {
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::connector(
            PlaudConnectorState::Error,
            "Local-only Mode blocked this request.",
            "Local-only Mode is on, so the application made no network request.",
            PlaudNextAction::ReviewLocalOnlyMode,
        )));
    let mut harness = harness(app);
    harness.run();

    harness.get_by_label("Review Local-only Mode").click();
    harness.run();
    assert!(harness
        .state()
        .intents
        .contains(&SourceIntent::OpenPrivacySettings));
    assert!(
        harness
            .state()
            .intents
            .iter()
            .all(|intent| !matches!(intent, SourceIntent::Connect)),
        "nothing reaches the network from this control"
    );
}

#[test]
fn issue_304_local_only_reads_switched_off_names_the_location_and_offers_an_explicit_way_on() {
    let mut app = SourcesApp::new();
    let mut snapshot = fixtures::connector(
        PlaudConnectorState::LocalOnlyBlocked,
        "Switched off by Local-only Mode",
        "The official PLAUD command line tool is installed in /opt/homebrew/bin (version 0.3.15). Nothing was sent.",
        PlaudNextAction::ReviewLocalOnlyMode,
    );
    snapshot.diagnostics.detected_version = Some("0.3.15".to_owned());
    snapshot.diagnostics.program_origin = Some("standard installation directory".to_owned());
    app.state.applied(SourceOutcome::Snapshot(snapshot));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "Switched off by Local-only Mode"));
    assert!(says(&harness, "Tool version: 0.3.15"));
    assert!(says(&harness, "Found in: standard installation directory"));
    assert!(!says(&harness, "Found in: Not found"));
    assert!(!says(&harness, "not reachable"));

    harness
        .get_by_label("Turn on the PLAUD connector\u{2026}")
        .click();
    harness.run();
    assert_eq!(
        harness.state().intents,
        vec![SourceIntent::OpenPrivacySettings],
        "the only way on is the user's own choice in Privacy settings; nothing is switched silently"
    );
}

#[test]
fn issue_304_node_missing_reads_as_its_own_state_and_can_be_checked_again() {
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::connector(
            PlaudConnectorState::RuntimeMissing,
            "PLAUD CLI found but Node.js is missing",
            "The official PLAUD command line tool is installed in /opt/homebrew/bin, but Node.js could not be found.",
            PlaudNextAction::InstallCli,
        )));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "PLAUD CLI found but Node.js is missing"));
    assert!(says(&harness, "Node.js missing"));
    assert!(!says(&harness, "not reachable"));
    harness.get_by_label("Check again").click();
    harness.run();
    assert!(harness.state().intents.contains(&SourceIntent::Probe));
}

/// The two panels #304 corrects, as committed baselines (compared on MARS
/// only, see `plaudon_ui_testkit::visual`). The copy is the diagnostics
/// layer's own wording for the reporter's machine.
#[test]
fn issue_304_visual_the_corrected_connector_panels() {
    let cases = [
        (
            "issue_304_local_only_blocked",
            PlaudConnectorState::LocalOnlyBlocked,
            "Switched off by Local-only Mode",
            "The official PLAUD command line tool is installed in /opt/homebrew/bin (version \
             0.3.15). Local-only Mode is on, so we did not start it. Nothing was sent. To use the \
             PLAUD connector, turn it on yourself in Privacy settings; we never turn Local-only \
             Mode off for you.",
            PlaudNextAction::ReviewLocalOnlyMode,
        ),
        (
            "issue_304_runtime_missing",
            PlaudConnectorState::RuntimeMissing,
            "PLAUD CLI found but Node.js is missing",
            "The official PLAUD command line tool is installed in /opt/homebrew/bin (version \
             0.3.15), but it needs Node.js to run and we could not find Node.js. Install Node.js \
             (for example with Homebrew), then check again.",
            PlaudNextAction::InstallCli,
        ),
    ];
    for (name, state, headline, detail, action) in cases {
        let mut app = SourcesApp::new();
        let mut snapshot = fixtures::connector(state, headline, detail, action);
        snapshot.diagnostics.detected_version = Some("0.3.15".to_owned());
        snapshot.diagnostics.program_origin = Some("standard installation directory".to_owned());
        // The reporter's release is a tested one, and nothing has been listed.
        snapshot.diagnostics.compatibility = plaudon_app::dto::PlaudCompatibility::Tested;
        snapshot.diagnostics.warnings.clear();
        snapshot.status.version_validated = true;
        snapshot.status.listing_possibly_incomplete = false;
        snapshot.status.scraping_warning = false;
        app.state.applied(SourceOutcome::Snapshot(snapshot));
        let mut harness = plaudon_ui_testkit::builder()
            .with_size(egui::vec2(1000.0, 560.0))
            .build_ui_state(
                |ui, app: &mut SourcesApp| {
                    let intents = sources_screen(ui, &mut app.state);
                    app.intents.extend(intents);
                },
                app,
            );
        harness.run();
        assert!(says(&harness, headline));
        assert!(!says(&harness, "Found in: Not found"));
        plaudon_ui_testkit::visual::assert_snapshot(&mut harness, name);
    }
}

#[test]
fn the_disconnect_wording_is_the_one_the_snapshot_carries() {
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::ready_connector()));
    let mut harness = harness(app);
    harness.run();

    assert!(says(
        &harness,
        PlaudDisconnectVariant::WithVendorLogout.label()
    ));
    assert!(says(
        &harness,
        PlaudDisconnectVariant::WithVendorLogout.body()
    ));
    assert!(!says(
        &harness,
        PlaudDisconnectVariant::AssociationOnly.label()
    ));

    harness
        .get_by_label(PlaudDisconnectVariant::WithVendorLogout.label())
        .click();
    harness.run();
    assert!(harness.state().intents.contains(&SourceIntent::Disconnect));

    let mut snapshot = fixtures::ready_connector();
    snapshot.disconnect = fixtures::connector(
        PlaudConnectorState::Unconfigured,
        "Not connected.",
        "No association is stored.",
        PlaudNextAction::InstallCli,
    )
    .disconnect;
    let after = PlaudDisconnectResponse {
        disconnect: snapshot.disconnect.clone(),
        snapshot,
    };
    harness
        .state_mut()
        .state
        .applied(SourceOutcome::Disconnected(after));
    harness.run();
    assert!(
        says(&harness, PlaudDisconnectVariant::AssociationOnly.label()),
        "the wording that was true for what just happened"
    );
}

#[test]
fn a_listing_that_may_be_partial_is_never_reconciled_from() {
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::ready_connector()));
    app.state
        .applied(SourceOutcome::Listed(fixtures::cloud_listing(vec![
            fixtures::cloud_item("ext-1", false),
            fixtures::cloud_item("ext-2", true),
        ])));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "May be incomplete"));
    assert!(says(
        &harness,
        "a partial listing cannot prove a recording is gone"
    ));
    assert!(
        harness
            .query_all_by_label("Reconcile with my library")
            .next()
            .is_none(),
        "the control is not offered for a listing that may be partial"
    );
    assert!(says(&harness, "In library"), "an imported item says so");
    assert!(says(&harness, "1:05"));
}

#[test]
fn the_real_0_3_15_listing_renders_without_inventing_anything() {
    // The committed capture of `@plaud-ai/cli` 0.3.15, parsed by the shipped
    // version-locked parser and handed to the screen. Three properties matter:
    // the vendor's own truncation survives as display text, the durations are
    // reformatted into the application's own spelling, and the listing's
    // day-precision date never becomes a capture time the screen could show.
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::ready_connector()));
    let listing = fixtures::plaud_0_3_15_listing();
    assert_eq!(listing.items.len(), 3);
    assert_eq!(listing.completeness, PlaudListingCompleteness::Complete);
    assert_eq!(listing.next_cursor.as_deref(), Some("2"));
    assert!(listing.items.iter().all(|item| item.captured_at.is_none()));
    assert!(listing
        .items
        .iter()
        .all(|item| item.declared_byte_length.is_none()));
    app.state.applied(SourceOutcome::Listed(listing));
    let mut harness = harness(app);
    harness.run();

    // 41m07s and 1h15m as the release printed them, in the application's own
    // format.
    assert!(says(&harness, "41:07"));
    assert!(says(&harness, "1:15:00"));
    assert!(!says(&harness, "Duration unknown"));
    // The vendor truncates a long name at 35 characters; that is display text
    // and the screen shows it rather than guessing the rest.
    assert!(says(&harness, "Synthetic fixture recording with a\u{2026}"));
    assert!(says(&harness, "Complete"));
    // A scraped listing still carries the compatibility warning the snapshot
    // was built with, and no identifier is ever shown as a title.
    assert!(!says(&harness, "of_1a2b3c4d5e6f708192a3b4c5d6e7f801"));
}

#[test]
fn a_recording_without_a_duration_does_not_claim_zero() {
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::ready_connector()));
    let mut item = fixtures::cloud_item("ext-1", false);
    item.duration_ms = None;
    app.state
        .applied(SourceOutcome::Listed(fixtures::cloud_listing(vec![item])));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "Duration unknown"));
    assert!(!says(&harness, "0:00"));
}

#[test]
fn a_complete_listing_can_be_reconciled() {
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::ready_connector()));
    let mut listing = fixtures::cloud_listing(vec![fixtures::cloud_item("ext-1", false)]);
    listing.completeness = PlaudListingCompleteness::Complete;
    app.state.applied(SourceOutcome::Listed(listing));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "Complete"));
    harness.get_by_label("Reconcile with my library").click();
    harness.run();
    assert!(harness.state().intents.iter().any(|intent| matches!(
        intent,
        SourceIntent::Reconcile { source_id } if source_id == "8f14e45f-ea0a-4f36-9f1a-000000000001"
    )));
}

#[test]
fn importing_cloud_recordings_reports_each_outcome_in_the_backends_words() {
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::ready_connector()));
    app.state
        .applied(SourceOutcome::Listed(fixtures::cloud_listing(vec![
            fixtures::cloud_item("ext-1", false),
        ])));
    let mut harness = harness(app);
    harness.run();

    harness.get_by_label("Cloud recording ext-1").click();
    harness.run();
    harness.get_by_label("Import selected (1)").click();
    harness.run();
    assert!(harness.state().intents.contains(&SourceIntent::Import {
        external_ids: vec!["ext-1".to_owned()],
    }));

    harness
        .state_mut()
        .state
        .applied(SourceOutcome::Imported(vec![PlaudImportOutcome::Failed {
            external_id: "ext-1".to_owned(),
            job_id: JobId::new(),
            error: fixtures::envelope("The download could not be verified.", true),
        }]));
    harness.run();
    assert!(says(&harness, "Failed"));
    assert!(says(&harness, "The download could not be verified."));
}

#[test]
fn an_empty_account_is_advice_and_not_an_error() {
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::ready_connector()));
    app.state
        .applied(SourceOutcome::Listed(fixtures::cloud_listing(Vec::new())));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "No recordings found"));
    assert!(says(
        &harness,
        "The account listed no recordings. That is not an error."
    ));
    harness.get_by_label("Import audio files instead").click();
    harness.run();
    assert!(harness
        .state()
        .intents
        .contains(&SourceIntent::ImportAudioFiles));
}

#[test]
fn a_watched_folder_is_added_with_the_rule_the_user_chose() {
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::ready_connector()));
    let mut harness = harness(app);
    harness.run();

    harness.get_by_label("Choose a folder to watch").click();
    harness.run();
    assert!(harness
        .state()
        .intents
        .contains(&SourceIntent::ChooseFolder));

    harness.state_mut().state.folder_chosen("/Users/me/Voice");
    harness.run();
    assert!(says(&harness, "Folder: /Users/me/Voice"));
    harness
        .get_by_label(
            "Import, and ask before any cloud step: no background dispatch without consent",
        )
        .click();
    harness.run();
    harness.get_by_label("Include subfolders").click();
    harness.run();
    harness.get_by_label("Watch this folder").click();
    harness.run();

    let intent = harness
        .state()
        .intents
        .iter()
        .find(|intent| matches!(intent, SourceIntent::AddFolder { .. }))
        .expect("an add-folder intent");
    assert_eq!(
        *intent,
        SourceIntent::AddFolder {
            path: "/Users/me/Voice".to_owned(),
            recursive: true,
            rule: WatchedFolderRule::AskBeforeCloud,
        }
    );
    let command = intent.clone().into_command().expect("a command");
    assert!(matches!(
        command,
        AppCommand::SourceAddFolder(request) if request.rule == WatchedFolderRule::AskBeforeCloud && request.recursive
    ));
}

#[test]
fn a_bounded_sweep_that_did_not_see_the_whole_folder_says_so() {
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::ready_connector()));
    app.state.folder_chosen("/Users/me/Voice");
    app.state
        .applied(SourceOutcome::FolderAdded(SourceAddFolderResponse {
            source_id: "folder-1".to_owned(),
            rule: WatchedFolderRule::ImportAndQueueLocalStt,
            recursive: false,
        }));
    app.state.applied(SourceOutcome::Synced(SourceSyncResponse {
        job_id: JobId::new(),
        source_id: "folder-1".to_owned(),
        source_kind: SyncedSourceKind::WatchedFolder,
        state: JobState::Completed,
        folder: Some(folder_counts(false)),
        listed: None,
        imported_recording_ids: Vec::new(),
    }));
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "/Users/me/Voice"));
    assert!(says(
        &harness,
        "Import and queue local transcription (nothing leaves this computer)"
    ));
    assert!(says(&harness, "Only the folder itself is watched."));
    assert!(says(&harness, "12 inspected · 3 imported"));
    assert!(says(
        &harness,
        "The sweep is bounded and did not see the whole folder"
    ));
    assert!(says(
        &harness,
        "This build has no command that lists the folders already stored"
    ));
}

#[test]
fn a_watched_folder_can_be_synced_and_reconciled_on_demand() {
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::ready_connector()));
    app.state.folder_chosen("/Users/me/Voice");
    app.state
        .applied(SourceOutcome::FolderAdded(SourceAddFolderResponse {
            source_id: "folder-1".to_owned(),
            rule: WatchedFolderRule::ImportOnly,
            recursive: true,
        }));
    let mut harness = harness(app);
    harness.run();

    harness.get_by_label("Sync now").click();
    harness.run();
    assert!(harness.state().intents.contains(&SourceIntent::Sync {
        source_id: "folder-1".to_owned(),
        trigger: SyncTrigger::Manual,
    }));
    assert!(harness.state().state.is_pending(SourceOp::Sync));

    harness
        .state_mut()
        .state
        .applied(SourceOutcome::Synced(SourceSyncResponse {
            job_id: JobId::new(),
            source_id: "folder-1".to_owned(),
            source_kind: SyncedSourceKind::WatchedFolder,
            state: JobState::Completed,
            folder: Some(folder_counts(true)),
            listed: None,
            imported_recording_ids: Vec::new(),
        }));
    harness.run();
    assert!(!says(
        &harness,
        "The sweep is bounded and did not see the whole folder"
    ));

    harness.get_by_label("Reconcile with my library").click();
    harness.run();
    assert!(harness.state().intents.contains(&SourceIntent::Reconcile {
        source_id: "folder-1".to_owned(),
    }));

    harness
        .state_mut()
        .state
        .applied(SourceOutcome::Reconciled(SourceReconcileResponse {
            job_id: JobId::new(),
            source_id: "folder-1".to_owned(),
            source_kind: SyncedSourceKind::WatchedFolder,
            decisions: vec![ReconcileDecisionSummary {
                external_id: None,
                event: "source_item_vanished".to_owned(),
                actions: vec!["mark_remote_state".to_owned()],
            }],
            listing_complete: None,
            folder: Some(folder_counts(true)),
        }));
    harness.run();
    assert!(says(&harness, "Last reconcile: 1 decision(s)"));
    assert!(says(&harness, "source_item_vanished → mark_remote_state"));
}

#[test]
fn a_failed_source_command_renders_its_envelope() {
    let mut app = SourcesApp::new();
    app.state
        .applied(SourceOutcome::Snapshot(fixtures::ready_connector()));
    app.state.applied(SourceOutcome::Failed {
        op: SourceOp::List,
        error: fixtures::envelope("The tool did not return a listing.", true),
    });
    let mut harness = harness(app);
    harness.run();

    assert!(says(&harness, "The tool did not return a listing."));
    assert!(says(&harness, "Diagnostic id:"));
}

#[test]
fn manual_import_is_always_available() {
    let mut harness = harness(SourcesApp::new());
    harness.run();

    assert!(says(&harness, "Import files from this computer"));
    harness.get_by_label("Choose audio files").click();
    harness.run();
    assert!(harness
        .state()
        .intents
        .contains(&SourceIntent::ImportAudioFiles));
}

#[test]
fn the_two_routes_are_never_confused_with_one_another() {
    let mut harness = harness(SourcesApp::new());
    harness.run();

    assert!(says(
        &harness,
        "A direct transfer never falls back to the cloud on its own, and this connector only runs when you ask it to."
    ));
}

#[test]
fn every_control_of_the_bare_screen_is_reachable_by_keyboard() {
    let mut harness = harness(SourcesApp::new());
    harness.run();

    let order = a11y::focus_order(&mut harness, 12);
    for control in ["Choose a folder to watch", "Choose audio files"] {
        assert!(
            order.iter().any(|label| label == control),
            "{control} is not in the tab order: {order:?}"
        );
    }
}
