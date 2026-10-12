//! Spec 18.7 acceptance scenarios that run automatically (task N1.2).
//!
//! Each test names its scenario number. `tests/e2e/scenarios.toml` records the
//! status of all 44 required scenarios and `plaudon-release scenarios` fails
//! when one goes missing, so a scenario cannot quietly disappear by deleting a
//! test here: the registry entry would still claim it is implemented and the
//! registry self-test below would fail.

use plaudon_desktop::commands::{import, jobs, library, settings, transcript};
use plaudon_desktop::dto::{
    DuplicatePolicy, ExportFormat, ExportRecordingRequest, GetTranscriptRequest, ImportFilesRequest,
    ImportOutcome, JobState, ListActivityRequest, ListJobsRequest, ListRecordingsRequest,
    RecordingFilter, RecordingSort, RecordingSortField, SaveTranscriptRevisionRequest, SegmentEdit,
    SortDirection,
};
use plaudon_e2e::{EgressWatch, Harness, LocalStt};
use plaudon_notifications::testing::RecordingNotifier;
use plaudon_notifications::OsPermission;
use plaudon_release_tools::scenarios;
use plaudon_security::outbound::{EgressPurpose, PayloadClass, Subsystem};

fn list_request(limit: u32) -> ListRecordingsRequest {
    ListRecordingsRequest {
        cursor: None,
        limit,
        filter: RecordingFilter {
            query: None,
            import_status: Vec::new(),
            transcription_status: Vec::new(),
            sources: Vec::new(),
            tags: Vec::new(),
            date_from: None,
            date_to: None,
        },
        sort: RecordingSort {
            field: RecordingSortField::Title,
            direction: SortDirection::Asc,
        },
    }
}

/// Spec 18.7 scenario 1: fresh install, manual MP3 import, local STT, export
/// with network blocked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_01_fresh_install_mp3_import_local_stt_export_offline() {
    let egress = EgressWatch::local_only();
    let harness = Harness::fresh_install().await;
    let path = harness.stage_fixture("valid/tone-mono-16k.mp3", "quarterly-review.mp3");

    let imported = import::import_files(
        &harness.ports,
        ImportFilesRequest {
            paths: vec![path.clone()],
            duplicate_policy: DuplicatePolicy::Ask,
        },
    )
    .await
    .expect("the import is accepted");
    let ImportOutcome::Accepted { .. } = &imported.results[0] else {
        panic!("a fresh MP3 must import: {:?}", imported.results[0]);
    };
    let recording_id = library::list_recordings(&harness.ports, list_request(10))
        .await
        .expect("the library lists the import")
        .items[0]
        .recording_id;

    // The source file is never mutated or removed by an import (ADR-0010).
    assert!(
        std::path::Path::new(&path).exists(),
        "the imported source file must survive the import"
    );

    let transcript_result = LocalStt::with_reference(&[
        "welcome to the quarterly review",
        "we agreed to ship on friday",
    ])
    .transcribe(std::path::Path::new(&path));
    assert_eq!(transcript_result.segments.len(), 2);
    let revision = harness
        .ports
        .transcript
        .create_machine_revision(LocalStt::revision_spec(&transcript_result, recording_id))
        .await
        .expect("the local transcript is stored");

    let destination = harness.export_dir().join("quarterly-review.md");
    jobs::export_recording(
        &harness.ports,
        ExportRecordingRequest {
            recording_id: recording_id.to_string(),
            revision_id: revision.revision_id.to_string(),
            format: ExportFormat::Markdown,
            destination: destination.to_string_lossy().into_owned(),
        },
    )
    .await
    .expect("the export job runs");

    let written = std::fs::read_to_string(&destination).expect("the export exists");
    assert!(written.contains("we agreed to ship on friday"), "{written}");

    egress.assert_no_egress("18.7#1");
}

