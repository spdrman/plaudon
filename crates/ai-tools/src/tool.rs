//! The versioned `TranscriptTool` contract (spec 9.7).
//!
//! A definition is immutable. Editing a prompt or an output schema produces a
//! new version, which is why `tool_runs` stores `(tool_id, tool_version)`: a
//! past run keeps pointing at the exact definition that produced it, and a
//! later edit cannot rewrite history.
//!
//! Capability matching is fail-closed. Every requirement is checked against a
//! [`Tri`] or [`Known`] capability, and `Unknown` never satisfies anything
//! (spec 8.3).

use std::fmt;

use plaudon_ai_contracts::capabilities::{PrivacyClass, TextModelCapabilities};
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::ToolError;

/// Contract version of the tool definition payload (spec 6.6).
pub const TOOL_CONTRACT_VERSION: u32 = 1;

/// Stable identity of a tool, independent of its version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolId(String);

impl ToolId {
    /// Wrap a tool identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a tool produces, used to route rendering and export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Executive meeting summary.
    MeetingSummary,
    /// Action items with owners and due dates.
    ActionItems,
    /// Decisions with evidence.
    Decisions,
    /// Topic outline with segment ranges.
    TopicOutline,
    /// Grounded question answering over one recording.
    AskRecording,
    /// Study flashcards.
    Flashcards,
    /// Study quiz with an answer key.
    Quiz,
    /// Important definitions.
    Definitions,
}

impl ToolKind {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MeetingSummary => "meeting_summary",
            Self::ActionItems => "action_items",
            Self::Decisions => "decisions",
            Self::TopicOutline => "topic_outline",
            Self::AskRecording => "ask_recording",
            Self::Flashcards => "flashcards",
            Self::Quiz => "quiz",
            Self::Definitions => "definitions",
        }
    }

    /// Whether this kind is one of the spec 9.2 default meeting outputs.
    ///
    /// Study artifacts are deliberately not default outputs: spec 9.3 requires
    /// them to stay optional so they do not obscure the meeting product.
    #[must_use]
    pub const fn is_default_meeting_output(self) -> bool {
        matches!(
            self,
            Self::MeetingSummary | Self::ActionItems | Self::Decisions | Self::TopicOutline
        )
    }
}

/// What a model must be able to do before a tool may run on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredCapabilities {
    /// Whether schema-constrained output is mandatory for this tool.
    pub structured_output: bool,
    /// Minimum verified context length, in tokens.
    pub min_context_tokens: u64,
    /// Output budget the tool needs, in tokens.
    pub min_output_tokens: u64,
    /// Whether the tool needs a cancellable engine.
    pub cancellable: bool,
}

impl Default for RequiredCapabilities {
    fn default() -> Self {
        Self {
            structured_output: true,
            min_context_tokens: 8_192,
            min_output_tokens: 1_024,
            cancellable: true,
        }
    }
}

/// Which routes a tool is allowed to use (spec 9.7, 10.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPolicy {
    /// Privacy classes the tool accepts. `Unresolved` is never acceptable.
    pub allowed_privacy_classes: Vec<PrivacyClass>,
    /// Whether the tool may run at all when the model is not `Verified`.
    pub allow_unverified_models: bool,
}

impl Default for ModelPolicy {
    fn default() -> Self {
        Self {
            allowed_privacy_classes: vec![
                PrivacyClass::OnDevice,
                PrivacyClass::CloudZeroRetention,
                PrivacyClass::CloudRetaining,
            ],
            allow_unverified_models: false,
        }
    }
}

impl ModelPolicy {
    /// A policy that refuses to leave the machine.
    #[must_use]
    pub fn on_device_only() -> Self {
        Self {
            allowed_privacy_classes: vec![PrivacyClass::OnDevice],
            allow_unverified_models: false,
        }
    }
}

/// Where a definition came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOrigin {
    /// Shipped with the application.
    Builtin,
    /// Created by the user by copying and editing a definition.
    User,
}

impl ToolOrigin {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::User => "user",
        }
    }
}

/// Why a model cannot run a tool. Returned instead of a bare boolean so the UI
/// can explain the refusal rather than hiding an option without a reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "gap", rename_all = "snake_case")]
pub enum CapabilityGap {
    /// The model was not validated for this configuration.
    ModelNotVerified {
        /// Observed status.
        status: CapabilityStatus,
    },
    /// Schema-constrained output is required but not available.
    StructuredOutputUnavailable,
    /// The engine cannot be cancelled.
    NotCancellable,
    /// Context length is unknown or too small.
    ContextTooSmall {
        /// Tokens the tool needs.
        required: u64,
        /// Tokens observed, when the value was observed at all.
        observed: Option<u64>,
    },
    /// Output budget is unknown or too small.
    OutputBudgetTooSmall {
        /// Tokens the tool needs.
        required: u64,
        /// Tokens observed, when the value was observed at all.
        observed: Option<u64>,
    },
    /// The route's privacy class is outside the tool's policy.
    PrivacyClassNotAllowed {
        /// Observed class, when it was resolved at all.
        observed: Option<PrivacyClass>,
    },
}

