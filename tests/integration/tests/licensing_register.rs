//! The licensing register and the ADR set are part of the build contract
//! (spec 2.6, 18.8, 21; task A1.5).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("root")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn every_spec_21_adr_exists() {
    let adr_dir = repo_root().join("docs/adr");
    let present: BTreeSet<String> = std::fs::read_dir(&adr_dir)
        .expect("docs/adr")
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect();

    for number in 1..=12u32 {
        let prefix = format!("ADR-{number:04}-");
        assert!(
            present.iter().any(|name| name.starts_with(&prefix)),
            "spec 21 requires ADR-{number:04}; docs/adr has {present:?}"
        );
    }
}

#[test]
fn every_adr_states_its_rejected_alternatives_and_consequences() {
    for entry in std::fs::read_dir(repo_root().join("docs/adr"))
        .expect("docs/adr")
        .flatten()
    {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("ADR-") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read adr");
        for section in [
            "## Context",
            "## Decision",
            "## Rejected alternatives",
            "## Consequences",
        ] {
            assert!(text.contains(section), "{name} is missing {section}");
        }
        assert!(text.contains("- **Status:**"), "{name} is missing a status");
        assert!(
            text.contains("- **Spec:**"),
            "{name} does not cite the specification"
        );
        let rejected = text
            .split("## Rejected alternatives")
            .nth(1)
            .and_then(|rest| rest.split("## Consequences").next())
            .unwrap_or_default();
        assert!(
            rejected.matches("- **").count() >= 2,
            "{name} must state at least two rejected alternatives"
        );
    }
}

#[test]
fn the_adr_index_matches_the_files_on_disk() {
    let index = read("docs/adr/README.md");
    for entry in std::fs::read_dir(repo_root().join("docs/adr"))
        .expect("docs/adr")
        .flatten()
    {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let is_markdown = std::path::Path::new(&name)
            .extension()
            .is_some_and(|extension| extension == "md");
        if name == "README.md" || !is_markdown {
            continue;
        }
        assert!(
            index.contains(&name),
            "docs/adr/README.md does not link {name}"
        );
    }
}

#[test]
fn every_register_entry_records_a_licence_and_a_decision() {
    let register = read("docs/licensing/third-party-register.md");
    let mut rows = 0;
    for line in register.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') || trimmed.starts_with("|---") {
            continue;
        }
        let cells: Vec<&str> = trimmed
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if cells.len() < 6
            || cells[0] == "Component"
            || cells[0] == "Weights"
            || cells[0] == "Decision"
        {
            continue;
        }
        rows += 1;
        assert!(!cells[0].is_empty(), "a register row has no component name");
        assert!(!cells[1].is_empty(), "{} has no licence recorded", cells[0]);
        assert!(
            !cells[2].is_empty(),
            "{} has no redistribution decision",
            cells[0]
        );
        assert!(
            cells.last().is_some_and(|decision| !decision.is_empty()),
            "{} has no decision",
            cells[0]
        );
    }
    assert!(
        rows >= 18,
        "the register should cover every component we intend to use, saw {rows}"
    );
}

#[test]
fn an_unknown_licence_is_never_marked_bundlable() {
    let register = read("docs/licensing/third-party-register.md");
    for line in register.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = trimmed
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if cells.len() < 3 {
            continue;
        }
        let licence = cells[1].to_ascii_lowercase();
        let redistribution = cells[2].to_ascii_lowercase();
        let unknown = licence.contains("none")
            || licence.contains("unknown")
            || licence.contains("not declared");
        if unknown {
            assert!(
                redistribution.contains("excluded"),
                "{} has an unknown licence but is marked {redistribution}",
                cells[0]
            );
        }
        if licence.contains("agpl") {
            assert!(
                redistribution.contains("excluded"),
                "{} is AGPL and must not be bundled",
                cells[0]
            );
        }
    }
}

