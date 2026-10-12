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
