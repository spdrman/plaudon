//! The single source of truth for secret-shaped patterns.
//!
//! Both the logging [`Redactor`](crate::redact::Redactor) and the
//! `plaudon-secret-scan` binary consume [`pattern_set()`]; there is exactly one
//! definition of "what a secret looks like" in the workspace so a sink and a CI
//! scan can never drift (spec §18.5, §1.5).
//!
//! Every pattern renders a *stable marker* instead of the matched bytes. A
//! marker keeps the shape of the record debuggable (you can see that a bearer
//! token was present, and for a URL which host was contacted) while destroying
//! the credential itself (spec §15.4, §16.2 "Signed URL leak").

use std::fmt;
use std::sync::LazyLock;

use regex::{Captures, Regex};

/// Whether a pattern may be matched on the separator-stripped ("compacted")
/// view of a record, which is how a secret split across two adjacent fields is
/// still caught. Shapes that depend on punctuation (URLs, `Authorization:`
/// headers, PEM blocks) are [`SplitTolerance::Contiguous`] because compaction
/// destroys the very punctuation they key on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitTolerance {
    /// Only matched on the literal (percent-decoded) text.
    Contiguous,
    /// Also matched after separator characters are removed.
    Splittable,
}

/// Whether a shape is also a *finding* when it appears in a committed file.
///
/// Every pattern is redacted out of a log line. Not every pattern means
/// "a secret is in this file": the VP-112 catch-alls match any long opaque
/// run of bytes or any long numeric vector, which is the right rule for a
/// record on its way to a log and the wrong one for a device fixture full of
/// base64 audio frames. Those shapes are [`ScanScope::RedactionOnly`]; the
/// scanner reports the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanScope {
    /// Redacted from text **and** reported by `plaudon-secret-scan`.
    RedactAndScan,
    /// Redacted from text only.
    RedactionOnly,
}

/// A rendered replacement: the pattern name that produced it and the marker
/// text that replaces the matched span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub pattern: &'static str,
    pub marker: String,
}

impl Rendered {
    fn new(pattern: &'static str, marker: impl Into<String>) -> Self {
        Self {
            pattern,
            marker: marker.into(),
        }
    }
}

/// One secret shape: a regex plus the renderer that turns a match into a marker.
///
/// A renderer may decline a match by returning `None` (used by the URL pattern,
/// which only redacts URLs that actually carry credential-ish parameters), and
/// may attribute the match to one of [`Pattern::extra_names`] so closely
/// related shapes share a regex but stay separately reportable/allowlistable.
pub struct Pattern {
    name: &'static str,
    description: &'static str,
    regex: Regex,
    /// Variant used on the separator-stripped view. It drops the leading word
    /// boundary, because after compaction a secret is glued to the end of the
    /// preceding field name (`part_a="sk-…"` becomes `part_ask-…`). The
    /// redactor compensates by requiring the match to start at the first byte
    /// of a retained run, i.e. exactly where a separator used to be.
    compact: Option<Regex>,
    render: fn(&Captures<'_>) -> Option<Rendered>,
    split: SplitTolerance,
    extra_names: &'static [&'static str],
    /// Whether a match in a committed file is a scanner finding as well as
    /// something the sink redacts.
    scan: ScanScope,
}

impl fmt::Debug for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pattern")
            .field("name", &self.name)
            .field("split", &self.split)
            .field("extra_names", &self.extra_names)
            .finish_non_exhaustive()
    }
}

impl Pattern {
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Human-readable reason this shape is treated as a secret. Used by the
    /// scanner's JSON output so a CI failure explains itself.
    #[must_use]
    pub const fn description(&self) -> &'static str {
        self.description
    }

    #[must_use]
    pub const fn split_tolerance(&self) -> SplitTolerance {
        self.split
    }

    #[must_use]
    pub const fn regex(&self) -> &Regex {
        &self.regex
    }

    /// Regex to use on the compacted view, if this shape may be split.
    #[must_use]
    pub const fn compact_regex(&self) -> Option<&Regex> {
        self.compact.as_ref()
    }

    /// Render a capture into a marker, or `None` if this occurrence is benign.
    #[must_use]
    pub fn render(&self, caps: &Captures<'_>) -> Option<Rendered> {
        (self.render)(caps)
    }

    /// Whether the scanner reports this shape, or only the redactor acts on it.
    #[must_use]
    pub const fn scan_scope(&self) -> ScanScope {
        self.scan
    }

    /// All names this pattern can report, including [`Pattern::name`].
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        std::iter::once(self.name).chain(self.extra_names.iter().copied())
    }
}

