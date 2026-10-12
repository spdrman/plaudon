//! The signed-download URL contract and its SSRF policy (spec 7.3).
//!
//! Spec 2.1 says `plaud audio <id>` yields a temporary URL that expires in
//! roughly 24 hours, and spec 2.6 requires us to record the scheme, method,
//! redirect chain, expiry behaviour, host allowlist, content type, checksum
//! availability and partial-download behaviour before relying on any of it.
//! This module is the enforcement half of that contract: it decides whether a
//! URL may be fetched at all, and it never stores or logs the URL itself.
//!
//! Resume is opt-in on *observed evidence only*. Spec 7.3 forbids resuming
//! unless the server's range semantics and validators have been verified, so
//! [`RangeSupport::Unknown`] and [`RangeSupport::NotSupported`] both restart
//! from zero.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use plaudon_recording_sources::source::SourceError;
use serde::{Deserialize, Serialize};

/// Why a URL was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DownloadRejection {
    /// The URL could not be parsed.
    #[error("the download link is not a usable URL")]
    Unparseable,
    /// The scheme is not `https`.
    #[error("the download link does not use https")]
    SchemeNotHttps,
    /// The host is not on the vendor allowlist.
    #[error("the download link points at a host we do not recognize")]
    HostNotAllowed,
    /// The host resolves to, or literally is, a private or loopback address.
    #[error("the download link points at a private or local network address")]
    PrivateAddress,
    /// The redirect chain exceeded the configured cap.
    #[error("the download link redirected too many times")]
    TooManyRedirects {
        /// Configured cap.
        limit: u8,
    },
    /// The body exceeded the configured cap.
    #[error("the download exceeded the configured size limit")]
    BodyTooLarge {
        /// Configured cap in bytes.
        limit_bytes: u64,
    },
    /// The response content type is not audio.
    #[error("the download returned an unexpected content type")]
    UnexpectedContentType {
        /// Content type the server declared.
        declared: String,
    },
    /// The URL carries a credential in its userinfo component.
    #[error("the download link embeds a credential")]
    EmbeddedCredential,
}

impl From<DownloadRejection> for SourceError {
    fn from(rejection: DownloadRejection) -> Self {
        match rejection {
            DownloadRejection::BodyTooLarge { .. } => Self::DownloadInterrupted,
            DownloadRejection::UnexpectedContentType { .. } => Self::IntegrityFailed,
            _ => Self::SourceUnavailable,
        }
    }
}

/// Whether resumable download is permitted for an observed host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeSupport {
    /// `Range` requests returned `206` **and** a strong validator was present.
    VerifiedWithStrongValidator,
    /// Range requests were tried and are not usable.
    NotSupported,
    /// Range behaviour has not been observed.
    #[serde(other)]
    Unknown,
}

impl RangeSupport {
    /// Whether an interrupted transfer may resume rather than restart.
    #[must_use]
    pub const fn may_resume(self) -> bool {
        matches!(self, Self::VerifiedWithStrongValidator)
    }
}

/// The observed URL contract for one vendor host (spec 2.6, 7.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlContract {
    /// Host the contract was observed on.
    pub host: String,
    /// Scheme observed. Always `https` for a usable contract.
    pub scheme: String,
    /// HTTP method used to fetch the asset.
    pub method: String,
    /// Number of redirect hops observed, if any.
    pub observed_redirect_hops: Option<u8>,
    /// Content type observed on the final response.
    pub observed_content_type: Option<String>,
    /// Whether a `Content-Length` was present.
    pub content_length_present: Option<bool>,
    /// Validator header observed, when present (`ETag`, `Last-Modified`).
    pub observed_validator: Option<String>,
    /// Checksum header observed, when present.
    pub observed_checksum_header: Option<String>,
    /// Approximate lifetime of the link, in seconds, as documented or observed.
    pub expiry_seconds: Option<u64>,
    /// Range and resume behaviour.
    pub range_support: RangeSupport,
    /// Date the contract was observed, `YYYY-MM-DD`, or `None` if never.
    pub observed_on: Option<String>,
}

impl UrlContract {
    /// A contract that records only what vendor documentation states, with
    /// every runtime-observable field left unobserved.
    #[must_use]
    pub fn undocumented(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            scheme: "https".to_owned(),
            method: "GET".to_owned(),
            observed_redirect_hops: None,
            observed_content_type: None,
            content_length_present: None,
            observed_validator: None,
            observed_checksum_header: None,
            expiry_seconds: None,
            range_support: RangeSupport::Unknown,
            observed_on: None,
        }
    }

    /// Whether resume may be used against this host.
    #[must_use]
    pub const fn may_resume(&self) -> bool {
        self.range_support.may_resume()
    }
}

