//! Pattern- and registry-based redaction.
//!
//! [`Redactor`] is the only thing a log sink needs: it takes an already
//! formatted record and returns a record with every credential-shaped span
//! replaced by a stable marker (spec §16.2 "Credential disclosure", §18.5,
//! §1.5 "Secrets in application logs: 0 in automated scanning").
//!
//! # Hostile variants
//!
//! Real leaks are rarely literal. The redactor therefore scans up to three
//! *views* of the same record, mapping every match back to byte offsets in the
//! original text:
//!
//! 1. **Decoded view** — `%XX` percent escapes and JSON `\/` escapes are
//!    resolved, so `%73k-or-v1-…` and `https:\/\/host\/o?sig=…` are caught.
//!    For a record with no escapes this view is the record itself, so the
//!    common case costs one pass.
//! 2. **Compacted view** — separator characters (quotes, `=`, `:`, `,`,
//!    whitespace except newline, brackets) are removed, so a secret split
//!    across two adjacent fields (`a="sk-or-v1-abc" b="defghij…"`) or printed
//!    inside a `Debug` struct still matches. Only patterns with a
//!    [`SplitTolerance::Splittable`] / `compact_regex` variant run here, and a
//!    match must begin where a separator was removed.
//! 3. **Registered secrets** — exact live values registered with
//!    [`Redactor::register_secret`], matched on the decoded, compacted and (if
//!    decoding changed anything) untouched views.
//!
//! Overlapping matches are merged into their union before replacement, so a
//! partial overlap can never leave a tail of a secret visible.
//!
//! # Stated limits
//!
//! * A compacted match that starts where a *literal* match already starts is
//!   discarded as greedy spill — compaction glues following fields onto a
//!   secret that was never split, and keeping that match would wipe the rest
//!   of the record (including the surrounding JSON). The consequence, stated
//!   plainly: if a secret is split across fields **and** its first fragment is
//!   independently shape-complete, the trailing fragment is scrubbed only when
//!   the value is registered.
//! * Redaction is idempotent: an already-emitted `<redacted:…>` /
//!   `<truncated:…>` marker is inert, so composing passes (sink, notification
//!   reducer, crash hook) cannot grow or re-wrap a record.
//! * Shapes and registered values are all this can see. An unregistered secret
//!   with no distinctive shape, or user content a caller logs deliberately, is
//!   not detectable here — which is why `plaudon-secret-scan` exists.
//!
//! # Cost
//!
//! Let `n` be the record length (capped at [`MAX_SCAN_BYTES`]), `p` the number
//! of patterns (fixed, ten) and `k` the number of distinct registered secret
//! lengths (capped at [`MAX_REGISTERED_SECRETS`] = 16). Pattern scanning is
//! `O(p·n)` per view. Registered-secret scanning is a Rabin–Karp rolling hash:
//! `O(k·n)` 64-bit updates plus one SHA-256 per fingerprint collision, which
//! for 64-bit fingerprints is a vanishing fraction of windows. Worst case is
//! therefore `O((p + k)·n)` with `n ≤ 256 KiB`.

use std::borrow::Cow;
use std::sync::{LazyLock, RwLock};

use regex::Regex;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::patterns::{self, Pattern, Rendered};

/// Longest record the redactor will scan. Anything beyond this is dropped
/// rather than emitted unscanned: an unscanned tail is a potential leak, so the
/// sink loses data instead of leaking it.
pub const MAX_SCAN_BYTES: usize = 256 * 1024;

/// Maximum number of live secrets that may be registered at once.
pub const MAX_REGISTERED_SECRETS: usize = 16;

/// Shortest registerable secret. Shorter values would match constantly and turn
/// the log into noise.
pub const MIN_REGISTERED_SECRET_LEN: usize = 8;

/// Longest registerable secret.
pub const MAX_REGISTERED_SECRET_LEN: usize = 512;

/// Why a secret could not be registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RegisterSecretError {
    #[error("secret is shorter than the {MIN_REGISTERED_SECRET_LEN}-byte minimum")]
    TooShort,
    #[error("secret is longer than the {MAX_REGISTERED_SECRET_LEN}-byte maximum")]
    TooLong,
    #[error("the redactor already holds {MAX_REGISTERED_SECRETS} registered secrets")]
    RegistryFull,
}