#[test]
fn the_audit_policy_allowlists_only_reviewed_licences() {
    let deny = read("deny.toml");
    assert!(deny.contains("unknown-registry = \"deny\""));
    assert!(deny.contains("unknown-git = \"deny\""));
    for reviewed in ["\"MIT\"", "\"Apache-2.0\"", "\"MPL-2.0\""] {
        assert!(deny.contains(reviewed), "deny.toml should allow {reviewed}");
    }
    for forbidden in ["\"AGPL-3.0\"", "\"GPL-3.0\"", "\"GPL-2.0\""] {
        assert!(
            !deny.contains(forbidden),
            "deny.toml must not allow {forbidden}"
        );
    }
    let script = read("scripts/license-audit.sh");
    assert!(script.contains("cargo deny"));
    assert!(script.contains("licenses"));
    assert!(script.contains("advisories"));
}

#[test]
fn no_model_weight_is_bundled_without_a_checksum_requirement() {
    let register = read("docs/licensing/third-party-register.md");
    let weights = register
        .split("## Model weights")
        .nth(1)
        .expect("the register must have a model weights section");
    assert!(weights.contains("never bundled"));
    assert!(weights.contains("checksum") || weights.contains("Checksum"));
    assert!(weights.contains("not bundlable"));
}

/// The speaker-model facts exist in four places; they must say one thing.
///
/// The compiled records in `plaudon-ai-speaker` are the source of truth: the
/// installer verifies against them, so a register row or a `NOTICE` line that
/// drifts from them is a licence document describing a file we would refuse.
/// Checking all four here is cheaper than discovering the disagreement when a
/// download fails or, worse, when it does not.
#[test]
fn the_speaker_model_artifacts_match_the_register() {
    let register = read("docs/licensing/third-party-register.md");
    let notice = read("NOTICE");
    let fetch = read("scripts/fetch-speaker-models.sh");
    let public = read("docs/public/model-licensing.md");

    for artifact in plaudon_ai_speaker::checkpoint::BUNDLE_TIER {
        assert!(
            register.contains(artifact.sha256_hex),
            "{} digest is not in the model-weights checksum table",
            artifact.model_id
        );
        assert!(
            register.contains(artifact.source_file_name),
            "{} is not named by its exact published file name in the register",
            artifact.model_id
        );
        assert!(
            register.contains(&artifact.byte_length.to_string()),
            "{} size is not recorded in the register",
            artifact.model_id
        );
        assert!(
            notice.contains(artifact.sha256_hex),
            "{} digest is not in NOTICE",
            artifact.model_id
        );
        assert!(
            fetch.contains(artifact.sha256_hex) && fetch.contains(artifact.source_url),
            "scripts/fetch-speaker-models.sh does not pin {} by URL and digest",
            artifact.model_id
        );
        assert!(
            public.contains(artifact.display_name),
            "{} is not disclosed in docs/public/model-licensing.md",
            artifact.model_id
        );
    }

    // The attribution the two licences oblige, in the file that carries it.
    for required in ["CNRS", "WeSpeaker", "VoxCeleb", "CC-BY-4.0"] {
        assert!(
            notice.contains(required),
            "NOTICE does not attribute {required}"
        );
    }
}

/// A model row may be bundlable only if its licence is one we reviewed.
///
/// The register's own vocabulary test covers "unknown licence, not bundled";
/// this is the other half, from the code side: an artifact record that
/// acquired an unreviewed licence would otherwise sail through because no
/// document mentions it.
#[test]
fn no_speaker_artifact_carries_an_unreviewed_licence() {
    const REVIEWED: &[&str] = &["MIT", "CC-BY-4.0"];
    for artifact in plaudon_ai_speaker::checkpoint::BUNDLE_TIER {
        assert!(
            REVIEWED.contains(&artifact.licence_spdx),
            "{} is marked bundlable under the unreviewed licence {}",
            artifact.model_id,
            artifact.licence_spdx
        );
        assert!(
            !artifact.source_url.contains("huggingface.co/pyannote/"),
            "{} is fetched from the gated upstream, which ADR-0400 condition 3 forbids",
            artifact.model_id
        );
    }
}

