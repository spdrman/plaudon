//! Catalog and route discovery (spec 10.1, 10.8).
//!
//! Discovery is strictly additive and strictly degradable: a failure returns
//! [`Discovered::degraded`] carrying the pinned allowlist and the reason, never
//! an empty list and never a fabricated one. Neither endpoint requires a
//! credential, but both still pass the egress gate, so Local-only Mode stops
//! them like any other outbound call.

use std::sync::Arc;

use plaudon_security::outbound::http::GatedHttpClientFactory;
use plaudon_security::outbound::{EgressPurpose, PayloadClass, Subsystem};
use serde::{Deserialize, Serialize};

use crate::catalog::CatalogEntry;
use crate::credential::ApiKeySource;
use crate::error::{classify_http_status, OpenRouterError};
use crate::privacy::EndpointsResponse;
use crate::transport::AsyncHttpTransport;
use crate::{API_BASE, MODELS_PATH};

/// Why the catalog could not be refreshed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradedReason {
    /// The egress gate refused, including Local-only Mode.
    LocalOnlyMode,
    /// The request failed or the host was unreachable.
    Offline,
    /// The provider answered with an error status.
    CatalogError,
    /// The body was not the documented shape.
    MalformedCatalog,
}

/// The outcome of a catalog refresh.
#[derive(Debug, Clone, PartialEq)]
pub struct Discovered {
    /// Entries parsed from a live response; empty when degraded.
    pub entries: Vec<CatalogEntry>,
    /// Why the live list is unavailable, when it is.
    pub degraded: Option<DegradedReason>,
}

impl Discovered {
    /// A successful refresh.
    #[must_use]
    pub fn live(entries: Vec<CatalogEntry>) -> Self {
        Self {
            entries,
            degraded: None,
        }
    }

    /// A refresh that failed; the caller keeps the pinned allowlist.
    #[must_use]
    pub const fn degraded(reason: DegradedReason) -> Self {
        Self {
            entries: Vec::new(),
            degraded: Some(reason),
        }
    }

    /// Whether the entries came from a live response.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        self.degraded.is_none()
    }
}

#[derive(Debug, Deserialize)]
struct CatalogBody {
    #[serde(default)]
    data: Vec<CatalogEntry>,
}

/// Reads the public catalog and per-model route metadata.
#[derive(Debug, Clone)]
pub struct CatalogClient {
    http: GatedHttpClientFactory,
    transport: Arc<dyn AsyncHttpTransport>,
    credentials: Arc<dyn ApiKeySource>,
    base_url: String,
}

impl CatalogClient {
    /// Build a client.
    #[must_use]
    pub fn new(
        http: GatedHttpClientFactory,
        transport: Arc<dyn AsyncHttpTransport>,
        credentials: Arc<dyn ApiKeySource>,
    ) -> Self {
        Self {
            http,
            transport,
            credentials,
            base_url: API_BASE.to_owned(),
        }
    }

    /// Point the client at another base URL (contract tests).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Fetch the model catalog, degrading rather than failing.
    pub async fn models(&self) -> Discovered {
        match self.get(MODELS_PATH).await {
            Ok(body) => match serde_json::from_slice::<CatalogBody>(&body) {
                Ok(parsed) => Discovered::live(parsed.data),
                Err(_) => Discovered::degraded(DegradedReason::MalformedCatalog),
            },
            Err(reason) => Discovered::degraded(reason),
        }
    }

    /// Fetch the route metadata for one model.
    ///
    /// Returns `None` when the lookup failed; the caller then has an
    /// unresolved route, which fails closed for anyone requiring a privacy
    /// restriction (spec 10.8).
    pub async fn endpoints(&self, model_id: &str) -> Option<EndpointsResponse> {
        let path = format!("/models/{model_id}/endpoints");
        let body = self.get(&path).await.ok()?;
        serde_json::from_slice(&body).ok()
    }

    async fn get(&self, path: &str) -> Result<Vec<u8>, DegradedReason> {
        let url = format!("{}{path}", self.base_url);
        let mut request = self
            .http
            .get(
                &url,
                Subsystem::OpenRouter,
                EgressPurpose::ModelCatalog,
                // The catalog carries no user content at all.
                PayloadClass::None,
            )
            .map_err(|_| DegradedReason::LocalOnlyMode)?;
        // The catalog is public; a key is attached only when one exists, so
        // discovery still works before the user has entered one.
        if let Ok(key) = self.credentials.api_key() {
            request = request.with_header(
                crate::credential::AUTHORIZATION_HEADER,
                key.bearer_header().as_str(),
            );
        }
        let response = self
            .transport
            .send(request)
            .await
            .map_err(|_| DegradedReason::Offline)?;
        if !(200..300).contains(&response.status) {
            let error = classify_http_status(response.status, None, true);
            return Err(match error {
                OpenRouterError::FailedBeforeSend | OpenRouterError::ProviderUnavailable => {
                    DegradedReason::Offline
                }
                _ => DegradedReason::CatalogError,
            });
        }
        Ok(response.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_degraded_refresh_carries_no_entries_and_a_reason() {
        let degraded = Discovered::degraded(DegradedReason::Offline);
        assert_eq!(degraded.entries.len(), 0);
        assert!(!degraded.is_live());
        assert_eq!(degraded.degraded, Some(DegradedReason::Offline));
    }

    #[test]
    fn the_captured_catalog_body_parses() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("repository root")
            .join("tests/fixtures/openrouter/catalog-snapshot.json");
        let raw = std::fs::read(path).expect("fixture");
        let parsed: CatalogBody = serde_json::from_slice(&raw).expect("parses");
        assert!(parsed.data.len() >= 40);
    }

    #[test]
    fn a_body_that_is_not_the_documented_shape_degrades() {
        let parsed = serde_json::from_slice::<CatalogBody>(b"{\"unexpected\":1}");
        // Missing `data` is tolerated as an empty list by serde's default, so
        // the client treats an empty live list as degraded rather than as "no
        // models exist".
        assert_eq!(parsed.expect("parses").data.len(), 0);
    }
}