/// The fetch policy applied to every signed URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadPolicy {
    /// Hosts we will fetch from.
    pub allowed_hosts: Vec<String>,
    /// Maximum redirect hops.
    pub max_redirects: u8,
    /// Maximum body size.
    pub max_body_bytes: u64,
    /// Content types we accept on the final response.
    pub accepted_content_types: Vec<String>,
}

impl Default for DownloadPolicy {
    fn default() -> Self {
        Self {
            // Deliberately empty. No vendor download host has been observed by
            // this build, and spec 7.3 requires the allowlist to come from
            // "observed legitimate hosts" rather than from a guess. Until a
            // host is recorded in `UrlContract`, every download is refused.
            allowed_hosts: Vec::new(),
            max_redirects: 3,
            max_body_bytes: 8 * 1024 * 1024 * 1024,
            accepted_content_types: vec![
                "audio/wav".to_owned(),
                "audio/x-wav".to_owned(),
                "audio/mpeg".to_owned(),
                "audio/mp4".to_owned(),
                "audio/aac".to_owned(),
                "audio/ogg".to_owned(),
                "audio/flac".to_owned(),
                "application/octet-stream".to_owned(),
            ],
        }
    }
}

impl DownloadPolicy {
    /// A policy limited to the given observed hosts.
    #[must_use]
    pub fn with_hosts(hosts: &[&str]) -> Self {
        Self {
            allowed_hosts: hosts.iter().map(|h| (*h).to_ascii_lowercase()).collect(),
            ..Self::default()
        }
    }

    /// Check a URL, including every hop of an observed redirect chain.
    ///
    /// The chain is checked in full because a redirect is exactly how a
    /// server-side request forgery escapes an allowlist that only validates
    /// the first URL.
    pub fn authorize_chain(&self, chain: &[&str]) -> Result<(), DownloadRejection> {
        if chain.is_empty() {
            return Err(DownloadRejection::Unparseable);
        }
        let hops = u8::try_from(chain.len().saturating_sub(1)).unwrap_or(u8::MAX);
        if hops > self.max_redirects {
            return Err(DownloadRejection::TooManyRedirects {
                limit: self.max_redirects,
            });
        }
        for url in chain {
            self.authorize(url)?;
        }
        Ok(())
    }

    /// Check a single URL.
    pub fn authorize(&self, url: &str) -> Result<(), DownloadRejection> {
        // The scheme is checked before anything else, so a non-https URL is
        // always reported as a scheme problem rather than as a parse problem.
        let scheme = url
            .split_once(':')
            .map(|(scheme, _)| scheme.to_ascii_lowercase());
        match scheme.as_deref() {
            Some("https") => {}
            Some(_) => return Err(DownloadRejection::SchemeNotHttps),
            None => return Err(DownloadRejection::Unparseable),
        }
        let parsed = ParsedUrl::parse(url).ok_or(DownloadRejection::Unparseable)?;
        if parsed.scheme != "https" {
            return Err(DownloadRejection::SchemeNotHttps);
        }
        if parsed.has_userinfo {
            return Err(DownloadRejection::EmbeddedCredential);
        }
        if is_private_host(&parsed.host) {
            return Err(DownloadRejection::PrivateAddress);
        }
        if !self
            .allowed_hosts
            .iter()
            .any(|allowed| host_matches(allowed, &parsed.host))
        {
            return Err(DownloadRejection::HostNotAllowed);
        }
        Ok(())
    }

    /// Check a declared response size against the cap.
    pub fn authorize_size(&self, declared_bytes: u64) -> Result<(), DownloadRejection> {
        if declared_bytes > self.max_body_bytes {
            return Err(DownloadRejection::BodyTooLarge {
                limit_bytes: self.max_body_bytes,
            });
        }
        Ok(())
    }

    /// Check a declared content type against the accept list.
    pub fn authorize_content_type(&self, declared: &str) -> Result<(), DownloadRejection> {
        let base = declared
            .split(';')
            .next()
            .unwrap_or(declared)
            .trim()
            .to_ascii_lowercase();
        if self.accepted_content_types.contains(&base) {
            Ok(())
        } else {
            Err(DownloadRejection::UnexpectedContentType { declared: base })
        }
    }
}

/// Minimal URL decomposition.
///
/// A full URL crate is not needed here and would pull a dependency into a
/// policy module whose whole job is to be auditable.
struct ParsedUrl {
    scheme: String,
    host: String,
    has_userinfo: bool,
}