/// An immutable tool definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptToolDefinition {
    /// Contract version of this payload.
    pub payload_version: u32,
    /// Stable identity.
    pub id: ToolId,
    /// Monotonic definition version, starting at 1.
    pub version: u32,
    /// Name shown in the tool picker.
    pub display_name: String,
    /// What the tool produces.
    pub kind: ToolKind,
    /// Version of the prompt text below.
    pub prompt_version: String,
    /// Instruction text. Transcript content is never interpolated into this;
    /// it is fenced separately by [`crate::prompt`].
    pub instruction: String,
    /// Version of `output_schema`.
    pub output_schema_version: String,
    /// JSON Schema the model response must satisfy.
    pub output_schema: serde_json::Value,
    /// Capabilities a model must have.
    pub required_capabilities: RequiredCapabilities,
    /// Routes the tool accepts.
    pub model_policy: ModelPolicy,
    /// Whether this tool is an opt-in extra rather than a default output.
    pub optional: bool,
    /// Where the definition came from.
    pub origin: ToolOrigin,
}

impl TranscriptToolDefinition {
    /// SHA-256 over the canonical serialization of the whole definition.
    ///
    /// Two definitions with the same hash are the same definition; a prompt or
    /// schema edit changes the hash, which is why a run stores it.
    ///
    /// # Panics
    ///
    /// Panics only if the definition cannot be serialized, which cannot happen
    /// for the types in this struct.
    #[must_use]
    pub fn definition_sha256(&self) -> String {
        let canonical = canonical_json(
            &serde_json::to_value(self).expect("a tool definition is always serializable"),
        );
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Check a model against this tool's requirements, fail-closed.
    pub fn check_capabilities(
        &self,
        caps: &TextModelCapabilities,
    ) -> Result<(), Vec<CapabilityGap>> {
        let mut gaps = Vec::new();

        if !self.model_policy.allow_unverified_models && caps.status != CapabilityStatus::Supported
        {
            gaps.push(CapabilityGap::ModelNotVerified {
                status: caps.status,
            });
        }
        if self.required_capabilities.structured_output
            && !caps.supports_structured_output.satisfies()
        {
            gaps.push(CapabilityGap::StructuredOutputUnavailable);
        }
        if self.required_capabilities.cancellable && !caps.can_cancel.satisfies() {
            gaps.push(CapabilityGap::NotCancellable);
        }

        let observed_context = caps.max_context_tokens.get().copied();
        if observed_context.is_none_or(|v| v < self.required_capabilities.min_context_tokens) {
            gaps.push(CapabilityGap::ContextTooSmall {
                required: self.required_capabilities.min_context_tokens,
                observed: observed_context,
            });
        }

        let observed_output = caps.max_output_tokens.get().copied();
        if observed_output.is_none_or(|v| v < self.required_capabilities.min_output_tokens) {
            gaps.push(CapabilityGap::OutputBudgetTooSmall {
                required: self.required_capabilities.min_output_tokens,
                observed: observed_output,
            });
        }

        let observed_privacy = caps.privacy_class.get().copied();
        let privacy_ok = observed_privacy.is_some_and(|class| {
            class.is_dispatchable() && self.model_policy.allowed_privacy_classes.contains(&class)
        });
        if !privacy_ok {
            gaps.push(CapabilityGap::PrivacyClassNotAllowed {
                observed: observed_privacy,
            });
        }

        if gaps.is_empty() {
            Ok(())
        } else {
            Err(gaps)
        }
    }

    /// Whether this tool may be offered for the given model.
    #[must_use]
    pub fn is_offerable(&self, caps: &TextModelCapabilities) -> bool {
        self.check_capabilities(caps).is_ok()
    }

    /// Fail-closed entry point used before dispatch.
    pub fn ensure_runnable(&self, caps: &TextModelCapabilities) -> Result<(), ToolError> {
        self.check_capabilities(caps)
            .map_err(|_| ToolError::CapabilityUnmet)
    }

    /// Whether this route needs explicit cloud consent before dispatch.
    #[must_use]
    pub fn requires_cloud_consent(caps: &TextModelCapabilities) -> bool {
        match caps.privacy_class {
            Known::Known(class) => class.requires_cloud_consent(),
            // Unresolved routing is never dispatched at all, but if a caller
            // asks, the conservative answer is "yes, ask first".
            Known::Unknown => true,
        }
    }

    /// Produce the next version of this definition with an edited prompt and
    /// schema. The receiver is left untouched, so prior runs keep resolving.
    #[must_use]
    pub fn edited(
        &self,
        instruction: String,
        prompt_version: String,
        output_schema: serde_json::Value,
        output_schema_version: String,
    ) -> Self {
        Self {
            version: self.version + 1,
            instruction,
            prompt_version,
            output_schema,
            output_schema_version,
            origin: ToolOrigin::User,
            ..self.clone()
        }
    }
}

/// Serialize a JSON value with object keys in sorted order.
///
/// `serde_json::Value` already stores maps in a `BTreeMap` by default, but the
/// hash contract must not depend on that build choice, so ordering is applied
/// explicitly here.
fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut parts: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}:{}",
                        serde_json::Value::String(k.clone()),
                        canonical_json(v)
                    )
                })
                .collect();
            parts.sort();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

