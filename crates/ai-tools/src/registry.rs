//! The registry of tool definitions and the user-editable configuration that
//! extends it (spec 9.7).
//!
//! Two rules are structural rather than documented:
//!
//! * A registry keeps every version of every tool. Looking a run up by
//!   `(id, version)` always resolves, even after the user edits the tool.
//! * A user edit never mutates a definition. It appends the next version, so
//!   "a tool version change never rewrites prior runs" is true by
//!   construction rather than by a check somewhere else.

use std::collections::BTreeMap;
use std::path::Path;

use plaudon_ai_contracts::capabilities::TextModelCapabilities;
use serde::{Deserialize, Serialize};

use crate::error::ToolError;
use crate::tool::{
    ModelPolicy, RequiredCapabilities, ToolId, ToolKind, ToolOrigin, TranscriptToolDefinition,
    TOOL_CONTRACT_VERSION,
};

/// A user-supplied edit to a tool, loaded from the user data directory.
///
/// The user can copy and customize a tool without editing source code
/// (spec 9.3). The override names the tool it derives from; it cannot invent a
/// kind or a capability requirement, because those drive dispatch safety.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolConfigOverride {
    /// Tool whose latest version is being edited.
    pub tool_id: String,
    /// Replacement instruction text.
    pub instruction: String,
    /// Version label for the replacement instruction.
    pub prompt_version: String,
    /// Replacement output schema, when the user also changed the shape.
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
    /// Version label for the replacement schema.
    #[serde(default)]
    pub output_schema_version: Option<String>,
}

/// Every known definition, keyed by identity then version.
#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<ToolId, BTreeMap<u32, TranscriptToolDefinition>>,
}

impl ToolRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The registry the application ships with (spec 9.7 default tool set).
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        for tool in builtin_tools() {
            registry.insert(tool);
        }
        registry
    }

    /// Insert a definition, replacing any definition with the same identity
    /// and version.
    pub fn insert(&mut self, tool: TranscriptToolDefinition) {
        self.tools
            .entry(tool.id.clone())
            .or_default()
            .insert(tool.version, tool);
    }

    /// Resolve an exact `(id, version)` pair.
    #[must_use]
    pub fn get(&self, id: &ToolId, version: u32) -> Option<&TranscriptToolDefinition> {
        self.tools.get(id)?.get(&version)
    }

    /// The highest registered version of a tool.
    #[must_use]
    pub fn latest(&self, id: &ToolId) -> Option<&TranscriptToolDefinition> {
        self.tools.get(id)?.values().next_back()
    }

    /// Every tool at its highest version, in identity order.
    #[must_use]
    pub fn latest_all(&self) -> Vec<&TranscriptToolDefinition> {
        self.tools
            .values()
            .filter_map(|versions| versions.values().next_back())
            .collect()
    }

    /// Every registered definition of every version.
    #[must_use]
    pub fn all_versions(&self) -> Vec<&TranscriptToolDefinition> {
        self.tools
            .values()
            .flat_map(BTreeMap::values)
            .collect::<Vec<_>>()
    }

    /// Tools that may be offered for a given model.
    ///
    /// A tool whose requirements the model does not meet is simply absent:
    /// spec 9.7 says it is not offered, and [`TranscriptToolDefinition::ensure_runnable`]
    /// refuses it if a caller invokes it anyway.
    #[must_use]
    pub fn offerable_for(&self, caps: &TextModelCapabilities) -> Vec<&TranscriptToolDefinition> {
        self.latest_all()
            .into_iter()
            .filter(|tool| tool.is_offerable(caps))
            .collect()
    }

    /// Apply a user override, appending a new version of the named tool.
    pub fn apply_override(
        &mut self,
        config: &ToolConfigOverride,
    ) -> Result<&TranscriptToolDefinition, ToolError> {
        let id = ToolId::new(config.tool_id.clone());
        let base = self
            .latest(&id)
            .ok_or_else(|| ToolError::ConfigurationInvalid(format!("unknown tool {id}")))?
            .clone();

        if config.instruction.trim().is_empty() {
            return Err(ToolError::ConfigurationInvalid(
                "instruction must not be empty".to_string(),
            ));
        }
        if config.prompt_version.trim().is_empty() {
            return Err(ToolError::ConfigurationInvalid(
                "prompt_version must not be empty".to_string(),
            ));
        }
        let schema = config
            .output_schema
            .clone()
            .unwrap_or_else(|| base.output_schema.clone());
        if !schema.is_object() {
            return Err(ToolError::ConfigurationInvalid(
                "output_schema must be a JSON object".to_string(),
            ));
        }
        let schema_version = config
            .output_schema_version
            .clone()
            .unwrap_or_else(|| base.output_schema_version.clone());
        if config.output_schema.is_some() && config.output_schema_version.is_none() {
            return Err(ToolError::ConfigurationInvalid(
                "a changed output_schema requires a new output_schema_version".to_string(),
            ));
        }

        let edited = base.edited(
            config.instruction.clone(),
            config.prompt_version.clone(),
            schema,
            schema_version,
        );
        let version = edited.version;
        self.insert(edited);
        Ok(self.get(&id, version).expect("just inserted"))
    }

    /// Load every `*.json` override in `dir` and apply it.
    ///
    /// A missing directory is not an error: the user simply has no custom
    /// tools. A malformed file is an error, because silently ignoring it would
    /// leave the user convinced their edit took effect.
    pub fn load_overrides_from(&mut self, dir: &Path) -> Result<usize, ToolError> {
        if !dir.exists() {
            return Ok(0);
        }
        let mut paths: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| ToolError::ConfigurationInvalid(e.to_string()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        paths.sort();

        let mut applied = 0;
        for path in paths {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| ToolError::ConfigurationInvalid(e.to_string()))?;
            let config: ToolConfigOverride = serde_json::from_str(&text)
                .map_err(|e| ToolError::ConfigurationInvalid(format!("{}: {e}", path.display())))?;
            self.apply_override(&config)?;
            applied += 1;
        }
        Ok(applied)
    }
}

