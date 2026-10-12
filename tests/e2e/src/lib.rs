//! Spec 18.7 end-to-end acceptance harness (task N1.2).
//!
//! Spec 18.1 asks for "a small set of reliable full-desktop E2E tests" and
//! warns against substituting coverage for meaningful assertions. This crate
//! provides the reliable part:
//!
//! * [`Harness`] drives the real application ports — `plaudon_app::AppPorts`,
//!   the same code path the shell's command dispatcher calls — over a
//!   temporary library, with no user interface, no network and no OS
//!   notification.
//! * [`EgressWatch`] proves a scenario initiated no outbound request, by
//!   running it behind the real `OutboundPolicy` with a recording observer.
//! * [`LocalStt`] runs a *local* transcription step over the real audio
//!   pipeline: the file is validated and decoded, and segment boundaries come
//!   from measured speech activity rather than invented numbers.
//! * The scenario registry in `tests/e2e/scenarios.toml` records the status of
//!   all 44 required scenarios; `plaudon-release scenarios` fails when one
//!   goes missing.
//!
//! # What the local transcription step is, honestly
//!
//! whisper.cpp needs a model artifact that CI does not download (spec 18.3:
//! hermetic by default). So by default [`LocalStt`] pairs *measured* speech
//! spans with the fixture's committed reference text and labels the engine
//! `test-local-stt`. When `PLAUDON_WHISPER_MODEL` points at a real model, the
//! same scenarios can be re-run against the real engine through
//! `cargo test -p plaudon-ai-whisper --features opt-in-model`. Neither path
//! fabricates a timestamp: boundaries are derived from the decoded audio and
//! labelled [`AlignmentOrigin::Derived`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use plaudon_ai_contracts::job::JobContext;
use plaudon_app::events::{EventSink, RecordingEventSink};
use plaudon_app::ports::{MachineRevisionSpec, MachineSegment};
use plaudon_app::secrets::{InMemorySecretStore, SecretStore};
use plaudon_app::{build_ports, AppPorts};
use plaudon_audio::decode::{decode_to_canonical_pcm, CanonicalPcmProfile, MemoryPcmSink};
use plaudon_audio::silence::{f32_samples_from_pcm_bytes, measure_speech_activity, SilenceOptions};
use plaudon_audio::validate::{validate_media, ValidationRequest};
use plaudon_domain::ids::JobId;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_notifications::testing::RecordingNotifier;
use plaudon_notifications::{Dispatcher, NotificationSettings as CrateSettings, Notifier};
use plaudon_security::outbound::{
    Destination, EgressObserver, EgressPurpose, EgressRequest, OutboundPolicy, PayloadClass,
    RecordingObserver, Subsystem,
};

/// Cargo package name, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-e2e";

/// Environment variable that points at a real whisper.cpp model, switching the
/// local transcription step onto the real engine (opt-in, spec 18.3).
pub const WHISPER_MODEL_ENV: &str = "PLAUDON_WHISPER_MODEL";

/// Repository root, resolved from this package's manifest directory.
#[must_use]
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_path_buf()
}

/// Path of a committed audio fixture.
#[must_use]
pub fn fixture(relative: &str) -> PathBuf {
    repo_root().join("tests/fixtures/audio").join(relative)
}

/// A temporary desktop installation: library, source folder and export folder.
#[derive(Debug)]
pub struct Harness {
    /// The real ports the IPC commands are implemented against.
    pub ports: AppPorts,
    /// Events the shell would have published.
    pub sink: Arc<RecordingEventSink>,
    /// Notifications the shell would have raised.
    pub notifier: Arc<RecordingNotifier>,
    library: tempfile::TempDir,
    sources: tempfile::TempDir,
    exports: tempfile::TempDir,
}

impl Harness {
    /// Opens a fresh library, as a first launch on a clean machine would.
    pub async fn fresh_install() -> Self {
        Self::with_notifier(RecordingNotifier::granted()).await
    }