impl ParsedUrl {
    fn parse(url: &str) -> Option<Self> {
        if url.contains(char::is_whitespace) || url.contains('\0') {
            return None;
        }
        let (scheme, rest) = url.split_once("://")?;
        if scheme.is_empty() || rest.is_empty() {
            return None;
        }
        let authority = rest.split(['/', '?', '#']).next()?;
        if authority.is_empty() {
            return None;
        }
        let (has_userinfo, hostport) = match authority.rsplit_once('@') {
            Some((_, host)) => (true, host),
            None => (false, authority),
        };
        // An IPv6 literal keeps its brackets until the port is stripped.
        let host = if let Some(end) = hostport.strip_prefix('[').and_then(|r| r.find(']')) {
            &hostport[1..=end]
        } else {
            hostport.split(':').next()?
        };
        if host.is_empty() {
            return None;
        }
        Some(Self {
            scheme: scheme.to_ascii_lowercase(),
            host: host.trim_end_matches('.').to_ascii_lowercase(),
            has_userinfo,
        })
    }
}

fn host_matches(allowed: &str, host: &str) -> bool {
    if let Some(suffix) = allowed.strip_prefix("*.") {
        return host == suffix || host.ends_with(&format!(".{suffix}"));
    }
    allowed == host
}

/// Whether a host literal is private, loopback, link-local or otherwise local.
///
/// Only literals are classified here. DNS resolution happens in the network
/// layer, which applies the same predicate to every resolved address before
/// connecting.
#[must_use]
pub fn is_private_host(host: &str) -> bool {
    // Suffix comparison on a hostname, not on a file extension: the host is
    // already lowercased by the parser.
    let local_suffixes = [".localhost", ".local"];
    if host == "localhost" || local_suffixes.iter().any(|suffix| host.ends_with(suffix)) {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => is_private_v4(v4),
        Ok(IpAddr::V6(v6)) => is_private_v6(v6),
        Err(_) => false,
    }
}

fn is_private_v4(addr: Ipv4Addr) -> bool {
    addr.is_private()
        || addr.is_loopback()
        || addr.is_link_local()
        || addr.is_broadcast()
        || addr.is_documentation()
        || addr.is_unspecified()
        || addr.octets()[0] == 0
        // Carrier-grade NAT, 100.64.0.0/10.
        || (addr.octets()[0] == 100 && (64..=127).contains(&addr.octets()[1]))
        // Benchmarking, 198.18.0.0/15.
        || (addr.octets()[0] == 198 && (18..=19).contains(&addr.octets()[1]))
}

