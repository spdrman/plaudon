//! The committed `compatibility.yaml` must parse, validate, satisfy its JSON
//! Schema, and never claim support it cannot evidence (spec 2.5B).

use std::path::{Path, PathBuf};

use plaudon_source_plaud_device::register::{
    CompatibilityRegister, EvidenceLevel, ProfileKey, Resolution, SupportStatus,
};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("root")
        .to_path_buf()
}

fn committed() -> CompatibilityRegister {
    let path = repo_root().join("compatibility.yaml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    CompatibilityRegister::from_yaml(&text).expect("the committed register must validate")
}

#[test]
fn the_committed_register_parses_and_validates() {
    let register = committed();
    assert_eq!(
        register.schema_version,
        CompatibilityRegister::SUPPORTED_SCHEMA_VERSION
    );
    assert!(
        register.cells.len() >= 8,
        "the register should cover every probed source"
    );
}

#[test]
fn the_committed_register_satisfies_its_json_schema() {
    let schema_path = repo_root().join("schemas/compatibility-register.v1.schema.json");
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schema_path).expect("read schema"))
            .expect("parse schema");
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .expect("schema compiles");

    let yaml = std::fs::read_to_string(repo_root().join("compatibility.yaml")).expect("read yaml");
    let document: serde_json::Value = serde_norway::from_str(&yaml).expect("yaml to json value");
    let errors: Vec<String> = validator
        .iter_errors(&document)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "register failed its schema: {errors:?}");
}

#[test]
fn no_device_cell_claims_support_without_hardware_evidence() {
    for cell in committed().cells {
        if cell.source_type.starts_with("plaud_device") {
            assert_ne!(
                cell.status,
                SupportStatus::Supported,
                "{} claims Supported with no attached hardware",
                cell.device_model
            );
        }
        if cell.status == SupportStatus::Supported {
            assert_eq!(
                cell.evidence_level,
                EvidenceLevel::ObservedOnIdentifiedHardware
            );
            assert!(cell.verified_import_recorded);
            assert!(cell.test_run_id.is_some());
            assert!(cell.last_verified_at.is_some());
        }
    }
}

#[test]
fn note_pro_and_notepin_are_recorded_as_known_unsupported() {
    let register = committed();
    for model in ["Note Pro", "NotePin S"] {
        let cell = register
            .cells
            .iter()
            .find(|cell| cell.device_model == model && cell.transport == "usb_mass_storage")
            .unwrap_or_else(|| panic!("{model} must have a USB row"));
        assert_eq!(cell.status, SupportStatus::KnownUnsupported);
        assert_ne!(cell.known_limitations.len(), 0);
        assert!(!cell.status.permits_use());
    }
}

#[test]
fn an_unknown_firmware_or_host_resolves_to_nothing() {
    let register = committed();
    let unknown = ProfileKey {
        source_type: "plaud_device_ble".into(),
        device_model: "Note".into(),
        firmware: "V0101".into(),
        host_os: "macos".into(),
        os_version: "27.0".into(),
        transport: "ble".into(),
        auth_mode: "legacy_static_token".into(),
    };
    let resolution = register.resolve(&unknown);
    assert_eq!(resolution, Resolution::Unmatched);
    assert!(!resolution.permits_use());
    assert_eq!(resolution.ui_label(), "Not evaluated");
}

#[test]
fn a_known_ble_profile_resolves_but_still_permits_nothing_yet() {
    let register = committed();
    let known = ProfileKey {
        source_type: "plaud_device_ble".into(),
        device_model: "Note".into(),
        firmware: "V0095".into(),
        host_os: "macos".into(),
        os_version: "27.0".into(),
        transport: "ble".into(),
        auth_mode: "legacy_static_token".into(),
    };
    let Resolution::Matched(cell) = register.resolve(&known) else {
        panic!("the legacy BLE profile should be described by the register");
    };
    assert_eq!(cell.status, SupportStatus::NotEvaluated);
    assert!(
        !cell.status.permits_use(),
        "described is not the same as validated"
    );
    assert_ne!(cell.known_limitations.len(), 0);
}

#[test]
fn every_label_the_register_can_produce_is_from_the_fixed_vocabulary() {
    const VOCABULARY: [&str; 4] = [
        "Not evaluated",
        "Experimental",
        "Supported",
        "Known unsupported",
    ];
    for label in committed().labels() {
        assert!(
            VOCABULARY.contains(&label),
            "{label} is not in the App. B.2 vocabulary"
        );
    }
}

#[test]
fn no_rust_or_typescript_source_hardcodes_a_compatibility_label() {
    // Spec 2.5B: user-visible compatibility strings are generated from the
    // register. The only place the literals may appear is the register, its
    // schema, the status vocabulary itself and these tests.
    // Quoted literals only: the bare words are legitimate Rust and TypeScript
    // identifiers, while a quoted label is a hardcoded user-visible claim.
    const LABELS: [&str; 4] = [
        "\"Known unsupported\"",
        "\"Not evaluated\"",
        "\"Experimental\"",
        "\"Supported\"",
    ];
    let allowed_suffixes = [
        "crates/source-plaud-device/src/register.rs",
        "crates/source-plaud-device/tests/committed_register.rs",
        "compatibility.yaml",
        "schemas/compatibility-register.v1.schema.json",
    ];

    let mut offenders = Vec::new();
    let roots = [repo_root().join("crates"), repo_root().join("apps")];
    for root in roots.iter().filter(|root| root.is_dir()) {
        let mut stack = vec![root.clone()];
        while let Some(directory) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if path
                        .file_name()
                        .is_some_and(|name| name == "target" || name == "node_modules")
                    {
                        continue;
                    }
                    stack.push(path);
                    continue;
                }
                let is_source = path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| matches!(extension, "rs" | "ts" | "tsx"));
                if !is_source {
                    continue;
                }
                let relative = path
                    .strip_prefix(repo_root())
                    .unwrap_or(&path)
                    .to_string_lossy();
                if allowed_suffixes
                    .iter()
                    .any(|allowed| relative.ends_with(allowed))
                {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                // A test may, and should, assert the exact label it expects:
                // that assertion is how the rule is enforced, not a violation
                // of it. What must not contain a literal is product code, so
                // Rust files are scanned only up to their test module, and
                // test files and test doubles are skipped entirely.
                if is_test_source(&relative) {
                    continue;
                }
                let text = production_part(&text);
                for label in LABELS {
                    if text.contains(label) {
                        offenders.push(format!("{relative} contains {label:?}"));
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "hardcoded compatibility claims: {offenders:?}"
    );
}

/// Whether a path is a test, a test double or an end-to-end spec.
fn is_test_source(relative: &str) -> bool {
    relative.ends_with(".test.ts")
        || relative.ends_with(".test.tsx")
        || relative.ends_with(".spec.ts")
        || relative.ends_with(".spec.tsx")
        || relative.contains("/tests/")
        || relative.contains("/e2e/")
        || relative.contains("/testing/")
}

/// The part of a Rust source that ships: everything before its test module.
fn production_part(text: &str) -> &str {
    text.find("\n#[cfg(test)]").map_or(text, |at| &text[..at])
}
