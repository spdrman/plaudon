//! Versioned output templates (spec 9.2, 9.3, task I1.2).
//!
//! A template is data, not code: purpose, extraction fields, instructions, the
//! expected JSON schema, rendering rules and the anchor requirement per field.
//! Users copy and edit templates without touching the binary, and a template
//! change never rewrites an output that was already generated — every stored
//! output keeps the `(template_id, template_version)` it ran with.
//!
//! The template *schema* is itself versioned ([`TEMPLATE_SCHEMA_VERSION`]). A
//! template declaring an unknown schema version is rejected rather than
//! interpreted optimistically.

use std::collections::BTreeSet;

use plaudon_domain::claim::ClaimKind;
use serde::{Deserialize, Serialize};

/// Version of the template schema this build understands.
pub const TEMPLATE_SCHEMA_VERSION: u32 = 1;

/// Identifier of the shared meeting extraction output schema.
pub const MEETING_EXTRACTION_SCHEMA_ID: &str = "meeting_extraction";

/// Where a template came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum TemplateOrigin {
    /// Shipped with the application and not editable in place.
    BuiltIn,
    /// A user copy of a built-in template.
    UserCopy {
        /// Template the copy was derived from.
        base_id: String,
        /// Version of the template the copy was derived from.
        base_version: String,
    },
}

impl TemplateOrigin {
    /// Whether the application may overwrite this template on upgrade.
    #[must_use]
    pub const fn is_built_in(&self) -> bool {
        matches!(self, Self::BuiltIn)
    }
}

/// How many anchors an extracted item must carry (spec 9.3 "required source anchors").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorRequirement {
    /// Minimum resolving anchors per item. Zero means anchors are optional.
    pub min_per_item: u32,
    /// Maximum anchors an item may carry, bounding model output size.
    pub max_per_item: u32,
}

impl AnchorRequirement {
    /// Whether an item of this field must cite the transcript.
    #[must_use]
    pub const fn is_required(self) -> bool {
        self.min_per_item > 0
    }
}

/// One extraction field of a template (one §9.2 output type).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionField {
    /// Key of the field inside the output body, e.g. `decisions`.
    pub key: String,
    /// Claim kind produced by items of this field.
    pub kind: ClaimKind,
    /// Heading shown in the UI and in exports.
    pub heading: String,
    /// Anchor requirement for items of this field.
    pub anchors: AnchorRequirement,
    /// Maximum number of items the model may return for this field.
    pub max_items: u32,
    /// Maximum length of any single string in an item, in characters.
    pub max_text_chars: u32,
}

/// Prompts the template contributes to each pipeline stage.
///
/// These are *task instructions only*. Transcript text is never concatenated
/// into them; the prompt assembler keeps the two in separate, fenced regions
/// (spec 9.6, task I1.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateInstructions {
    /// Role and refusal rules shared by every stage.
    pub system: String,
    /// Instruction for summarizing one chunk.
    pub section_summary: String,
    /// Instruction for consolidating section summaries.
    pub synthesis: String,
    /// Instruction for structured extraction.
    pub extraction: String,
}

/// How an output is rendered in the UI and in exports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderingRules {
    /// Field keys in presentation order.
    pub order: Vec<String>,
    /// Text shown when a field has no items. Never omitted silently.
    pub empty_placeholder: String,
    /// Whether the renderer shows an anchor chip per item.
    pub show_anchors: bool,
}

/// The expected output schema, either inline or referenced by id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExpectedSchema {
    /// A reference to a schema shipped under `schemas/outputs/`.
    Reference {
        /// Identifier of the shipped schema.
        schema_ref: String,
    },
    /// A complete inline JSON Schema, used by user templates.
    Inline(serde_json::Value),
}

/// A versioned output template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputTemplate {
    /// Version of the template schema this document conforms to.
    pub template_schema_version: u32,
    /// Stable template identifier, e.g. `general_meeting`.
    pub id: String,
    /// Template version, e.g. `1.0.0`. Part of every output's provenance.
    pub version: String,
    /// Name shown in the Templates screen.
    pub display_name: String,
    /// What the template is for (spec 9.3 "purpose").
    pub purpose: String,
    /// Where the template came from.
    #[serde(flatten)]
    pub origin: TemplateOrigin,
    /// Version of the prompt text, recorded on every output.
    pub prompt_version: String,
    /// Version of the expected output schema, recorded on every output.
    pub output_schema_version: String,
    /// Stage instructions.
    pub instructions: TemplateInstructions,
    /// Extraction fields, in declaration order.
    pub fields: Vec<ExtractionField>,
    /// Expected output schema.
    pub output_schema: ExpectedSchema,
    /// Rendering rules.
    pub rendering: RenderingRules,
}