// ---------------------------------------------------------------------------
// Pattern names (stable: they appear in scanner output and allowlists)
// ---------------------------------------------------------------------------

pub const PRIVATE_KEY: &str = "private_key_pem";
pub const SIGNED_URL: &str = "signed_url";
pub const URL_USERINFO: &str = "url_userinfo";
pub const OPENROUTER_KEY: &str = "openrouter_key";
pub const API_KEY: &str = "api_key";
pub const AUTHORIZATION: &str = "authorization_header";
pub const BEARER: &str = "bearer_token";
pub const JWT: &str = "jwt";
pub const CREDENTIAL_PARAM: &str = "credential_param";
pub const DEVICE_TOKEN: &str = "device_token_hex";
pub const VOICE_EMBEDDING: &str = "voice_embedding";
pub const BINARY_BLOB: &str = "binary_blob";
pub const FLOAT_VECTOR: &str = "float_vector";

/// Name reported for a value registered through
/// [`Redactor::register_secret`](crate::redact::Redactor::register_secret).
pub const REGISTERED: &str = "registered_secret";

/// Name reported for a display name registered through
/// [`register_profile_name`](crate::redact::register_profile_name).
pub const PROFILE_NAME: &str = "profile_name";

/// Every marker this crate can emit. A test asserts the renderers stay inside
/// this vocabulary so downstream log parsing has a closed set.
pub const MARKER_PREFIXES: &[&str] = &[
    "<redacted:private_key>",
    "<redacted:signed_url",
    "<redacted:url_userinfo",
    "<redacted:openrouter_key>",
    "<redacted:api_key>",
    "<redacted:bearer>",
    "<redacted:jwt>",
    "<redacted:credential>",
    "<redacted:device_token>",
    "<redacted:voice_embedding>",
    "<redacted:binary_blob>",
    "<redacted:float_vector>",
    "<redacted:profile_name>",
    "<redacted:registered>",
];

// ---------------------------------------------------------------------------
// Signed-URL classification
// ---------------------------------------------------------------------------

/// Query parameter names that, alone, make a URL a credentialed URL.
///
/// Covers the AWS `SigV4` / `CloudFront` / generic set named in the task and
/// spec §16.2 ("Signed URL leak ... URL scrubber").
const CREDENTIAL_QUERY_PARAMS: &[&str] = &[
    "sig",
    "signature",
    "x-amz-signature",
    "x-amz-credential",
    "x-amz-security-token",
    "token",
    "access_token",
    "accesstoken",
    "id_token",
    "refresh_token",
    "key",
    "apikey",
    "api_key",
    "policy",
    "credential",
    "password",
    "auth",
    "authorization",
];

/// Azure SAS: `se` (expiry), `sp` (permissions) and `sv` (service version) are
/// individually innocuous, so they only count together. `sig` is already in the
/// single-parameter set above.
const AZURE_SAS_SET: &[&str] = &["se", "sp", "sv"];

/// `CloudFront` canned policies use `Expires` together with `Signature`;
/// `Signature` is already singly sufficient, so `Expires` only counts when it
/// is paired with a key pair id.
const CLOUDFRONT_PAIR: &[&str] = &["expires", "key-pair-id"];

fn query_is_credentialed(query: &str) -> bool {
    let mut names = Vec::new();
    for pair in query.split(['&', ';']) {
        let name = pair
            .split('=')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !name.is_empty() {
            names.push(name);
        }
    }
    if names
        .iter()
        .any(|n| CREDENTIAL_QUERY_PARAMS.contains(&n.as_str()))
    {
        return true;
    }
    if AZURE_SAS_SET.iter().all(|p| names.iter().any(|n| n == p)) {
        return true;
    }
    CLOUDFRONT_PAIR.iter().all(|p| names.iter().any(|n| n == p))
}

