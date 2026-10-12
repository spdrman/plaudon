//! The HTTPS transport the typed clients use (spec 10.3, 10.6, 16.2A).
//!
//! # Why an async trait instead of [`plaudon_security::outbound::http::HttpTransport`]
//!
//! The security crate's trait is synchronous and returns a response or a flat
//! failure. The OpenRouter clients run inside async job stages and must be able
//! to tell a failure that happened **before** any byte left the machine from
//! one that happened after, because only the former is safe to retry
//! (spec 10.7). [`AsyncHttpTransport`] therefore takes the same
//! [`GatedRequest`] — which can only be minted by
//! [`plaudon_security::outbound::http::GatedHttpClientFactory`], so the egress
//! gate is still the only way to reach the network — and returns the richer
//! [`TransportFailure`].

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use plaudon_security::outbound::http::{GatedRequest, HttpResponse, Method};

/// Maximum redirect hops we will follow before refusing (spec 10.3 bounded
/// redirects).
pub const MAX_REDIRECTS: u8 = 2;

/// Default per-request timeout. Chosen so a long multipart upload can finish
/// while a dead connection still fails in bounded time.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// A transport-level failure, carrying the one fact billing safety depends on:
/// whether any byte of the request body reached the network.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportFailure {
    /// The connection could not be established. Nothing was transmitted.
    #[error("the connection could not be established")]
    ConnectFailed {
        /// Non-secret reason label. Never a URL or a header value.
        detail: String,
    },
    /// The request timed out before the body was written.
    #[error("the request timed out before anything was sent")]
    TimedOutBeforeSend,
    /// The request timed out after the body had started to transmit, so the
    /// provider may already have accepted and billed it.
    #[error("the request timed out after transmission began")]
    TimedOutAfterSend,
    /// The connection closed while the response was still arriving.
    #[error("the response was truncated")]
    ResponseTruncated,
    /// A redirect left the authorized destination, or there were too many.
    #[error("the provider redirected to an unauthorized destination")]
    UnsafeRedirect {
        /// Redacted destination label of the refused hop.
        destination_label: String,
    },
    /// More hops than [`MAX_REDIRECTS`].
    #[error("the provider redirected too many times")]
    TooManyRedirects,
    /// Any other failure after transmission began.
    #[error("the request failed after transmission began")]
    FailedAfterSend {
        /// Non-secret reason label.
        detail: String,
    },
}

impl TransportFailure {
    /// Whether any byte of the request may have reached the provider.
    ///
    /// Drives [`crate::error::OpenRouterError::OutcomeUnknown`]: once this is
    /// true, nothing is retried without the user's explicit acknowledgement.
    #[must_use]
    pub const fn bytes_transmitted(&self) -> bool {
        match self {
            TransportFailure::ConnectFailed { .. }
            | TransportFailure::TimedOutBeforeSend
            | TransportFailure::UnsafeRedirect { .. } => false,
            TransportFailure::TimedOutAfterSend
            | TransportFailure::ResponseTruncated
            | TransportFailure::TooManyRedirects
            | TransportFailure::FailedAfterSend { .. } => true,
        }
    }
}

/// An async HTTPS transport that can only be driven with an authorized request.
#[async_trait]
pub trait AsyncHttpTransport: Send + Sync + fmt::Debug {
    /// Send one authorized request.
    ///
    /// Implementations must not log the URL, headers or body, must not follow
    /// a redirect that changes scheme, host or port, and must not retry
    /// internally: retry policy lives in [`crate::retry`].
    async fn send(&self, request: GatedRequest) -> Result<HttpResponse, TransportFailure>;
}

/// Whether a redirect target stays inside the authorized destination.
///
/// The [`plaudon_security::outbound::EgressTicket`] authorized exactly one
/// scheme/host/port. A redirect to anything else would need a second
/// authorization that the transport cannot mint, so it is refused rather than
/// followed (spec 16.2 SSRF row).
#[must_use]
pub fn redirect_is_authorized(original: &str, location: &str) -> bool {
    let Ok(from) = url::Url::parse(original) else {
        return false;
    };
    let Ok(to) = url::Url::parse(location).or_else(|_| from.join(location)) else {
        return false;
    };
    to.scheme() == "https"
        && from.scheme() == "https"
        && to.host_str() == from.host_str()
        && to.port_or_known_default() == from.port_or_known_default()
        && to.username().is_empty()
        && to.password().is_none()
}

/// Redacted label for a destination, for the refusal message.
#[must_use]
pub fn redacted_label(location: &str) -> String {
    match url::Url::parse(location) {
        Ok(parsed) => format!(
            "{}://{}",
            parsed.scheme(),
            parsed.host_str().unwrap_or("<no-host>")
        ),
        Err(_) => "<unparseable-location>".to_owned(),
    }
}

/// The real HTTPS transport.
#[cfg(feature = "reqwest-transport")]
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

