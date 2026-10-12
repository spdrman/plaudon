//! The mandatory HTTP chokepoint (D1.1, spec §16.2A).
//!
//! `crates/security` deliberately contains **no** HTTP implementation. Instead
//! it owns the only type that an HTTP transport is allowed to accept:
//! [`super::EgressTicket`]. A transport implements [`HttpTransport`], whose
//! single method requires a ticket, so a subsystem physically cannot issue a
//! request without first passing [`super::OutboundPolicy::authorize`].
//!
//! Concretely: `crates/ai-openrouter`, the model manager and the update checker
//! own `reqwest` (or whichever client), but their `send` entry point takes an
//! `&EgressTicket` they cannot construct.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::{
    Destination, EgressDenied, EgressPurpose, EgressRequest, EgressTicket, OutboundPolicy,
    PayloadClass, Subsystem,
};

/// HTTP methods we permit. There is no generic "any method" escape hatch and no
/// generic HTTP proxy command (§15.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
        }
    }
}

/// An authorized request: the ticket plus the parts a transport needs.
///
/// The full URL (including any signed query string) is held here and is never
/// rendered by `Debug`; see the manual impl below (§16.2 "Signed URL leak").
pub struct GatedRequest {
    ticket: EgressTicket,
    method: Method,
    /// Kept out of `Debug`/logs. Only the transport dereferences it.
    url: String,
    headers: BTreeMap<String, String>,
    body: Option<Vec<u8>>,
}

impl std::fmt::Debug for GatedRequest {
    /// Renders the redacted destination label only, never the URL path, query
    /// string, headers or body.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatedRequest")
            .field("method", &self.method.as_str())
            .field("destination", &self.ticket.destination_label())
            .field("subsystem", &self.ticket.subsystem().label())
            .field("purpose", &self.ticket.purpose().label())
            .field("payload", &self.ticket.payload().label())
            .field("url", &"<redacted>")
            .field(
                "headers",
                &format_args!("<{} redacted>", self.headers.len()),
            )
            .field("body_bytes", &self.body.as_ref().map_or(0, Vec::len))
            .finish()
    }
}

impl GatedRequest {
    #[must_use]
    pub fn ticket(&self) -> &EgressTicket {
        &self.ticket
    }

    #[must_use]
    pub fn method(&self) -> Method {
        self.method
    }

    /// The full URL. Callers must not log it.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    #[must_use]
    pub fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
    }

    #[must_use]
    pub fn body(&self) -> Option<&[u8]> {
        self.body.as_deref()
    }

    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    #[must_use]
    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = Some(body);
        self
    }
}

/// A minimal HTTP response shape. Transports map their own types into it.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

/// Transport error that is not an egress-policy denial.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport failed: {0}")]
    Failed(String),
    #[error("transport timed out")]
    Timeout,
    #[error("too many redirects")]
    TooManyRedirects,
}

/// Implemented by the real HTTP client crates. The `&GatedRequest` argument can
/// only be produced by [`GatedHttpClientFactory`], which requires the policy.
pub trait HttpTransport: Send + Sync {
    fn send(&self, request: &GatedRequest) -> Result<HttpResponse, TransportError>;
}

/// The only way to build an outbound HTTP request in this application.
#[derive(Debug, Clone)]
pub struct GatedHttpClientFactory {
    policy: Arc<OutboundPolicy>,
}

impl GatedHttpClientFactory {
    #[must_use]
    pub fn new(policy: Arc<OutboundPolicy>) -> Self {
        Self { policy }
    }

    #[must_use]
    pub fn policy(&self) -> &OutboundPolicy {
        &self.policy
    }

    /// Authorize a GET.
    pub fn get(
        &self,
        url: &str,
        subsystem: Subsystem,
        purpose: EgressPurpose,
        payload: PayloadClass,
    ) -> Result<GatedRequest, EgressDenied> {
        self.request(Method::Get, url, subsystem, purpose, payload)
    }

    /// Authorize a POST.
    pub fn post(
        &self,
        url: &str,
        subsystem: Subsystem,
        purpose: EgressPurpose,
        payload: PayloadClass,
    ) -> Result<GatedRequest, EgressDenied> {
        self.request(Method::Post, url, subsystem, purpose, payload)
    }

