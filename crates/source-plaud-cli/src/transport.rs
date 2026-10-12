//! Streaming HTTPS transport for signed download URLs (spec 7.3).
//!
//! The redirect policy is `none`, deliberately. Letting the HTTP client follow
//! redirects would mean the allowlist only ever sees the first URL, and a
//! redirect is precisely how a signed-URL service is turned into a
//! server-side request forgery primitive. Instead each hop is surfaced, run
//! through [`DownloadPolicy::authorize`], and only then followed.
//!
//! Nothing here logs a URL. A signed URL is a bearer credential, so it is
//! carried in locals, never in a tracing field, never in an error payload and
//! never in a `Display` implementation.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use plaudon_recording_sources::source::SourceError;

use crate::download::{DownloadPolicy, DownloadRejection};

/// Why a transfer failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// The policy refused a URL or one of its redirect hops.
    #[error("the download address was refused by policy")]
    Refused(#[from] DownloadRejection),
    /// The connection could not be established or was lost mid-transfer.
    #[error("the connection to the download host failed")]
    Connection,
    /// The request exceeded its wall-clock budget.
    #[error("the download timed out")]
    Timeout,
    /// The server answered with a status we treat as terminal.
    #[error("the download host answered with status {status}")]
    Status {
        /// HTTP status code.
        status: u16,
    },
    /// A redirect response carried no usable `Location`.
    #[error("the download host sent a redirect we could not follow")]
    MalformedRedirect,
    /// The body exceeded the configured cap.
    #[error("the download exceeded the configured size limit")]
    BodyTooLarge {
        /// The cap that was exceeded.
        limit_bytes: u64,
    },
    /// The body stopped before the declared length arrived.
    #[error("the download stopped before all bytes arrived")]
    Incomplete,
}

impl From<TransportError> for SourceError {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::Refused(rejection) => Self::from(rejection),
            TransportError::Connection | TransportError::Timeout => Self::DownloadInterrupted,
            TransportError::Status { status } => match status {
                401 => Self::AuthenticationRequired,
                // A signed URL that has expired is answered with 403 by every
                // signer we know of; 410 is the explicit "gone" variant.
                403 | 410 => Self::LinkExpired,
                404 => Self::AssetNotFound,
                429 => Self::RemoteRateLimited,
                _ => Self::SourceUnavailable,
            },
            TransportError::MalformedRedirect => Self::SourceUnavailable,
            TransportError::BodyTooLarge { .. } | TransportError::Incomplete => {
                Self::DownloadInterrupted
            }
        }
    }
}

/// Response metadata, with the body still unread.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ResponseHead {
    /// Declared content type, lowercased, parameters stripped.
    pub content_type: Option<String>,
    /// Declared content length.
    pub content_length: Option<u64>,
    /// Strong validator (`ETag`), when present.
    pub etag: Option<String>,
    /// Whether the server advertised byte ranges.
    pub accepts_ranges: bool,
    /// Number of redirect hops followed to reach this response.
    pub redirect_hops: u8,
}

/// An open response body.
#[async_trait]
pub trait ByteStream: Send + fmt::Debug {
    /// Next chunk of the body, or `None` at end of stream.
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, TransportError>;
}

/// A fetched response: head plus an unread body.
#[derive(Debug)]
pub struct FetchedBody {
    /// Response metadata.
    pub head: ResponseHead,
    /// The body stream.
    pub stream: Box<dyn ByteStream>,
}

/// Fetches a URL under policy.
#[async_trait]
pub trait HttpFetcher: Send + Sync + fmt::Debug {
    /// Issue a `GET` and return the final response after authorized redirects.
    async fn get(&self, url: &str) -> Result<FetchedBody, TransportError>;
}

/// The real transport.
#[derive(Debug, Clone)]
pub struct ReqwestFetcher {
    client: reqwest::Client,
    policy: Arc<DownloadPolicy>,
}

/// How long a single request may take to produce response headers.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the transport will wait between body chunks.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