#[cfg(feature = "reqwest-transport")]
impl ReqwestTransport {
    /// Build a transport with the default timeout.
    ///
    /// # Errors
    ///
    /// Returns the client-construction error when the TLS backend cannot be
    /// initialised.
    pub fn new() -> Result<Self, reqwest::Error> {
        Self::with_timeout(DEFAULT_TIMEOUT)
    }

    /// Build a transport with an explicit timeout.
    ///
    /// # Errors
    ///
    /// Returns the client-construction error when the TLS backend cannot be
    /// initialised.
    pub fn with_timeout(timeout: Duration) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            // Redirects are inspected by us, never followed by the client:
            // the egress ticket authorized one destination only.
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .timeout(timeout)
            .build()?;
        Ok(Self { client })
    }

    async fn send_once(
        &self,
        method: Method,
        url: &str,
        headers: &std::collections::BTreeMap<String, String>,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, TransportFailure> {
        let mut builder = match method {
            Method::Get => self.client.get(url),
            Method::Post => self.client.post(url),
        };
        for (name, value) in headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(bytes) = body {
            builder = builder.body(bytes.to_vec());
        }

        let response = builder.send().await.map_err(|error| {
            // `is_connect` and `is_timeout` are the only two reqwest predicates
            // that tell us whether the body was written.
            if error.is_connect() {
                TransportFailure::ConnectFailed {
                    detail: "connect failed".to_owned(),
                }
            } else if error.is_timeout() {
                if body.is_some() {
                    TransportFailure::TimedOutAfterSend
                } else {
                    TransportFailure::TimedOutBeforeSend
                }
            } else if error.is_body() || error.is_decode() {
                TransportFailure::ResponseTruncated
            } else {
                TransportFailure::FailedAfterSend {
                    detail: "transport error".to_owned(),
                }
            }
        })?;

        let status = response.status().as_u16();
        let mut response_headers = std::collections::BTreeMap::new();
        for (name, value) in response.headers() {
            if let Ok(text) = value.to_str() {
                response_headers.insert(name.as_str().to_ascii_lowercase(), text.to_owned());
            }
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| TransportFailure::ResponseTruncated)?;
        Ok(HttpResponse {
            status,
            headers: response_headers,
            body: bytes.to_vec(),
        })
    }
}

#[cfg(feature = "reqwest-transport")]
#[async_trait]
impl AsyncHttpTransport for ReqwestTransport {
    async fn send(&self, request: GatedRequest) -> Result<HttpResponse, TransportFailure> {
        let mut url = request.url().to_owned();
        let headers = request.headers().clone();
        let body = request.body().map(<[u8]>::to_vec);

        for _ in 0..=MAX_REDIRECTS {
            let response = self
                .send_once(request.method(), &url, &headers, body.as_deref())
                .await?;
            if !(300..400).contains(&response.status) {
                return Ok(response);
            }
            let Some(location) = response.headers.get("location") else {
                return Ok(response);
            };
            if !redirect_is_authorized(&url, location) {
                return Err(TransportFailure::UnsafeRedirect {
                    destination_label: redacted_label(location),
                });
            }
            url = url::Url::parse(&url)
                .ok()
                .and_then(|base| base.join(location).ok())
                .map(String::from)
                .ok_or_else(|| TransportFailure::UnsafeRedirect {
                    destination_label: redacted_label(location),
                })?;
        }
        Err(TransportFailure::TooManyRedirects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_same_origin_https_redirect_is_authorized() {
        assert!(redirect_is_authorized(
            "https://openrouter.ai/api/v1/audio/transcriptions",
            "https://openrouter.ai/api/v1/audio/transcriptions/"
        ));
        assert!(redirect_is_authorized(
            "https://openrouter.ai/api/v1/chat/completions",
            "/api/v1/chat/completions/"
        ));
        // A relative target that is not a URL at all still resolves against
        // the authorized origin, so it stays inside it.
        assert!(redirect_is_authorized(
            "https://openrouter.ai/api/v1/models",
            "models/"
        ));
    }

    #[test]
    fn leaving_the_authorized_destination_is_refused() {
        for location in [
            "https://evil.example/api",
            "http://openrouter.ai/api/v1",
            "https://openrouter.ai:8443/api/v1",
            "https://user:pass@openrouter.ai/api/v1",
            "//evil.example/api",
        ] {
            assert!(
                !redirect_is_authorized("https://openrouter.ai/api/v1/models", location),
                "{location} must be refused"
            );
        }
    }

    #[test]
    fn the_refusal_label_carries_no_path_or_query() {
        let label = redacted_label("https://evil.example/steal?token=abc");
        assert_eq!(label, "https://evil.example");
    }

    #[test]
    fn only_pre_transmission_failures_report_no_bytes_sent() {
        assert!(!TransportFailure::TimedOutBeforeSend.bytes_transmitted());
        assert!(!TransportFailure::ConnectFailed {
            detail: String::new()
        }
        .bytes_transmitted());
        assert!(TransportFailure::TimedOutAfterSend.bytes_transmitted());
        assert!(TransportFailure::ResponseTruncated.bytes_transmitted());
    }
}