    fn request(
        &self,
        method: Method,
        url: &str,
        subsystem: Subsystem,
        purpose: EgressPurpose,
        payload: PayloadClass,
    ) -> Result<GatedRequest, EgressDenied> {
        let destination = Destination::url(url).map_err(|mut denied| {
            // Re-attribute the parse failure to the calling subsystem so the
            // audit trail names the right owner.
            denied.subsystem = subsystem;
            self.policy.observer().record(super::EgressRecord {
                subsystem,
                destination_label: denied.destination_label.clone(),
                purpose,
                payload,
                allowed: false,
                deny_reason: Some(denied.reason),
            });
            denied
        })?;

        let request = EgressRequest::new(subsystem, destination, purpose, payload);
        let ticket = self.policy.authorize(&request)?;

        Ok(GatedRequest {
            ticket,
            method,
            url: url.to_owned(),
            headers: BTreeMap::new(),
            body: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbound::{ApprovalSource, DenyReason, NetworkGrant};

    fn permitted() -> Arc<OutboundPolicy> {
        Arc::new(OutboundPolicy::network_permitted(NetworkGrant::new(
            ApprovalSource::TestHarness,
            0,
            "privacy-v1",
        )))
    }

    #[test]
    fn local_only_denies_every_http_request() {
        let factory = GatedHttpClientFactory::new(Arc::new(OutboundPolicy::local_only()));
        let err = factory
            .post(
                "https://openrouter.ai/api/v1/chat/completions",
                Subsystem::OpenRouter,
                EgressPurpose::InferenceDispatch,
                PayloadClass::AudioBytes,
            )
            .unwrap_err();
        assert_eq!(err.reason, DenyReason::LocalOnlyMode);
    }

    #[test]
    fn permitted_mode_allows_allowlisted_host() {
        let factory = GatedHttpClientFactory::new(permitted());
        let req = factory
            .get(
                "https://openrouter.ai/api/v1/models",
                Subsystem::OpenRouter,
                EgressPurpose::ModelCatalog,
                PayloadClass::None,
            )
            .expect("allowlisted host should be permitted");
        assert_eq!(req.ticket().subsystem(), Subsystem::OpenRouter);
        assert_eq!(req.method(), Method::Get);
    }

    #[test]
    fn debug_never_renders_url_headers_or_body() {
        let factory = GatedHttpClientFactory::new(permitted());
        let req = factory
            .post(
                "https://openrouter.ai/api/v1/audio/transcriptions?sig=SECRETSIGNATURE",
                Subsystem::OpenRouter,
                EgressPurpose::InferenceDispatch,
                PayloadClass::AudioBytes,
            )
            .unwrap()
            .with_header("Authorization", "Bearer sk-or-v1-0123456789abcdef")
            .with_body(b"secret-audio-bytes".to_vec());

        let rendered = format!("{req:?}");
        assert!(!rendered.contains("SECRETSIGNATURE"), "{rendered}");
        assert!(!rendered.contains("sk-or-v1"), "{rendered}");
        assert!(!rendered.contains("secret-audio-bytes"), "{rendered}");
        assert!(rendered.contains("https://openrouter.ai:443"));
    }

    #[test]
    fn malformed_url_is_recorded_against_calling_subsystem() {
        let policy = Arc::new(OutboundPolicy::network_permitted(NetworkGrant::new(
            ApprovalSource::TestHarness,
            0,
            "privacy-v1",
        )));
        let factory = GatedHttpClientFactory::new(Arc::clone(&policy));
        let err = factory
            .get(
                "http://openrouter.ai/api/v1/models",
                Subsystem::OpenRouter,
                EgressPurpose::ModelCatalog,
                PayloadClass::None,
            )
            .unwrap_err();
        assert_eq!(err.reason, DenyReason::SchemeNotHttps);
        assert_eq!(err.subsystem, Subsystem::OpenRouter);

        let records = policy.observer().records();
        assert_eq!(records.len(), 1);
        assert!(!records[0].allowed);
        assert_eq!(records[0].subsystem, Subsystem::OpenRouter);
    }
}
