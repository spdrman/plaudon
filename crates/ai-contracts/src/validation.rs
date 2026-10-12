//! Structured output contracts, validation and bounded repair (spec 9.5, task I1.4).
//!
//! Three properties matter more than convenience here:
//!
//! * **Local validation always runs.** Whether or not the provider enforced a
//!   JSON Schema response format, the body is validated here. A provider's
//!   claim of schema support is not evidence that it complied.
//! * **Malformed items are rejected, not dropped.** Every rejection is a typed
//!   [`RejectedItem`] with a JSON Pointer and a reason, so the UI can show what
//!   was discarded instead of silently presenting a shorter list.
//! * **Repair is bounded.** [`RepairPolicy`] caps the number of re-prompts.
//!   Exceeding it is a visible failure, never a partially fabricated output.

use std::collections::BTreeSet;

use plaudon_domain::anchor::{SourceAnchor, MAX_EXCERPT_CHARS};
use plaudon_domain::ids::TranscriptRevisionId;
use serde::{Deserialize, Serialize};

use crate::extraction::{ExtractedItem, OwnerRef};
use crate::template::OutputTemplate;

/// How the structured output contract was enforced on the provider side.
///
/// Defined in [`plaudon_domain::output`] so the repository can persist it
/// without depending on this crate's validator (spec 9.5).
pub use plaudon_domain::output::CompatibilityMode;

/// How many repair attempts are permitted (spec 9.5 "bounded schema repair").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairPolicy {
    /// Maximum number of *additional* generations after the first.
    pub max_attempts: u8,
}

impl RepairPolicy {
    /// The shipped default: one initial generation plus at most two repairs.
    pub const DEFAULT: Self = Self { max_attempts: 2 };

    /// No repair at all. Used when the provider enforced the schema and a
    /// violation therefore indicates a provider contract break.
    pub const NONE: Self = Self { max_attempts: 0 };

    /// Whether another attempt is permitted after `used` attempts.
    #[must_use]
    pub const fn permits(self, used: u8) -> bool {
        used < self.max_attempts
    }
}

impl Default for RepairPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Why one item was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    /// The body is not an object, or a field is not an array.
    EnvelopeShape,
    /// The body contains a key the template does not declare.
    UnknownField,
    /// The item does not match the expected item schema.
    SchemaViolation,
    /// The item could not be parsed into its typed representation.
    UnparsableItem,
    /// A date is not `YYYY-MM-DD` or the `not_specified` sentinel.
    DateInvalid,
    /// The field requires an anchor and the item has none.
    AnchorMissing,
    /// An anchor is structurally impossible (empty span, excessive length).
    AnchorMalformed,
    /// An anchor cites a revision other than the pipeline's input revision.
    AnchorForeignRevision,
    /// The item carries more anchors than the template permits.
    AnchorCountExceeded,
    /// A speaker or participant reference names nobody in the recording.
    UnknownSpeaker,
    /// A string exceeds the template's character ceiling.
    StringTooLong,
    /// A field returned more items than the template permits.
    ArrayTooLarge,
    /// The item contains an instruction aimed at the application rather than content.
    EmbeddedDirective,
}

impl RejectionReason {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EnvelopeShape => "envelope_shape",
            Self::UnknownField => "unknown_field",
            Self::SchemaViolation => "schema_violation",
            Self::UnparsableItem => "unparsable_item",
            Self::DateInvalid => "date_invalid",
            Self::AnchorMissing => "anchor_missing",
            Self::AnchorMalformed => "anchor_malformed",
            Self::AnchorForeignRevision => "anchor_foreign_revision",
            Self::AnchorCountExceeded => "anchor_count_exceeded",
            Self::UnknownSpeaker => "unknown_speaker",
            Self::StringTooLong => "string_too_long",
            Self::ArrayTooLarge => "array_too_large",
            Self::EmbeddedDirective => "embedded_directive",
        }
    }
}

/// One rejected item, reported rather than dropped.
///
/// `detail` is a short structural description. It never contains transcript
/// text or provider payloads (spec 15.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedItem {
    /// JSON Pointer of the offending item or field.
    pub item_path: String,
    /// Why it was rejected.
    pub reason: RejectionReason,
    /// Short structural description, safe to log and to display.
    pub detail: String,
}

