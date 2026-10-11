//! Workspace layout and dependency-boundary conformance (spec 6.3(1), 6.4, 18.8).
//!
//! These tests read the repository itself, so a crate rename, a missing
//! directory or a forbidden dependency fails the build rather than a review.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // tests/integration/ -> tests/ -> repository root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_path_buf()
}

/// Every crate directory required by spec 6.4.
const REQUIRED_CRATES: &[&str] = &[
    "domain",
    "app-core",
    "recording-sources",
    "source-plaud-device",
    "plaud-protocol",
    "plaud-ble",
    "plaud-usb",
    "plaud-wifi",
    "source-plaud-cli",
    "source-folder",
    "audio",
    "ai-contracts",
    "ai-openrouter",
    "ai-whisper",
    "ai-diarization",
    "ai-tools",
    "ai-apple",
    "jobs",
    "persistence",
    "security",
    "export",
    "notifications",
    "automation",
    "diagnostics",
];

/// Every non-crate directory required by spec 6.4.
const REQUIRED_DIRECTORIES: &[&str] = &[
    "native/apple-bridge",
    "apps/desktop",
    "schemas",
    "tests/fixtures",
    "tests/integration",
    "tests/hardware",
    "tests/e2e",
    "tests/chaos",
    "scripts",
    "docs/adr",
    "docs/threat-model",
    "docs/protocol",
    "docs/benchmarks",
];

fn read_toml(path: &Path) -> toml::Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    text.parse::<toml::Value>().unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

#[test]
fn every_spec_6_4_crate_exists_with_the_expected_package_name() {
    let root = repo_root();
    for crate_dir in REQUIRED_CRATES {
        let manifest = root.join("crates").join(crate_dir).join("Cargo.toml");
        assert!(manifest.is_file(), "missing crate manifest for crates/{crate_dir}");
        let value = read_toml(&manifest);
        let name = value["package"]["name"].as_str().expect("package name");
        assert_eq!(
            name,
            format!("plaudon-{crate_dir}"),
            "crates/{crate_dir} must be named plaudon-{crate_dir}"
        );
        assert!(
            root.join("crates").join(crate_dir).join("src/lib.rs").is_file(),
            "crates/{crate_dir} must be a library crate"
        );
    }
}

#[test]
fn every_spec_6_4_directory_exists() {
    let root = repo_root();
    for directory in REQUIRED_DIRECTORIES {
        assert!(root.join(directory).is_dir(), "missing required directory {directory}");
    }
}

#[test]
fn the_workspace_member_globs_cover_every_crate_and_test_package() {
    let root = repo_root();
    let manifest = read_toml(&root.join("Cargo.toml"));
    let members: BTreeSet<&str> = manifest["workspace"]["members"]
        .as_array()
        .expect("workspace members")
        .iter()
        .map(|entry| entry.as_str().expect("member string"))
        .collect();

    assert!(members.contains("crates/*"), "crates/* glob keeps member additions conflict free");
    assert!(members.contains("tests/integration"));
    assert!(members.contains("tests/chaos"));
    assert_eq!(manifest["workspace"]["resolver"].as_str(), Some("2"));
}

#[test]
fn the_lockfile_is_committed() {
    assert!(repo_root().join("Cargo.lock").is_file(), "Cargo.lock must be committed");
}

#[test]
fn the_toolchain_is_pinned_with_fmt_and_clippy() {
    let manifest = read_toml(&repo_root().join("rust-toolchain.toml"));
    let components: BTreeSet<&str> = manifest["toolchain"]["components"]
        .as_array()
        .expect("components")
        .iter()
        .map(|c| c.as_str().expect("component string"))
        .collect();
    assert!(components.contains("rustfmt"));
    assert!(components.contains("clippy"));
    assert!(manifest["toolchain"]["channel"].as_str().is_some(), "the channel must be pinned");
}

/// Dependencies the core must never acquire (spec 6.3(1)).
///
/// The core may not depend on the UI shell, a vendor transport, a vendor AI
/// gateway, an Apple bridge or an HTTP client.
const FORBIDDEN_CORE_DEPENDENCIES: &[&str] = &[
    "tauri",
    "tauri-build",
    "btleplug",
    "reqwest",
    "hyper",
    "ureq",
    "objc2",
    "block2",
    "swift-bridge",
    "plaudon-ai-openrouter",
    "plaudon-ai-apple",
    "plaudon-ai-whisper",
    "plaudon-plaud-ble",
    "plaudon-plaud-usb",
    "plaudon-plaud-wifi",
    "plaudon-plaud-protocol",
    "plaudon-source-plaud-cli",
    "plaudon-source-plaud-device",
];

fn declared_dependencies(manifest: &toml::Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for table in ["dependencies", "build-dependencies"] {
        if let Some(map) = manifest.get(table).and_then(toml::Value::as_table) {
            out.extend(map.keys().cloned());
        }
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            for table in ["dependencies", "build-dependencies"] {
                if let Some(map) = target.get(table).and_then(toml::Value::as_table) {
                    out.extend(map.keys().cloned());
                }
            }
        }
    }
    out
}

#[test]
fn the_core_crates_have_no_vendor_ui_or_network_dependency() {
    let root = repo_root();
    for crate_dir in ["domain", "app-core"] {
        let manifest = read_toml(&root.join("crates").join(crate_dir).join("Cargo.toml"));
        let dependencies = declared_dependencies(&manifest);
        for forbidden in FORBIDDEN_CORE_DEPENDENCIES {
            assert!(
                !dependencies.contains(*forbidden),
                "crates/{crate_dir} must not depend on {forbidden} (spec 6.3(1))"
            );
        }
    }
}

#[test]
fn the_core_crates_are_free_of_transitive_vendor_dependencies() {
    // A direct-dependency check is not enough: a core crate could pull a
    // forbidden crate in through one of our own crates. Walk our own graph.
    let root = repo_root();
    let mut manifests: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for crate_dir in REQUIRED_CRATES {
        let manifest = read_toml(&root.join("crates").join(crate_dir).join("Cargo.toml"));
        manifests.insert(format!("plaudon-{crate_dir}"), declared_dependencies(&manifest));
    }

    for seed in ["plaudon-domain", "plaudon-app-core"] {
        let mut seen = BTreeSet::new();
        let mut queue = vec![seed.to_owned()];
        while let Some(current) = queue.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            let Some(dependencies) = manifests.get(&current) else { continue };
            for dependency in dependencies {
                assert!(
                    !FORBIDDEN_CORE_DEPENDENCIES.contains(&dependency.as_str()),
                    "{seed} reaches forbidden dependency {dependency} via {current}"
                );
                queue.push(dependency.clone());
            }
        }
    }
}

#[test]
fn every_crate_inherits_the_workspace_lint_configuration() {
    let root = repo_root();
    for crate_dir in REQUIRED_CRATES {
        let manifest = read_toml(&root.join("crates").join(crate_dir).join("Cargo.toml"));
        assert_eq!(
            manifest["lints"]["workspace"].as_bool(),
            Some(true),
            "crates/{crate_dir} must inherit [workspace.lints]"
        );
    }
    let workspace = read_toml(&root.join("Cargo.toml"));
    assert_eq!(
        workspace["workspace"]["lints"]["clippy"]["all"]["level"].as_str(),
        Some("deny"),
        "clippy::all must be denied workspace wide"
    );
}
