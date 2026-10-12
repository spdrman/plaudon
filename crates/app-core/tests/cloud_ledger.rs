//! Durable-ledger, budget and model-selection behaviour over a real SQLite
//! library (tasks G2.1, G2.2, G2.3, G2.5; spec 10.4, 10.7, 10.8, 14.3).

use plaudon_ai_contracts::capabilities::RequestFamily;
use plaudon_ai_openrouter::allowlist::ModelTask;
use plaudon_ai_openrouter::catalog::{ModelProbeRecord, ProbeEvidence, TimestampSupport};
use plaudon_ai_openrouter::ledger::{AttemptIntent, AttemptLedger, AttemptUpdate};
use plaudon_ai_openrouter::pricing::{CostEstimate, CostOrigin, EstimateGap, SettledCost};
use plaudon_ai_openrouter::privacy::DocumentedPolicy;
use plaudon_app_core::cloud::{
    evaluate_budget, BudgetDecision, BudgetLimit, CloudAi, CloudBudget, ModelChoice, Stage,
};
use plaudon_domain::capability::{DispatchState, Known, Tri};
use plaudon_domain::money::{Currency, Money, MoneyEstimate};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::db::Database;
use plaudon_persistence::migrations::MigrationRunner;
use tempfile::TempDir;

const SECRET: &str = "sk-or-v1-never-stored";
const SCALE: u8 = plaudon_ai_openrouter::catalog::PRICE_SCALE;

async fn library() -> (TempDir, Database, CloudAi) {
    let dir = TempDir::new().expect("tempdir");
    let db = Database::open(dir.path().join("app.sqlite"))
        .await
        .expect("open");
    MigrationRunner::new().run(&db).await.expect("migrate");
    sqlx::query(
        "INSERT INTO consent_grants (id, scope, privacy_policy_version, engine, model_or_provider, approved_at) \
         VALUES ('consent-1','model_policy','2026-10-11','openrouter','openai/whisper-1',0)",
    )
    .execute(db.pool())
    .await
    .expect("consent row");
    sqlx::query(
        "INSERT INTO jobs (id, job_type, state, created_at) \
         VALUES ('job-1','transcribe','queued',0)",
    )
    .execute(db.pool())
    .await
    .expect("job row");
    let cloud = CloudAi::open(db.clone()).expect("allowlist");
    (dir, db, cloud)
}

fn usd(units: i128) -> Money {
    Money::new(units, SCALE, Currency::USD)
}

fn intent() -> AttemptIntent {
    AttemptIntent {
        job_id: "job-1".to_owned(),
        content_sha256: "a".repeat(64),
        request_family: RequestFamily::MultipartStt,
        relay: "openrouter".to_owned(),
        model_id: "openai/whisper-1".to_owned(),
        route_label: Some("openrouter|openai/whisper-1|OpenAI".to_owned()),
        privacy_snapshot: "relay=openrouter;model=openai/whisper-1;provider=OpenAI;\
                           content=audio_bytes;retention=unknown;source=unresolved"
            .to_owned(),
        consent_id: "consent-1".to_owned(),
        estimate: CostEstimate::unknown(EstimateGap::AudioTokenRateUnobserved),
    }
}

#[tokio::test]
async fn an_attempt_row_is_durable_before_any_dispatch() {
    let (_dir, db, cloud) = library().await;
    let id = cloud.ledger().open(&intent()).await.expect("row");

    let (state, sha, consent): (String, String, String) = sqlx::query_as(
        "SELECT dispatch_state, content_sha256, consent_id FROM inference_attempts WHERE id = ?1",
    )
    .bind(id.as_str())
    .fetch_one(db.pool())
    .await
    .expect("row is readable from a second connection");

    assert_eq!(state, "not_sent");
    assert_eq!(sha, "a".repeat(64));
    assert_eq!(consent, "consent-1");
}