fn definition(
    id: &str,
    kind: ToolKind,
    display_name: &str,
    instruction: &str,
    schema: serde_json::Value,
    optional: bool,
) -> TranscriptToolDefinition {
    TranscriptToolDefinition {
        payload_version: TOOL_CONTRACT_VERSION,
        id: ToolId::new(id),
        version: 1,
        display_name: display_name.to_string(),
        kind,
        prompt_version: format!("{id}/1"),
        instruction: instruction.to_string(),
        output_schema_version: format!("{id}.v1"),
        output_schema: schema,
        required_capabilities: RequiredCapabilities::default(),
        model_policy: ModelPolicy::default(),
        optional,
        origin: ToolOrigin::Builtin,
    }
}

/// JSON Schema fragment for an array of source anchors.
///
/// Every grounded item carries at least one anchor; `minItems: 1` makes an
/// uncited item a schema violation rather than something to notice later.
#[must_use]
pub fn anchors_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "minItems": 1,
        "maxItems": 8,
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["seq_index", "char_start", "char_end", "excerpt"],
            "properties": {
                "seq_index": { "type": "integer", "minimum": 0 },
                "char_start": { "type": "integer", "minimum": 0 },
                "char_end": { "type": "integer", "minimum": 1 },
                "excerpt": { "type": "string", "minLength": 1, "maxLength": 600 }
            }
        }
    })
}