/// Render a URL match: destroy everything except the host.
///
/// Returns `None` for ordinary URLs (`https://openrouter.ai/api/v1/models`) so
/// that logs stay useful; only userinfo and credentialed query strings are
/// scrubbed.
fn render_url(caps: &Captures<'_>) -> Option<Rendered> {
    let raw = caps.get(0)?.as_str();
    if let Ok(parsed) = url::Url::parse(raw) {
        let host = parsed.host_str().unwrap_or("unknown");
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Some(Rendered::new(
                URL_USERINFO,
                format!("<redacted:url_userinfo host={host}>"),
            ));
        }
        if query_is_credentialed(parsed.query().unwrap_or("")) {
            return Some(Rendered::new(
                SIGNED_URL,
                format!("<redacted:signed_url host={host}>"),
            ));
        }
        return None;
    }

    // Unparseable but credential-shaped: fail closed, drop the lot.
    let (_, after) = raw.split_once("://")?;
    if after.contains('@') {
        return Some(Rendered::new(
            URL_USERINFO,
            "<redacted:url_userinfo host=unknown>",
        ));
    }
    let query = after.split_once('?').map_or("", |(_, q)| q);
    if query_is_credentialed(query) {
        return Some(Rendered::new(
            SIGNED_URL,
            "<redacted:signed_url host=unknown>",
        ));
    }
    None
}

// ---------------------------------------------------------------------------
// The pattern set
// ---------------------------------------------------------------------------

/// Characters that may not appear inside a URL token in a log line.
const URL_BODY: &str = r#"[^\s"'<>\\^`{}|]+"#;

/// Shortest continuous base64/hex run treated as an opaque blob (VP-112).
///
/// Chosen well below any embedding this product can produce and well above the
/// identifiers a log line legitimately carries. The smallest speaker embedding
/// in the supported model tier is 128 float32 components — 512 bytes, 684
/// base64 characters — and its sealed form is longer still, while a UUID (36),
/// a ULID (26), a SHA-256 hex digest (64) and a rotated log file name all stay
/// far below 160. A run this long in a log line is opaque payload whatever it
/// is, so destroying it costs nothing a reader of the log could have used.
pub const MIN_BLOB_CHARS: usize = 160;