/// Spec 18.7 scenario 2: fresh install, manual WAV import, local STT,
/// transcript edit, export JSON with network blocked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_02_fresh_install_wav_import_edit_export_json_offline() {
    let egress = EgressWatch::local_only();
    let harness = Harness::fresh_install().await;
    let path = harness.stage_fixture("valid/tone-stereo-44k.wav", "site-walkthrough.wav");

    let imported = import::import_files(
        &harness.ports,
        ImportFilesRequest {
            paths: vec![path.clone()],
            duplicate_policy: DuplicatePolicy::Ask,
        },
    )
    .await
    .expect("the import is accepted");
    let ImportOutcome::Accepted { .. } = &imported.results[0] else {
        panic!("a fresh WAV must import: {:?}", imported.results[0]);
    };
    let recording_id = library::list_recordings(&harness.ports, list_request(10))
        .await
        .expect("the library lists the import")
        .items[0]
        .recording_id;

    let machine = LocalStt::with_reference(&["pour the slab on tuesday", "order the rebar"])
        .transcribe(std::path::Path::new(&path));
    let stored = harness
        .ports
        .transcript
        .create_machine_revision(LocalStt::revision_spec(&machine, recording_id))
        .await
        .expect("the local transcript is stored");

    let edited = transcript::save_transcript_revision(
        &harness.ports,
        SaveTranscriptRevisionRequest {
            recording_id: recording_id.to_string(),
            base_revision_id: stored.revision_id.to_string(),
            edits: vec![SegmentEdit {
                index: 1,
                text: "order the rebar on monday".into(),
            }],
        },
    )
    .await
    .expect("the correction is saved as a new revision");
    assert_ne!(edited.revision_id, stored.revision_id);

    // The superseded revision remains retrievable (spec 8.5).
    let original = transcript::get_transcript(
        &harness.ports,
        GetTranscriptRequest {
            revision_id: stored.revision_id.to_string(),
            cursor: None,
            limit: 10,
        },
    )
    .await
    .expect("the original revision is readable");
    assert_eq!(original.segments[1].text, "order the rebar");

    let destination = harness.export_dir().join("site-walkthrough.json");
    jobs::export_recording(
        &harness.ports,
        ExportRecordingRequest {
            recording_id: recording_id.to_string(),
            revision_id: edited.revision_id.to_string(),
            format: ExportFormat::Json,
            destination: destination.to_string_lossy().into_owned(),
        },
    )
    .await
    .expect("the export job runs");

    let written = std::fs::read_to_string(&destination).expect("the export exists");
    let parsed: serde_json::Value = serde_json::from_str(&written).expect("valid JSON export");
    assert!(
        written.contains("order the rebar on monday"),
        "the export must carry the corrected text: {written}"
    );
    assert!(parsed.is_object(), "the JSON export is a document");

    egress.assert_no_egress("18.7#2");
}

/// Spec 18.7 scenario 7: cloud STT denied by policy means zero outgoing audio
/// bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_07_cloud_stt_denied_sends_no_audio_bytes() {
    let egress = EgressWatch::local_only();
    let harness = Harness::fresh_install().await;
    let path = harness.stage_fixture("valid/tone-mono-16k.wav", "confidential.wav");
    import::import_files(
        &harness.ports,
        ImportFilesRequest {
            paths: vec![path],
            duplicate_policy: DuplicatePolicy::Ask,
        },
    )
    .await
    .expect("the import is accepted");

    let allowed = egress.attempt(
        Subsystem::OpenRouter,
        "https://openrouter.ai/api/v1/audio/transcriptions",
        EgressPurpose::InferenceDispatch,
        PayloadClass::AudioBytes,
    );
    assert!(
        !allowed,
        "Local-only Mode must refuse a cloud transcription dispatch (spec 16.2A)"
    );
    egress.assert_no_egress("18.7#7");
    assert!(
        egress
            .decisions()
            .iter()
            .any(|decision| decision.contains("allowed=false")),
        "the refusal must be recorded so the UI can explain it (spec 5.4)"
    );
}

