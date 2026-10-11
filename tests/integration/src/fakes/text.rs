//! A deterministic in-memory [`TextModel`].

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::TextModelCapabilities;
use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::text::{
    StructuredRequest, StructuredResponse, TextModel, TextModelError,
};

/// A text model that returns a canned body.
#[derive(Debug)]
pub struct FakeTextModel {
    capabilities: TextModelCapabilities,
    body: serde_json::Value,
    failure: Option<TextModelError>,
}

impl FakeTextModel {
    /// Create a model that returns `body`.
    #[must_use]
    pub fn new(capabilities: TextModelCapabilities, body: serde_json::Value) -> Self {
        Self {
            capabilities,
            body,
            failure: None,
        }
    }

    /// Create a model that always fails with `error`.
    #[must_use]
    pub fn failing(capabilities: TextModelCapabilities, error: TextModelError) -> Self {
        Self {
            capabilities,
            body: serde_json::Value::Null,
            failure: Some(error),
        }
    }
}

#[async_trait]
impl TextModel for FakeTextModel {
    async fn capabilities(&self) -> TextModelCapabilities {
        self.capabilities.clone()
    }

    async fn generate_structured(
        &self,
        _request: StructuredRequest,
        ctx: JobContext,
    ) -> Result<StructuredResponse, TextModelError> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if ctx.is_cancelled() {
            return Err(TextModelError::Cancelled);
        }
        Ok(StructuredResponse {
            body: self.body.clone(),
            repair_attempts: 0,
            usage: None,
        })
    }
}