impl RejectedItem {
    /// Construct a rejection.
    #[must_use]
    pub fn new(
        item_path: impl Into<String>,
        reason: RejectionReason,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            item_path: item_path.into(),
            reason,
            detail: detail.into(),
        }
    }
}

/// One accepted item together with the field it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedItem {
    /// Template field key.
    pub field: String,
    /// Index inside the field array.
    pub index: u32,
    /// JSON Pointer of the item.
    pub item_path: String,
    /// The typed item.
    pub item: ExtractedItem,
}

/// Outcome of validating one model response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReport {
    /// Items that passed every check.
    pub accepted: Vec<AcceptedItem>,
    /// Items that did not, with reasons.
    pub rejected: Vec<RejectedItem>,
    /// How the contract was enforced upstream.
    pub compatibility_mode: CompatibilityMode,
    /// Number of repair attempts consumed before this report.
    pub repair_attempts: u8,
}

impl ValidationReport {
    /// Whether every item passed.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.rejected.is_empty()
    }

    /// Whether the report is usable as a final output.
    ///
    /// An output with zero accepted items and at least one rejection is a
    /// visible failure, not an empty summary.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        !self.accepted.is_empty() || self.rejected.is_empty()
    }

    /// A deterministic, content-free repair instruction listing what to fix.
    ///
    /// Only JSON Pointers and reason codes are echoed back to the model, never
    /// transcript text and never the model's own rejected strings.
    #[must_use]
    pub fn repair_instruction(&self) -> String {
        let mut lines = vec![
            "Your previous response was rejected by local schema validation.".to_owned(),
            "Fix exactly the listed problems and return the whole document again.".to_owned(),
            "Do not invent values to satisfy a rejection: omit the item instead.".to_owned(),
        ];
        for rejection in &self.rejected {
            lines.push(format!(
                "- {} : {} ({})",
                rejection.item_path,
                rejection.reason.as_str(),
                rejection.detail
            ));
        }
        lines.join("\n")
    }
}

/// Speaker labels the recording actually has.
///
/// When `enforced` is false — no diarization ran — participant names cannot be
/// checked and are accepted. We do not invent a check we cannot perform.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpeakerDirectory {
    labels: BTreeSet<String>,
    enforced: bool,
}

impl SpeakerDirectory {
    /// A directory that cannot check anything, because no speaker is known.
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            labels: BTreeSet::new(),
            enforced: false,
        }
    }

    /// A directory that rejects references outside `labels`.
    #[must_use]
    pub fn of(labels: impl IntoIterator<Item = String>) -> Self {
        Self {
            labels: labels.into_iter().collect(),
            enforced: true,
        }
    }

    /// Whether `name` is an acceptable speaker reference.
    #[must_use]
    pub fn accepts(&self, name: &str) -> bool {
        !self.enforced || self.labels.contains(name)
    }
}

/// Validates a model response against a template's expected schema.
#[derive(Debug)]
pub struct OutputValidator {
    template: OutputTemplate,
    schema: jsonschema::Validator,
    input_revision: TranscriptRevisionId,
    speakers: SpeakerDirectory,
}

/// Why a validator could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the expected output schema does not compile: {detail}")]
pub struct ValidatorBuildError {
    /// Short, non-sensitive reason.
    pub detail: String,
}

impl OutputValidator {
    /// Build a validator for `template` against the already-resolved `schema`.
    pub fn new(
        template: OutputTemplate,
        schema: &serde_json::Value,
        input_revision: TranscriptRevisionId,
        speakers: SpeakerDirectory,
    ) -> Result<Self, ValidatorBuildError> {
        let compiled = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .build(schema)
            .map_err(|error| ValidatorBuildError {
                detail: error.to_string(),
            })?;
        Ok(Self {
            template,
            schema: compiled,
            input_revision,
            speakers,
        })
    }

    /// The template this validator enforces.
    #[must_use]
    pub const fn template(&self) -> &OutputTemplate {
        &self.template
    }