/// One flat, ordered table. It is long by nature and splitting it would hide
/// the ordering, which is part of the semantics.
#[allow(clippy::too_many_lines)]
fn build() -> Vec<Pattern> {
    // Order matters: broader, structure-bearing shapes first. Overlapping
    // matches are resolved first-wins by the redactor, so a URL claims its
    // whole span before the parameter-level patterns can nibble at it.
    let mut patterns = Vec::new();

    patterns.push(Pattern {
        name: PRIVATE_KEY,
        description: "PEM-encoded private key block",
        regex: regex(
            r"(?is)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        ),
        compact: None,
        render: |_| Some(Rendered::new(PRIVATE_KEY, "<redacted:private_key>")),
        split: SplitTolerance::Contiguous,
        extra_names: &[],
        scan: ScanScope::RedactAndScan,
    });

    patterns.push(Pattern {
        name: SIGNED_URL,
        description: "URL carrying credentials in userinfo or query parameters",
        regex: regex(&format!(r"(?i)\b[a-z][a-z0-9+.\-]{{1,15}}://{URL_BODY}")),
        compact: None,
        render: render_url,
        split: SplitTolerance::Contiguous,
        extra_names: &[URL_USERINFO],
        scan: ScanScope::RedactAndScan,
    });

    patterns.push(Pattern {
        name: OPENROUTER_KEY,
        description: "OpenRouter API key (sk-or-v1-...)",
        regex: regex(r"(?i)\bsk-or-v1-[a-z0-9]{16,}"),
        compact: Some(regex(r"(?i)sk-or-v1-[a-z0-9]{16,}")),
        render: |_| Some(Rendered::new(OPENROUTER_KEY, "<redacted:openrouter_key>")),
        split: SplitTolerance::Splittable,
        extra_names: &[],
        scan: ScanScope::RedactAndScan,
    });

    patterns.push(Pattern {
        name: API_KEY,
        description: "Generic `sk-` prefixed API key",
        regex: regex(r"(?i)\bsk-[a-z0-9_\-]{20,}"),
        compact: Some(regex(r"(?i)sk-[a-z0-9_\-]{20,}")),
        render: |_| Some(Rendered::new(API_KEY, "<redacted:api_key>")),
        split: SplitTolerance::Splittable,
        extra_names: &[],
        scan: ScanScope::RedactAndScan,
    });

    patterns.push(Pattern {
        name: AUTHORIZATION,
        description: "Authorization header or field with its value",
        regex: regex(
            r#"(?i)(authorization"?\s*[:=]\s*"?)((?:bearer|basic|token)\s+)?([^\s"',;}\]]+)"#,
        ),
        compact: None,
        render: |caps| {
            let prefix = caps.get(1).map_or("", |m| m.as_str());
            Some(Rendered::new(
                AUTHORIZATION,
                format!("{prefix}<redacted:bearer>"),
            ))
        },
        split: SplitTolerance::Contiguous,
        extra_names: &[],
        scan: ScanScope::RedactAndScan,
    });

    patterns.push(Pattern {
        name: BEARER,
        description: "Bare `Bearer <token>` credential",
        regex: regex(r"(?i)(bearer\s+)([A-Za-z0-9\-._~+/]{8,}={0,2})"),
        compact: None,
        render: |caps| {
            let prefix = caps.get(1).map_or("", |m| m.as_str());
            Some(Rendered::new(BEARER, format!("{prefix}<redacted:bearer>")))
        },
        split: SplitTolerance::Contiguous,
        extra_names: &[],
        scan: ScanScope::RedactAndScan,
    });

    patterns.push(Pattern {
        name: JWT,
        description: "JWT-shaped three-segment token",
        regex: regex(r"\beyJ[A-Za-z0-9_\-]{6,}\.[A-Za-z0-9_\-]{6,}\.[A-Za-z0-9_\-]{6,}"),
        compact: Some(regex(
            r"eyJ[A-Za-z0-9_\-]{6,}\.[A-Za-z0-9_\-]{6,}\.[A-Za-z0-9_\-]{6,}",
        )),
        render: |_| Some(Rendered::new(JWT, "<redacted:jwt>")),
        split: SplitTolerance::Splittable,
        extra_names: &[],
        scan: ScanScope::RedactAndScan,
    });

    patterns.push(Pattern {
        name: CREDENTIAL_PARAM,
        description: "Credential-named key/value pair",
        regex: regex(
            r#"(?i)\b(access[_\-]?token|refresh[_\-]?token|id[_\-]?token|api[_\-]?key|apikey|client[_\-]?secret|secret|password|passwd|pwd|session[_\-]?id|token)("?\s*[:=]\s*"?)([^\s"',;&}\]]{4,})"#,
        ),
        compact: None,
        render: |caps| {
            let name = caps.get(1).map_or("", |m| m.as_str());
            let sep = caps.get(2).map_or("", |m| m.as_str());
            Some(Rendered::new(CREDENTIAL_PARAM, format!("{name}{sep}<redacted:credential>")))
        },
        split: SplitTolerance::Contiguous,
        extra_names: &[],
        scan: ScanScope::RedactAndScan,
    });

    // ----------------------------------------------------------------- VP-112
    //
    // Voice-biometric shapes. A speaker embedding is not a credential, but
    // §15.4 and VOICE_PROFILES_SPEC VP-112 put it under the same rule: an
    // embedding may not appear in a log line, a crash report or a notification.
    // Three shapes cover the forms one can take in formatted text — a named
    // field, a long opaque run of bytes, and a long numeric vector.

    patterns.push(Pattern {
        name: VOICE_EMBEDDING,
        description: "field named like a speaker embedding, with its value",
        // The value has to look like data, not like prose: a bracketed vector,
        // or a run of at least sixteen token characters. "scripted embeddings:
        // speaker direction" in a sentence is not a leaked embedding, and a
        // redactor that ate it would make every design note unreadable.
        regex: regex(concat!(
            r#"(?i)\b([a-z0-9_.\-]*(?:embedding|embeddings|voiceprint|speaker_vector"#,
            r#"|exemplar_vector|centroid_vector|exemplar_cipher)[a-z0-9_.\-]*)"#,
            r#"("?\s*[:=]\s*"?)(\[[^\]\n]*\]?|[A-Za-z0-9+/=_.\-]{16,})"#,
        )),
        compact: None,
        render: |caps| {
            let name = caps.get(1).map_or("", |m| m.as_str());
            let sep = caps.get(2).map_or("", |m| m.as_str());
            Some(Rendered::new(
                VOICE_EMBEDDING,
                format!("{name}{sep}<redacted:voice_embedding>"),
            ))
        },
        split: SplitTolerance::Contiguous,
        extra_names: &[],
        scan: ScanScope::RedactAndScan,
    });

    patterns.push(Pattern {
        name: DEVICE_TOKEN,
        description: "16/32/64 hex-character device token (PLAUD legacy static token, App. B.5)",
        regex: regex(r"(?i)\b([0-9a-f]{64}|[0-9a-f]{32}|[0-9a-f]{16})\b"),
        compact: Some(regex(r"(?i)([0-9a-f]{64}|[0-9a-f]{32}|[0-9a-f]{16})\b")),
        render: |_| Some(Rendered::new(DEVICE_TOKEN, "<redacted:device_token>")),
        split: SplitTolerance::Splittable,
        extra_names: &[],
        scan: ScanScope::RedactAndScan,
    });

    patterns.push(Pattern {
        name: FLOAT_VECTOR,
        description: "numeric vector long enough to be a speaker embedding",
        regex: regex(concat!(
            r"\[\s*-?\d+(?:\.\d+)?(?:[eE][+\-]?\d+)?",
            r"(?:\s*,\s*-?\d+(?:\.\d+)?(?:[eE][+\-]?\d+)?){15,}\s*,?\s*\]?",
        )),
        compact: None,
        render: |_| Some(Rendered::new(FLOAT_VECTOR, "<redacted:float_vector>")),
        split: SplitTolerance::Contiguous,
        extra_names: &[],
        scan: ScanScope::RedactionOnly,
    });

    patterns.push(Pattern {
        name: BINARY_BLOB,
        description: "opaque base64/hex run long enough to carry an embedding",
        regex: regex(&format!(
            r"(?:[A-Za-z0-9+/_\-]{{{MIN_BLOB_CHARS},}}={{0,2}})"
        )),
        compact: None,
        render: |_| Some(Rendered::new(BINARY_BLOB, "<redacted:binary_blob>")),
        split: SplitTolerance::Contiguous,
        extra_names: &[],
        scan: ScanScope::RedactionOnly,
    });

    patterns
}

/// Compile a pattern, panicking only on a literal authored here. The pattern
/// table is a compile-time constant in everything but name: an invalid regex is
/// a build-time authoring bug, caught by the first unit test in this module.
fn regex(pattern: &str) -> Regex {
    match Regex::new(pattern) {
        Ok(re) => re,
        Err(err) => unreachable!("built-in redaction pattern failed to compile: {err}"),
    }
}

/// The process-wide pattern set, compiled once on first use.
static SET: LazyLock<Vec<Pattern>> = LazyLock::new(build);

/// The process-wide pattern set.
pub fn pattern_set() -> &'static [Pattern] {
    SET.as_slice()
}

