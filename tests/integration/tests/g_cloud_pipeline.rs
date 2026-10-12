//! EPIC G acceptance, end to end over a real library (spec §10, §16.2A).
//!
//! Proves the three claims the EPIC is judged on, with no network: a recording
//! is transcribed through the OpenRouter client with its cost recorded in the
//! ledger; a per-recording model override changes which model runs without
//! touching the default; and Local-only Mode blocks every cloud request before
//! a byte leaves.
//!
//! The provider is a replay transport over the captured wire fixtures in
//! `tests/fixtures/openrouter/`. Everything else — egress gate, consent
//! preflight, SQLite ledger, transcript persistence — is the production code.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::{MediaFormat, RequestFamily};
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::stt::{SttError, SttInput};
use plaudon_ai_openrouter::allowlist::{AllowlistEntry, ModelTask};
use plaudon_ai_openrouter::catalog::{ModelProbeRecord, ProbeEvidence, TimestampSupport};
use plaudon_ai_openrouter::client::{CloudDispatcher, JobBinding, OpenRouterSpeechToText};
use plaudon_ai_openrouter::credential::StaticApiKey;
use plaudon_ai_openrouter::registry::{ModelCard, Verification};
use plaudon_ai_openrouter::service::{CloudAi, ModelChoice, Stage};
use plaudon_ai_openrouter::transport::{AsyncHttpTransport, TransportFailure};
use plaudon_domain::capability::{DispatchState, Known, Tri};
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::money::MoneyEstimate;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_persistence::db::Database;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::transcript::{RevisionContext, TranscriptRepository};
use plaudon_security::consent::{
    dispatch_preflight, ConsentError, ConsentGrant, ConsentPurpose, ContentType, DispatchApproval,
    DispatchRequest, ProviderRoute, RELAY_OPENROUTER,
};
use plaudon_security::outbound::http::{GatedHttpClientFactory, GatedRequest, HttpResponse};
use plaudon_security::outbound::{ApprovalSource, NetworkGrant, OutboundPolicy};
use tempfile::TempDir;

const MODEL: &str = "openai/whisper-1";
const DISCLOSURE: &str = "2026-10-11";
const NOW: i64 = 1_793_000_000;

// ---------------------------------------------------------------------------
// Replay transport over the captured fixtures
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct ReplayTransport {
    response: Mutex<Option<HttpResponse>>,
    sent_bytes: Mutex<usize>,
    requests: Mutex<usize>,
}

impl ReplayTransport {
    fn replaying(fixture: &str) -> Arc<Self> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("repository root")
            .join("tests/fixtures/openrouter")
            .join(format!("{fixture}.json"));
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("fixture")).expect("fixture json");
        Arc::new(Self {
            response: Mutex::new(Some(HttpResponse {
                status: u16::try_from(value["status"].as_u64().expect("status")).expect("fits"),
                headers: BTreeMap::from([(
                    "x-request-id".to_owned(),
                    "req_integration".to_owned(),
                )]),
                body: serde_json::to_vec(&value["body"]).expect("body"),
            })),
            sent_bytes: Mutex::new(0),
            requests: Mutex::new(0),
        })
    }

    fn sent_bytes(&self) -> usize {
        *self.sent_bytes.lock().expect("lock")
    }

    fn requests(&self) -> usize {
        *self.requests.lock().expect("lock")
    }
}

#[async_trait]
impl AsyncHttpTransport for ReplayTransport {
    async fn send(&self, request: GatedRequest) -> Result<HttpResponse, TransportFailure> {
        *self.requests.lock().expect("lock") += 1;
        *self.sent_bytes.lock().expect("lock") += request.body().unwrap_or_default().len();
        self.response
            .lock()
            .expect("lock")
            .take()
            .ok_or(TransportFailure::ConnectFailed {
                detail: "replay exhausted".to_owned(),
            })
    }
}

// ---------------------------------------------------------------------------
// The production consent path, over a fixed policy and grant
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Authority {
    policy: Arc<OutboundPolicy>,
    grants: Vec<ConsentGrant>,
}

impl Authority {
    fn new(policy: Arc<OutboundPolicy>, model_id: &str) -> Arc<Self> {
        Arc::new(Self {
            policy,
            grants: vec![ConsentGrant::for_model_policy(
                "consent-1",
                ProviderRoute::new(RELAY_OPENROUTER, model_id),
                ConsentPurpose::Transcription,
                DISCLOSURE,
                NOW - 60,
            )
            .with_content_type(ContentType::AudioBytes)],
        })
    }
}