#[tokio::test]
async fn a_crash_between_dispatch_and_response_becomes_outcome_unknown() {
    let (_dir, _db, cloud) = library().await;
    let id = cloud.ledger().open(&intent()).await.expect("row");
    cloud
        .ledger()
        .update(&id, AttemptUpdate::Sending)
        .await
        .expect("sending");

    // Simulate startup after a kill.
    let moved = cloud
        .ledger()
        .reconcile_in_flight(Timestamp::from_millis(10_000))
        .await
        .expect("reconcile");
    assert_eq!(moved, vec![id.as_str().to_owned()]);

    let rows = cloud
        .ledger()
        .attempts_for_job("job-1")
        .await
        .expect("rows");
    assert_eq!(rows[0].state(), DispatchState::OutcomeUnknown);
    assert!(rows[0].possibly_billed());
    assert!(
        !rows[0].state().auto_retry_safe(),
        "a possibly-billed attempt must never auto-retry"
    );
}

#[tokio::test]
async fn a_failure_before_send_stays_safe_to_retry() {
    let (_dir, _db, cloud) = library().await;
    let id = cloud.ledger().open(&intent()).await.expect("row");
    cloud
        .ledger()
        .update(&id, AttemptUpdate::FailedBeforeSend { reason: "connect" })
        .await
        .expect("update");
    let rows = cloud
        .ledger()
        .attempts_for_job("job-1")
        .await
        .expect("rows");
    assert_eq!(rows[0].state(), DispatchState::FailedBeforeSend);
    assert!(rows[0].state().auto_retry_safe());
    assert!(!rows[0].possibly_billed());
}

#[tokio::test]
async fn settling_an_attempt_records_usage_without_turning_unknown_into_zero() {
    let (_dir, db, cloud) = library().await;

    let finalized = cloud.ledger().open(&intent()).await.expect("row");
    cloud
        .ledger()
        .update(
            &finalized,
            AttemptUpdate::Completed {
                provider_request_id: Some("req_1".to_owned()),
                cost: SettledCost {
                    amount: MoneyEstimate::Finalized(usd(1_000_000)),
                    origin: CostOrigin::ProviderReported,
                },
                usage: Some(serde_json::json!({ "cost": 0.000001 })),
            },
        )
        .await
        .expect("settle");

    let unknown = cloud.ledger().open(&intent()).await.expect("row");
    cloud
        .ledger()
        .update(
            &unknown,
            AttemptUpdate::Completed {
                provider_request_id: None,
                cost: SettledCost::unavailable(),
                usage: None,
            },
        )
        .await
        .expect("settle");

    let confidences: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT cost_confidence, estimated_cost, actual_cost FROM usage_events ORDER BY created_at, id",
    )
    .fetch_all(db.pool())
    .await
    .expect("usage rows");
    assert_eq!(confidences.len(), 2);
    assert!(confidences.iter().any(|(c, _, actual)| c == "finalized"
        && actual.is_some()));
    assert!(confidences
        .iter()
        .any(|(c, estimated, actual)| c == "unknown"
            && estimated.is_none()
            && actual.is_none()));

    let summary = cloud
        .usage(Timestamp::from_millis(0), Timestamp::now())
        .await
        .expect("usage");
    assert_eq!(summary.totals.finalized, Some(usd(1_000_000)));
    assert_eq!(summary.totals.unknown_count, 1);
    assert_eq!(summary.totals.attempts, 2);
}

#[tokio::test]
async fn the_ledger_never_stores_a_credential() {
    let (_dir, db, cloud) = library().await;
    let id = cloud.ledger().open(&intent()).await.expect("row");
    cloud
        .ledger()
        .update(
            &id,
            AttemptUpdate::Completed {
                provider_request_id: Some("req_1".to_owned()),
                cost: SettledCost::unavailable(),
                usage: Some(serde_json::json!({ "prompt_tokens": 10 })),
            },
        )
        .await
        .expect("settle");

    let dump: Vec<(String,)> = sqlx::query_as(
        "SELECT COALESCE(id,'') || COALESCE(route,'') || COALESCE(privacy_policy_snapshot,'') \
            || COALESCE(provider_request_id,'') || COALESCE(final_cost_json,'') \
         FROM inference_attempts",
    )
    .fetch_all(db.pool())
    .await
    .expect("dump");
    for (row,) in dump {
        assert!(!row.contains(SECRET), "{row}");
        assert!(!row.to_lowercase().contains("bearer"), "{row}");
        assert!(!row.contains("https://"), "no signed URL may be stored");
    }
}

