//! The committed DER artifact schema must accept what the Rust types write,
//! refuse a document that claims more than it measured, and stay stable
//! (ADR-0400 condition 4).
//!
//! The stability check is a content hash of the schema file: changing the
//! published artifact shape means bumping `schema_version`, adding
//! `schemas/diarization-der-report.v2.schema.json` and updating the digest here
//! in the same commit — the same discipline as the speaker-matching report.

use std::path::{Path, PathBuf};

use plaudon_ai_diarization::der::{diarization_error_rate_by_overlap, DerOptions};
use plaudon_ai_diarization::report::{
    open_conditions, ConditionPool, CorpusRecord, DerArtifact, ModelRecord, ParameterRecord,
    PooledDer, ProviderRecord, ReleaseEvidence, RuntimeRecord, SessionMeasurement,
    DIARIZATION_DER_REPORT_SCHEMA_VERSION,
};
use plaudon_ai_diarization::{ReferenceTurn, SpeakerTurn};
use plaudon_domain::media::Sha256Digest;

/// SHA-256 of `schemas/diarization-der-report.v1.schema.json`.
const COMMITTED_SCHEMA_SHA256: &str =
    "b8df1672dc5414faea096f391ba2e7885d45eb27c0b4c6e41625e0c2b2c04033";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_path_buf()
}

fn schema_path() -> PathBuf {
    repo_root().join("schemas/diarization-der-report.v1.schema.json")
}

fn validator() -> jsonschema::Validator {
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(schema_path()).expect("read schema"))
            .expect("parse schema");
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .expect("the schema must compile")
}

fn errors(instance: &serde_json::Value) -> Vec<String> {
    validator()
        .iter_errors(instance)
        .map(|error| error.to_string())
        .collect()
}

/// An artifact of the shape the measurement bridge writes.
fn artifact() -> DerArtifact {
    let reference = vec![
        ReferenceTurn::new("roster-01", 0, 4_000),
        ReferenceTurn::new("roster-02", 3_400, 7_400),
    ];
    let hypothesis = vec![
        SpeakerTurn::new("cluster-01", 0, 3_900),
        SpeakerTurn::new("cluster-02", 3_400, 7_500),
    ];
    let der = diarization_error_rate_by_overlap(&reference, &hypothesis, DerOptions::default());
    let sessions = vec![SessionMeasurement {
        session_id: "librispeech-session-01".to_owned(),
        condition: "clean".to_owned(),
        media_sha256: "a".repeat(64),
        duration_ms: 7_400,
        reference_speakers: 2,
        reference_speech_ms: der.overall.total_reference_ms,
        overlapping_reference_ms: der.overall.overlapping_reference_ms,
        hypothesis_clusters: der.overall.hypothesis_cluster_count,
        hypothesis_turns: hypothesis.len(),
        elapsed_ms: 1_200,
        der,
    }];
    let provider = ProviderRecord {
        provider_id: "onnx-pyannote-wespeaker".to_owned(),
        provider_version: "3.0+voxceleb-2022-10".to_owned(),
        validated: false,
        models: vec![ModelRecord {
            model_id: "pyannote-segmentation".to_owned(),
            version: "3.0".to_owned(),
            sha256: "b".repeat(64),
            licence_spdx: "MIT".to_owned(),
            role: "frame-level speaker-activity segmentation".to_owned(),
        }],
        parameters: vec![ParameterRecord {
            name: "cluster.threshold".to_owned(),
            value: "0.5000".to_owned(),
            provenance: "declared default".to_owned(),
        }],
    };
    let runtime = RuntimeRecord {
        platform: "macos-arm64".to_owned(),
        host_cpu: "Apple M5".to_owned(),
        os_build: "macos 26.0 (25A000)".to_owned(),
        intra_op_threads: 1,
        peak_rss_bytes: 512_000_000,
        startup_ms: 400,
        real_time_factor: 0.2,
    };
    let blockers = open_conditions(&provider, &runtime, &sessions);
    DerArtifact {
        schema_version: DIARIZATION_DER_REPORT_SCHEMA_VERSION,
        generator: "plaudon-speaker-embed 0.1.0".to_owned(),
        measured_on: "2026-10-12T00:00:00Z".to_owned(),
        commit: "0".repeat(40),
        provider,
        runtime,
        corpus: CorpusRecord {
            library_id: "librispeech-session-v1".to_owned(),
            corpus: "libri_speech_test_clean_derived".to_owned(),
            licence: "CC-BY-4.0".to_owned(),
            attribution: "LibriSpeech (Panayotov et al., openslr.org/12), CC-BY-4.0".to_owned(),
            audio_committed: false,
            recipe: "mixed by plaudon-speaker-embed build-session".to_owned(),
        },
        by_condition: ConditionPool::of(&sessions),
        overall: PooledDer::of(&sessions),
        sessions,
        release_evidence: ReleaseEvidence::new(blockers),
        caveats: vec![
            "synthetic mix".to_owned(),
            "zero collar, overlap included".to_owned(),
            "a cluster is not an identity".to_owned(),
        ],
    }
}