/// The vendored inference runtime's facts exist in four places too.
///
/// The build links whatever `ORT_LIB_LOCATION` points at, so the digest in
/// `crates/ai-speaker/src/runtime.rs` is the only thing standing between "the
/// reviewed ONNX Runtime" and "whatever was in that directory". A register
/// row, a `NOTICE` line or a fetch script that disagreed with it would be a
/// provenance claim about bytes nobody checked.
#[test]
fn the_vendored_onnx_runtime_matches_the_register() {
    let register = read("docs/licensing/third-party-register.md");
    let notice = read("NOTICE");
    let fetch_sh = read("scripts/fetch-onnxruntime.sh");
    let fetch_ps1 = read("scripts/fetch-onnxruntime.ps1");

    for archive in plaudon_ai_speaker::runtime::VENDORED_RUNTIMES {
        for (document, name) in [
            (&register, "the licence register"),
            (&notice, "NOTICE"),
            (&fetch_sh, "scripts/fetch-onnxruntime.sh"),
            (&fetch_ps1, "scripts/fetch-onnxruntime.ps1"),
        ] {
            assert!(
                document.contains(archive.sha256_hex),
                "{} does not pin the {} archive by digest",
                name,
                archive.target
            );
        }
        assert!(
            fetch_sh.contains(archive.url) && fetch_ps1.contains(archive.url),
            "a fetch script does not name the {} archive by its exact URL",
            archive.target
        );
        assert!(
            register.contains(&archive.byte_length.to_string())
                && register.contains(&archive.library_byte_length.to_string()),
            "the register does not record both sizes for {}",
            archive.target
        );
        assert!(
            register.contains(archive.library_file_name),
            "the register does not name the library inside the {} archive",
            archive.target
        );
        assert_eq!(
            archive.licence_spdx, "MIT",
            "the ONNX Runtime binary ships under MIT; a different licence needs a review, not an \
             edit"
        );
    }

    // The build must not be able to fetch it: `download-binaries` pulls an
    // HTTP client into the build graph and makes a release build depend on a
    // CDN (#283).
    let workspace = read("Cargo.toml");
    let ort = workspace
        .split("ort = {")
        .nth(1)
        .expect("the workspace must pin ort");
    let ort = &ort[..ort.find('}').expect("the ort dependency table must close")];
    assert!(
        !ort.contains("download-binaries"),
        "ort's `download-binaries` feature is back: the build would fetch a runtime over TLS"
    );

    // And the evidence register must say which target was actually linked.
    let evidence = read("docs/release/voice-profiles-evidence.md");
    for archive in plaudon_ai_speaker::runtime::VENDORED_RUNTIMES {
        if !archive.evidence.is_linked() {
            assert!(
                evidence.to_ascii_lowercase().contains("windows"),
                "{} was never linked here and the release-evidence register does not say so",
                archive.target
            );
        }
    }
}

/// The release-evidence register must list what this release cannot prove.
#[test]
fn the_release_evidence_register_lists_the_benchmarks_and_the_counsel_review() {
    let evidence = read("docs/release/voice-profiles-evidence.md");
    for artifact in [
        "speaker-match-wespeaker-resnet34-lm-eer.json",
        "speaker-match-scripted-synthetic-eer.json",
        "speaker-match-wespeaker-fixtures.json",
    ] {
        assert!(
            evidence.contains(artifact),
            "the release-evidence register does not list {artifact}"
        );
    }
    let lowercase = evidence.to_ascii_lowercase();
    assert!(
        lowercase.contains("privacy-counsel") || lowercase.contains("privacy counsel"),
        "the counsel review is not a listed release-evidence item"
    );
    assert!(
        lowercase.contains("windows"),
        "the missing Windows x64 measurement is not listed"
    );
    for required in ["Owner", "Status"] {
        assert!(
            evidence.contains(required),
            "a release-evidence item without an {required} column is not actionable"
        );
    }
    let collect = read("scripts/collect-evidence.sh");
    assert!(
        collect.contains("docs/release/voice-profiles-evidence.md"),
        "the evidence collector does not put the register into the bundle"
    );
    assert!(
        collect.contains("docs/benchmarks/."),
        "the evidence collector does not put the benchmark artifacts into the bundle"
    );
}