/// One match of a pattern (or of a registered secret) in the *original* record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Stable pattern name, e.g. [`patterns::OPENROUTER_KEY`].
    pub pattern: &'static str,
    /// Byte offset of the first byte of the match in the original record.
    pub start: usize,
    /// Byte offset one past the last byte of the match.
    pub end: usize,
    marker: String,
    rank: usize,
}

impl Finding {
    /// The replacement marker this match renders to.
    #[must_use]
    pub fn marker(&self) -> &str {
        &self.marker
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RegisteredSecret {
    len: usize,
    /// Non-cryptographic 64-bit Rabin–Karp fingerprint used only as an
    /// in-memory prefilter. Never persisted, never logged.
    fingerprint: u64,
    /// SHA-256 over `salt || secret`, used to confirm a fingerprint hit.
    digest: [u8; 32],
}

#[derive(Debug)]
struct Registry {
    salt: [u8; 32],
    secrets: Vec<RegisteredSecret>,
}

/// Scrubs secrets out of already formatted text.
///
/// Cheap to share: wrap in an `Arc` and hand it to the log sink, the crash hook
/// and the scanner. Registration takes `&self` so a key entered at runtime can
/// be registered without rebuilding the subscriber.
#[derive(Debug)]
pub struct Redactor {
    registry: RwLock<Registry>,
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

/// Process-wide redactor used by surfaces that are not attached to a logging
/// handle (panic payloads, notifications). It only ever grows knowledge, so
/// sharing it is safe; code that needs isolation constructs its own.
static SHARED: LazyLock<Redactor> = LazyLock::new(Redactor::new);

impl Redactor {
    /// The process-wide redactor (see [`SHARED`]).
    #[must_use]
    pub fn shared() -> &'static Self {
        &SHARED
    }

    #[must_use]
    pub fn new() -> Self {
        Self {
            registry: RwLock::new(Registry {
                salt: rand::random(),
                secrets: Vec::new(),
            }),
        }
    }

    /// Register an exact live secret so it is scrubbed even when it matches no
    /// known shape (a bare device token, a provider key with a new prefix).
    ///
    /// Only a random-salted SHA-256 digest, the length and a 64-bit prefilter
    /// fingerprint are retained; the copy made for hashing is zeroized on drop.
    /// The caller still owns the lifetime of its own plaintext buffer — pass a
    /// `Zeroizing<String>` there if it is long-lived.
    pub fn register_secret(&self, secret: &str) -> Result<(), RegisterSecretError> {
        if secret.len() < MIN_REGISTERED_SECRET_LEN {
            return Err(RegisterSecretError::TooShort);
        }
        if secret.len() > MAX_REGISTERED_SECRET_LEN {
            return Err(RegisterSecretError::TooLong);
        }
        let mut registry = lock_write(&self.registry);
        let entry = {
            let bytes = Zeroizing::new(secret.as_bytes().to_vec());
            RegisteredSecret {
                len: bytes.len(),
                fingerprint: fingerprint(&bytes),
                digest: salted_digest(&registry.salt, &bytes),
            }
        };
        if registry.secrets.contains(&entry) {
            return Ok(());
        }
        if registry.secrets.len() >= MAX_REGISTERED_SECRETS {
            return Err(RegisterSecretError::RegistryFull);
        }
        registry.secrets.push(entry);
        Ok(())
    }

    /// Number of currently registered secrets.
    #[must_use]
    pub fn registered_len(&self) -> usize {
        lock_read(&self.registry).secrets.len()
    }

    /// Forget every registered secret (used when a key is removed).
    pub fn clear_registered(&self) {
        lock_write(&self.registry).secrets.clear();
    }

    /// All matches in `input`, in original byte offsets, sorted by start.
    ///
    /// Overlapping matches from different patterns are all reported; this is
    /// the scanner's view. [`Redactor::redact`] merges them.
    #[must_use]
    pub fn matches(&self, input: &str) -> Vec<Finding> {
        let head = truncate_at_boundary(input, MAX_SCAN_BYTES);
        let mut found = self.collect(head);
        found.sort_by_key(|f| (f.start, f.end, f.rank));
        found.dedup_by(|a, b| a.pattern == b.pattern && a.start == b.start && a.end == b.end);
        found
    }

    /// Replace every secret-shaped span in `input` with its marker.
    ///
    /// Returns the input unchanged (borrowed) when there is nothing to redact
    /// and the record is within [`MAX_SCAN_BYTES`].
    #[must_use]
    pub fn redact<'a>(&self, input: &'a str) -> Cow<'a, str> {
        let head = truncate_at_boundary(input, MAX_SCAN_BYTES);
        let dropped = input.len() - head.len();
        let spans = merge(self.collect(head));
        if spans.is_empty() && dropped == 0 {
            return Cow::Borrowed(input);
        }
        let mut out = String::with_capacity(head.len() + 32);
        let mut cursor = 0usize;
        for span in spans {
            if span.start < cursor {
                continue;
            }
            out.push_str(&head[cursor..span.start]);
            out.push_str(&span.marker);
            cursor = span.end;
        }
        out.push_str(&head[cursor..]);
        if dropped > 0 {
            out.push_str("<truncated:");
            out.push_str(&dropped.to_string());
            out.push_str(" bytes dropped unscanned>");
        }
        Cow::Owned(out)
    }