#[test]
fn the_schema_file_has_not_changed_without_a_version_bump() {
    let bytes = std::fs::read(schema_path()).expect("read schema");
    assert_eq!(
        Sha256Digest::of(&bytes).to_hex(),
        COMMITTED_SCHEMA_SHA256,
        "the published artifact shape changed: bump schema_version, add a v2 schema and update \
         this digest in the same commit"
    );
}

#[test]
fn an_artifact_the_measurement_bridge_writes_satisfies_the_schema() {
    let instance = serde_json::to_value(artifact()).expect("serialize");
    let errors = errors(&instance);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn a_document_that_claims_to_qualify_while_listing_blockers_is_refused() {
    let mut instance = serde_json::to_value(artifact()).expect("serialize");
    instance["release_evidence"]["qualifies"] = serde_json::json!(true);
    assert!(
        !errors(&instance).is_empty(),
        "the schema must refuse a verdict that contradicts its own blockers"
    );
}

#[test]
fn a_document_that_claims_the_audio_is_committed_is_refused() {
    let mut instance = serde_json::to_value(artifact()).expect("serialize");
    instance["corpus"]["audio_committed"] = serde_json::json!(true);
    assert!(
        !errors(&instance).is_empty(),
        "LibriSpeech-derived audio is never committed (VP-181)"
    );
}

/// Every committed DER artifact, not just the one this test can build.
#[test]
fn every_committed_diarization_artifact_satisfies_the_schema() {
    let directory = repo_root().join("docs/benchmarks");
    let mut checked = 0;
    for entry in std::fs::read_dir(&directory)
        .expect("docs/benchmarks")
        .flatten()
    {
        let path = entry.path();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if !name.starts_with("diarization-") || !name.ends_with("-der.json") {
            continue;
        }
        let committed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read artifact"))
                .unwrap_or_else(|error| panic!("{name} is not valid JSON: {error}"));
        let errors = errors(&committed);
        assert!(errors.is_empty(), "{name}: {errors:?}");

        // ADR-0400 condition 4 is about overlapping speech: an artifact whose
        // reference carries none measures something else.
        let overlapping = committed["overall"]["overlapping"]["total_reference_ms"]
            .as_u64()
            .unwrap_or_default();
        assert!(
            overlapping > 0,
            "{name} scores no overlapping speech, so it cannot address condition 4"
        );

        // The provider it measures must not be in the allowlist unless every
        // other condition is closed too.
        let provider = committed["provider"]["provider_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let qualifies = committed["release_evidence"]["qualifies"]
            .as_bool()
            .unwrap_or_default();
        if plaudon_ai_diarization::is_validated(&provider) {
            assert!(
                qualifies,
                "{name}: `{provider}` is in the allowlist, so its evidence must qualify"
            );
        }
        checked += 1;
    }
    assert!(checked >= 1, "expected at least one committed DER artifact");
}

/// The components of the two halves must add up to the whole, in the committed
/// document as well as in the metric's own tests.
#[test]
fn the_committed_artifacts_halves_add_up() {
    let directory = repo_root().join("docs/benchmarks");
    for entry in std::fs::read_dir(&directory)
        .expect("docs/benchmarks")
        .flatten()
    {
        let path = entry.path();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if !name.starts_with("diarization-") || !name.ends_with("-der.json") {
            continue;
        }
        let artifact: DerArtifact =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read artifact"))
                .unwrap_or_else(|error| panic!("{name} does not parse as a DER artifact: {error}"));
        for session in &artifact.sessions {
            assert_eq!(
                session.der.single_speaker.total_reference_ms
                    + session.der.overlapping.total_reference_ms,
                session.der.overall.total_reference_ms,
                "{name}: {} halves do not add up",
                session.session_id
            );
        }
        assert_eq!(
            artifact.overall.single_speaker.missed_ms + artifact.overall.overlapping.missed_ms,
            artifact.overall.overall.missed_ms,
            "{name}: pooled halves do not add up"
        );
    }
}