    /// Validate one model response body.
    pub fn validate(
        &self,
        body: &serde_json::Value,
        compatibility_mode: CompatibilityMode,
        repair_attempts: u8,
    ) -> ValidationReport {
        let mut accepted = Vec::new();
        let mut rejected = Vec::new();

        let Some(object) = body.as_object() else {
            rejected.push(RejectedItem::new(
                "",
                RejectionReason::EnvelopeShape,
                "response body is not a JSON object",
            ));
            return ValidationReport {
                accepted,
                rejected,
                compatibility_mode,
                repair_attempts,
            };
        };

        // Whole-document schema errors are recorded first, keyed by pointer, so
        // a schema violation is attributed to the item that caused it.
        for error in self.schema.iter_errors(body) {
            rejected.push(RejectedItem::new(
                error.instance_path.to_string(),
                RejectionReason::SchemaViolation,
                schema_detail(&error),
            ));
        }

        for key in object.keys() {
            if self.template.field(key).is_none() {
                rejected.push(RejectedItem::new(
                    format!("/{key}"),
                    RejectionReason::UnknownField,
                    "the template does not declare this field",
                ));
            }
        }

        for field in &self.template.fields {
            let pointer = format!("/{}", field.key);
            let Some(value) = object.get(&field.key) else {
                continue;
            };
            let Some(items) = value.as_array() else {
                rejected.push(RejectedItem::new(
                    &pointer,
                    RejectionReason::EnvelopeShape,
                    "field is not an array",
                ));
                continue;
            };
            if items.len() > field.max_items as usize {
                rejected.push(RejectedItem::new(
                    &pointer,
                    RejectionReason::ArrayTooLarge,
                    format!(
                        "{} items exceed the maximum of {}",
                        items.len(),
                        field.max_items
                    ),
                ));
                continue;
            }
            for (index, raw) in items.iter().enumerate() {
                let item_path = format!("{pointer}/{index}");
                if rejected.iter().any(|r| {
                    r.reason == RejectionReason::SchemaViolation
                        && r.item_path.starts_with(&item_path)
                }) {
                    continue;
                }
                match self.validate_item(&item_path, field.key.as_str(), raw) {
                    Ok(item) => accepted.push(AcceptedItem {
                        field: field.key.clone(),
                        index: u32::try_from(index).unwrap_or(u32::MAX),
                        item_path,
                        item,
                    }),
                    Err(reasons) => rejected.extend(reasons),
                }
            }
        }

        rejected.sort_by(|a, b| {
            a.item_path
                .cmp(&b.item_path)
                .then_with(|| a.reason.cmp(&b.reason))
        });
        rejected.dedup();

        ValidationReport {
            accepted,
            rejected,
            compatibility_mode,
            repair_attempts,
        }
    }

    fn validate_item(
        &self,
        item_path: &str,
        field_key: &str,
        raw: &serde_json::Value,
    ) -> Result<ExtractedItem, Vec<RejectedItem>> {
        let field = self
            .template
            .field(field_key)
            .expect("field key came from the template");

        let item: ExtractedItem = match serde_json::from_value(raw.clone()) {
            Ok(item) => item,
            Err(error) => {
                let reason = if error.to_string().contains("date must be") {
                    RejectionReason::DateInvalid
                } else {
                    RejectionReason::UnparsableItem
                };
                return Err(vec![RejectedItem::new(
                    item_path,
                    reason,
                    structural_detail(&error),
                )]);
            }
        };

        let mut problems = Vec::new();

        if item.claim_kind() != field.kind {
            problems.push(RejectedItem::new(
                item_path,
                RejectionReason::SchemaViolation,
                "item kind does not match the field it was returned under",
            ));
        }

        for text in item.text_fields() {
            let chars = text.chars().count();
            if chars > field.max_text_chars as usize {
                problems.push(RejectedItem::new(
                    item_path,
                    RejectionReason::StringTooLong,
                    format!(
                        "a string of {chars} characters exceeds the {} allowed",
                        field.max_text_chars
                    ),
                ));
                break;
            }
        }

        let anchors = item.anchors();
        if anchors.len() < field.anchors.min_per_item as usize {
            problems.push(RejectedItem::new(
                item_path,
                RejectionReason::AnchorMissing,
                format!(
                    "{} anchors supplied, {} required",
                    anchors.len(),
                    field.anchors.min_per_item
                ),
            ));
        }
        if anchors.len() > field.anchors.max_per_item as usize {
            problems.push(RejectedItem::new(
                item_path,
                RejectionReason::AnchorCountExceeded,
                format!(
                    "{} anchors supplied, at most {} permitted",
                    anchors.len(),
                    field.anchors.max_per_item
                ),
            ));
        }
        for (index, anchor) in anchors.iter().enumerate() {
            if let Some(problem) = self.anchor_problem(item_path, index, anchor) {
                problems.push(problem);
            }
        }

        for name in Self::speaker_references(&item) {
            if !self.speakers.accepts(name) {
                problems.push(RejectedItem::new(
                    item_path,
                    RejectionReason::UnknownSpeaker,
                    "references a speaker the recording does not have",
                ));
                break;
            }
        }

        if problems.is_empty() {
            Ok(item)
        } else {
            Err(problems)
        }
    }