    fn collect(&self, input: &str) -> Vec<Finding> {
        let mut out = Vec::new();
        let decoded = View::decoded(input);
        scan_patterns(&decoded, Mode::Literal, &mut out);
        let literal_spans: Vec<(usize, usize)> = out.iter().map(|f| (f.start, f.end)).collect();
        let compacted = View::compacted(&decoded);
        let compacted_differs = compacted.text != decoded.text;
        if compacted_differs {
            let mut spilled = Vec::new();
            scan_patterns(&compacted, Mode::Compacted, &mut spilled);
            // A compacted match that starts where a literal match already
            // starts is greedy spill: compaction glued the following fields
            // onto a secret that was never split, and keeping it would wipe
            // the rest of the record. The literal match already covers the
            // secret, so drop the spill and keep the record readable. The
            // honest limit of that choice: if a secret is split *and* its
            // first fragment is independently shape-complete, the trailing
            // fragment is only scrubbed when the value is registered.
            spilled.retain(|finding| {
                !literal_spans
                    .iter()
                    .any(|(start, end)| finding.start >= *start && finding.start < *end)
            });
            out.append(&mut spilled);
        }

        {
            let registry = lock_read(&self.registry);
            if !registry.secrets.is_empty() {
                scan_registered(&registry, &decoded, &mut out);
                if compacted_differs {
                    scan_registered(&registry, &compacted, &mut out);
                }
                // A registered secret is matched byte-for-byte, so a secret
                // that itself contains `%XX` or `\/` is invisible in the
                // decoded view. Scan the untouched record as well whenever
                // decoding actually changed something.
                if decoded.text != input {
                    scan_registered(&registry, &View::identity(input), &mut out);
                }
            }
        }

        // Markers emitted by an earlier pass are inert: redaction composes
        // (the sink, the notification reducer and the crash hook can each see
        // an already-redacted record), so `redact(redact(x)) == redact(x)`
        // must hold or a hot log line would grow on every pass.
        let protected = protected_spans(input);
        out.retain(|finding| {
            !protected
                .iter()
                .any(|(start, end)| finding.start < *end && *start < finding.end)
        });
        out
    }
}

/// Rank offset keeping registered-secret markers behind shape markers: shape
/// markers carry more context (`host=…`), and safety does not depend on which
/// marker wins because overlapping spans are merged into their union.
const REGISTERED_RANK: usize = 1000;

/// Which view a pattern pass is running over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The percent/escape-decoded text, matched with the anchored patterns.
    Literal,
    /// The separator-stripped text, matched with the boundary-relaxed
    /// [`Pattern::compact_regex`] variants.
    Compacted,
}

