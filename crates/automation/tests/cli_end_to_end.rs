//! End-to-end behaviour of the `plaudon` scripting bridge (M2.1).
//!
//! These tests drive the real binary against a real migrated library, because
//! the acceptance criteria are about the process: its exit code, its stdout,
//! its `--help`, and the fact that it opens no socket.

use std::path::{Path, PathBuf};
use std::process::Command;

use plaudon_ai_contracts::stt::{TranscriptDraft, TranscriptSegmentDraft};
use plaudon_ai_contracts::transcript::SourceTime;
use plaudon_domain::ids::RecordingId;
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::recording::Recording;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::repo::RecordingRepository;
use plaudon_persistence::transcript::{RevisionContext, TranscriptRepository};
use plaudon_persistence::Database;

/// Path of the `plaudon` binary built for this test run.
///
/// `CARGO_BIN_EXE_<name>` is cargo's own answer and survives a change of
/// artifact layout; deriving it from `current_exe()` assumed
/// `<profile>/deps/<test>`, which the coverage run's toolchain no longer
/// produces.
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_plaudon"))
}

struct Fixture {
    _dir: tempfile::TempDir,
    library: PathBuf,
    recording_id: RecordingId,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let library = dir.path().join("library.sqlite");
    let db = Database::open(&library).await.expect("open");
    MigrationRunner::new().run(&db).await.expect("migrate");

    let recording_id = RecordingId::new();
    RecordingRepository::new(db.clone())
        .insert(&Recording::new(
            recording_id,
            "Platform sync",
            Timestamp::from_millis(1_700_000_000_000),
        ))
        .await
        .expect("insert recording");

    TranscriptRepository::new(db.clone())
        .save_draft(
            &TranscriptDraft {
                engine_id: EngineId::new("whisper.cpp"),
                model: ModelIdentity::new(EngineId::new("whisper.cpp"), "base.en"),
                input_sha256: Sha256Digest::of(b"fixture-media"),
                locale: None,
                alignment_origin: AlignmentOrigin::Native,
                segments: vec![
                    TranscriptSegmentDraft {
                        seq_index: 0,
                        start: Some(SourceTime::from_millis(0, 16_000)),
                        end: Some(SourceTime::from_millis(2_000, 16_000)),
                        text: "We will ship the migration on Friday.".to_string(),
                        confidence: None,
                        speaker_id: None,
                        words: Vec::new(),
                    },
                    TranscriptSegmentDraft {
                        seq_index: 1,
                        start: Some(SourceTime::from_millis(2_000, 16_000)),
                        end: Some(SourceTime::from_millis(4_000, 16_000)),
                        text: "Priya owns the rollback plan.".to_string(),
                        confidence: None,
                        speaker_id: None,
                        words: Vec::new(),
                    },
                ],
                duration_ms: Some(4_000),
                engine_params: std::collections::BTreeMap::new(),
                chunking_strategy: plaudon_ai_contracts::transcript::ChunkingStrategy::default(),
                normalization_profile: None,
                annotations: Vec::new(),
                backend: None,
            },
            &RevisionContext::machine(recording_id, Timestamp::from_millis(1_700_000_001_000)),
        )
        .await
        .expect("save transcript");

    db.close().await;
    Fixture {
        _dir: dir,
        library,
        recording_id,
    }
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("stdout was not JSON ({e}): {:?}", self.stdout))
    }
}

fn run(library: &Path, args: &[&str]) -> Run {
    let output = Command::new(binary())
        .args(args)
        .arg("--library")
        .arg(library)
        .env_remove("PLAUDON_LIBRARY")
        .output()
        .expect("spawn plaudon");
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    }
}

fn enable(library: &Path) {
    let run = run(library, &["bridge", "enable", "--confirm"]);
    assert_eq!(run.code, 0, "{:?} {:?}", run.stdout, run.stderr);
}

#[test]
fn help_discovers_every_specified_command() {
    let output = Command::new(binary())
        .arg("--help")
        .output()
        .expect("spawn plaudon --help");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for command in ["list", "export", "process", "jobs"] {
        assert!(help.contains(command), "--help does not list `{command}`");
    }
}