    fn anchor_problem(
        &self,
        item_path: &str,
        index: usize,
        anchor: &SourceAnchor,
    ) -> Option<RejectedItem> {
        let path = format!("{item_path}/anchors/{index}");
        if anchor.revision_id != self.input_revision {
            return Some(RejectedItem::new(
                path,
                RejectionReason::AnchorForeignRevision,
                "anchor cites a transcript revision this run did not read",
            ));
        }
        if anchor.char_end <= anchor.char_start {
            return Some(RejectedItem::new(
                path,
                RejectionReason::AnchorMalformed,
                "anchor span is empty or inverted",
            ));
        }
        if anchor.char_end - anchor.char_start > MAX_EXCERPT_CHARS {
            return Some(RejectedItem::new(
                path,
                RejectionReason::AnchorMalformed,
                "anchor span exceeds the excerpt ceiling",
            ));
        }
        if anchor.excerpt.trim().is_empty() {
            return Some(RejectedItem::new(
                path,
                RejectionReason::AnchorMalformed,
                "anchor carries no excerpt",
            ));
        }
        if anchor.excerpt.chars().count() > MAX_EXCERPT_CHARS as usize {
            return Some(RejectedItem::new(
                path,
                RejectionReason::AnchorMalformed,
                "anchor excerpt exceeds the excerpt ceiling",
            ));
        }
        None
    }

    fn speaker_references(item: &ExtractedItem) -> Vec<&str> {
        match item {
            ExtractedItem::Decision(decision) => {
                decision.participants.iter().map(String::as_str).collect()
            }
            ExtractedItem::ActionItem(action) => match &action.owner {
                OwnerRef::Named(name) => vec![name.as_str()],
                OwnerRef::Unassigned => Vec::new(),
            },
            ExtractedItem::Question(question) => match &question.owner {
                OwnerRef::Named(name) => vec![name.as_str()],
                OwnerRef::Unassigned => Vec::new(),
            },
            _ => Vec::new(),
        }
    }
}

fn schema_detail(error: &jsonschema::ValidationError<'_>) -> String {
    // The failing keyword location is structural; the instance value is model
    // output and is deliberately not echoed (spec 15.4).
    format!("fails schema rule {}", error.schema_path)
}