    /// Opens a fresh library with a specific notification permission state.
    pub async fn with_notifier(notifier: Arc<RecordingNotifier>) -> Self {
        let library = tempfile::tempdir().expect("tempdir");
        let sources = tempfile::tempdir().expect("tempdir");
        let exports = tempfile::tempdir().expect("tempdir");
        let sink = Arc::new(RecordingEventSink::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        let dispatcher = Arc::new(
            Dispatcher::new(
                Arc::clone(&notifier) as Arc<dyn Notifier>,
                CrateSettings::uniform(false),
            )
            .with_timeout(Duration::from_millis(100)),
        );
        let ports = build_ports(
            library.path(),
            Arc::clone(&sink) as Arc<dyn EventSink>,
            dispatcher,
            Arc::clone(&secrets) as Arc<dyn SecretStore>,
            vec![sources.path().to_path_buf()],
        )
        .await
        .expect("the library opens");
        Self {
            ports,
            sink,
            notifier,
            library,
            sources,
            exports,
        }
    }

    /// Directory the library lives in, so a scenario can reopen it after a
    /// simulated crash.
    #[must_use]
    pub fn library_dir(&self) -> &Path {
        self.library.path()
    }

    /// Directory exports are written to.
    #[must_use]
    pub fn export_dir(&self) -> &Path {
        self.exports.path()
    }

    /// Copies a committed fixture into the user's source folder and returns
    /// the path the UI would pass to the import command.
    pub fn stage_fixture(&self, relative: &str, as_name: &str) -> String {
        let source = fixture(relative);
        let destination = self.sources.path().join(as_name);
        std::fs::copy(&source, &destination).unwrap_or_else(|e| {
            panic!(
                "copy {} -> {}: {e}",
                source.display(),
                destination.display()
            )
        });
        destination.to_string_lossy().into_owned()
    }

    /// Reopens the same library directory, as a relaunch after a kill would.
    pub async fn reopen(&self) -> AppPorts {
        let sink = Arc::new(RecordingEventSink::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        let dispatcher = Arc::new(Dispatcher::new(
            RecordingNotifier::granted() as Arc<dyn Notifier>,
            CrateSettings::uniform(false),
        ));
        build_ports(
            self.library.path(),
            sink as Arc<dyn EventSink>,
            dispatcher,
            secrets as Arc<dyn SecretStore>,
            vec![self.sources.path().to_path_buf()],
        )
        .await
        .expect("the library reopens")
    }
}

/// Proof that a scenario initiated no outbound request (spec 22(15), 18.6A
/// "Local-only" row).
///
/// The application routes every network-capable subsystem through
/// `OutboundPolicy`. A scenario constructs one of these, runs, and then asserts
/// that the policy authorized nothing. The complementary libc-level proof —
/// that no socket was opened at all — is the separate D2.3 egress fixture in
/// `tests/egress/`; this one proves the decision layer.
#[derive(Debug)]
pub struct EgressWatch {
    policy: OutboundPolicy,
    observer: Arc<RecordingObserver>,
}

impl EgressWatch {
    /// Installs a deny-by-default policy with a recording observer.
    #[must_use]
    pub fn local_only() -> Self {
        let observer = Arc::new(RecordingObserver::new(64));
        let policy = OutboundPolicy::local_only().with_observer(Arc::clone(&observer) as Arc<_>);
        Self { policy, observer }
    }

    /// The policy a subsystem under test must consult.
    #[must_use]
    pub fn policy(&self) -> &OutboundPolicy {
        &self.policy
    }

    /// Attempts an egress the way a cloud subsystem would, returning whether
    /// it was allowed.
    pub fn attempt(
        &self,
        subsystem: Subsystem,
        url: &str,
        purpose: EgressPurpose,
        payload: PayloadClass,
    ) -> bool {
        let Ok(destination) = Destination::url(url) else {
            return false;
        };
        self.policy
            .authorize(&EgressRequest::new(
                subsystem,
                destination,
                purpose,
                payload,
            ))
            .is_ok()
    }

    /// Fails the test when anything was authorized.
    ///
    /// # Panics
    /// When the policy allowed any egress during the scenario.
    pub fn assert_no_egress(&self, scenario: &str) {
        let records = EgressObserver::records(self.observer.as_ref());
        let allowed: Vec<String> = records
            .iter()
            .filter(|record| record.allowed)
            .map(|record| record.destination_label.clone())
            .collect();
        assert!(
            allowed.is_empty(),
            "spec 18.7 scenario `{scenario}` must initiate no outbound request; the policy allowed: {allowed:?}"
        );
    }

    /// Every decision the policy made, for an evidence artifact.
    #[must_use]
    pub fn decisions(&self) -> Vec<String> {
        EgressObserver::records(self.observer.as_ref())
            .iter()
            .map(|record| {
                format!(
                    "{} {} {} allowed={}",
                    record.subsystem.label(),
                    record.purpose.label(),
                    record.destination_label,
                    record.allowed
                )
            })
            .collect()
    }
}

/// A transcription segment produced by the local step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSegment {
    /// Text of the segment.
    pub text: String,
    /// Start offset in milliseconds, derived from the decoded audio.
    pub start_ms: u64,
    /// End offset in milliseconds, derived from the decoded audio.
    pub end_ms: u64,
}

/// Outcome of the local transcription step.
#[derive(Debug, Clone)]
pub struct LocalTranscript {
    /// Engine identifier recorded on the revision.
    pub engine_id: String,
    /// Model identifier recorded on the revision.
    pub model_id: String,
    /// Where the timestamps came from.
    pub alignment_origin: AlignmentOrigin,
    /// The segments.
    pub segments: Vec<LocalSegment>,
}

/// The local transcription step used by the acceptance scenarios.
#[derive(Debug)]
pub struct LocalStt {
    reference: Vec<String>,
}

impl LocalStt {
    /// Builds a step that pairs measured speech spans with `reference` text.
    #[must_use]
    pub fn with_reference(reference: &[&str]) -> Self {
        Self {
            reference: reference.iter().map(|line| (*line).to_owned()).collect(),
        }
    }

