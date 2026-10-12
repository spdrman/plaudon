//! Hermetic harness for the wire-contract suite (spec 18.3).
//!
//! There is no socket and no mock HTTP server: the request still passes the
//! real egress gate and the real consent preflight, and is then handed to a
//! [`FixtureTransport`] that replays a captured response from
//! `tests/fixtures/openrouter/`. That keeps the suite hermetic (CI has no
//! network) while exercising exactly the code a live request would take —
//! including the fact that a refused request never reaches the transport at
//! all, which is how "zero outgoing audio bytes" is asserted.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::{MediaFormat, RequestFamily};
use plaudon_ai_openrouter::client::{CloudDispatchAuthority, DispatchRefused};
use plaudon_ai_openrouter::ledger::RecordingLedger;
use plaudon_ai_openrouter::registry::{FeatureMatrix, ModelCard, Verification};
use plaudon_ai_openrouter::transport::{AsyncHttpTransport, TransportFailure};
use plaudon_ai_openrouter::{
    AllowlistEntry, CloudDispatcher, JobBinding, ModelTask, OpenRouterSpeechToText,
};
use plaudon_domain::capability::Known;
use plaudon_security::consent::{
    dispatch_preflight, ConsentGrant, ConsentPurpose, ContentType, DispatchApproval,
    DispatchRequest, ProviderRoute, RELAY_OPENROUTER,
};
use plaudon_security::outbound::http::{GatedHttpClientFactory, GatedRequest, HttpResponse};
use plaudon_security::outbound::{ApprovalSource, NetworkGrant, OutboundPolicy};

/// Disclosure version used across the suite.
pub const DISCLOSURE: &str = "2026-10-11";

/// Fixed clock reading, so `Retry-After` HTTP-date arithmetic is deterministic.
pub const NOW_UNIX: i64 = 1_793_000_000;

/// Load a captured fixture.
pub fn fixture(name: &str) -> serde_json::Value {
    let path = fixtures_dir().join(format!("{name}.json"));
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// Directory holding the captured fixtures.
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .join("tests/fixtures/openrouter")
}

/// Turn a fixture into the response the transport replays.
pub fn response_from(name: &str) -> HttpResponse {
    let value = fixture(name);
    let status = u16::try_from(value["status"].as_u64().expect("status")).expect("status fits");
    let mut headers = BTreeMap::new();
    if let Some(object) = value.get("headers").and_then(|v| v.as_object()) {
        for (key, header) in object {
            headers.insert(
                key.to_ascii_lowercase(),
                header.as_str().unwrap_or_default().to_owned(),
            );
        }
    }
    if let Some(seconds) = value.get("retry_after_seconds").and_then(|v| v.as_u64()) {
        headers.insert("retry-after".to_owned(), seconds.to_string());
    }
    if let Some(location) = value.get("location").and_then(|v| v.as_str()) {
        headers.insert("location".to_owned(), location.to_owned());
    }
    let body = match value.get("truncated").and_then(serde_json::Value::as_bool) {
        Some(true) => value["body_prefix"]
            .as_str()
            .unwrap_or_default()
            .as_bytes()
            .to_vec(),
        _ => serde_json::to_vec(&value["body"]).unwrap_or_default(),
    };
    HttpResponse {
        status,
        headers,
        body,
    }
}

/// What the transport observed about one request. Never holds the credential.
#[derive(Debug, Clone)]
pub struct ObservedRequest {
    /// Request URL.
    pub url: String,
    /// Header names, lowercased. Values are deliberately dropped except for
    /// the ones a test must assert on.
    pub header_names: Vec<String>,
    /// `Content-Type` header, which carries no secret.
    pub content_type: Option<String>,
    /// Whether an `Authorization` header was attached at all.
    pub has_authorization: bool,
    /// Exact `Authorization` value, so the secret-leak tests can prove it is
    /// present on the wire and absent everywhere else.
    pub authorization: Option<String>,
    /// Body bytes.
    pub body: Vec<u8>,
}

/// A transport that replays queued fixtures and records what it was asked to
/// send.
#[derive(Debug, Default)]
pub struct FixtureTransport {
    queue: Mutex<Vec<Result<HttpResponse, TransportFailure>>>,
    seen: Mutex<Vec<ObservedRequest>>,
}