/// Spans of already-emitted markers, in original byte offsets.
static MARKER_RE: LazyLock<Regex> =
    LazyLock::new(|| match Regex::new(r"<(?:redacted|truncated):[^<>]*>") {
        Ok(re) => re,
        Err(err) => unreachable!("marker pattern failed to compile: {err}"),
    });

fn protected_spans(input: &str) -> Vec<(usize, usize)> {
    MARKER_RE
        .find_iter(input)
        .map(|m| (m.start(), m.end()))
        .collect()
}

fn scan_patterns(view: &View, mode: Mode, out: &mut Vec<Finding>) {
    for (rank, pattern) in patterns::pattern_set().iter().enumerate() {
        let regex = match mode {
            Mode::Literal => pattern.regex(),
            Mode::Compacted => match pattern.compact_regex() {
                Some(regex) => regex,
                None => continue,
            },
        };
        for caps in regex.captures_iter(&view.text) {
            let Some(whole) = caps.get(0) else { continue };
            let (start, mut end) = (whole.start(), whole.end());
            // On the compacted view a match must begin where a separator was
            // removed (or at the start of the record); otherwise a relaxed
            // pattern could latch onto the middle of an ordinary word.
            if mode == Mode::Compacted && !view.is_run_start(start) {
                continue;
            }
            trim_trailing_punctuation(&view.text, start, &mut end);
            if end <= start {
                continue;
            }
            let Some(Rendered {
                pattern: name,
                marker,
            }) = render(regex, pattern, &view.text, start, end)
            else {
                continue;
            };
            let (orig_start, orig_end) = view.span(start, end);
            out.push(Finding {
                pattern: name,
                start: orig_start,
                end: orig_end,
                marker,
                rank,
            });
        }
    }
}

/// Re-run the pattern on the (possibly punctuation-trimmed) slice so the
/// renderer sees exactly the span that will be replaced.
fn render(
    regex: &Regex,
    pattern: &Pattern,
    text: &str,
    start: usize,
    end: usize,
) -> Option<Rendered> {
    let slice = text.get(start..end)?;
    let caps = regex.captures(slice)?;
    pattern.render(&caps)
}

/// URLs and tokens at the end of a sentence pick up trailing punctuation; drop
/// it from the span so `...host=x>.` keeps its full stop.
fn trim_trailing_punctuation(text: &str, start: usize, end: &mut usize) {
    let bytes = text.as_bytes();
    while *end > start {
        let last = bytes[*end - 1];
        if matches!(
            last,
            b'.' | b',' | b';' | b':' | b'!' | b'?' | b')' | b']' | b'}' | b'>'
        ) {
            *end -= 1;
        } else {
            break;
        }
    }
}

fn scan_registered(registry: &Registry, view: &View, out: &mut Vec<Finding>) {
    let bytes = view.text.as_bytes();
    let mut lengths: Vec<usize> = registry.secrets.iter().map(|s| s.len).collect();
    lengths.sort_unstable();
    lengths.dedup();
    for len in lengths {
        if bytes.len() < len || len == 0 {
            continue;
        }
        let candidates: Vec<&RegisteredSecret> =
            registry.secrets.iter().filter(|s| s.len == len).collect();
        let mut hash = fingerprint(&bytes[..len]);
        let pow = high_power(len);
        let mut start = 0usize;
        loop {
            if candidates.iter().any(|c| c.fingerprint == hash) {
                let window = &bytes[start..start + len];
                let digest = salted_digest(&registry.salt, window);
                if candidates.iter().any(|c| c.digest == digest)
                    && view.text.is_char_boundary(start)
                    && view.text.is_char_boundary(start + len)
                {
                    let (orig_start, orig_end) = view.span(start, start + len);
                    out.push(Finding {
                        pattern: patterns::REGISTERED,
                        start: orig_start,
                        end: orig_end,
                        marker: "<redacted:registered>".to_owned(),
                        rank: REGISTERED_RANK,
                    });
                }
            }
            if start + len >= bytes.len() {
                break;
            }
            hash = roll(hash, bytes[start], bytes[start + len], pow);
            start += 1;
        }
    }
}