/// The spec 9.7 default tool set.
#[must_use]
pub fn builtin_tools() -> Vec<TranscriptToolDefinition> {
    let anchors = anchors_schema();
    vec![
        definition(
            "meeting_summary",
            ToolKind::MeetingSummary,
            "Meeting summary",
            "Produce an executive summary of the meeting: objectives, outcomes and open \
             questions. Every statement must be supported by at least one anchor into the \
             transcript passages provided. Do not invent deadlines, owners, amounts or \
             commitments; use \"not_specified\" when the transcript does not say.",
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["objectives", "outcomes", "open_questions"],
                "properties": {
                    "objectives": grounded_list(&anchors, 0, 20),
                    "outcomes": grounded_list(&anchors, 0, 20),
                    "open_questions": grounded_list(&anchors, 0, 20)
                }
            }),
            false,
        ),
        definition(
            "action_items",
            ToolKind::ActionItems,
            "Action items",
            "Extract action items. Each item has a verb phrase, an owner (or \"Unassigned\"), \
             a due date (or \"not_specified\") and at least one anchor. Do not infer an owner \
             or a date that was not stated.",
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["items"],
                "properties": {
                    "items": {
                        "type": "array", "maxItems": 50,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["action", "owner", "due", "anchors"],
                            "properties": {
                                "action": { "type": "string", "minLength": 1, "maxLength": 400 },
                                "owner": { "type": "string", "minLength": 1, "maxLength": 120 },
                                "due": { "type": "string", "minLength": 1, "maxLength": 120 },
                                "anchors": anchors
                            }
                        }
                    }
                }
            }),
            false,
        ),
        definition(
            "decisions",
            ToolKind::Decisions,
            "Decisions",
            "Extract decisions that were actually stated. Each decision has a statement, the \
             participants named in the transcript (or an empty list) and at least one anchor. \
             A proposal that was not agreed is not a decision.",
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["decisions"],
                "properties": {
                    "decisions": {
                        "type": "array", "maxItems": 50,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["statement", "participants", "anchors"],
                            "properties": {
                                "statement": { "type": "string", "minLength": 1, "maxLength": 600 },
                                "participants": {
                                    "type": "array", "maxItems": 20,
                                    "items": { "type": "string", "minLength": 1, "maxLength": 120 }
                                },
                                "anchors": anchors
                            }
                        }
                    }
                }
            }),
            false,
        ),
        definition(
            "topic_outline",
            ToolKind::TopicOutline,
            "Topic outline",
            "Produce an ordered topic outline. Each heading covers a contiguous part of the \
             transcript and carries at least one anchor into it.",
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["topics"],
                "properties": { "topics": grounded_list(&anchors, 0, 40) }
            }),
            false,
        ),
        definition(
            "ask_recording",
            ToolKind::AskRecording,
            "Ask about this recording",
            "Answer the user's question using only the transcript passages provided. If the \
             passages do not contain the answer, set outcome to \"insufficient_evidence\". If \
             the question is about something this recording does not cover at all, set outcome \
             to \"not_supported_by_recording\". Never answer from general knowledge.",
            ask_recording_schema(&anchors),
            false,
        ),
        definition(
            "flashcards",
            ToolKind::Flashcards,
            "Study flashcards",
            "Produce study flashcards from the transcript passages. Each card has a front \
             (prompt), a back (answer) and at least one anchor to the passage it came from. \
             Do not produce a card whose answer is not stated in the passages.",
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["flashcards"],
                "properties": {
                    "flashcards": {
                        "type": "array", "maxItems": 100,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["front", "back", "anchors"],
                            "properties": {
                                "front": { "type": "string", "minLength": 1, "maxLength": 400 },
                                "back": { "type": "string", "minLength": 1, "maxLength": 800 },
                                "anchors": anchors
                            }
                        }
                    }
                }
            }),
            true,
        ),
        definition(
            "quiz",
            ToolKind::Quiz,
            "Study quiz",
            "Produce quiz questions with an answer key from the transcript passages. Each item \
             has a question, the correct answer, optional distractors and at least one anchor. \
             Every answer must be stated in the passages.",
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["quiz"],
                "properties": {
                    "quiz": {
                        "type": "array", "maxItems": 100,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["question", "answer", "anchors"],
                            "properties": {
                                "question": { "type": "string", "minLength": 1, "maxLength": 400 },
                                "answer": { "type": "string", "minLength": 1, "maxLength": 800 },
                                "distractors": {
                                    "type": "array", "maxItems": 5,
                                    "items": { "type": "string", "minLength": 1, "maxLength": 400 }
                                },
                                "anchors": anchors
                            }
                        }
                    }
                }
            }),
            true,
        ),
        definition(
            "definitions",
            ToolKind::Definitions,
            "Important definitions",
            "Extract important terms and the definition the transcript gives for each. Only \
             include a term whose meaning the transcript actually states.",
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["definitions"],
                "properties": {
                    "definitions": {
                        "type": "array", "maxItems": 100,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["term", "definition", "anchors"],
                            "properties": {
                                "term": { "type": "string", "minLength": 1, "maxLength": 200 },
                                "definition": { "type": "string", "minLength": 1, "maxLength": 800 },
                                "anchors": anchors
                            }
                        }
                    }
                }
            }),
            true,
        ),
    ]
}

/// Schema for the `ask_recording` response.
#[must_use]
pub fn ask_recording_schema(anchors: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["outcome", "answer", "anchors"],
        "properties": {
            "outcome": {
                "type": "string",
                "enum": ["answered", "insufficient_evidence", "not_supported_by_recording"]
            },
            "answer": { "type": "string", "maxLength": 4000 },
            "anchors": {
                "type": "array",
                "maxItems": 8,
                "items": anchors.get("items").cloned().unwrap_or(serde_json::Value::Bool(true))
            }
        }
    })
}