impl FixtureTransport {
    /// A transport that will replay `responses` in order.
    pub fn new(responses: Vec<Result<HttpResponse, TransportFailure>>) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(responses),
            seen: Mutex::new(Vec::new()),
        })
    }

    /// A transport replaying one fixture.
    pub fn replaying(name: &str) -> Arc<Self> {
        Self::new(vec![Ok(response_from(name))])
    }

    /// Everything the transport was asked to send.
    pub fn requests(&self) -> Vec<ObservedRequest> {
        self.seen.lock().expect("transport lock").clone()
    }

    /// Total body bytes handed to the transport. Zero proves nothing left.
    pub fn transmitted_bytes(&self) -> usize {
        self.requests().iter().map(|r| r.body.len()).sum()
    }

    /// Number of attempts.
    pub fn attempts(&self) -> usize {
        self.requests().len()
    }
}

#[async_trait]
impl AsyncHttpTransport for FixtureTransport {
    async fn send(&self, request: GatedRequest) -> Result<HttpResponse, TransportFailure> {
        let headers = request.headers();
        self.seen
            .lock()
            .expect("transport lock")
            .push(ObservedRequest {
                url: request.url().to_owned(),
                header_names: headers.keys().map(|k| k.to_ascii_lowercase()).collect(),
                content_type: headers.get("Content-Type").cloned(),
                has_authorization: headers.contains_key("Authorization"),
                authorization: headers.get("Authorization").cloned(),
                body: request.body().unwrap_or_default().to_vec(),
            });
        let mut queue = self.queue.lock().expect("transport lock");
        if queue.is_empty() {
            return Err(TransportFailure::ConnectFailed {
                detail: "fixture queue exhausted".to_owned(),
            });
        }
        queue.remove(0)
    }
}

/// The real consent preflight over a fixed policy and grant set.
#[derive(Debug)]
pub struct PreflightAuthority {
    policy: Arc<OutboundPolicy>,
    grants: Vec<ConsentGrant>,
    downstream_provider: Option<String>,
}

impl PreflightAuthority {
    /// An authority that approves `model_id` for `content_type`.
    pub fn allowing(model_id: &str, content_type: ContentType) -> Arc<Self> {
        let route = route_for(model_id);
        let grant = ConsentGrant::for_model_policy(
            "consent-1",
            route,
            purpose_for(content_type),
            DISCLOSURE,
            NOW_UNIX - 60,
        )
        .with_content_type(content_type);
        Arc::new(Self {
            policy: Arc::new(OutboundPolicy::network_permitted(NetworkGrant::new(
                ApprovalSource::TestHarness,
                NOW_UNIX - 120,
                DISCLOSURE,
            ))),
            grants: vec![grant],
            downstream_provider: Some("OpenAI".to_owned()),
        })
    }

    /// An authority with network permitted but no consent grant at all.
    pub fn without_consent() -> Arc<Self> {
        Arc::new(Self {
            policy: Arc::new(OutboundPolicy::network_permitted(NetworkGrant::new(
                ApprovalSource::TestHarness,
                NOW_UNIX - 120,
                DISCLOSURE,
            ))),
            grants: Vec::new(),
            downstream_provider: Some("OpenAI".to_owned()),
        })
    }

    /// An authority in Local-only Mode, where no grant can help.
    pub fn local_only(model_id: &str, content_type: ContentType) -> Arc<Self> {
        let route = route_for(model_id);
        let grant = ConsentGrant::for_model_policy(
            "consent-1",
            route,
            purpose_for(content_type),
            DISCLOSURE,
            NOW_UNIX - 60,
        )
        .with_content_type(content_type);
        Arc::new(Self {
            policy: Arc::new(OutboundPolicy::local_only()),
            grants: vec![grant],
            downstream_provider: Some("OpenAI".to_owned()),
        })
    }

    /// The egress policy, so a test can read its decision log.
    pub fn policy(&self) -> Arc<OutboundPolicy> {
        Arc::clone(&self.policy)
    }
}

/// The route the harness consents to.
pub fn route_for(model_id: &str) -> ProviderRoute {
    ProviderRoute::new(RELAY_OPENROUTER, model_id)
}