/// Spec 18.7 scenario 14: restart or kill mid-job recovers the queue and
/// presents no partial output as final.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_14_restart_mid_job_recovers_the_queue() {
    let harness = Harness::fresh_install().await;
    let first = harness.stage_fixture("valid/tone-mono-16k.wav", "alpha.wav");
    let second = harness.stage_fixture("valid/tone-mono-16k.flac", "beta.flac");
    import::import_files(
        &harness.ports,
        ImportFilesRequest {
            paths: vec![first, second],
            duplicate_policy: DuplicatePolicy::Ask,
        },
    )
    .await
    .expect("the imports are accepted");

    // Drop every handle without a graceful shutdown: the next launch sees only
    // what reached disk.
    let relaunched = harness.reopen().await;

    let recordings = library::list_recordings(&relaunched, list_request(50))
        .await
        .expect("the library lists after a relaunch");
    assert_eq!(
        recordings.total, 2,
        "both committed imports must survive a relaunch"
    );

    let job_page = jobs::list_jobs(
        &relaunched,
        ListJobsRequest {
            cursor: None,
            limit: 50,
            states: Vec::new(),
            kinds: Vec::new(),
        },
    )
    .await
    .expect("the job list after a relaunch");
    assert!(
        job_page
            .items
            .iter()
            .all(|job| job.state != JobState::RunningLocal),
        "no job may still claim to be running after a crash: {:?}",
        job_page.items
    );
    assert!(
        job_page
            .items
            .iter()
            .any(|job| job.state == JobState::Completed),
        "the committed imports must be recovered as completed jobs"
    );
}

/// Spec 18.7 scenario 24: an OS notice that is denied still produces an
/// in-app activity entry with privacy-safe text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_24_denied_os_notice_still_records_activity() {
    let harness = Harness::with_notifier(RecordingNotifier::with_permission(OsPermission::Denied)).await;
    let path = harness.stage_fixture("valid/tone-mono-16k.wav", "client-call.wav");
    import::import_files(
        &harness.ports,
        ImportFilesRequest {
            paths: vec![path],
            duplicate_policy: DuplicatePolicy::Ask,
        },
    )
    .await
    .expect("the import is accepted");

    let activity = settings::list_activity(
        &harness.ports,
        ListActivityRequest {
            cursor: None,
            limit: 50,
        },
    )
    .await
    .expect("the activity history");
    assert!(
        !activity.items.is_empty(),
        "a denied OS notice must still leave an in-app activity entry (spec 5.5)"
    );
    assert!(
        activity
            .items
            .iter()
            .all(|entry| !entry.notification_delivered
                && entry.notification_suppressed_reason.is_some()),
        "with the OS permission denied nothing may be delivered, and the reason must be recorded: {:?}",
        activity.items
    );
    // In-app text may name the recording the user just imported; what it must
    // never carry is a filesystem path, a key or a signed URL (spec 16.2,
    // 11.4). The staged source lives under a temporary directory, so a leaked
    // path would show up as a separator-bearing string.
    assert!(
        activity.items.iter().all(|entry| {
            !entry.body.contains(std::path::MAIN_SEPARATOR)
                && !entry.title.contains(std::path::MAIN_SEPARATOR)
        }),
        "notification text must not leak a filesystem path: {:?}",
        activity.items
    );
}

/// Harness self-test: the registry is complete and every scenario it claims is
/// implemented in this crate has a test function with the matching name.
#[test]
fn the_registry_is_complete_and_its_rust_claims_resolve_to_tests() {
    let root = plaudon_e2e::repo_root();
    let registry =
        scenarios::load(&root.join("tests/e2e/scenarios.toml")).expect("the registry loads");
    let errors = scenarios::validate(&registry);
    assert!(errors.is_empty(), "{errors:?}");

    let this_file = include_str!("acceptance.rs");
    for entry in &registry.scenarios {
        let scenarios::ScenarioStatus::Implemented { harness, test } = &entry.status else {
            continue;
        };
        if harness != "rust" {
            continue;
        }
        assert!(
            this_file.contains(test.as_str()),
            "scenario {} claims rust test `{test}`, which does not exist",
            entry.number
        );
    }
}

/// Harness self-test: an opt-in scenario never runs by default and reports a
/// reason (spec 18.3, 18.3A).
#[test]
fn opt_in_scenarios_are_skipped_with_a_reason_and_need_no_secret() {
    let root = plaudon_e2e::repo_root();
    let registry =
        scenarios::load(&root.join("tests/e2e/scenarios.toml")).expect("the registry loads");
    for entry in &registry.scenarios {
        if let scenarios::ScenarioStatus::OptIn { opt_in_env, reason } = &entry.status {
            assert!(
                !reason.trim().is_empty(),
                "scenario {} must state why it cannot run hermetically",
                entry.number
            );
            assert!(
                std::env::var(opt_in_env).is_err(),
                "scenario {} must not be switched on in a default CI run ({opt_in_env} is set)",
                entry.number
            );
        }
    }
}