/// Tri-state helper kept here so `Tri` stays a domain concern.
#[must_use]
pub const fn tri_satisfied(value: Tri) -> bool {
    value.satisfies()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ToolRegistry;
    use plaudon_domain::model::{EngineId, ModelIdentity};

    fn capable() -> TextModelCapabilities {
        TextModelCapabilities {
            payload_version: 1,
            status: CapabilityStatus::Supported,
            engine_id: EngineId::new("openrouter"),
            model: ModelIdentity::new(EngineId::new("openrouter"), "test/model"),
            offline: Tri::No,
            max_context_tokens: Known::Known(128_000),
            max_output_tokens: Known::Known(8_192),
            supports_structured_output: Tri::Yes,
            can_cancel: Tri::Yes,
            privacy_class: Known::Known(PrivacyClass::CloudZeroRetention),
            pricing_basis: Known::Unknown,
            availability_reason: plaudon_ai_contracts::capabilities::AvailabilityReason::Available,
        }
    }

    fn summary_tool() -> TranscriptToolDefinition {
        ToolRegistry::with_defaults()
            .latest(&ToolId::new("meeting_summary"))
            .expect("builtin tool")
            .clone()
    }

    #[test]
    fn a_capable_verified_model_satisfies_a_default_tool() {
        assert!(summary_tool().check_capabilities(&capable()).is_ok());
    }

    #[test]
    fn unknown_capabilities_never_satisfy_a_requirement() {
        let mut caps = capable();
        caps.supports_structured_output = Tri::Unknown;
        caps.max_context_tokens = Known::Unknown;
        let gaps = summary_tool().check_capabilities(&caps).unwrap_err();
        assert!(gaps.contains(&CapabilityGap::StructuredOutputUnavailable));
        assert!(gaps
            .iter()
            .any(|g| matches!(g, CapabilityGap::ContextTooSmall { observed: None, .. })));
    }

    #[test]
    fn an_unresolved_privacy_route_is_never_allowed() {
        let mut caps = capable();
        caps.privacy_class = Known::Known(PrivacyClass::Unresolved);
        let gaps = summary_tool().check_capabilities(&caps).unwrap_err();
        assert!(gaps
            .iter()
            .any(|g| matches!(g, CapabilityGap::PrivacyClassNotAllowed { .. })));
    }

    #[test]
    fn an_unverified_model_is_refused_unless_the_policy_allows_it() {
        let mut caps = capable();
        caps.status = CapabilityStatus::Unknown;
        let tool = summary_tool();
        assert!(tool.check_capabilities(&caps).is_err());
        assert_eq!(tool.ensure_runnable(&caps), Err(ToolError::CapabilityUnmet));
    }

    #[test]
    fn an_on_device_only_tool_refuses_a_cloud_route() {
        let mut tool = summary_tool();
        tool.model_policy = ModelPolicy::on_device_only();
        assert!(tool.check_capabilities(&capable()).is_err());
    }

    #[test]
    fn editing_a_definition_bumps_the_version_and_leaves_the_original_intact() {
        let original = summary_tool();
        let edited = original.edited(
            "Different instruction".to_string(),
            "custom/1".to_string(),
            original.output_schema.clone(),
            original.output_schema_version.clone(),
        );
        assert_eq!(edited.version, original.version + 1);
        assert_eq!(edited.origin, ToolOrigin::User);
        assert_eq!(original.instruction, summary_tool().instruction);
        assert_ne!(original.definition_sha256(), edited.definition_sha256());
    }

    #[test]
    fn the_definition_hash_is_stable_across_repeated_serialization() {
        let tool = summary_tool();
        assert_eq!(tool.definition_sha256(), tool.clone().definition_sha256());
        assert_eq!(tool.definition_sha256().len(), 64);
    }

    #[test]
    fn study_kinds_are_not_default_meeting_outputs() {
        for kind in [ToolKind::Flashcards, ToolKind::Quiz, ToolKind::Definitions] {
            assert!(!kind.is_default_meeting_output());
        }
        for kind in [
            ToolKind::MeetingSummary,
            ToolKind::ActionItems,
            ToolKind::Decisions,
            ToolKind::TopicOutline,
        ] {
            assert!(kind.is_default_meeting_output());
        }
    }

    #[test]
    fn an_unresolved_route_is_treated_as_requiring_consent() {
        let mut caps = capable();
        caps.privacy_class = Known::Unknown;
        assert!(TranscriptToolDefinition::requires_cloud_consent(&caps));
        caps.privacy_class = Known::Known(PrivacyClass::OnDevice);
        assert!(!TranscriptToolDefinition::requires_cloud_consent(&caps));
    }

    #[test]
    fn tri_helper_matches_the_domain_rule() {
        assert!(tri_satisfied(Tri::Yes));
        assert!(!tri_satisfied(Tri::Unknown));
    }
}