/// Why a template document was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// The document is not valid JSON or does not match the template struct.
    #[error("template document is malformed: {detail}")]
    Malformed {
        /// Short, non-sensitive reason.
        detail: String,
    },
    /// The document declares a template-schema version this build cannot read.
    #[error("template declares schema version {found}, this build understands {supported}")]
    UnsupportedSchemaVersion {
        /// Version the document declared.
        found: u32,
        /// Version this build understands.
        supported: u32,
    },
    /// A required structural invariant was violated.
    #[error("template {id} is invalid: {detail}")]
    Invalid {
        /// Template identifier, when it could be read.
        id: String,
        /// Short, non-sensitive reason.
        detail: String,
    },
    /// The template references a schema this build does not ship.
    #[error("template {id} references unknown output schema {schema_ref}")]
    UnknownSchemaReference {
        /// Template identifier.
        id: String,
        /// Reference that could not be resolved.
        schema_ref: String,
    },
}

impl OutputTemplate {
    /// Parse and validate a template document.
    ///
    /// Validation is structural and total: an invalid template is rejected
    /// with a typed error and never silently replaced by a default.
    pub fn parse(document: &str) -> Result<Self, TemplateError> {
        let template: Self = serde_json::from_str(document).map_err(|error| {
            TemplateError::Malformed {
                detail: format!("line {}: {}", error.line(), category_label(&error)),
            }
        })?;
        template.validate()?;
        Ok(template)
    }