/// Merge overlapping findings into their union, keeping the marker of the
/// lowest-ranked (most specific) contributor. Union merging is what guarantees
/// that a partially overlapping pair cannot leave an unredacted tail.
fn merge(mut found: Vec<Finding>) -> Vec<Finding> {
    found.sort_by_key(|f| (f.start, std::cmp::Reverse(f.end), f.rank));
    let mut merged: Vec<Finding> = Vec::with_capacity(found.len());
    for finding in found {
        match merged.last_mut() {
            Some(last) if finding.start < last.end => {
                if finding.end > last.end {
                    last.end = finding.end;
                }
                if finding.rank < last.rank {
                    last.rank = finding.rank;
                    last.pattern = finding.pattern;
                    last.marker = finding.marker;
                }
            }
            _ => merged.push(finding),
        }
    }
    merged
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// A transformed copy of a record plus a map from transformed byte offsets back
/// to original byte offsets. `map` has `text.len() + 1` entries so an exclusive
/// end offset is always mappable.
#[derive(Debug)]
struct View {
    text: String,
    map: Vec<usize>,
    /// For the compacted view: whether the byte at this index starts a run of
    /// retained bytes (the position a removed separator used to precede).
    /// Empty for views that drop nothing, where every position is legal.
    run_start: Vec<bool>,
}

impl View {
    /// The record itself, with a trivial offset map.
    fn identity(input: &str) -> Self {
        Self {
            text: input.to_owned(),
            map: (0..=input.len()).collect(),
            run_start: Vec::new(),
        }
    }

    /// Resolve `%XX` percent escapes (to printable ASCII only, so the result
    /// stays valid UTF-8) and JSON `\/` escapes.
    fn decoded(input: &str) -> Self {
        let bytes = input.as_bytes();
        let mut text = Vec::with_capacity(bytes.len());
        let mut map = Vec::with_capacity(bytes.len() + 1);
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    let decoded = hi * 16 + lo;
                    if (0x20..0x7f).contains(&decoded) {
                        text.push(decoded);
                        map.push(i);
                        i += 3;
                        continue;
                    }
                }
            }
            if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                text.push(b'/');
                map.push(i);
                i += 2;
                continue;
            }
            text.push(bytes[i]);
            map.push(i);
            i += 1;
        }
        map.push(bytes.len());
        Self {
            text: String::from_utf8(text).unwrap_or_else(|_| input.to_owned()),
            map,
            run_start: Vec::new(),
        }
    }

    /// Remove separator bytes so a split secret becomes contiguous. Newlines
    /// are retained so a match can never span two records.
    fn compacted(decoded: &Self) -> Self {
        let bytes = decoded.text.as_bytes();
        let mut text = Vec::with_capacity(bytes.len());
        let mut map = Vec::with_capacity(bytes.len() + 1);
        let mut run_start = Vec::with_capacity(bytes.len());
        let mut dropped_previous = true;
        for (i, &b) in bytes.iter().enumerate() {
            if is_separator(b) {
                dropped_previous = true;
                continue;
            }
            text.push(b);
            map.push(decoded.map[i]);
            run_start.push(dropped_previous);
            dropped_previous = false;
        }
        map.push(decoded.map[bytes.len()]);
        Self {
            text: String::from_utf8(text).unwrap_or_else(|_| decoded.text.clone()),
            map,
            run_start,
        }
    }

    /// Whether a match may begin at `index` in this view.
    fn is_run_start(&self, index: usize) -> bool {
        self.run_start.is_empty() || self.run_start.get(index).copied().unwrap_or(false)
    }

    /// Map a span in this view back to a span in the original record.
    fn span(&self, start: usize, end: usize) -> (usize, usize) {
        let s = self.map.get(start).copied().unwrap_or(0);
        let e = self
            .map
            .get(end)
            .copied()
            .unwrap_or_else(|| self.map.last().copied().unwrap_or(0));
        (s, e.max(s))
    }
}

