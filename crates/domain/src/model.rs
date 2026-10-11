//! Typed model and engine identities (spec 6.5).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Identity of a local or remote inference engine implementation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EngineId(String);

impl EngineId {
    /// Construct an engine identity.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identity string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EngineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identity of a specific model served by an engine.
///
/// Spec 10.2 forbids collapsing a provider-neutral preset into a bare string:
/// provenance needs the provider namespace, the model identifier and, when the
/// provider exposes one, a concrete version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ModelIdentity {
    /// Engine or gateway that serves the model.
    pub engine: EngineId,
    /// Provider-scoped model identifier, verbatim.
    pub model_id: String,
    /// Concrete version string when the provider exposes one.
    pub model_version: Option<String>,
}

impl ModelIdentity {
    /// Construct a model identity.
    #[must_use]
    pub fn new(engine: EngineId, model_id: impl Into<String>) -> Self {
        Self { engine, model_id: model_id.into(), model_version: None }
    }

    /// Attach a concrete version.
    #[must_use]
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.model_version = Some(version.into());
        self
    }
}

impl fmt::Display for ModelIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.model_version {
            Some(version) => write!(f, "{}/{}@{version}", self.engine, self.model_id),
            None => write!(f, "{}/{}", self.engine, self.model_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_identities_with_different_engines_are_distinct() {
        let a = ModelIdentity::new(EngineId::new("openrouter"), "whisper-large-v3");
        let b = ModelIdentity::new(EngineId::new("whisper-cpp"), "whisper-large-v3");
        assert_ne!(a, b);
    }

    #[test]
    fn versioned_identities_render_unambiguously() {
        let model = ModelIdentity::new(EngineId::new("openrouter"), "vendor/model").with_version("2026-01");
        assert_eq!(model.to_string(), "openrouter/vendor/model@2026-01");
    }
}