fn structural_detail(error: &serde_json::Error) -> String {
    match error.classify() {
        serde_json::error::Category::Data => "a field has the wrong type or is missing".to_owned(),
        serde_json::error::Category::Syntax => "the item is not valid JSON".to_owned(),
        serde_json::error::Category::Eof => "the item ended unexpectedly".to_owned(),
        serde_json::error::Category::Io => "the item could not be read".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::template::{
        AnchorRequirement, ExpectedSchema, ExtractionField, OutputTemplate, RenderingRules,
        TemplateInstructions, TemplateOrigin,
    };
    use plaudon_domain::claim::ClaimKind;

    fn template() -> OutputTemplate {
        OutputTemplate {
            template_schema_version: 1,
            id: "t".into(),
            version: "1.0.0".into(),
            display_name: "T".into(),
            purpose: "test".into(),
            origin: TemplateOrigin::BuiltIn,
            prompt_version: "1".into(),
            output_schema_version: "1".into(),
            instructions: TemplateInstructions {
                system: "s".into(),
                section_summary: "a".into(),
                synthesis: "b".into(),
                extraction: "c".into(),
            },
            fields: vec![ExtractionField {
                key: "decisions".into(),
                kind: ClaimKind::Decision,
                heading: "Decisions".into(),
                anchors: AnchorRequirement {
                    min_per_item: 1,
                    max_per_item: 2,
                },
                max_items: 3,
                max_text_chars: 40,
            }],
            output_schema: ExpectedSchema::Reference {
                schema_ref: "meeting_extraction".into(),
            },
            rendering: RenderingRules {
                order: vec!["decisions".into()],
                empty_placeholder: "None recorded".into(),
                show_anchors: true,
            },
        }
    }

    fn permissive_schema() -> serde_json::Value {
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "decisions": { "type": "array" }
            }
        })
    }

    fn validator(revision: TranscriptRevisionId) -> OutputValidator {
        OutputValidator::new(
            template(),
            &permissive_schema(),
            revision,
            SpeakerDirectory::unknown(),
        )
        .unwrap()
    }

    fn decision(revision: TranscriptRevisionId, anchors: usize, text: &str) -> serde_json::Value {
        let anchor_values: Vec<serde_json::Value> = (0..anchors)
            .map(|_| {
                serde_json::json!({
                    "revision_id": revision.to_string(),
                    "seq_index": 0,
                    "char_start": 0,
                    "char_end": 5,
                    "excerpt": "Alpha"
                })
            })
            .collect();
        serde_json::json!({
            "kind": "decision",
            "statement": text,
            "participants": [],
            "time_reference": "not_specified",
            "assertion": "directly_stated",
            "anchors": anchor_values
        })
    }

    #[test]
    fn a_valid_body_is_accepted_without_rejections() {
        let revision = TranscriptRevisionId::new();
        let body = serde_json::json!({ "decisions": [decision(revision, 1, "Ship Friday")] });
        let report = validator(revision).validate(&body, CompatibilityMode::SchemaEnforced, 0);
        assert!(report.is_clean(), "{:?}", report.rejected);
        assert_eq!(report.accepted.len(), 1);
        assert_eq!(report.accepted[0].item_path, "/decisions/0");
    }

    #[test]
    fn an_item_without_the_required_anchor_is_rejected_not_dropped() {
        let revision = TranscriptRevisionId::new();
        let body = serde_json::json!({ "decisions": [decision(revision, 0, "Ship Friday")] });
        let report = validator(revision).validate(&body, CompatibilityMode::SchemaEnforced, 0);
        assert_eq!(report.accepted, [] as [AcceptedItem; 0]);
        assert_eq!(report.rejected.len(), 1);
        assert_eq!(report.rejected[0].reason, RejectionReason::AnchorMissing);
        assert_eq!(report.rejected[0].item_path, "/decisions/0");
    }

    #[test]
    fn an_anchor_into_a_foreign_revision_is_rejected() {
        let revision = TranscriptRevisionId::new();
        let body = serde_json::json!({
            "decisions": [decision(TranscriptRevisionId::new(), 1, "Ship Friday")]
        });
        let report = validator(revision).validate(&body, CompatibilityMode::SchemaEnforced, 0);
        assert_eq!(
            report.rejected[0].reason,
            RejectionReason::AnchorForeignRevision
        );
    }

    #[test]
    fn too_many_anchors_are_rejected() {
        let revision = TranscriptRevisionId::new();
        let body = serde_json::json!({ "decisions": [decision(revision, 3, "Ship")] });
        let report = validator(revision).validate(&body, CompatibilityMode::SchemaEnforced, 0);
        assert!(report
            .rejected
            .iter()
            .any(|r| r.reason == RejectionReason::AnchorCountExceeded));
    }

    #[test]
    fn an_oversized_array_is_rejected_whole() {
        let revision = TranscriptRevisionId::new();
        let items: Vec<serde_json::Value> = (0..4).map(|_| decision(revision, 1, "Ship")).collect();
        let body = serde_json::json!({ "decisions": items });
        let report = validator(revision).validate(&body, CompatibilityMode::SchemaEnforced, 0);
        assert_eq!(report.rejected[0].reason, RejectionReason::ArrayTooLarge);
        assert_eq!(report.accepted, [] as [AcceptedItem; 0]);
    }

    #[test]
    fn an_oversized_string_is_rejected() {
        let revision = TranscriptRevisionId::new();
        let long = "x".repeat(41);
        let body = serde_json::json!({ "decisions": [decision(revision, 1, &long)] });
        let report = validator(revision).validate(&body, CompatibilityMode::SchemaEnforced, 0);
        assert_eq!(report.rejected[0].reason, RejectionReason::StringTooLong);
    }

    #[test]
    fn an_unknown_top_level_field_is_reported_rather_than_ignored() {
        let revision = TranscriptRevisionId::new();
        let body = serde_json::json!({
            "decisions": [],
            "exfiltrate": [{"url": "https://example.invalid"}]
        });
        let report = validator(revision).validate(&body, CompatibilityMode::SchemaEnforced, 0);
        assert!(report
            .rejected
            .iter()
            .any(|r| r.reason == RejectionReason::UnknownField
                || r.reason == RejectionReason::SchemaViolation));
    }

    #[test]
    fn a_non_object_body_fails_the_envelope_check() {
        let revision = TranscriptRevisionId::new();
        let report = validator(revision).validate(
            &serde_json::json!([1, 2]),
            CompatibilityMode::PromptConstrained,
            0,
        );
        assert_eq!(report.rejected[0].reason, RejectionReason::EnvelopeShape);
        assert!(!report.is_usable());
    }

    #[test]
    fn an_unparsable_date_is_reported_as_a_date_problem() {
        let revision = TranscriptRevisionId::new();
        let mut template = template();
        template.fields[0].key = "action_items".into();
        template.fields[0].kind = ClaimKind::ActionItem;
        template.rendering.order = vec!["action_items".into()];
        let schema = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": { "action_items": { "type": "array" } }
        });
        let validator =
            OutputValidator::new(template, &schema, revision, SpeakerDirectory::unknown()).unwrap();
        let body = serde_json::json!({
            "action_items": [{
                "kind": "action_item",
                "verb": "Send",
                "description": "quote",
                "owner": "unassigned",
                "due_date": "next friday",
                "status": "open",
                "assertion": "directly_stated",
                "anchors": []
            }]
        });
        let report = validator.validate(&body, CompatibilityMode::PromptConstrained, 0);
        assert_eq!(report.rejected[0].reason, RejectionReason::DateInvalid);
    }

    #[test]
    fn a_speaker_reference_outside_the_recording_is_rejected() {
        let revision = TranscriptRevisionId::new();
        let validator = OutputValidator::new(
            template(),
            &permissive_schema(),
            revision,
            SpeakerDirectory::of(["Speaker 1".to_owned()]),
        )
        .unwrap();
        let mut item = decision(revision, 1, "Ship");
        item["participants"] = serde_json::json!(["Mallory"]);
        let body = serde_json::json!({ "decisions": [item] });
        let report = validator.validate(&body, CompatibilityMode::SchemaEnforced, 0);
        assert_eq!(report.rejected[0].reason, RejectionReason::UnknownSpeaker);
    }

    #[test]
    fn an_unenforceable_speaker_check_accepts_rather_than_pretends() {
        let revision = TranscriptRevisionId::new();
        let mut item = decision(revision, 1, "Ship");
        item["participants"] = serde_json::json!(["Mallory"]);
        let body = serde_json::json!({ "decisions": [item] });
        let report = validator(revision).validate(&body, CompatibilityMode::SchemaEnforced, 0);
        assert!(report.is_clean());
    }

    #[test]
    fn the_repair_bound_is_respected() {
        assert!(RepairPolicy::DEFAULT.permits(0));
        assert!(RepairPolicy::DEFAULT.permits(1));
        assert!(!RepairPolicy::DEFAULT.permits(2));
        assert!(!RepairPolicy::NONE.permits(0));
    }

    #[test]
    fn the_repair_instruction_names_paths_and_reasons_but_no_content() {
        let revision = TranscriptRevisionId::new();
        let long = "secret client name ".repeat(5);
        let body = serde_json::json!({ "decisions": [decision(revision, 1, &long)] });
        let report = validator(revision).validate(&body, CompatibilityMode::PromptConstrained, 0);
        let instruction = report.repair_instruction();
        assert!(instruction.contains("/decisions/0"));
        assert!(instruction.contains("string_too_long"));
        assert!(!instruction.contains("secret client name"));
    }
}