/// Every reportable pattern name, including [`REGISTERED`]. Used to validate
/// allowlist files so a typo cannot silently disable a rule.
pub fn pattern_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = pattern_set()
        .iter()
        .flat_map(Pattern::names)
        .chain(std::iter::once(REGISTERED))
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pattern_compiles_and_has_unique_names() {
        let names = pattern_names();
        assert!(names.contains(&OPENROUTER_KEY));
        assert!(names.contains(&REGISTERED));
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "pattern names must be unique");
        assert!(pattern_set().len() >= 9);
    }

    #[test]
    fn plain_url_is_not_a_signed_url() {
        assert!(!query_is_credentialed(""));
        assert!(!query_is_credentialed("page=2&limit=50"));
        assert!(!query_is_credentialed("expires=123"));
    }

    #[test]
    fn credentialed_queries_are_detected() {
        assert!(query_is_credentialed("X-Amz-Signature=abc"));
        assert!(query_is_credentialed("sig=abc"));
        assert!(query_is_credentialed(
            "sv=2021-08-06&sp=r&se=2024-01-01T00%3A00Z"
        ));
        assert!(query_is_credentialed("Expires=1700000000&Key-Pair-Id=APK"));
        assert!(query_is_credentialed("Policy=eyJ"));
    }
}