#[tokio::test]
async fn a_recording_override_never_changes_the_task_default() {
    let (_dir, db, cloud) = library().await;
    sqlx::query(
        "INSERT INTO recordings (id, title, captured_at, created_at) \
         VALUES ('rec-1','Weekly',0,0)",
    )
    .execute(db.pool())
    .await
    .expect("recording");

    let at = Timestamp::from_millis(1);
    let default = ModelChoice::new("whisper.cpp", Some("base.en".to_owned()));
    cloud
        .store()
        .set_task_default(Stage::Transcribe, &default, at)
        .await
        .expect("default");
    let override_choice = ModelChoice::new("openrouter", Some("openai/whisper-1".to_owned()));
    cloud
        .store()
        .set_recording_override(&"rec-1".to_owned(), Stage::Transcribe, &override_choice, at)
        .await
        .expect("override");

    let effective = cloud
        .effective_choice("rec-1", Stage::Transcribe, None)
        .await
        .expect("query")
        .expect("choice");
    assert_eq!(effective.choice, override_choice);
    assert_eq!(
        cloud
            .store()
            .task_default(Stage::Transcribe)
            .await
            .expect("default"),
        Some(default.clone())
    );

    // Another recording still gets the default.
    sqlx::query(
        "INSERT INTO recordings (id, title, captured_at, created_at) \
         VALUES ('rec-2','Other',0,0)",
    )
    .execute(db.pool())
    .await
    .expect("recording");
    let other = cloud
        .effective_choice("rec-2", Stage::Transcribe, None)
        .await
        .expect("query")
        .expect("choice");
    assert_eq!(other.choice, default);

    // Clearing the override restores the default for the first recording too.
    cloud
        .store()
        .clear_recording_override("rec-1", Stage::Transcribe)
        .await
        .expect("clear");
    assert_eq!(
        cloud
            .effective_choice("rec-1", Stage::Transcribe, None)
            .await
            .expect("query")
            .expect("choice")
            .choice,
        default
    );
}

#[tokio::test]
async fn a_catalog_only_probe_cannot_verify_a_model() {
    let (_dir, _db, cloud) = library().await;
    let record = ModelProbeRecord {
        model_id: "openai/whisper-1".to_owned(),
        request_family: RequestFamily::MultipartStt,
        evidence: ProbeEvidence::CatalogOnly,
        accepted_formats: Known::Unknown,
        max_input_bytes: Known::Unknown,
        max_duration_ms: Known::Unknown,
        timestamps: TimestampSupport::NotObserved,
        diarization: Tri::Unknown,
        language_detection: Tri::Unknown,
        provider_route: Known::Unknown,
        retention_control: Known::Unknown,
        audio_input_price: Known::Unknown,
        pricing_unit: Known::Unknown,
        observed_on: "2026-10-11".to_owned(),
        notes: String::new(),
    };
    cloud
        .store()
        .record_probe(ModelTask::Transcription, &record, "fixture", Timestamp::from_millis(1))
        .await
        .expect("record");

    let view = cloud
        .catalog(
            ModelTask::Transcription,
            None,
            plaudon_ai_openrouter::discovery::DegradedReason::LocalOnlyMode,
        )
        .await
        .expect("catalog");
    let card = view
        .cards
        .iter()
        .find(|card| card.model_id == "openai/whisper-1")
        .expect("card");
    assert!(!card.verification.is_verified());
    assert!(!card.is_selectable());
    assert!(view.degraded.is_some(), "no live catalogue was fetched");
}