    /// Runs the real decode + speech-activity pipeline over `path`.
    ///
    /// # Panics
    /// When the fixture cannot be validated or decoded: an acceptance scenario
    /// that cannot read its own fixture is a failure, not a skip.
    #[must_use]
    pub fn transcribe(&self, path: &Path) -> LocalTranscript {
        let media = validate_media(path, &ValidationRequest::manual_import())
            .unwrap_or_else(|e| panic!("validate {}: {e}", path.display()));
        let profile = CanonicalPcmProfile::whisper_mono_16k();
        let mut sink = MemoryPcmSink::default();
        let context = JobContext::new(JobId::new());
        decode_to_canonical_pcm(&media, &profile, &mut sink, &context)
            .unwrap_or_else(|e| panic!("decode {}: {e}", path.display()));

        let samples = f32_samples_from_pcm_bytes(&sink.bytes);
        let activity = measure_speech_activity(
            &samples,
            profile.sample_rate_hz,
            1,
            &SilenceOptions::default(),
        );

        let rate = u64::from(activity.sample_rate_hz().max(1));
        let to_ms = |samples: u64| samples.saturating_mul(1000) / rate;
        let spans = activity.speech_intervals();
        let total_ms = to_ms(activity.total_samples());
        let mut segments = Vec::new();
        for (index, text) in self.reference.iter().enumerate() {
            // No measured span for this line: the whole measured timeline is
            // the honest bound, never a made-up offset (spec 8.6A).
            let (start_ms, end_ms) = spans
                .get(index)
                .map_or((0, total_ms), |(start, end)| (to_ms(*start), to_ms(*end)));
            segments.push(LocalSegment {
                text: text.clone(),
                start_ms,
                end_ms,
            });
        }

        LocalTranscript {
            engine_id: "test-local-stt".to_owned(),
            model_id: "fixture-reference".to_owned(),
            alignment_origin: AlignmentOrigin::Derived,
            segments,
        }
    }

    /// Converts the result into the revision the library stores.
    #[must_use]
    pub fn revision_spec(
        transcript: &LocalTranscript,
        recording_id: plaudon_domain::ids::RecordingId,
    ) -> MachineRevisionSpec {
        MachineRevisionSpec {
            recording_id,
            segments: transcript
                .segments
                .iter()
                .map(|segment| {
                    MachineSegment::timed(&segment.text, segment.start_ms, segment.end_ms)
                })
                .collect(),
            engine_id: transcript.engine_id.clone(),
            model_id: transcript.model_id.clone(),
            alignment_origin: transcript.alignment_origin,
        }
    }
}