impl ReqwestFetcher {
    /// Build a fetcher bound to a download policy.
    ///
    /// # Errors
    /// Fails only if the platform TLS stack cannot be initialized.
    pub fn new(policy: Arc<DownloadPolicy>) -> Result<Self, TransportError> {
        let client = reqwest::Client::builder()
            // Manual redirect handling: see the module docs.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            // Signed URLs are the credential; the client is built without the
            // cookie feature at all, so there is no jar to disable, and the
            // referer header is suppressed so one host never learns another's
            // URL.
            .referer(false)
            .user_agent(concat!("plaudon/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| TransportError::Connection)?;
        Ok(Self { client, policy })
    }

    /// The policy this fetcher enforces.
    #[must_use]
    pub fn policy(&self) -> &DownloadPolicy {
        &self.policy
    }
}

#[async_trait]
impl HttpFetcher for ReqwestFetcher {
    async fn get(&self, url: &str) -> Result<FetchedBody, TransportError> {
        let mut current = url.to_owned();
        let mut hops: u8 = 0;
        loop {
            self.policy.authorize(&current)?;
            let response = self.client.get(&current).send().await.map_err(|error| {
                if error.is_timeout() {
                    TransportError::Timeout
                } else {
                    TransportError::Connection
                }
            })?;
            let status = response.status();
            if status.is_redirection() {
                hops = hops.saturating_add(1);
                if hops > self.policy.max_redirects {
                    return Err(TransportError::Refused(
                        DownloadRejection::TooManyRedirects {
                            limit: self.policy.max_redirects,
                        },
                    ));
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(TransportError::MalformedRedirect)?;
                // Resolve relative redirects against the current URL, then
                // re-authorize from scratch on the next iteration.
                current = url::Url::parse(&current)
                    .and_then(|base| base.join(location))
                    .map_err(|_| TransportError::MalformedRedirect)?
                    .to_string();
                tracing::debug!(
                    target: "plaudon::plaud_cli",
                    hop = hops,
                    "following an authorized redirect"
                );
                continue;
            }
            if !status.is_success() {
                return Err(TransportError::Status {
                    status: status.as_u16(),
                });
            }

            let headers = response.headers();
            let head = ResponseHead {
                content_type: headers
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase()),
                content_length: response.content_length(),
                etag: headers
                    .get(reqwest::header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
                accepts_ranges: headers
                    .get(reqwest::header::ACCEPT_RANGES)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.eq_ignore_ascii_case("bytes")),
                redirect_hops: hops,
            };
            return Ok(FetchedBody {
                head,
                stream: Box::new(ReqwestStream { response }),
            });
        }
    }
}

/// Body stream over a `reqwest` response.
#[derive(Debug)]
struct ReqwestStream {
    response: reqwest::Response,
}

#[async_trait]
impl ByteStream for ReqwestStream {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        match self.response.chunk().await {
            Ok(Some(bytes)) => Ok(Some(bytes.to_vec())),
            Ok(None) => Ok(None),
            Err(error) if error.is_timeout() => Err(TransportError::Timeout),
            Err(_) => Err(TransportError::Connection),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_map_onto_the_stable_error_set() {
        for (status, expected) in [
            (401, SourceError::AuthenticationRequired),
            (403, SourceError::LinkExpired),
            (410, SourceError::LinkExpired),
            (404, SourceError::AssetNotFound),
            (429, SourceError::RemoteRateLimited),
            (500, SourceError::SourceUnavailable),
            (503, SourceError::SourceUnavailable),
            (418, SourceError::SourceUnavailable),
        ] {
            assert_eq!(
                SourceError::from(TransportError::Status { status }),
                expected,
                "status {status}"
            );
        }
    }

    #[test]
    fn transport_failures_map_onto_the_stable_error_set() {
        assert_eq!(
            SourceError::from(TransportError::Connection),
            SourceError::DownloadInterrupted
        );
        assert_eq!(
            SourceError::from(TransportError::Timeout),
            SourceError::DownloadInterrupted
        );
        assert_eq!(
            SourceError::from(TransportError::Incomplete),
            SourceError::DownloadInterrupted
        );
        assert_eq!(
            SourceError::from(TransportError::BodyTooLarge { limit_bytes: 1 }),
            SourceError::DownloadInterrupted
        );
        assert_eq!(
            SourceError::from(TransportError::MalformedRedirect),
            SourceError::SourceUnavailable
        );
    }

    #[test]
    fn a_policy_rejection_keeps_its_own_meaning() {
        assert_eq!(
            SourceError::from(TransportError::Refused(DownloadRejection::PrivateAddress)),
            SourceError::SourceUnavailable
        );
        assert_eq!(
            SourceError::from(TransportError::Refused(
                DownloadRejection::UnexpectedContentType {
                    declared: "text/html".to_owned()
                }
            )),
            SourceError::IntegrityFailed
        );
    }

    #[test]
    fn no_transport_error_renders_a_url() {
        // Spec 7.3: the signed URL is never logged. Every message here is
        // constant text, so no formatting path can interpolate one.
        for error in [
            TransportError::Connection,
            TransportError::Timeout,
            TransportError::Status { status: 403 },
            TransportError::MalformedRedirect,
            TransportError::Incomplete,
            TransportError::BodyTooLarge { limit_bytes: 1 },
            TransportError::Refused(DownloadRejection::HostNotAllowed),
        ] {
            let rendered = error.to_string();
            assert!(!rendered.contains("http"), "{rendered}");
            assert!(!rendered.contains("://"), "{rendered}");
        }
    }

    #[tokio::test]
    async fn the_real_fetcher_refuses_a_disallowed_host_before_connecting() {
        let fetcher =
            ReqwestFetcher::new(Arc::new(DownloadPolicy::default())).expect("build fetcher");
        let error = fetcher
            .get("https://cdn.example-vendor.com/a.wav")
            .await
            .unwrap_err();
        assert_eq!(
            error,
            TransportError::Refused(DownloadRejection::HostNotAllowed)
        );
    }

    #[tokio::test]
    async fn the_real_fetcher_refuses_a_loopback_destination() {
        let fetcher = ReqwestFetcher::new(Arc::new(DownloadPolicy::with_hosts(&["127.0.0.1"])))
            .expect("build fetcher");
        let error = fetcher.get("https://127.0.0.1:9/a.wav").await.unwrap_err();
        assert_eq!(
            error,
            TransportError::Refused(DownloadRejection::PrivateAddress)
        );
    }

    #[tokio::test]
    async fn the_real_fetcher_refuses_a_scheme_downgrade() {
        let fetcher = ReqwestFetcher::new(Arc::new(DownloadPolicy::with_hosts(&[
            "cdn.example-vendor.com",
        ])))
        .expect("build fetcher");
        let error = fetcher
            .get("http://cdn.example-vendor.com/a.wav")
            .await
            .unwrap_err();
        assert_eq!(
            error,
            TransportError::Refused(DownloadRejection::SchemeNotHttps)
        );
    }
}