fn is_private_v6(addr: Ipv6Addr) -> bool {
    if addr.is_loopback() || addr.is_unspecified() {
        return true;
    }
    let segments = addr.segments();
    // Unique local fc00::/7 and link-local fe80::/10.
    if (segments[0] & 0xfe00) == 0xfc00 || (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // IPv4-mapped and IPv4-compatible addresses inherit the v4 verdict.
    if let Some(v4) = addr.to_ipv4() {
        return is_private_v4(v4);
    }
    false
}

impl fmt::Display for RangeSupport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::VerifiedWithStrongValidator => "verified_with_strong_validator",
            Self::NotSupported => "not_supported",
            Self::Unknown => "unknown",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> DownloadPolicy {
        DownloadPolicy::with_hosts(&["cdn.example-vendor.com", "*.assets.example-vendor.com"])
    }

    #[test]
    fn the_default_policy_allows_no_host_at_all() {
        let default = DownloadPolicy::default();
        assert_eq!(default.allowed_hosts.len(), 0);
        assert_eq!(
            default.authorize("https://cdn.example-vendor.com/a.wav"),
            Err(DownloadRejection::HostNotAllowed),
            "the allowlist must come from observed hosts, never from a guess"
        );
    }

    #[test]
    fn an_allowlisted_https_host_is_accepted() {
        policy()
            .authorize("https://cdn.example-vendor.com/audio/rec-1.wav?sig=abc")
            .unwrap();
        policy()
            .authorize("https://eu.assets.example-vendor.com/a.wav")
            .unwrap();
    }

    #[test]
    fn a_scheme_downgrade_is_refused() {
        for url in [
            "http://cdn.example-vendor.com/a.wav",
            "ftp://cdn.example-vendor.com/a.wav",
            "file:///etc/passwd",
            "gopher://cdn.example-vendor.com/a.wav",
        ] {
            assert_eq!(
                policy().authorize(url),
                Err(DownloadRejection::SchemeNotHttps),
                "{url}"
            );
        }
    }

    #[test]
    fn an_off_allowlist_host_is_refused_including_lookalikes() {
        for url in [
            "https://attacker.example.com/a.wav",
            "https://cdn.example-vendor.com.attacker.example/a.wav",
            "https://assets.example-vendor.com.evil/a.wav",
            "https://xcdn.example-vendor.com/a.wav",
        ] {
            assert_eq!(
                policy().authorize(url),
                Err(DownloadRejection::HostNotAllowed),
                "{url}"
            );
        }
    }

    #[test]
    fn private_loopback_and_link_local_targets_are_refused() {
        let permissive = DownloadPolicy {
            allowed_hosts: vec!["*.example-vendor.com".into()],
            ..DownloadPolicy::default()
        };
        for host in [
            "127.0.0.1",
            "0.0.0.0",
            "10.0.0.5",
            "172.16.3.4",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "localhost",
            "service.local",
            "[::1]",
            "[fe80::1]",
            "[fc00::1]",
            "[::ffff:127.0.0.1]",
        ] {
            let url = format!("https://{host}/a.wav");
            assert_eq!(
                permissive.authorize(&url),
                Err(DownloadRejection::PrivateAddress),
                "{url} must be refused"
            );
        }
    }

    #[test]
    fn a_url_embedding_a_credential_is_refused() {
        assert_eq!(
            policy().authorize("https://user:secret@cdn.example-vendor.com/a.wav"),
            Err(DownloadRejection::EmbeddedCredential)
        );
    }

    #[test]
    fn a_redirect_chain_is_validated_hop_by_hop() {
        let chain = [
            "https://cdn.example-vendor.com/a.wav",
            "https://eu.assets.example-vendor.com/a.wav",
        ];
        policy().authorize_chain(&chain).unwrap();

        let to_private = [
            "https://cdn.example-vendor.com/a.wav",
            "https://169.254.169.254/latest/meta-data",
        ];
        assert_eq!(
            policy().authorize_chain(&to_private),
            Err(DownloadRejection::PrivateAddress),
            "a redirect into link-local space must be refused"
        );

        let off_allowlist = [
            "https://cdn.example-vendor.com/a.wav",
            "https://attacker.example.com/a.wav",
        ];
        assert_eq!(
            policy().authorize_chain(&off_allowlist),
            Err(DownloadRejection::HostNotAllowed)
        );
    }

    #[test]
    fn a_redirect_loop_is_bounded() {
        let long: Vec<&str> =
            std::iter::repeat_n("https://cdn.example-vendor.com/a.wav", 9).collect();
        assert_eq!(
            policy().authorize_chain(&long),
            Err(DownloadRejection::TooManyRedirects { limit: 3 })
        );
    }

    #[test]
    fn oversized_bodies_and_wrong_content_types_are_refused() {
        let mut policy = policy();
        policy.max_body_bytes = 1_024;
        assert_eq!(
            policy.authorize_size(2_048),
            Err(DownloadRejection::BodyTooLarge { limit_bytes: 1_024 })
        );
        policy.authorize_size(1_024).unwrap();

        policy
            .authorize_content_type("audio/wav; charset=binary")
            .unwrap();
        assert_eq!(
            policy.authorize_content_type("text/html"),
            Err(DownloadRejection::UnexpectedContentType {
                declared: "text/html".to_owned()
            })
        );
    }

    #[test]
    fn malformed_urls_are_refused_rather_than_normalized() {
        for url in [
            "",
            "https://",
            "not a url",
            "https:// cdn.example-vendor.com/a",
            "://x",
        ] {
            assert!(policy().authorize(url).is_err(), "{url:?} must be refused");
        }
    }

    #[test]
    fn resume_requires_observed_range_semantics_and_a_validator() {
        let mut contract = UrlContract::undocumented("cdn.example-vendor.com");
        assert_eq!(contract.range_support, RangeSupport::Unknown);
        assert!(
            !contract.may_resume(),
            "unknown range behaviour must restart from zero"
        );

        contract.range_support = RangeSupport::NotSupported;
        assert!(!contract.may_resume());

        contract.range_support = RangeSupport::VerifiedWithStrongValidator;
        contract.observed_validator = Some("ETag".to_owned());
        assert!(contract.may_resume());

        let decoded: RangeSupport = serde_json::from_str("\"probably\"").unwrap();
        assert_eq!(decoded, RangeSupport::Unknown);
    }

    #[test]
    fn rejections_map_onto_stable_source_errors_without_vendor_text() {
        assert_eq!(
            SourceError::from(DownloadRejection::PrivateAddress),
            SourceError::SourceUnavailable
        );
        assert_eq!(
            SourceError::from(DownloadRejection::BodyTooLarge { limit_bytes: 1 }),
            SourceError::DownloadInterrupted
        );
        assert_eq!(
            SourceError::from(DownloadRejection::UnexpectedContentType {
                declared: "text/html".into()
            }),
            SourceError::IntegrityFailed
        );
        let rendered = DownloadRejection::HostNotAllowed.to_string();
        assert!(
            !rendered.contains("http"),
            "an error message must not echo a signed URL"
        );
    }
}
