//! Every committed fixture satisfies `schemas/update-manifest.v1.schema.json`
//! (SPEC v1.3 §6.6 versioned contracts, §18.5 output-schema testing).
//!
//! Including the hostile fixtures is the point: structural validity proves
//! nothing about authenticity, so a document that passes this schema must
//! still be rejected by `tests/security.rs`. If a hostile fixture ever failed
//! the schema, the security suite would be testing the parser instead of the
//! cryptography.

mod support;

use plaudon_updater::testkeys::fixture_set;

fn schema() -> jsonschema::Validator {
    let path = support::repo_root().join("schemas/update-manifest.v1.schema.json");
    let document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read schema"))
            .expect("parse schema");
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&document)
        .expect("schema compiles")
}

#[test]
fn every_committed_fixture_satisfies_the_published_schema() {
    let validator = schema();
    for (name, _) in fixture_set() {
        let artifact = support::load(name);
        let instance: serde_json::Value =
            serde_json::from_str(&artifact.envelope).expect("fixture is JSON");
        let errors: Vec<String> = validator
            .iter_errors(&instance)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{name} failed its schema: {errors:?}");
    }
}

#[test]
fn the_schema_rejects_an_envelope_with_an_unknown_member() {
    let validator = schema();
    let mut instance: serde_json::Value =
        serde_json::from_str(&support::load("valid").envelope).expect("fixture is JSON");
    instance["rollout_percentage"] = serde_json::json!(50);
    assert!(
        validator.iter_errors(&instance).next().is_some(),
        "an unknown envelope member must be refused"
    );
}

#[test]
fn the_schema_rejects_a_non_https_package_url() {
    let validator = schema();
    let mut instance: serde_json::Value =
        serde_json::from_str(&support::load("valid").envelope).expect("fixture is JSON");
    instance["manifest"]["package_url"] = serde_json::json!("http://updates.example/p.pkg");
    assert!(
        validator.iter_errors(&instance).next().is_some(),
        "a plaintext package URL must be refused"
    );
}

#[test]
fn the_schema_rejects_a_malformed_version() {
    let validator = schema();
    let mut instance: serde_json::Value =
        serde_json::from_str(&support::load("valid").envelope).expect("fixture is JSON");
    instance["manifest"]["version"] = serde_json::json!("1.1");
    assert!(
        validator.iter_errors(&instance).next().is_some(),
        "a two-component version must be refused"
    );
}