fn purpose_for(content_type: ContentType) -> ConsentPurpose {
    match content_type {
        ContentType::AudioBytes => ConsentPurpose::Transcription,
        _ => ConsentPurpose::Summarization,
    }
}

#[async_trait]
impl CloudDispatchAuthority for PreflightAuthority {
    async fn authorize(
        &self,
        content_type: ContentType,
        model_id: &str,
    ) -> Result<DispatchApproval, DispatchRefused> {
        let request = DispatchRequest::new(
            purpose_for(content_type),
            content_type,
            route_for(model_id),
            "recording-1",
            DISCLOSURE,
        );
        dispatch_preflight(&self.policy, &self.grants, &request, NOW_UNIX).map_err(|error| {
            use plaudon_security::consent::ConsentError;
            match error {
                ConsentError::Egress(_) => DispatchRefused::Egress,
                ConsentError::PrivacyPostureUnresolved { .. } => DispatchRefused::PrivacyUnresolved,
                _ => DispatchRefused::ConsentRequired,
            }
        })
    }
}

/// A verified card, as a passing capability smoke test would leave it.
pub fn verified_card(model_id: &str, family: RequestFamily) -> ModelCard {
    let entry = AllowlistEntry {
        model_id: model_id.to_owned(),
        display_name: model_id.to_owned(),
        task: ModelTask::Transcription,
        candidate_family: family,
        fixture_id: "transcription-success-segments".to_owned(),
        notes: String::new(),
    };
    let mut card = ModelCard::from_allowlist(&entry);
    card.request_family = family;
    card.verification = Verification::Verified {
        fixture_id: "transcription-success-segments".to_owned(),
        validated_at: "2026-10-11".to_owned(),
    };
    card.features = FeatureMatrix::default();
    card.max_input_bytes = Known::Known(26_214_400);
    card.max_duration_ms = Known::Known(1_500_000);
    card
}

/// The harness's assembled client plus the pieces a test asserts on.
pub struct Harness {
    /// The bound client.
    pub client: OpenRouterSpeechToText,
    /// The transport that would have carried the request.
    pub transport: Arc<FixtureTransport>,
    /// The ledger the dispatch had to write to first.
    pub ledger: Arc<RecordingLedger>,
}

/// Build a speech client over a fixture transport.
pub fn harness(
    card: ModelCard,
    transport: Arc<FixtureTransport>,
    authority: Arc<dyn CloudDispatchAuthority>,
    ledger: Arc<RecordingLedger>,
) -> Harness {
    let policy = Arc::new(OutboundPolicy::network_permitted(NetworkGrant::new(
        ApprovalSource::TestHarness,
        NOW_UNIX - 120,
        DISCLOSURE,
    )));
    let dispatcher = CloudDispatcher::new(
        GatedHttpClientFactory::new(policy),
        transport.clone(),
        Arc::new(plaudon_ai_openrouter::credential::StaticApiKey::new(
            TEST_API_KEY,
        )),
        ledger.clone(),
        authority,
    )
    .with_retry_policy(plaudon_ai_openrouter::RetryPolicy {
        max_retries: 2,
        base_delay: std::time::Duration::from_millis(1),
        max_delay: std::time::Duration::from_millis(2),
        max_retry_after: std::time::Duration::from_millis(5),
    });
    let client = OpenRouterSpeechToText::new(
        card,
        dispatcher,
        JobBinding {
            job_id: "job-1".to_owned(),
            recording_id: "recording-1".to_owned(),
            consent_id: "consent-1".to_owned(),
            privacy_snapshot: "relay=openrouter;model=m;provider=OpenAI;content=audio_bytes;retention=unknown;source=unresolved".to_owned(),
            route_label: Some("openrouter|m|OpenAI".to_owned()),
        },
    )
    .with_boundary_nonce("contract")
    .with_now_unix(NOW_UNIX);
    Harness {
        client,
        transport,
        ledger,
    }
}

/// The credential the harness uses. Tests assert this string appears nowhere
/// except the `Authorization` header.
pub const TEST_API_KEY: &str = "sk-or-v1-harness-secret-value";

/// Formats the harness accepts.
pub fn wav() -> MediaFormat {
    MediaFormat::new("wav", "pcm_s16le")
}