    /// Check every structural invariant.
    pub fn validate(&self) -> Result<(), TemplateError> {
        if self.template_schema_version != TEMPLATE_SCHEMA_VERSION {
            return Err(TemplateError::UnsupportedSchemaVersion {
                found: self.template_schema_version,
                supported: TEMPLATE_SCHEMA_VERSION,
            });
        }
        let invalid = |detail: &str| TemplateError::Invalid {
            id: self.id.clone(),
            detail: detail.to_owned(),
        };
        if self.id.is_empty() || !is_identifier(&self.id) {
            return Err(invalid("id must be a non-empty lowercase identifier"));
        }
        if self.version.is_empty() {
            return Err(invalid("version must not be empty"));
        }
        if self.purpose.trim().is_empty() {
            return Err(invalid("purpose must not be empty"));
        }
        if self.prompt_version.is_empty() || self.output_schema_version.is_empty() {
            return Err(invalid("prompt and output schema versions are required"));
        }
        if self.instructions.system.trim().is_empty()
            || self.instructions.section_summary.trim().is_empty()
            || self.instructions.synthesis.trim().is_empty()
            || self.instructions.extraction.trim().is_empty()
        {
            return Err(invalid("every stage instruction must be present"));
        }
        if self.fields.is_empty() {
            return Err(invalid("a template must declare at least one field"));
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for field in &self.fields {
            if !is_identifier(&field.key) {
                return Err(invalid("field keys must be lowercase identifiers"));
            }
            if !seen.insert(field.key.as_str()) {
                return Err(invalid("duplicate field key"));
            }
            if field.max_items == 0 || field.max_text_chars == 0 {
                return Err(invalid("field ceilings must be positive"));
            }
            if field.anchors.max_per_item < field.anchors.min_per_item {
                return Err(invalid("anchor maximum is below the minimum"));
            }
            if field.anchors.max_per_item == 0 {
                return Err(invalid("a field must permit at least one anchor"));
            }
        }
        if self.rendering.order.len() != self.fields.len() {
            return Err(invalid("rendering order must list every field exactly once"));
        }
        for key in &self.rendering.order {
            if !seen.contains(key.as_str()) {
                return Err(invalid("rendering order names an unknown field"));
            }
        }
        Ok(())
    }

    /// The field that owns `key`.
    #[must_use]
    pub fn field(&self, key: &str) -> Option<&ExtractionField> {
        self.fields.iter().find(|field| field.key == key)
    }

    /// Whether items of `key` must carry at least one anchor.
    #[must_use]
    pub fn anchors_required(&self, key: &str) -> bool {
        self.field(key)
            .is_some_and(|field| field.anchors.is_required())
    }

    /// `template_id@version`, the form recorded in output provenance.
    #[must_use]
    pub fn qualified_version(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }

    /// Produce an editable user copy of this template.
    ///
    /// The copy carries a new id and records what it was derived from, so a
    /// built-in template can never be overwritten in place (spec 9.3).
    #[must_use]
    pub fn to_user_copy(&self, new_id: impl Into<String>) -> Self {
        let mut copy = self.clone();
        copy.id = new_id.into();
        copy.origin = TemplateOrigin::UserCopy {
            base_id: self.id.clone(),
            base_version: self.version.clone(),
        };
        copy
    }
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Structural description of a JSON parse failure.
///
/// The offending text is never echoed: a template document may be a user copy
/// containing their own wording, and parse errors are logged.
fn category_label(error: &serde_json::Error) -> &'static str {
    match error.classify() {
        serde_json::error::Category::Data => "a field has the wrong type or is missing",
        serde_json::error::Category::Syntax => "the document is not valid JSON",
        serde_json::error::Category::Eof => "the document ended unexpectedly",
        serde_json::error::Category::Io => "the document could not be read",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> serde_json::Value {
        serde_json::json!({
            "template_schema_version": 1,
            "id": "general_meeting",
            "version": "1.0.0",
            "display_name": "General Meeting",
            "purpose": "Summarize a general meeting.",
            "origin": "built_in",
            "prompt_version": "1.0.0",
            "output_schema_version": "1.0.0",
            "instructions": {
                "system": "s",
                "section_summary": "a",
                "synthesis": "b",
                "extraction": "c"
            },
            "fields": [{
                "key": "decisions",
                "kind": "decision",
                "heading": "Decisions",
                "anchors": {"min_per_item": 1, "max_per_item": 4},
                "max_items": 50,
                "max_text_chars": 600
            }],
            "output_schema": {"schema_ref": "meeting_extraction"},
            "rendering": {
                "order": ["decisions"],
                "empty_placeholder": "None recorded",
                "show_anchors": true
            }
        })
    }

    #[test]
    fn a_well_formed_template_parses() {
        let template = OutputTemplate::parse(&minimal().to_string()).unwrap();
        assert_eq!(template.qualified_version(), "general_meeting@1.0.0");
        assert!(template.origin.is_built_in());
        assert!(template.anchors_required("decisions"));
    }

    #[test]
    fn an_unknown_template_schema_version_is_rejected_not_defaulted() {
        let mut doc = minimal();
        doc["template_schema_version"] = serde_json::json!(99);
        let error = OutputTemplate::parse(&doc.to_string()).unwrap_err();
        assert!(matches!(
            error,
            TemplateError::UnsupportedSchemaVersion { found: 99, .. }
        ));
    }

    #[test]
    fn a_template_without_instructions_is_rejected() {
        let mut doc = minimal();
        doc["instructions"]["extraction"] = serde_json::json!("  ");
        assert!(matches!(
            OutputTemplate::parse(&doc.to_string()),
            Err(TemplateError::Invalid { .. })
        ));
    }

    #[test]
    fn duplicate_field_keys_are_rejected() {
        let mut doc = minimal();
        let field = doc["fields"][0].clone();
        doc["fields"] = serde_json::json!([field.clone(), field]);
        doc["rendering"]["order"] = serde_json::json!(["decisions", "decisions"]);
        assert!(matches!(
            OutputTemplate::parse(&doc.to_string()),
            Err(TemplateError::Invalid { .. })
        ));
    }

    #[test]
    fn a_rendering_order_that_omits_a_field_is_rejected() {
        let mut doc = minimal();
        doc["rendering"]["order"] = serde_json::json!([]);
        assert!(matches!(
            OutputTemplate::parse(&doc.to_string()),
            Err(TemplateError::Invalid { .. })
        ));
    }

    #[test]
    fn a_user_copy_records_its_base_and_keeps_the_builtin_intact() {
        let template = OutputTemplate::parse(&minimal().to_string()).unwrap();
        let copy = template.to_user_copy("my_meeting");
        assert_eq!(copy.id, "my_meeting");
        assert_eq!(
            copy.origin,
            TemplateOrigin::UserCopy {
                base_id: "general_meeting".into(),
                base_version: "1.0.0".into()
            }
        );
        assert!(template.origin.is_built_in());
        assert_eq!(template.id, "general_meeting");
    }

    #[test]
    fn malformed_json_is_a_typed_error_without_leaking_content() {
        let error = OutputTemplate::parse("{ not json ").unwrap_err();
        let rendered = error.to_string();
        assert!(matches!(error, TemplateError::Malformed { .. }));
        assert!(!rendered.contains("not json"));
    }
}