#[async_trait]
impl plaudon_ai_openrouter::client::CloudDispatchAuthority for Authority {
    async fn authorize(
        &self,
        content_type: ContentType,
        model_id: &str,
    ) -> Result<DispatchApproval, plaudon_ai_openrouter::client::DispatchRefused> {
        let request = DispatchRequest::new(
            ConsentPurpose::Transcription,
            content_type,
            ProviderRoute::new(RELAY_OPENROUTER, model_id),
            "recording-1",
            DISCLOSURE,
        );
        dispatch_preflight(&self.policy, &self.grants, &request, NOW).map_err(|error| {
            use plaudon_ai_openrouter::client::DispatchRefused;
            match error {
                ConsentError::Egress(_) => DispatchRefused::Egress,
                ConsentError::PrivacyPostureUnresolved { .. } => DispatchRefused::PrivacyUnresolved,
                _ => DispatchRefused::ConsentRequired,
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Library fixture
// ---------------------------------------------------------------------------

struct Library {
    _dir: TempDir,
    db: Database,
    cloud: CloudAi,
    recording_id: RecordingId,
    job_id: JobId,
}

async fn library() -> Library {
    let dir = TempDir::new().expect("tempdir");
    let db = Database::open(dir.path().join("app.sqlite"))
        .await
        .expect("open");
    MigrationRunner::new().run(&db).await.expect("migrate");

    let recording_id = RecordingId::new();
    let job_id = JobId::new();
    sqlx::query(
        "INSERT INTO recordings (id, title, captured_at, created_at) VALUES (?1,'Weekly',0,0)",
    )
    .bind(recording_id.to_string())
    .execute(db.pool())
    .await
    .expect("recording");
    sqlx::query(
        "INSERT INTO consent_grants (id, scope, privacy_policy_version, engine, model_or_provider, approved_at) \
         VALUES ('consent-1','model_policy',?1,'openrouter',?2,0)",
    )
    .bind(DISCLOSURE)
    .bind(MODEL)
    .execute(db.pool())
    .await
    .expect("consent");
    sqlx::query(
        "INSERT INTO jobs (id, job_type, state, created_at) VALUES (?1,'transcribe','queued',0)",
    )
    .bind(job_id.to_string())
    .execute(db.pool())
    .await
    .expect("job");

    let cloud = CloudAi::open(db.clone()).expect("cloud");
    Library {
        _dir: dir,
        db,
        cloud,
        recording_id,
        job_id,
    }
}

fn verified_card(model_id: &str) -> ModelCard {
    let mut card = ModelCard::from_allowlist(&AllowlistEntry {
        model_id: model_id.to_owned(),
        display_name: model_id.to_owned(),
        task: ModelTask::Transcription,
        candidate_family: RequestFamily::MultipartStt,
        fixture_id: "transcription-success-segments".to_owned(),
        notes: String::new(),
    });
    card.request_family = RequestFamily::MultipartStt;
    card.verification = Verification::Verified {
        fixture_id: "transcription-success-segments".to_owned(),
        validated_at: "2026-10-11".to_owned(),
    };
    card.max_input_bytes = Known::Known(26_214_400);
    card
}

fn audio(dir: &std::path::Path) -> (std::path::PathBuf, Vec<u8>) {
    let bytes = b"RIFF....WAVEfixture-audio".to_vec();
    let path = dir.join("audio.wav");
    std::fs::write(&path, &bytes).expect("write audio");
    (path, bytes)
}

fn client(
    library: &Library,
    card: ModelCard,
    policy: Arc<OutboundPolicy>,
    transport: Arc<ReplayTransport>,
) -> OpenRouterSpeechToText {
    let dispatcher = CloudDispatcher::new(
        GatedHttpClientFactory::new(Arc::clone(&policy)),
        transport,
        Arc::new(StaticApiKey::new("sk-or-v1-integration")),
        library.cloud.ledger_handle(),
        Authority::new(policy, &card.model_id),
    );
    OpenRouterSpeechToText::new(
        card,
        dispatcher,
        JobBinding {
            job_id: library.job_id.to_string(),
            recording_id: library.recording_id.to_string(),
            consent_id: "consent-1".to_owned(),
            privacy_snapshot: format!(
                "relay=openrouter;model={MODEL};provider=<undisclosed>;content=audio_bytes;\
                 retention=unknown;source=unresolved"
            ),
            route_label: None,
        },
    )
    .with_now_unix(NOW)
}

fn network_permitted() -> Arc<OutboundPolicy> {
    Arc::new(OutboundPolicy::network_permitted(NetworkGrant::new(
        ApprovalSource::TestHarness,
        NOW - 100,
        DISCLOSURE,
    )))
}

// ---------------------------------------------------------------------------
// The three EPIC-level claims
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_recording_is_transcribed_through_openrouter_and_its_cost_is_ledgered() {
    let library = library().await;
    let dir = TempDir::new().expect("tempdir");
    let (path, bytes) = audio(dir.path());
    let transport = ReplayTransport::replaying("transcription-success-segments");
    let client = client(
        &library,
        verified_card(MODEL),
        network_permitted(),
        Arc::clone(&transport),
    );

    let mut input = SttInput::whole_file(
        path,
        Sha256Digest::of(&bytes),
        bytes.len() as u64,
        MediaFormat::new("wav", "pcm_s16le"),
        16_000,
        1,
    );
    input.duration_ms = Some(1_840);

    let outcome = client
        .transcribe_with_cost(input, JobContext::new(library.job_id))
        .await
        .expect("transcribes");

    assert_eq!(outcome.draft.alignment_origin, AlignmentOrigin::Native);
    assert_eq!(outcome.draft.plain_text(), "Good morning everyone.");
    assert_eq!(transport.requests(), 1);
    assert!(transport.sent_bytes() > 0);

    // The revision persists like any other machine revision.
    let revisions = TranscriptRepository::new(library.db.clone());
    let revision_id = revisions
        .save_draft(
            &outcome.draft,
            &RevisionContext::machine(library.recording_id, Timestamp::from_millis(NOW * 1_000)),
        )
        .await
        .expect("persists");
    let stored = revisions.revision(revision_id).await.expect("stored");
    assert_eq!(stored.engine.as_str(), "openrouter");

    // The ledger row exists, is settled, and names the route and consent.
    let attempts = library
        .cloud
        .ledger()
        .attempts_for_job(&library.job_id.to_string())
        .await
        .expect("attempts");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].state(), DispatchState::Completed);
    assert_eq!(attempts[0].model_id, MODEL);
    assert_eq!(attempts[0].consent_id.as_deref(), Some("consent-1"));
    assert_eq!(
        attempts[0].provider_request_id.as_deref(),
        Some("req_integration")
    );

    // The fixture reports no usage, so the cost is explicitly unknown rather
    // than silently zero, and the Jobs view can say so.
    assert_eq!(
        attempts[0].settled_cost().expect("settled").amount,
        MoneyEstimate::Unknown
    );
    let summary = library
        .cloud
        .usage(Timestamp::from_millis(0), Timestamp::now())
        .await
        .expect("usage");
    assert_eq!(summary.totals.attempts, 1);
    assert_eq!(summary.totals.unknown_count, 1);
    assert_eq!(summary.totals.finalized, None);
}

#[tokio::test]
async fn a_provider_reported_cost_is_recorded_as_a_finalized_amount() {
    let library = library().await;
    let dir = TempDir::new().expect("tempdir");
    let (path, bytes) = audio(dir.path());
    let mut card = verified_card("openai/gpt-audio");
    card.request_family = RequestFamily::AudioChat;
    let transport = ReplayTransport::replaying("chat-completion-structured");
    let client = client(&library, card, network_permitted(), Arc::clone(&transport));

    let input = SttInput::whole_file(
        path,
        Sha256Digest::of(&bytes),
        bytes.len() as u64,
        MediaFormat::new("wav", "pcm_s16le"),
        16_000,
        1,
    );
    let outcome = client
        .transcribe_with_cost(input, JobContext::new(library.job_id))
        .await
        .expect("transcribes");
    assert!(outcome.cost.amount.is_finalized());

    let summary = library
        .cloud
        .usage(Timestamp::from_millis(0), Timestamp::now())
        .await
        .expect("usage");
    assert!(summary.totals.finalized.is_some());
    assert_eq!(summary.totals.unknown_count, 0);
}

#[tokio::test]
async fn local_only_mode_blocks_every_cloud_request() {
    let library = library().await;
    let dir = TempDir::new().expect("tempdir");
    let (path, bytes) = audio(dir.path());
    let transport = ReplayTransport::replaying("transcription-success-segments");
    let client = client(
        &library,
        verified_card(MODEL),
        Arc::new(OutboundPolicy::local_only()),
        Arc::clone(&transport),
    );

    let input = SttInput::whole_file(
        path,
        Sha256Digest::of(&bytes),
        bytes.len() as u64,
        MediaFormat::new("wav", "pcm_s16le"),
        16_000,
        1,
    );
    let error = client
        .transcribe_with_cost(input, JobContext::new(library.job_id))
        .await
        .expect_err("blocked");

    assert_eq!(error, SttError::Unavailable);
    assert_eq!(transport.requests(), 0, "no request may be attempted");
    assert_eq!(transport.sent_bytes(), 0, "no byte may leave the machine");
    assert!(
        library
            .cloud
            .ledger()
            .attempts_for_job(&library.job_id.to_string())
            .await
            .expect("attempts")
            .is_empty(),
        "a blocked request has nothing to bill and nothing to record"
    );
}

#[tokio::test]
async fn a_per_recording_override_selects_a_different_model_than_the_default() {
    let library = library().await;
    let at = Timestamp::from_millis(1);
    let default = ModelChoice::new("whisper.cpp", Some("base.en".to_owned()));
    let override_choice = ModelChoice::new("openrouter", Some(MODEL.to_owned()));
    library
        .cloud
        .store()
        .set_task_default(Stage::Transcribe, &default, at)
        .await
        .expect("default");
    library
        .cloud
        .store()
        .set_recording_override(
            &library.recording_id.to_string(),
            Stage::Transcribe,
            &override_choice,
            at,
        )
        .await
        .expect("override");

    let resolved = library
        .cloud
        .effective_choice(&library.recording_id.to_string(), Stage::Transcribe, None)
        .await
        .expect("resolve")
        .expect("choice");
    assert_eq!(resolved.choice, override_choice);
    assert!(resolved.choice.is_cloud());
    assert_eq!(
        library
            .cloud
            .store()
            .task_default(Stage::Transcribe)
            .await
            .expect("default"),
        Some(default),
        "the global default is untouched by a per-recording override"
    );
}

#[tokio::test]
async fn an_unverified_model_cannot_be_dispatched_to_even_with_consent() {
    let library = library().await;
    let dir = TempDir::new().expect("tempdir");
    let (path, bytes) = audio(dir.path());
    let mut card = verified_card(MODEL);
    card.verification = Verification::NotVerified {
        reason: plaudon_ai_openrouter::registry::NotVerifiedReason::NoObservedExchange,
    };
    let transport = ReplayTransport::replaying("transcription-success-segments");
    let client = client(&library, card, network_permitted(), Arc::clone(&transport));

    let input = SttInput::whole_file(
        path,
        Sha256Digest::of(&bytes),
        bytes.len() as u64,
        MediaFormat::new("wav", "pcm_s16le"),
        16_000,
        1,
    );
    assert_eq!(
        client
            .transcribe_with_cost(input, JobContext::new(library.job_id))
            .await
            .expect_err("refused"),
        SttError::Unavailable
    );
    assert_eq!(transport.sent_bytes(), 0);
}

#[tokio::test]
async fn a_stored_probe_record_is_what_makes_a_model_offerable() {
    let library = library().await;
    let before = library
        .cloud
        .catalog(
            ModelTask::Transcription,
            None,
            plaudon_ai_openrouter::discovery::DegradedReason::LocalOnlyMode,
        )
        .await
        .expect("catalog");
    assert!(
        before.cards.iter().all(|card| !card.is_selectable()),
        "nothing ships pre-verified"
    );

    library
        .cloud
        .store()
        .record_probe(
            ModelTask::Transcription,
            &ModelProbeRecord {
                model_id: MODEL.to_owned(),
                request_family: RequestFamily::MultipartStt,
                evidence: ProbeEvidence::ObservedExchange,
                accepted_formats: Known::Known(vec!["wav".to_owned()]),
                max_input_bytes: Known::Unknown,
                max_duration_ms: Known::Unknown,
                timestamps: TimestampSupport::ObservedSegments,
                diarization: Tri::No,
                language_detection: Tri::Yes,
                provider_route: Known::Known("OpenAI".to_owned()),
                retention_control: Known::Unknown,
                audio_input_price: Known::Unknown,
                pricing_unit: Known::Unknown,
                observed_on: "2026-10-11".to_owned(),
                audio_tokens_per_second: Known::Unknown,
                structured_output: Tri::Unknown,
                max_output_tokens: Known::Unknown,
                notes: String::new(),
            },
            "capability-check",
            Timestamp::from_millis(1),
        )
        .await
        .expect("record");

    let after = library
        .cloud
        .catalog(
            ModelTask::Transcription,
            None,
            plaudon_ai_openrouter::discovery::DegradedReason::LocalOnlyMode,
        )
        .await
        .expect("catalog");
    let card = after
        .cards
        .iter()
        .find(|card| card.model_id == MODEL)
        .expect("card");
    assert!(card.is_selectable());
}
