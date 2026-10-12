//! Output schema validation with bounded repair (spec 9.5).
//!
//! Validation is always local, even when the provider claims to enforce the
//! schema itself: spec 9.5 requires "local schema validation in all cases".
//! Repair is bounded and never silently drops items — a malformed item is
//! rejected with its path so the caller can refuse the whole output.

use serde_json::Value;

use crate::error::ToolError;

/// Maximum number of repair attempts before an output is rejected.
pub const MAX_REPAIR_ATTEMPTS: u8 = 2;

/// A compiled output schema.
#[derive(Debug)]
pub struct OutputSchema {
    validator: jsonschema::Validator,
    version: String,
}

impl OutputSchema {
    /// Compile a schema.
    pub fn compile(version: impl Into<String>, schema: &Value) -> Result<Self, ToolError> {
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .build(schema)
            .map_err(|e| ToolError::ConfigurationInvalid(e.to_string()))?;
        Ok(Self {
            validator,
            version: version.into(),
        })
    }

    /// Schema version label.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Whether an instance satisfies the schema.
    #[must_use]
    pub fn is_valid(&self, instance: &Value) -> bool {
        self.validator.is_valid(instance)
    }

    /// Every violation, as JSON Pointer paths.
    ///
    /// Paths rather than provider text: an error envelope must never carry a
    /// transcript excerpt (spec 15.4).
    #[must_use]
    pub fn violations(&self, instance: &Value) -> Vec<String> {
        self.validator
            .iter_errors(instance)
            .map(|error| error.instance_path.to_string())
            .collect()
    }

    /// Validate, returning the instance or a typed failure.
    pub fn check(&self, instance: &Value) -> Result<(), ToolError> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(ToolError::SchemaValidationFailed)
        }
    }
}

/// Extract a JSON document from a model response that may be wrapped.
///
/// Some models that cannot be schema-constrained still answer with the JSON
/// surrounded by a fenced code block or a sentence. This is the only repair
/// performed: nothing is invented, nothing is reshaped.
#[must_use]
pub fn extract_json(raw: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(raw.trim()) {
        return Some(value);
    }
    let start = raw.find(['{', '['])?;
    let open = raw.as_bytes()[start];
    let close = if open == b'{' { b'}' } else { b']' };
    let end = raw.rfind(char::from(close))?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&raw[start..=end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> OutputSchema {
        OutputSchema::compile(
            "test.v1",
            &serde_json::json!({
                "type": "object",
                "required": ["items"],
                "additionalProperties": false,
                "properties": {
                    "items": {
                        "type": "array",
                        "items": { "type": "object", "required": ["text"],
                                   "properties": { "text": { "type": "string" } },
                                   "additionalProperties": false }
                    }
                }
            }),
        )
        .unwrap()
    }

    #[test]
    fn a_conforming_document_validates() {
        assert!(schema()
            .check(&serde_json::json!({"items": [{"text": "a"}]}))
            .is_ok());
    }

    #[test]
    fn a_malformed_item_is_reported_by_path_and_not_dropped() {
        let schema = schema();
        let instance = serde_json::json!({"items": [{"text": "a"}, {"wrong": 1}]});
        let violations = schema.violations(&instance);
        assert!(
            !violations.is_empty(),
            "expected violations to be non-empty"
        );
        assert!(violations.iter().any(|p| p.starts_with("/items/1")));
        assert_eq!(
            schema.check(&instance),
            Err(ToolError::SchemaValidationFailed)
        );
    }

    #[test]
    fn an_unknown_property_is_a_violation_rather_than_an_ignored_extra() {
        assert!(!schema().is_valid(&serde_json::json!({"items": [], "extra": true})));
    }

    #[test]
    fn json_wrapped_in_a_code_fence_is_recovered() {
        let raw = "Here you go:\n```json\n{\"items\": []}\n```\n";
        assert_eq!(extract_json(raw), Some(serde_json::json!({"items": []})));
    }

    #[test]
    fn plain_json_is_recovered_unchanged() {
        assert_eq!(
            extract_json("  {\"a\": 1}  "),
            Some(serde_json::json!({"a": 1}))
        );
    }

    #[test]
    fn prose_without_json_is_not_repaired_into_something() {
        assert_eq!(extract_json("I cannot do that."), None);
    }

    #[test]
    fn an_uncompilable_schema_is_a_configuration_error() {
        let err = OutputSchema::compile("bad", &serde_json::json!({"type": 7})).unwrap_err();
        assert!(matches!(err, ToolError::ConfigurationInvalid(_)));
    }

    /// The repair budget is a compile-time constant, so the bound is checked
    /// at compile time: a `const` block fails the build rather than the test
    /// run if someone raises it.
    #[test]
    fn the_repair_budget_is_bounded() {
        const { assert!(MAX_REPAIR_ATTEMPTS <= 3) };
    }
}
