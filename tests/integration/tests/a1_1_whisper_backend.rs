//! A1.1: backend probe mapping, model manifest verification and the
//! benchmark result contract.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use plaudon_integration_tests::spikes::whisper_bench::{
    availability_reason, select_backend, AvailabilityReason, Backend, BackendProbe,
    BenchmarkResult, ManifestRejection, ModelManifestEntry,
};
use sha2::{Digest, Sha256};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("root")
        .to_path_buf()
}

fn entry_for(bytes: &[u8]) -> ModelManifestEntry {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        });
    ModelManifestEntry {
        model_id: "ggml-base.en-q5_1".to_owned(),
        display_size: "57 MB".to_owned(),
        byte_length: bytes.len() as u64,
        sha256: digest,
        license: "MIT".to_owned(),
        redistributable: false,
        source_url: "https://example.invalid/ggml-base.en-q5_1.bin".to_owned(),
        languages: vec!["en".to_owned()],
        min_ram_bytes: 1_024 * 1_024 * 1_024,
    }
}

#[test]
fn an_unavailable_backend_yields_a_typed_reason_not_a_silent_cpu_downgrade() {
    let cases = [
        (BackendProbe::NotCompiledIn, AvailabilityReason::NotBuilt),
        (
            BackendProbe::NoDevice,
            AvailabilityReason::HardwareUnsupported {
                detail: "no_device".to_owned(),
            },
        ),
        (
            BackendProbe::PlatformUnsupported {
                platform: "windows".to_owned(),
            },
            AvailabilityReason::PlatformUnsupported {
                platform: "windows".to_owned(),
            },
        ),
        (
            BackendProbe::ProbeFailed {
                detail: "driver_error".to_owned(),
            },
            AvailabilityReason::ProbeFailed {
                detail: "driver_error".to_owned(),
            },
        ),
    ];
    for (probe, expected) in cases {
        let reason = availability_reason(&probe);
        assert_eq!(reason, expected);
        assert_ne!(reason, AvailabilityReason::Available);
    }
    assert_eq!(
        availability_reason(&BackendProbe::Available {
            device: "Apple M-series".to_owned()
        }),
        AvailabilityReason::Available
    );
}

#[test]
fn backend_selection_reports_why_every_candidate_was_rejected() {
    let probes: BTreeMap<Backend, BackendProbe> = [
        (Backend::Metal, BackendProbe::NoDevice),
        (
            Backend::Cuda,
            BackendProbe::PlatformUnsupported {
                platform: "macos".to_owned(),
            },
        ),
        (
            Backend::Cpu,
            BackendProbe::Available {
                device: "cpu".to_owned(),
            },
        ),
    ]
    .into_iter()
    .collect();

    let (chosen, rejected) =
        select_backend(&probes, &[Backend::Metal, Backend::Cuda, Backend::Cpu]);
    assert_eq!(chosen, Some(Backend::Cpu));
    assert_eq!(rejected.len(), 2);
    assert_eq!(
        rejected[0],
        (
            Backend::Metal,
            AvailabilityReason::HardwareUnsupported {
                detail: "no_device".to_owned()
            }
        )
    );
    assert_eq!(
        rejected[1],
        (
            Backend::Cuda,
            AvailabilityReason::PlatformUnsupported {
                platform: "macos".to_owned()
            }
        )
    );
}

#[test]
fn selection_fails_rather_than_inventing_a_backend() {
    let probes: BTreeMap<Backend, BackendProbe> = [(Backend::Metal, BackendProbe::NoDevice)]
        .into_iter()
        .collect();
    let (chosen, rejected) = select_backend(&probes, &[Backend::Metal, Backend::Vulkan]);
    assert_eq!(chosen, None);
    assert_eq!(rejected.len(), 2);
    assert_eq!(rejected[1].1, AvailabilityReason::NotBuilt);
}

#[test]
fn a_matching_model_file_verifies() {
    let bytes = b"ggml weights, pretend".to_vec();
    entry_for(&bytes)
        .verify(&bytes)
        .expect("a matching file must verify");
}

#[test]
fn a_tampered_model_file_is_refused_before_it_is_loaded() {
    let bytes = b"ggml weights, pretend".to_vec();
    let entry = entry_for(&bytes);

    let mut flipped = bytes.clone();
    flipped[0] ^= 0xFF;
    let rejection = entry.verify(&flipped).unwrap_err();
    assert!(matches!(
        rejection,
        ManifestRejection::DigestMismatch { .. }
    ));

    let mut truncated = bytes.clone();
    truncated.pop();
    assert_eq!(
        entry.verify(&truncated),
        Err(ManifestRejection::LengthMismatch {
            expected: bytes.len() as u64,
            actual: (bytes.len() - 1) as u64
        })
    );

    let mut extended = bytes.clone();
    extended.extend_from_slice(b"trailing payload");
    assert!(matches!(
        entry.verify(&extended),
        Err(ManifestRejection::LengthMismatch { .. })
    ));
}