#[tokio::test]
async fn an_observed_probe_with_a_resolved_route_verifies_and_persists() {
    let (_dir, _db, cloud) = library().await;
    let record = ModelProbeRecord {
        model_id: "openai/whisper-1".to_owned(),
        request_family: RequestFamily::MultipartStt,
        evidence: ProbeEvidence::ObservedExchange,
        accepted_formats: Known::Known(vec!["wav".to_owned()]),
        max_input_bytes: Known::Known(26_214_400),
        max_duration_ms: Known::Known(1_500_000),
        timestamps: TimestampSupport::ObservedSegments,
        diarization: Tri::No,
        language_detection: Tri::Yes,
        provider_route: Known::Known("OpenAI".to_owned()),
        retention_control: Known::Unknown,
        audio_input_price: Known::Unknown,
        pricing_unit: Known::Unknown,
        observed_on: "2026-10-11".to_owned(),
        notes: String::new(),
    };
    cloud
        .store()
        .record_probe(
            ModelTask::Transcription,
            &record,
            "transcription-success-segments",
            Timestamp::from_millis(1),
        )
        .await
        .expect("record");

    let view = cloud
        .catalog(
            ModelTask::Transcription,
            None,
            plaudon_ai_openrouter::discovery::DegradedReason::Offline,
        )
        .await
        .expect("catalog");
    let card = view
        .cards
        .iter()
        .find(|card| card.model_id == "openai/whisper-1")
        .expect("card");
    assert!(card.verification.is_verified());
    assert!(card.is_selectable());
}

#[tokio::test]
async fn a_documented_route_policy_is_scoped_to_its_content_type() {
    let (_dir, _db, cloud) = library().await;
    cloud
        .store()
        .record_route_policy(
            &DocumentedPolicy {
                model_id: "openai/whisper-1".to_owned(),
                downstream_provider: None,
                content_type: plaudon_security::consent::ContentType::TranscriptText,
                posture: plaudon_security::consent::RetentionPosture::ZeroRetention,
            },
            "provider documentation",
            Timestamp::from_millis(1),
        )
        .await
        .expect("record");

    let policies = cloud.store().route_policies().await.expect("policies");
    assert_eq!(policies.len(), 1);
    assert_eq!(
        policies[0].content_type,
        plaudon_security::consent::ContentType::TranscriptText
    );

    // The transcription catalogue resolves audio uploads, which this policy
    // does not cover, so the posture stays unknown.
    let view = cloud
        .catalog(
            ModelTask::Transcription,
            None,
            plaudon_ai_openrouter::discovery::DegradedReason::Offline,
        )
        .await
        .expect("catalog");
    let card = view
        .cards
        .iter()
        .find(|card| card.model_id == "openai/whisper-1")
        .expect("card");
    assert_eq!(
        card.privacy.as_ref().expect("privacy").posture,
        plaudon_security::consent::RetentionPosture::Unknown
    );
}

#[tokio::test]
async fn a_stored_budget_round_trips_and_blocks_an_over_limit_dispatch() {
    let (_dir, _db, cloud) = library().await;
    let budget = CloudBudget {
        per_job_limit: Some(usd(500_000_000)),
        monthly_limit: Some(usd(5_000_000_000)),
    };
    cloud.store().set_budget(&budget).await.expect("set");
    let loaded = cloud.store().budget().await.expect("get");
    assert_eq!(loaded, budget);

    let month = cloud
        .month_to_date(Timestamp::now())
        .await
        .expect("month to date");
    let decision = evaluate_budget(
        &loaded,
        MoneyEstimate::Estimated(usd(600_000_000)),
        &month,
    );
    assert_eq!(
        decision,
        BudgetDecision::Block {
            limit: BudgetLimit::PerJob,
            allowed: usd(500_000_000),
            projected: usd(600_000_000),
        }
    );
}

#[tokio::test]
async fn the_default_budget_is_unlimited_rather_than_zero() {
    let (_dir, _db, cloud) = library().await;
    let budget = cloud.store().budget().await.expect("get");
    assert_eq!(budget, CloudBudget::unlimited());
    assert!(!budget.is_enforcing());
    assert!(evaluate_budget(
        &budget,
        MoneyEstimate::Unknown,
        &plaudon_app_core::cloud::CostTotals::default()
    )
    .is_allowed());
}
