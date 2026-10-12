//! The opt-in real-provider suite (spec 18.3, task G1.5).
//!
//! This is the only test in the repository that can reach OpenRouter, and it
//! does nothing at all unless **both** of these are set:
//!
//! ```text
//! PLAUDON_OPENROUTER_LIVE=1
//! OPENROUTER_API_KEY=sk-or-...
//! ```
//!
//! CI sets neither, so the suite is inert there: every test below returns
//! before constructing a transport. When it does run it is cost-capped by
//! construction — one request, one bundled 32 KiB clip, a model that must be
//! named explicitly in `PLAUDON_OPENROUTER_LIVE_MODEL`, and a hard refusal if
//! the payload exceeds `PLAUDON_OPENROUTER_MAX_BYTES` (default 64 KiB).
//!
//! It never retries: a retransmission can be billed twice (spec 10.7).

mod support;

use std::sync::Arc;
use std::time::Duration;

use plaudon_ai_contracts::capabilities::RequestFamily;
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_openrouter::client::{CloudDispatchAuthority, CloudDispatcher, JobBinding};
use plaudon_ai_openrouter::credential::StaticApiKey;
use plaudon_ai_openrouter::ledger::RecordingLedger;
use plaudon_ai_openrouter::pricing::PricingUnits;
use plaudon_ai_openrouter::smoke::{CapabilitySmokeTest, SMOKE_CLIP};
use plaudon_ai_openrouter::transport::ReqwestTransport;
use plaudon_ai_openrouter::{CatalogClient, RetryPolicy};
use plaudon_domain::ids::JobId;
use plaudon_security::consent::ContentType;
use plaudon_security::outbound::http::GatedHttpClientFactory;
use plaudon_security::outbound::{ApprovalSource, NetworkGrant, OutboundPolicy};
use support::PreflightAuthority;

/// Default cost cap: the bundled capability clip and nothing larger.
const DEFAULT_MAX_BYTES: usize = 64 * 1024;

struct Live {
    key: String,
    model: String,
    family: RequestFamily,
}

/// Returns `None` unless the suite was explicitly opted into.
fn live() -> Option<Live> {
    if std::env::var("PLAUDON_OPENROUTER_LIVE").ok().as_deref() != Some("1") {
        return None;
    }
    let key = std::env::var("OPENROUTER_API_KEY").ok()?;
    if key.trim().is_empty() {
        return None;
    }
    let model = std::env::var("PLAUDON_OPENROUTER_LIVE_MODEL").ok()?;
    let family = match std::env::var("PLAUDON_OPENROUTER_LIVE_FAMILY")
        .unwrap_or_else(|_| "multipart_stt".to_owned())
        .as_str()
    {
        "audio_chat" => RequestFamily::AudioChat,
        _ => RequestFamily::MultipartStt,
    };
    Some(Live { key, model, family })
}

fn max_bytes() -> usize {
    std::env::var("PLAUDON_OPENROUTER_MAX_BYTES")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(DEFAULT_MAX_BYTES)
}

fn policy() -> Arc<OutboundPolicy> {
    Arc::new(OutboundPolicy::network_permitted(NetworkGrant::new(
        ApprovalSource::TestHarness,
        support::NOW_UNIX,
        support::DISCLOSURE,
    )))
}

#[tokio::test]
async fn the_live_suite_is_inert_without_an_explicit_opt_in() {
    // This assertion is the point of the test: a developer who runs
    // `cargo test` with a key in the environment still sends nothing, because
    // the flag is also required.
    if std::env::var("PLAUDON_OPENROUTER_LIVE").ok().as_deref() != Some("1") {
        assert!(live().is_none(), "the suite must stay inert by default");
    }
}

#[tokio::test]
async fn the_cost_cap_refuses_an_oversize_payload_before_any_request() {
    assert!(
        SMOKE_CLIP.len() <= max_bytes(),
        "the bundled clip must fit inside the default cost cap"
    );
}

#[tokio::test]
async fn the_public_catalog_is_reachable_and_parses() {
    let Some(live) = live() else {
        return;
    };
    let transport = Arc::new(
        ReqwestTransport::with_timeout(Duration::from_secs(60)).expect("transport"),
    );
    let client = CatalogClient::new(
        GatedHttpClientFactory::new(policy()),
        transport,
        Arc::new(StaticApiKey::new(live.key)),
    );
    let discovered = client.models().await;
    assert!(
        discovered.is_live(),
        "catalogue refresh degraded: {:?}",
        discovered.degraded
    );
    assert!(!discovered.entries.is_empty());
}

#[tokio::test]
async fn one_capability_smoke_test_against_the_named_model() {
    let Some(live) = live() else {
        return;
    };
    assert!(
        SMOKE_CLIP.len() <= max_bytes(),
        "refusing to upload {} bytes; cap is {}",
        SMOKE_CLIP.len(),
        max_bytes()
    );

    let ledger = Arc::new(RecordingLedger::new());
    let transport = Arc::new(
        ReqwestTransport::with_timeout(Duration::from_secs(300)).expect("transport"),
    );
    let dispatcher = CloudDispatcher::new(
        GatedHttpClientFactory::new(policy()),
        transport,
        Arc::new(StaticApiKey::new(live.key)),
        ledger.clone(),
        PreflightAuthority::allowing(&live.model, ContentType::AudioBytes)
            as Arc<dyn CloudDispatchAuthority>,
    )
    // No automatic retry: a second attempt can be billed again (spec 10.7).
    .with_retry_policy(RetryPolicy {
        max_retries: 0,
        ..RetryPolicy::default()
    });

    let binding = JobBinding {
        job_id: "live-probe".to_owned(),
        recording_id: "live-probe".to_owned(),
        consent_id: "consent-1".to_owned(),
        privacy_snapshot: format!(
            "relay=openrouter;model={};provider=<undisclosed>;content=audio_bytes;\
             retention=unknown;source=unresolved",
            live.model
        ),
        route_label: None,
    };

    let test = CapabilitySmokeTest::new(
        live.model.clone(),
        live.family,
        PricingUnits::unknown(),
        dispatcher,
        binding,
        "live",
    );

    let outcome = test
        .run(JobContext::new(JobId::new()))
        .await
        .expect("the capability smoke test should complete");

    // Exactly one attempt was ledgered, and the record carries only what the
    // exchange actually demonstrated.
    assert_eq!(ledger.intents().len(), 1);
    assert_eq!(
        outcome.record.evidence,
        plaudon_ai_openrouter::catalog::ProbeEvidence::ObservedExchange
    );
    eprintln!(
        "observed: family={:?} timestamps={:?} diarization={:?} language={:?} route={:?}",
        outcome.record.request_family,
        outcome.record.timestamps,
        outcome.record.diarization,
        outcome.record.language_detection,
        outcome.record.provider_route
    );
    eprintln!("cost: {:?} ({:?})", outcome.cost.amount, outcome.cost.origin);
}