#[test]
fn a_malformed_manifest_entry_is_rejected_before_any_bytes_are_judged() {
    let bytes = b"weights".to_vec();
    for (mutate, field) in [
        (
            Box::new(|e: &mut ModelManifestEntry| e.model_id.clear())
                as Box<dyn Fn(&mut ModelManifestEntry)>,
            "model_id",
        ),
        (
            Box::new(|e: &mut ModelManifestEntry| e.sha256 = "abc".to_owned()),
            "sha256",
        ),
        (
            Box::new(|e: &mut ModelManifestEntry| e.sha256 = e.sha256.to_uppercase()),
            "sha256",
        ),
        (
            Box::new(|e: &mut ModelManifestEntry| e.license.clear()),
            "license",
        ),
        (
            Box::new(|e: &mut ModelManifestEntry| e.byte_length = 0),
            "byte_length",
        ),
    ] {
        let mut entry = entry_for(&bytes);
        mutate(&mut entry);
        assert_eq!(
            entry.validate(),
            Err(ManifestRejection::MalformedEntry { field })
        );
        assert!(entry.verify(&bytes).is_err());
    }
}

#[test]
fn weights_without_clear_redistribution_rights_are_never_bundlable() {
    let bytes = b"weights".to_vec();
    let mut entry = entry_for(&bytes);
    assert!(!entry.may_bundle(), "the default must be user-installed");
    entry.redistributable = true;
    assert!(entry.may_bundle());
}

#[test]
fn a_benchmark_result_is_only_citable_when_its_host_is_identified() {
    let mut result = BenchmarkResult {
        schema_version: BenchmarkResult::SCHEMA_VERSION,
        host_os: "macOS 27.0.1 (26A434)".to_owned(),
        host_cpu: "Apple M-series".to_owned(),
        host_ram_bytes: 32 * 1024 * 1024 * 1024,
        backend: Backend::Metal,
        model_id: "ggml-base.en-q5_1".to_owned(),
        audio_duration_ms: 60_000,
        wall_clock_ms: 15_000,
        peak_rss_bytes: 900 * 1024 * 1024,
        cancellation_latency_ms: Some(120),
        segments_reported: true,
        word_timestamps_reported: false,
        run_on: "2026-10-11".to_owned(),
    };
    assert!(result.is_citable());
    assert!((result.real_time_factor().expect("rtf") - 0.25).abs() < f64::EPSILON);

    for strip in [
        Box::new(|r: &mut BenchmarkResult| r.host_cpu.clear()) as Box<dyn Fn(&mut BenchmarkResult)>,
        Box::new(|r: &mut BenchmarkResult| r.host_os.clear()),
        Box::new(|r: &mut BenchmarkResult| r.host_ram_bytes = 0),
        Box::new(|r: &mut BenchmarkResult| r.audio_duration_ms = 0),
        Box::new(|r: &mut BenchmarkResult| r.schema_version = 99),
    ] {
        let mut broken = result.clone();
        strip(&mut broken);
        assert!(
            !broken.is_citable(),
            "an unidentified host is evidence of nothing"
        );
    }

    result.audio_duration_ms = 0;
    assert_eq!(
        result.real_time_factor(),
        None,
        "no fabricated ratio for zero-length audio"
    );
}

#[test]
fn a_benchmark_result_satisfies_its_committed_schema() {
    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("schemas/benchmark-result.v1.schema.json"))
            .expect("read schema"),
    )
    .expect("parse schema");
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .expect("schema compiles");

    let result = BenchmarkResult {
        schema_version: BenchmarkResult::SCHEMA_VERSION,
        host_os: "Windows 11 26100".to_owned(),
        host_cpu: "x86-64".to_owned(),
        host_ram_bytes: 16 * 1024 * 1024 * 1024,
        backend: Backend::Cpu,
        model_id: "ggml-small".to_owned(),
        audio_duration_ms: 1_000,
        wall_clock_ms: 2_500,
        peak_rss_bytes: 1_024,
        cancellation_latency_ms: None,
        segments_reported: false,
        word_timestamps_reported: false,
        run_on: "2026-10-11".to_owned(),
    };
    let value = serde_json::to_value(&result).expect("serialize");
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");

    let mut invalid = value.clone();
    invalid["backend"] = serde_json::json!("quantum");
    assert!(validator.iter_errors(&invalid).next().is_some());
}

#[test]
fn no_performance_target_is_asserted_before_a_run_exists() {
    // Spec 17.5 sets concrete targets only after A1.1 establishes baselines.
    // Until a result file is committed, the benchmark document must say so
    // rather than carrying an invented number.
    let document = std::fs::read_to_string(repo_root().join("docs/benchmarks/README.md"))
        .expect("the benchmark document must be committed");
    let results_dir = repo_root().join("docs/benchmarks/results");
    let has_results = results_dir.read_dir().is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
    });
    if !has_results {
        assert!(
            document.contains("No baseline has been measured"),
            "with no committed result the document must say no baseline exists"
        );
    }
}