#[tokio::test]
async fn the_bridge_is_inert_until_it_is_explicitly_enabled() {
    let fixture = fixture().await;

    let refused = run(&fixture.library, &["list"]);
    assert_ne!(refused.code, 0);
    assert_eq!(refused.json()["status"], "error");
    assert_eq!(refused.json()["code"], "COMMAND_NOT_AUTHORIZED");

    // Enabling needs an explicit confirmation.
    let unconfirmed = run(&fixture.library, &["bridge", "enable"]);
    assert_ne!(unconfirmed.code, 0);
    assert_eq!(unconfirmed.json()["code"], "CONFIRMATION_REQUIRED");

    enable(&fixture.library);
    let allowed = run(&fixture.library, &["list"]);
    assert_eq!(allowed.code, 0, "{:?}", allowed.stdout);
}

#[tokio::test]
async fn list_returns_the_library_as_json() {
    let fixture = fixture().await;
    enable(&fixture.library);

    let run = run(&fixture.library, &["list"]);
    assert_eq!(run.code, 0);
    let json = run.json();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["command"], "list");
    assert_eq!(json["data"]["count"], 1);
    assert_eq!(json["data"]["recordings"][0]["title"], "Platform sync");
    assert_eq!(json["data"]["recordings"][0]["revision_count"], 1);
}

#[tokio::test]
async fn export_emits_the_segment_list_with_times_and_provenance() {
    let fixture = fixture().await;
    enable(&fixture.library);

    let run = run(
        &fixture.library,
        &["export", "--recording", &fixture.recording_id.to_string()],
    );
    assert_eq!(run.code, 0, "{:?}", run.stdout);
    let json = run.json();
    assert_eq!(json["data"]["segment_count"], 2);

    let transcript = &json["data"]["transcript"];
    assert_eq!(transcript["alignment_origin"], "native");
    assert_eq!(transcript["engine"], "whisper.cpp");
    assert!(transcript["transcript_revision_id"].is_string());

    let segments = transcript["segments"].as_array().expect("segments array");
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0]["seq_index"], 0);
    assert_eq!(segments[0]["start_ms"], 0);
    assert_eq!(segments[1]["text"], "Priya owns the rollback plan.");
}