fn grounded_list(anchors: &serde_json::Value, min: u32, max: u32) -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "minItems": min,
        "maxItems": max,
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["text", "anchors"],
            "properties": {
                "text": { "type": "string", "minLength": 1, "maxLength": 1000 },
                "anchors": anchors
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_set_is_exactly_the_specified_tools() {
        let registry = ToolRegistry::with_defaults();
        let ids: Vec<String> = registry
            .latest_all()
            .iter()
            .map(|t| t.id.to_string())
            .collect();
        assert_eq!(
            ids,
            vec![
                "action_items",
                "ask_recording",
                "decisions",
                "definitions",
                "flashcards",
                "meeting_summary",
                "quiz",
                "topic_outline",
            ]
        );
    }

    #[test]
    fn study_tools_are_optional_and_default_meeting_tools_are_not() {
        let registry = ToolRegistry::with_defaults();
        for id in ["flashcards", "quiz", "definitions"] {
            assert!(registry.latest(&ToolId::new(id)).unwrap().optional, "{id}");
        }
        for id in [
            "meeting_summary",
            "action_items",
            "decisions",
            "topic_outline",
        ] {
            assert!(!registry.latest(&ToolId::new(id)).unwrap().optional, "{id}");
        }
    }

    #[test]
    fn applying_an_override_appends_a_version_and_keeps_the_old_one_resolvable() {
        let mut registry = ToolRegistry::with_defaults();
        let id = ToolId::new("meeting_summary");
        let original_hash = registry.latest(&id).unwrap().definition_sha256();

        let new_version = registry
            .apply_override(&ToolConfigOverride {
                tool_id: "meeting_summary".to_string(),
                instruction: "Summarize in German.".to_string(),
                prompt_version: "meeting_summary/de-1".to_string(),
                output_schema: None,
                output_schema_version: None,
            })
            .unwrap()
            .version;

        assert_eq!(new_version, 2);
        assert_eq!(
            registry.get(&id, 1).unwrap().definition_sha256(),
            original_hash
        );
        assert_eq!(registry.latest(&id).unwrap().version, 2);
        assert_eq!(registry.get(&id, 1).unwrap().origin, ToolOrigin::Builtin);
        assert_eq!(registry.get(&id, 2).unwrap().origin, ToolOrigin::User);
    }

    #[test]
    fn an_override_naming_an_unknown_tool_is_refused() {
        let mut registry = ToolRegistry::with_defaults();
        let err = registry
            .apply_override(&ToolConfigOverride {
                tool_id: "does_not_exist".to_string(),
                instruction: "x".to_string(),
                prompt_version: "v1".to_string(),
                output_schema: None,
                output_schema_version: None,
            })
            .unwrap_err();
        assert!(matches!(err, ToolError::ConfigurationInvalid(_)));
    }

    #[test]
    fn a_schema_change_without_a_schema_version_is_refused() {
        let mut registry = ToolRegistry::with_defaults();
        let err = registry
            .apply_override(&ToolConfigOverride {
                tool_id: "quiz".to_string(),
                instruction: "x".to_string(),
                prompt_version: "v2".to_string(),
                output_schema: Some(serde_json::json!({"type": "object"})),
                output_schema_version: None,
            })
            .unwrap_err();
        assert!(matches!(err, ToolError::ConfigurationInvalid(_)));
    }

    #[test]
    fn an_empty_instruction_is_refused() {
        let mut registry = ToolRegistry::with_defaults();
        assert!(registry
            .apply_override(&ToolConfigOverride {
                tool_id: "quiz".to_string(),
                instruction: "   ".to_string(),
                prompt_version: "v2".to_string(),
                output_schema: None,
                output_schema_version: None,
            })
            .is_err());
    }

    #[test]
    fn overrides_load_from_a_directory_and_a_missing_directory_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ToolRegistry::with_defaults();
        assert_eq!(
            registry
                .load_overrides_from(&dir.path().join("absent"))
                .unwrap(),
            0
        );

        std::fs::write(
            dir.path().join("a.json"),
            serde_json::to_string(&ToolConfigOverride {
                tool_id: "quiz".to_string(),
                instruction: "Ask harder questions.".to_string(),
                prompt_version: "quiz/custom-1".to_string(),
                output_schema: None,
                output_schema_version: None,
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(registry.load_overrides_from(dir.path()).unwrap(), 1);
        assert_eq!(registry.latest(&ToolId::new("quiz")).unwrap().version, 2);
    }

    #[test]
    fn a_malformed_override_file_is_an_error_not_a_silent_skip() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.json"), "{ not json").unwrap();
        let mut registry = ToolRegistry::with_defaults();
        assert!(registry.load_overrides_from(dir.path()).is_err());
    }

    #[test]
    fn every_generative_builtin_schema_demands_at_least_one_anchor_per_item() {
        let registry = ToolRegistry::with_defaults();
        for tool in registry.latest_all() {
            // `ask_recording` is the one tool whose anchor list may legally be
            // empty, because an empty list is how the model reports
            // `insufficient_evidence`. That outcome is enforced in `ask.rs`,
            // which refuses an `answered` response with no verified anchor.
            if tool.kind == ToolKind::AskRecording {
                continue;
            }
            let text = tool.output_schema.to_string();
            assert!(
                text.contains("\"minItems\":1"),
                "{} has no anchor cardinality floor",
                tool.id
            );
        }
    }

    #[test]
    fn the_ask_schema_allows_an_empty_anchor_list_only_as_a_shape() {
        let registry = ToolRegistry::with_defaults();
        let tool = registry.latest(&ToolId::new("ask_recording")).unwrap();
        let anchors = &tool.output_schema["properties"]["anchors"];
        assert_eq!(anchors["type"], "array");
        assert!(anchors.get("minItems").is_none());
    }
}
