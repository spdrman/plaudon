//! Writes a cloud request ledger row up to a chosen durable point, then aborts
//! the process without unwinding, cleanup or flushing.
//!
//! Usage: `cloud-dispatch-runner <db-path> <point>` where `point` is one of
//! `before_row`, `after_row`, `after_sending`, `after_accepted`.
//!
//! The whole point of the suite is that a process killed between dispatch and
//! response must come back as `outcome_unknown` and must never be retried on
//! its own (spec 10.7, 14.3).

use plaudon_ai_contracts::capabilities::RequestFamily;
use plaudon_ai_openrouter::ledger::{AttemptIntent, AttemptLedger, AttemptUpdate};
use plaudon_ai_openrouter::pricing::{CostEstimate, EstimateGap};
use plaudon_ai_openrouter::service::InferenceLedger;
use plaudon_persistence::db::Database;
use plaudon_persistence::migrations::MigrationRunner;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(args.len(), 3, "usage: cloud-dispatch-runner <db> <point>");
    let point = args[2].clone();

    let db = Database::open(&args[1]).await.expect("open database");
    MigrationRunner::new().run(&db).await.expect("migrate");

    sqlx::query(
        "INSERT OR IGNORE INTO consent_grants \
             (id, scope, privacy_policy_version, engine, model_or_provider, approved_at) \
         VALUES ('consent-1','model_policy','2026-10-11','openrouter','openai/whisper-1',0)",
    )
    .execute(db.pool())
    .await
    .expect("consent grant");
    sqlx::query(
        "INSERT OR IGNORE INTO jobs (id, job_type, state, created_at) \
         VALUES ('job-1','transcribe','cloud_sending',0)",
    )
    .execute(db.pool())
    .await
    .expect("job row");

    let ledger = InferenceLedger::new(db);
    if point == "before_row" {
        crash();
    }

    let id = ledger
        .open(&AttemptIntent {
            job_id: "job-1".to_owned(),
            content_sha256: "b".repeat(64),
            request_family: RequestFamily::MultipartStt,
            relay: "openrouter".to_owned(),
            model_id: "openai/whisper-1".to_owned(),
            route_label: None,
            privacy_snapshot: "relay=openrouter;model=openai/whisper-1;provider=<undisclosed>;\
                               content=audio_bytes;retention=unknown;source=unresolved"
                .to_owned(),
            consent_id: "consent-1".to_owned(),
            estimate: CostEstimate::unknown(EstimateGap::AudioTokenRateUnobserved),
        })
        .await
        .expect("open attempt");
    println!("{id}");
    if point == "after_row" {
        crash();
    }

    ledger
        .update(&id, AttemptUpdate::Sending)
        .await
        .expect("sending");
    if point == "after_sending" {
        crash();
    }

    ledger
        .update(
            &id,
            AttemptUpdate::Accepted {
                provider_request_id: Some("req_chaos".to_owned()),
            },
        )
        .await
        .expect("accepted");
    if point == "after_accepted" {
        crash();
    }

    panic!("unknown crash point: {point}");
}

/// Abort without unwinding: no destructors, no buffered writes.
fn crash() -> ! {
    std::process::abort();
}