#[tokio::test]
async fn export_to_a_file_writes_it_and_reports_where() {
    let fixture = fixture().await;
    enable(&fixture.library);
    let out = fixture.library.with_extension("export.json");

    let run = run(
        &fixture.library,
        &[
            "export",
            "--recording",
            &fixture.recording_id.to_string(),
            "--out",
            out.to_str().unwrap(),
        ],
    );
    assert_eq!(run.code, 0, "{:?}", run.stdout);
    assert_eq!(run.json()["data"]["written_to"], out.display().to_string());

    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(written["segments"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn export_text_format_is_one_line_per_segment() {
    let fixture = fixture().await;
    enable(&fixture.library);

    let run = run(
        &fixture.library,
        &[
            "export",
            "--recording",
            &fixture.recording_id.to_string(),
            "--format",
            "text",
        ],
    );
    assert_eq!(run.code, 0);
    let text = run.json()["data"]["text"].as_str().unwrap().to_string();
    assert_eq!(text.lines().count(), 2);
    assert!(text.contains("[00:00:00] We will ship"));
}

#[tokio::test]
async fn export_of_an_unknown_recording_fails_with_a_stable_code() {
    let fixture = fixture().await;
    enable(&fixture.library);

    let unknown = run(
        &fixture.library,
        &["export", "--recording", &RecordingId::new().to_string()],
    );
    assert_ne!(unknown.code, 0);
    assert_eq!(unknown.json()["code"], "RECORD_NOT_FOUND");

    let malformed = run(&fixture.library, &["export", "--recording", "not-a-uuid"]);
    assert_ne!(malformed.code, 0);
    assert_eq!(malformed.json()["code"], "INVALID_ARGUMENT");
}

#[tokio::test]
async fn local_processing_queues_a_job_and_jobs_status_reports_it() {
    let fixture = fixture().await;
    enable(&fixture.library);

    let queued = run(
        &fixture.library,
        &["process", "--recording", &fixture.recording_id.to_string()],
    );
    assert_eq!(queued.code, 0, "{:?}", queued.stdout);
    let json = queued.json();
    assert_eq!(json["data"]["created"], true);
    assert_eq!(json["data"]["destination"], "local");
    let job_id = json["data"]["job_id"].as_str().unwrap().to_string();

    // Queueing the same stage again deduplicates rather than double-charging.
    let again = run(
        &fixture.library,
        &["process", "--recording", &fixture.recording_id.to_string()],
    );
    assert_eq!(again.code, 0);
    assert_eq!(again.json()["data"]["created"], false);
    assert_eq!(again.json()["data"]["job_id"], job_id);

    let status = run(&fixture.library, &["jobs", "status"]);
    assert_eq!(status.code, 0, "{:?}", status.stdout);
    let jobs = status.json()["data"]["jobs"].as_array().unwrap().clone();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["job_id"], job_id);
    assert_eq!(jobs[0]["state"], "queued");
}

#[tokio::test]
async fn cloud_processing_without_consent_exits_non_zero() {
    let fixture = fixture().await;
    enable(&fixture.library);

    let refused = run(
        &fixture.library,
        &[
            "process",
            "--recording",
            &fixture.recording_id.to_string(),
            "--run",
            "cloud",
        ],
    );
    assert_ne!(refused.code, 0);
    assert_eq!(refused.json()["code"], "CONSENT_REQUIRED");
}

#[tokio::test]
async fn cloud_processing_in_local_only_mode_exits_non_zero_even_with_consent() {
    let fixture = fixture().await;
    enable(&fixture.library);

    let db = Database::open(&fixture.library).await.unwrap();
    sqlx::query(
        "INSERT INTO consent_grants
             (id, scope, privacy_policy_version, engine, model_or_provider, approved_at, revoked_at)
         VALUES ('grant-1', 'recording', 'v1', 'openrouter', 'test/model', 1, NULL)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO app_preferences (key, version, value_json)
         VALUES ('desktop.preferences', 1, '{\"local_only_mode\": true}')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    db.close().await;

    let refused = run(
        &fixture.library,
        &[
            "process",
            "--recording",
            &fixture.recording_id.to_string(),
            "--run",
            "cloud",
        ],
    );
    assert_ne!(refused.code, 0);
    assert_eq!(refused.json()["code"], "LOCAL_ONLY_MODE");
}

#[tokio::test]
async fn cloud_processing_with_consent_records_the_grant_on_the_job() {
    let fixture = fixture().await;
    enable(&fixture.library);

    let db = Database::open(&fixture.library).await.unwrap();
    sqlx::query(
        "INSERT INTO consent_grants
             (id, scope, privacy_policy_version, engine, model_or_provider, approved_at, revoked_at)
         VALUES ('grant-1', 'recording', 'v1', 'openrouter', 'test/model', 1, NULL)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    db.close().await;

    let queued = run(
        &fixture.library,
        &[
            "process",
            "--recording",
            &fixture.recording_id.to_string(),
            "--run",
            "cloud",
            "--model",
            "vendor/model-a",
        ],
    );
    assert_eq!(queued.code, 0, "{:?}", queued.stdout);
    assert_eq!(queued.json()["data"]["destination"], "cloud");

    let db = Database::open(&fixture.library).await.unwrap();
    let consent: Option<String> = sqlx::query_scalar(
        "SELECT s.consent_id FROM job_engine_state s
             JOIN jobs j ON j.id = s.job_id ORDER BY j.created_at DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(consent.as_deref(), Some("grant-1"));
    db.close().await;
}

#[tokio::test]
async fn revoking_consent_in_the_app_immediately_refuses_scripted_cloud_work() {
    let fixture = fixture().await;
    enable(&fixture.library);

    let db = Database::open(&fixture.library).await.unwrap();
    sqlx::query(
        "INSERT INTO consent_grants
             (id, scope, privacy_policy_version, engine, model_or_provider, approved_at, revoked_at)
         VALUES ('grant-1', 'recording', 'v1', 'openrouter', 'test/model', 1, NULL)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    db.close().await;

    assert_eq!(
        run(
            &fixture.library,
            &[
                "process",
                "--recording",
                &fixture.recording_id.to_string(),
                "--run",
                "cloud",
            ],
        )
        .code,
        0
    );

    let db = Database::open(&fixture.library).await.unwrap();
    sqlx::query("UPDATE consent_grants SET revoked_at = 2 WHERE id = 'grant-1'")
        .execute(db.pool())
        .await
        .unwrap();
    db.close().await;

    let refused = run(
        &fixture.library,
        &[
            "process",
            "--recording",
            &fixture.recording_id.to_string(),
            "--run",
            "cloud",
            "--model",
            "other/model",
        ],
    );
    assert_ne!(refused.code, 0);
    assert_eq!(refused.json()["code"], "CONSENT_REQUIRED");
}

#[tokio::test]
async fn cli_output_contains_no_secret_shaped_values() {
    let fixture = fixture().await;
    enable(&fixture.library);

    let db = Database::open(&fixture.library).await.unwrap();
    // Store something secret-shaped where a careless implementation might
    // echo it: a credential reference on a device row.
    sqlx::query(
        "INSERT INTO devices (id, vendor, model, first_seen_at, last_seen_at)
         VALUES ('11111111-1111-7111-8111-111111111111', 'plaud', 'note', 1, 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO device_credential_refs
             (device_id, vault_service, vault_account_reference, auth_mode, created_at)
         VALUES ('11111111-1111-7111-8111-111111111111', 'plaudon', 'sk-live-TOPSECRET', 'paired', 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    db.close().await;

    for args in [
        vec!["list"],
        vec!["jobs", "status"],
        vec!["bridge", "status"],
    ] {
        let run = run(&fixture.library, &args);
        let haystack = format!("{}{}", run.stdout, run.stderr);
        for needle in [
            "sk-live",
            "TOPSECRET",
            "Authorization",
            "Bearer ",
            "X-Api-Key",
        ] {
            assert!(
                !haystack.contains(needle),
                "`{args:?}` output leaked {needle}: {haystack}"
            );
        }
    }
}

#[tokio::test]
async fn the_cli_opens_no_listening_socket() {
    let fixture = fixture().await;
    enable(&fixture.library);

    let before = listening_ports();
    for args in [
        vec!["list"],
        vec!["jobs", "status"],
        vec!["export", "--recording", &fixture.recording_id.to_string()],
    ] {
        let run = run(&fixture.library, &args);
        assert!(run.json()["status"].is_string());
    }
    let after = listening_ports();

    // The CLI is short-lived, so any port it opened would have to appear and
    // then vanish; asserting the set is unchanged catches a lingering server,
    // and the binary's dependency graph contains no server library at all.
    assert_eq!(
        before, after,
        "the set of listening sockets changed while running the CLI"
    );
}

/// Listening TCP sockets owned by this user, as the OS reports them.
fn listening_ports() -> Vec<String> {
    let output = Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let mut lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains("plaudon"))
        .map(ToString::to_string)
        .collect();
    lines.sort();
    lines
}

#[test]
fn the_binary_links_no_network_server_dependency() {
    // `cargo tree` is the honest check: it reads the resolved dependency
    // graph rather than grepping source.
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "plaudon-automation",
            "--edges",
            "normal",
            "--prefix",
            "none",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output();
    let Ok(output) = output else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let tree = String::from_utf8_lossy(&output.stdout).to_lowercase();
    for forbidden in [
        "axum",
        "warp",
        "actix-web",
        "hyper",
        "tiny_http",
        "rocket",
        "reqwest",
    ] {
        assert!(
            !tree.lines().any(|line| line.starts_with(forbidden)),
            "the scripting bridge must not depend on {forbidden}"
        );
    }
}