fn is_separator(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t'
            | b'\r'
            | b'"'
            | b'\''
            | b'='
            | b','
            | b';'
            | b':'
            | b'|'
            | b'('
            | b')'
            | b'{'
            | b'}'
            | b'['
            | b']'
            | b'<'
            | b'>'
            | b'\\'
    )
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn truncate_at_boundary(input: &str, max: usize) -> &str {
    if input.len() <= max {
        return input;
    }
    let mut end = max;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    &input[..end]
}

// ---------------------------------------------------------------------------
// Hashing
// ---------------------------------------------------------------------------

const RK_BASE: u64 = 257;

fn fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |h, &b| {
        h.wrapping_mul(RK_BASE).wrapping_add(u64::from(b))
    })
}

fn high_power(len: usize) -> u64 {
    let mut pow = 1u64;
    for _ in 1..len {
        pow = pow.wrapping_mul(RK_BASE);
    }
    pow
}

fn roll(hash: u64, out_byte: u8, in_byte: u8, pow: u64) -> u64 {
    hash.wrapping_sub(u64::from(out_byte).wrapping_mul(pow))
        .wrapping_mul(RK_BASE)
        .wrapping_add(u64::from(in_byte))
}

fn salted_digest(salt: &[u8; 32], bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(bytes);
    hasher.finalize().into()
}

fn lock_read(lock: &RwLock<Registry>) -> std::sync::RwLockReadGuard<'_, Registry> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_write(lock: &RwLock<Registry>) -> std::sync::RwLockWriteGuard<'_, Registry> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn decoded_view_maps_percent_escapes_back() {
        let view = View::decoded("a%73b");
        assert_eq!(view.text, "asb");
        assert_eq!(view.span(1, 2), (1, 4));
    }

    #[test]
    fn compacted_view_removes_separators_but_not_newlines() {
        let decoded = View::decoded("a=\"b\", c\nd");
        let compacted = View::compacted(&decoded);
        assert_eq!(compacted.text, "abc\nd");
    }

    #[test]
    fn rolling_hash_matches_direct_hash() {
        let data = b"abcdefghij";
        let len = 4;
        let pow = high_power(len);
        let mut hash = fingerprint(&data[..len]);
        for start in 1..=(data.len() - len) {
            hash = roll(hash, data[start - 1], data[start + len - 1], pow);
            assert_eq!(hash, fingerprint(&data[start..start + len]));
        }
    }

    #[test]
    fn registry_enforces_bounds() {
        let redactor = Redactor::new();
        assert_eq!(
            redactor.register_secret("short"),
            Err(RegisterSecretError::TooShort)
        );
        assert_eq!(
            redactor.register_secret(&"x".repeat(MAX_REGISTERED_SECRET_LEN + 1)),
            Err(RegisterSecretError::TooLong)
        );
        for i in 0..MAX_REGISTERED_SECRETS {
            redactor
                .register_secret(&format!("secret-value-{i:03}"))
                .unwrap();
        }
        assert_eq!(redactor.registered_len(), MAX_REGISTERED_SECRETS);
        assert_eq!(
            redactor.register_secret("one-too-many-secret"),
            Err(RegisterSecretError::RegistryFull)
        );
        redactor.clear_registered();
        assert_eq!(redactor.registered_len(), 0);
    }

    #[test]
    fn duplicate_registration_is_idempotent() {
        let redactor = Redactor::new();
        redactor.register_secret("repeated-secret-value").unwrap();
        redactor.register_secret("repeated-secret-value").unwrap();
        assert_eq!(redactor.registered_len(), 1);
    }

    #[test]
    fn oversized_input_is_truncated_not_leaked() {
        let redactor = Redactor::new();
        let mut input = "a".repeat(MAX_SCAN_BYTES);
        input.push_str("sk-or-v1-0123456789abcdef0123456789abcdef");
        let out = redactor.redact(&input);
        assert!(!out.contains("sk-or-v1-"));
        assert!(out.contains("<truncated:"));
    }

    #[test]
    fn clean_record_is_borrowed() {
        let redactor = Redactor::new();
        assert!(matches!(
            redactor.redact("import finished: 3 recordings"),
            Cow::Borrowed(_)
        ));
    }
}
