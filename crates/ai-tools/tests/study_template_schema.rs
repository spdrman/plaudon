//! The study template schema stays in step with the Rust types (spec §9.3,
//! §9.5).

use plaudon_ai_tools::study::{validate_study_artifacts, STUDY_TEMPLATE_VERSION};
use plaudon_ai_tools::testing::fixture_revision;
use plaudon_domain::transcript::AlignmentOrigin;
use std::path::{Path, PathBuf};

fn schemas_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("root")
        .join("schemas")
}

fn load_schema(name: &str) -> serde_json::Value {
    let path = schemas_dir().join(name);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn validator(name: &str) -> jsonschema::Validator {
    let schema = load_schema(name);
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .expect("schema compiles")
}

fn assert_valid(validator: &jsonschema::Validator, instance: &serde_json::Value) {
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| e.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "instance failed schema: {errors:?}\n{instance:#}"
    );
}

fn assert_invalid(validator: &jsonschema::Validator, instance: &serde_json::Value) {
    assert!(
        !validator.is_valid(instance),
        "instance should have failed schema but passed:\n{instance:#}"
    );
}

#[test]
fn the_schema_compiles() {
    validator("study-template.v1.schema.json");
}

#[test]
fn a_validated_study_artifact_satisfies_the_schema() {
    let revision = fixture_revision(AlignmentOrigin::Native);
    // Spec §9.5: `validate_study_artifacts` rejects items with empty anchors,
    // so if we build from a raw payload with cited items the result must pass.
    let payload = plaudon_ai_tools::study::RawStudyPayload {
        flashcards: vec![plaudon_ai_tools::study::RawStudyItem {
            front: "What does idempotency mean?".to_string(),
            back: "Retry safety".to_string(),
            anchors: vec![plaudon_ai_tools::study::RawStudyAnchor {
                seq_index: 4,
                char_start: 0,
                char_end: 10,
                excerpt: "Idempotent".to_string(),
            }],
        }],
        quiz: vec![plaudon_ai_tools::study::RawQuizItem {
            question: "When does the migration ship?".to_string(),
            answer: "Friday".to_string(),
            distractors: vec!["Thursday".to_string(), "Monday".to_string()],
            anchors: vec![plaudon_ai_tools::study::RawStudyAnchor {
                seq_index: 0,
                char_start: 0,
                char_end: 7,
                excerpt: "We will".to_string(),
            }],
        }],
        definitions: vec![plaudon_ai_tools::study::RawDefinition {
            term: "Blast radius".to_string(),
            definition: "The scope of damage".to_string(),
            anchors: vec![plaudon_ai_tools::study::RawStudyAnchor {
                seq_index: 2,
                char_start: 0,
                char_end: 11,
                excerpt: "The staging".to_string(),
            }],
        }],
    };

    let result = validate_study_artifacts(&payload, &revision);
    assert!(!result.accepted.is_empty(), "fixture should produce items");

    let validator = validator("study-template.v1.schema.json");
    let json = serde_json::to_value(&result.accepted).expect("serialization succeeds");
    assert_valid(&validator, &json);
}

#[test]
fn an_artifact_with_an_empty_anchors_array_fails_validation() {
    let validator = validator("study-template.v1.schema.json");
    // Spec §9.5: an uncited item is a schema violation, not a runtime skip.
    let invalid = serde_json::json!({
        "template_version": "study-revision/1",
        "flashcards": [{
            "front": "Q",
            "back": "A",
            "anchors": []
        }],
        "quiz": [],
        "definitions": []
    });
    assert_invalid(&validator, &invalid);
}

#[test]
fn the_schema_template_version_matches_the_rust_constant() {
    let schema = load_schema("study-template.v1.schema.json");
    let version_const = schema
        .pointer("/properties/template_version/const")
        .expect("template_version const in schema")
        .as_str()
        .expect("const is string");
    assert_eq!(
        version_const, STUDY_TEMPLATE_VERSION,
        "schema and Rust must agree on template version"
    );
}
