//! The pure, allocation-free core of the egress monitor.
//!
//! Everything in this module is an ordinary function with no I/O and no
//! `unsafe`, so it is unit-tested directly (see the tests at the bottom of this
//! file). The interposed libc wrappers in [`crate::interpose_apple`] /
//! [`crate::interpose_linux`] only translate raw pointers into these types and
//! hand the formatted bytes to a single `write(2)`.

/// Longest host/IP string we record, in raw bytes, before truncation.
pub const HOST_CAP: usize = 128;
/// Longest program path we record for an `exec` line, in raw bytes.
pub const PROGRAM_CAP: usize = 256;
/// Capacity of the fixed stack buffer a trace line is rendered into.
///
/// Chosen so that the worst case — both strings at their cap with every byte
/// needing a six-character `\u00xx` escape — still fits. `line_fits_buffer`
/// proves it.
pub const MAX_LINE: usize = 2560;

/// The libc entry point that was intercepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    /// Emitted once, from the library constructor, as proof of injection.
    MonitorReady,
    Socket,
    Connect,
    Connectx,
    Sendto,
    Getaddrinfo,
    Gethostbyname,
    ResQuery,
    Exec,
}

impl Call {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Call::MonitorReady => "monitor_ready",
            Call::Socket => "socket",
            Call::Connect => "connect",
            Call::Connectx => "connectx",
            Call::Sendto => "sendto",
            Call::Getaddrinfo => "getaddrinfo",
            Call::Gethostbyname => "gethostbyname",
            Call::ResQuery => "res_query",
            Call::Exec => "exec",
        }
    }
}

/// Address family of the intercepted call, as recorded in the trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Not applicable (resolver calls, `exec`, the ready marker).
    None,
    Unix,
    Inet,
    Inet6,
    /// Any other `AF_*` value; carries the raw number so the trace stays honest.
    Other(i32),
}

/// Raw `AF_*` values. Taken from libc where this crate can actually intercept
/// anything; elsewhere they are the Winsock values, present only so the crate
/// still compiles and reports [`crate::MonitorStatus::Unsupported`].
pub mod af {
    #[cfg(unix)]
    pub const UNIX: i32 = libc::AF_UNIX;
    #[cfg(unix)]
    pub const INET: i32 = libc::AF_INET;
    #[cfg(unix)]
    pub const INET6: i32 = libc::AF_INET6;

    #[cfg(not(unix))]
    pub const UNIX: i32 = 1;
    #[cfg(not(unix))]
    pub const INET: i32 = 2;
    #[cfg(not(unix))]
    pub const INET6: i32 = 23;
}

impl Family {
    /// Map a raw `AF_*` constant.
    #[must_use]
    pub fn from_raw(value: i32) -> Family {
        match value {
            af::UNIX => Family::Unix,
            af::INET => Family::Inet,
            af::INET6 => Family::Inet6,
            other => Family::Other(other),
        }
    }

    /// True for the two families that can carry internet traffic.
    #[must_use]
    pub const fn is_internet(self) -> bool {
        matches!(self, Family::Inet | Family::Inet6)
    }
}

/// An IP address read out of a `sockaddr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addr {
    V4([u8; 4]),
    V6([u8; 16]),
}

/// What the monitor decided to do with an intercepted call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Loopback or non-internet: forwarded to the real libc function.
    Permit,
    /// Non-loopback internet destination: refused with `EPERM` / `EAI_FAIL`.
    Deny,
}

impl Verdict {
    #[must_use]
    pub const fn denied(self) -> bool {
        matches!(self, Verdict::Deny)
    }
}

/// `127.0.0.0/8`.
#[must_use]
pub const fn is_loopback_v4(octets: [u8; 4]) -> bool {
    octets[0] == 127
}

/// `::1`, plus IPv4-mapped (`::ffff:127.0.0.0/8`) and IPv4-compatible loopback.
#[must_use]
pub fn is_loopback_v6(octets: [u8; 16]) -> bool {
    const UNSPEC_PREFIX: [u8; 10] = [0; 10];
    if octets == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1] {
        return true;
    }
    if octets[..10] == UNSPEC_PREFIX && (octets[10], octets[11]) == (0xff, 0xff) {
        return is_loopback_v4([octets[12], octets[13], octets[14], octets[15]]);
    }
    false
}

/// Loopback classification for an address read out of a `sockaddr`.
#[must_use]
pub fn classify_addr(addr: Addr) -> Verdict {
    let loopback = match addr {
        Addr::V4(o) => is_loopback_v4(o),
        Addr::V6(o) => is_loopback_v6(o),
    };
    if loopback {
        Verdict::Permit
    } else {
        Verdict::Deny
    }
}

/// Loopback classification for a name passed to a resolver entry point.
///
/// A name is permitted only when it unambiguously denotes this machine. Note
/// that `.local` (mDNS) is **denied**: it is a real multicast network lookup.
#[must_use]
pub fn classify_host(name: &[u8]) -> Verdict {
    if name.is_empty() {
        // `getaddrinfo(NULL, service, ...)` is a local bind, not a lookup.
        return Verdict::Permit;
    }
    let lower = {
        let mut buf = [0u8; HOST_CAP];
        let len = name.len().min(HOST_CAP);
        for (dst, src) in buf[..len].iter_mut().zip(&name[..len]) {
            *dst = src.to_ascii_lowercase();
        }
        (buf, len)
    };
    let name = &lower.0[..lower.1];

    if name == b"localhost" || name == b"localhost." || ends_with(name, b".localhost") {
        return Verdict::Permit;
    }
    if let Some(addr) = parse_ip_literal(name) {
        return classify_addr(addr);
    }
    Verdict::Deny
}

fn ends_with(haystack: &[u8], suffix: &[u8]) -> bool {
    haystack.len() >= suffix.len() && &haystack[haystack.len() - suffix.len()..] == suffix
}

/// Parse a bare IPv4 dotted-quad or a textual IPv6 address (including `::`
/// compression). Returns `None` for anything that is not an IP literal.
#[must_use]
pub fn parse_ip_literal(name: &[u8]) -> Option<Addr> {
    if let Some(v4) = parse_ipv4(name) {
        return Some(Addr::V4(v4));
    }
    parse_ipv6(name).map(Addr::V6)
}

fn parse_ipv4(name: &[u8]) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut idx = 0usize;
    let mut digits = 0usize;
    let mut value = 0u32;
    for &b in name {
        if b == b'.' {
            if digits == 0 || idx >= 3 {
                return None;
            }
            out[idx] = u8::try_from(value).ok()?;
            idx += 1;
            digits = 0;
            value = 0;
        } else if b.is_ascii_digit() {
            digits += 1;
            if digits > 3 {
                return None;
            }
            value = value * 10 + u32::from(b - b'0');
            if value > 255 {
                return None;
            }
        } else {
            return None;
        }
    }
    if idx != 3 || digits == 0 {
        return None;
    }
    out[3] = u8::try_from(value).ok()?;
    Some(out)
}

#[allow(clippy::cast_possible_truncation)]
fn parse_ipv6(name: &[u8]) -> Option<[u8; 16]> {
    if name.is_empty() {
        return None;
    }
    let mut head = [0u8; 16];
    let mut head_len = 0usize;
    let mut tail = [0u8; 16];
    let mut tail_len = 0usize;
    let mut seen_gap = false;

    let mut rest = name;
    if rest.starts_with(b"::") {
        seen_gap = true;
        rest = &rest[2..];
    } else if rest.starts_with(b":") {
        return None;
    }

    while !rest.is_empty() {
        let end = rest.iter().position(|&b| b == b':').unwrap_or(rest.len());
        // An embedded IPv4 tail, e.g. `::ffff:127.0.0.1`: the current group
        // contains a dot, and an IPv4 tail is always the final element.
        if rest[..end].contains(&b'.') {
            let v4 = parse_ipv4(rest)?;
            let target = if seen_gap { &mut tail } else { &mut head };
            let len = if seen_gap {
                &mut tail_len
            } else {
                &mut head_len
            };
            if *len + 4 > 16 {
                return None;
            }
            target[*len..*len + 4].copy_from_slice(&v4);
            *len += 4;
            break;
        }
        let group = &rest[..end];
        if group.is_empty() || group.len() > 4 {
            return None;
        }
        let mut value = 0u16;
        for &b in group {
            let digit = (b as char).to_digit(16)?;
            value = value.checked_mul(16)?.checked_add(digit as u16)?;
        }
        let target = if seen_gap { &mut tail } else { &mut head };
        let len = if seen_gap {
            &mut tail_len
        } else {
            &mut head_len
        };
        if *len + 2 > 16 {
            return None;
        }
        target[*len] = (value >> 8) as u8;
        target[*len + 1] = (value & 0xff) as u8;
        *len += 2;

        rest = &rest[end..];
        if rest.is_empty() {
            break;
        }
        // Consume the separator(s).
        if rest.starts_with(b"::") {
            if seen_gap {
                return None;
            }
            seen_gap = true;
            rest = &rest[2..];
            if rest.is_empty() {
                break;
            }
        } else {
            rest = &rest[1..];
            if rest.is_empty() {
                return None;
            }
        }
    }

    let mut out = [0u8; 16];
    if seen_gap {
        if head_len + tail_len > 14 {
            return None;
        }
        out[..head_len].copy_from_slice(&head[..head_len]);
        out[16 - tail_len..].copy_from_slice(&tail[..tail_len]);
    } else {
        if head_len != 16 {
            return None;
        }
        out.copy_from_slice(&head[..16]);
    }
    Some(out)
}

/// One intercepted call, ready to be rendered as a trace line.
#[derive(Debug, Clone, Copy)]
pub struct Attempt<'a> {
    pub ts_unix: i64,
    pub call: Call,
    pub family: Family,
    /// Raw bytes as seen at the libc boundary; may not be valid UTF-8.
    pub host_or_ip: &'a [u8],
    pub port: u16,
    pub denied: bool,
    /// Only used by [`Call::Exec`]; empty means "field omitted".
    pub program: &'a [u8],
}

impl<'a> Attempt<'a> {
    #[must_use]
    pub const fn new(ts_unix: i64, call: Call) -> Attempt<'a> {
        Attempt {
            ts_unix,
            call,
            family: Family::None,
            host_or_ip: &[],
            port: 0,
            denied: false,
            program: &[],
        }
    }
}

/// A fixed-capacity byte buffer. Writes past [`MAX_LINE`] are dropped, which
/// cannot happen for a well-formed [`Attempt`] (see `line_fits_buffer`).
#[derive(Debug)]
pub struct LineBuf {
    buf: [u8; MAX_LINE],
    len: usize,
}

impl Default for LineBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl LineBuf {
    #[must_use]
    pub const fn new() -> LineBuf {
        LineBuf {
            buf: [0; MAX_LINE],
            len: 0,
        }
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push(&mut self, byte: u8) {
        if self.len < MAX_LINE {
            self.buf[self.len] = byte;
            self.len += 1;
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.push(b);
        }
    }

    fn push_str(&mut self, s: &str) {
        self.push_bytes(s.as_bytes());
    }

    fn push_i64(&mut self, value: i64) {
        let mut digits = [0u8; 20];
        let negative = value < 0;
        // Work in u64 so i64::MIN is representable.
        let mut magnitude = value.unsigned_abs();
        let mut idx = digits.len();
        loop {
            idx -= 1;
            digits[idx] = b'0' + u8::try_from(magnitude % 10).unwrap_or(0);
            magnitude /= 10;
            if magnitude == 0 || idx == 0 {
                break;
            }
        }
        if negative {
            self.push(b'-');
        }
        self.push_bytes(&digits[idx..]);
    }

    /// Append a JSON string literal. Every byte outside printable ASCII is
    /// emitted as `\u00xx`, so the line is always valid JSON even when libc
    /// handed us a non-UTF-8 name.
    fn push_json_string(&mut self, raw: &[u8], cap: usize) {
        self.push(b'"');
        for &b in raw.iter().take(cap) {
            match b {
                b'"' => self.push_str("\\\""),
                b'\\' => self.push_str("\\\\"),
                0x20..=0x7e => self.push(b),
                other => {
                    self.push_str("\\u00");
                    self.push(hex_nibble(other >> 4));
                    self.push(hex_nibble(other & 0x0f));
                }
            }
        }
        self.push(b'"');
    }

    fn push_family(&mut self, family: Family) {
        match family {
            Family::None => self.push_str("\"none\""),
            Family::Unix => self.push_str("\"unix\""),
            Family::Inet => self.push_str("\"inet\""),
            Family::Inet6 => self.push_str("\"inet6\""),
            Family::Other(raw) => {
                self.push_str("\"af_");
                self.push_i64(i64::from(raw));
                self.push(b'"');
            }
        }
    }
}

const fn hex_nibble(value: u8) -> u8 {
    if value < 10 {
        b'0' + value
    } else {
        b'a' + (value - 10)
    }
}

/// Render one trace line, newline included. Allocation-free and panic-free.
#[must_use]
pub fn format_attempt(attempt: &Attempt<'_>) -> LineBuf {
    let mut out = LineBuf::new();
    out.push_str("{\"ts_unix\":");
    out.push_i64(attempt.ts_unix);
    out.push_str(",\"call\":\"");
    out.push_str(attempt.call.label());
    out.push_str("\",\"family\":");
    out.push_family(attempt.family);
    out.push_str(",\"host_or_ip\":");
    out.push_json_string(attempt.host_or_ip, HOST_CAP);
    out.push_str(",\"port\":");
    out.push_i64(i64::from(attempt.port));
    out.push_str(",\"denied\":");
    out.push_str(if attempt.denied { "true" } else { "false" });
    if !attempt.program.is_empty() {
        out.push_str(",\"program\":");
        out.push_json_string(attempt.program, PROGRAM_CAP);
    }
    out.push_str("}\n");
    out
}

/// Render an IPv4/IPv6 address into a caller-owned buffer, returning the used
/// length. IPv6 is written in full (uncompressed) lowercase hex form.
#[must_use]
pub fn render_addr(addr: Addr, out: &mut [u8; 46]) -> usize {
    let mut buf = LineBuf::new();
    match addr {
        Addr::V4(o) => {
            for (i, octet) in o.iter().enumerate() {
                if i > 0 {
                    buf.push(b'.');
                }
                buf.push_i64(i64::from(*octet));
            }
        }
        Addr::V6(o) => {
            for group in 0..8 {
                if group > 0 {
                    buf.push(b':');
                }
                let value = (u16::from(o[group * 2]) << 8) | u16::from(o[group * 2 + 1]);
                let mut started = false;
                for shift in [12u32, 8, 4, 0] {
                    let nibble = u8::try_from((value >> shift) & 0xf).unwrap_or(0);
                    if nibble != 0 || started || shift == 0 {
                        buf.push(hex_nibble(nibble));
                        started = true;
                    }
                }
            }
        }
    }
    let len = buf.len().min(out.len());
    out[..len].copy_from_slice(&buf.as_bytes()[..len]);
    len
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn rendered(attempt: &Attempt<'_>) -> String {
        String::from_utf8(format_attempt(attempt).as_bytes().to_vec()).unwrap()
    }

    #[test]
    fn formats_a_denied_connect_line() {
        let attempt = Attempt {
            ts_unix: 1_760_000_000,
            call: Call::Connect,
            family: Family::Inet,
            host_or_ip: b"203.0.113.1",
            port: 443,
            denied: true,
            program: &[],
        };
        assert_eq!(
            rendered(&attempt),
            "{\"ts_unix\":1760000000,\"call\":\"connect\",\"family\":\"inet\",\
             \"host_or_ip\":\"203.0.113.1\",\"port\":443,\"denied\":true}\n"
        );
    }

    #[test]
    fn formats_a_permitted_loopback_line() {
        let attempt = Attempt {
            ts_unix: 0,
            call: Call::Connect,
            family: Family::Inet6,
            host_or_ip: b"0:0:0:0:0:0:0:1",
            port: 8080,
            denied: false,
            program: &[],
        };
        assert!(rendered(&attempt).contains("\"denied\":false"));
        assert!(rendered(&attempt).contains("\"family\":\"inet6\""));
    }

    #[test]
    fn formats_an_exec_line_with_program() {
        let attempt = Attempt {
            ts_unix: 7,
            call: Call::Exec,
            family: Family::None,
            host_or_ip: &[],
            port: 0,
            denied: false,
            program: b"/opt/bin/plaud",
        };
        assert_eq!(
            rendered(&attempt),
            "{\"ts_unix\":7,\"call\":\"exec\",\"family\":\"none\",\"host_or_ip\":\"\",\
             \"port\":0,\"denied\":false,\"program\":\"/opt/bin/plaud\"}\n"
        );
    }

    #[test]
    fn escapes_quotes_backslashes_and_non_ascii() {
        let attempt = Attempt {
            ts_unix: 1,
            call: Call::Getaddrinfo,
            family: Family::None,
            host_or_ip: b"a\"b\\c\n\xffd",
            port: 0,
            denied: true,
            program: &[],
        };
        let line = rendered(&attempt);
        assert!(
            line.contains(r#""host_or_ip":"a\"b\\c\u000a\u00ffd""#),
            "{line}"
        );
    }

    #[test]
    fn renders_negative_and_extreme_timestamps() {
        let mut attempt = Attempt::new(-1, Call::MonitorReady);
        assert!(rendered(&attempt).starts_with("{\"ts_unix\":-1,"));
        attempt.ts_unix = i64::MIN;
        assert!(rendered(&attempt).starts_with("{\"ts_unix\":-9223372036854775808,"));
    }

    #[test]
    fn line_fits_buffer() {
        // Worst case: every byte of both strings needs a 6-byte escape.
        let host = vec![0x01u8; HOST_CAP * 2];
        let program = vec![0x01u8; PROGRAM_CAP * 2];
        let attempt = Attempt {
            ts_unix: i64::MIN,
            call: Call::Exec,
            family: Family::Other(i32::MIN),
            host_or_ip: &host,
            port: u16::MAX,
            denied: true,
            program: &program,
        };
        let buf = format_attempt(&attempt);
        assert!(
            buf.len() < MAX_LINE,
            "line used {} of {MAX_LINE}",
            buf.len()
        );
        let text = String::from_utf8(buf.as_bytes().to_vec()).unwrap();
        assert!(text.ends_with("}\n"));
        // Caps applied: exactly HOST_CAP and PROGRAM_CAP source bytes survive.
        assert_eq!(text.matches("\\u0001").count(), HOST_CAP + PROGRAM_CAP);
    }

    #[test]
    fn loopback_v4_is_the_whole_127_slash_8() {
        assert!(is_loopback_v4([127, 0, 0, 1]));
        assert!(is_loopback_v4([127, 255, 255, 254]));
        assert!(!is_loopback_v4([128, 0, 0, 1]));
        assert!(!is_loopback_v4([0, 0, 0, 0]));
        assert!(!is_loopback_v4([203, 0, 113, 1]));
        assert!(!is_loopback_v4([10, 0, 0, 1]));
    }

    #[test]
    fn loopback_v6_covers_colon_colon_one_and_mapped_v4() {
        let mut one = [0u8; 16];
        one[15] = 1;
        assert!(is_loopback_v6(one));

        let mut mapped = [0u8; 16];
        mapped[10] = 0xff;
        mapped[11] = 0xff;
        mapped[12] = 127;
        mapped[15] = 1;
        assert!(is_loopback_v6(mapped));

        let mut mapped_public = mapped;
        mapped_public[12] = 203;
        assert!(!is_loopback_v6(mapped_public));

        assert!(!is_loopback_v6([0u8; 16]));
        let mut link_local = [0u8; 16];
        link_local[0] = 0xfe;
        link_local[1] = 0x80;
        assert!(!is_loopback_v6(link_local));
    }

    #[test]
    fn classify_addr_permits_only_loopback() {
        assert_eq!(classify_addr(Addr::V4([127, 0, 0, 1])), Verdict::Permit);
        assert_eq!(classify_addr(Addr::V4([203, 0, 113, 1])), Verdict::Deny);
        let mut one = [0u8; 16];
        one[15] = 1;
        assert_eq!(classify_addr(Addr::V6(one)), Verdict::Permit);
        assert_eq!(classify_addr(Addr::V6([0x20; 16])), Verdict::Deny);
    }

    #[test]
    fn classify_host_permits_localhost_and_ip_loopback_only() {
        assert_eq!(classify_host(b""), Verdict::Permit);
        assert_eq!(classify_host(b"localhost"), Verdict::Permit);
        assert_eq!(classify_host(b"LocalHost."), Verdict::Permit);
        assert_eq!(classify_host(b"db.localhost"), Verdict::Permit);
        assert_eq!(classify_host(b"127.0.0.1"), Verdict::Permit);
        assert_eq!(classify_host(b"127.9.9.9"), Verdict::Permit);
        assert_eq!(classify_host(b"::1"), Verdict::Permit);
        assert_eq!(classify_host(b"::ffff:127.0.0.1"), Verdict::Permit);

        assert_eq!(classify_host(b"openrouter.ai"), Verdict::Deny);
        assert_eq!(classify_host(b"api.plaud.ai"), Verdict::Deny);
        assert_eq!(classify_host(b"203.0.113.1"), Verdict::Deny);
        assert_eq!(classify_host(b"2606:4700::1111"), Verdict::Deny);
        // mDNS is a real network lookup, not loopback.
        assert_eq!(classify_host(b"printer.local"), Verdict::Deny);
        // Lookalikes must not be mistaken for loopback.
        assert_eq!(classify_host(b"localhost.evil.example"), Verdict::Deny);
        assert_eq!(classify_host(b"notlocalhost"), Verdict::Deny);
        assert_eq!(classify_host(b"127.0.0.1.evil.example"), Verdict::Deny);
    }

    #[test]
    fn parses_ip_literals() {
        assert_eq!(parse_ip_literal(b"1.2.3.4"), Some(Addr::V4([1, 2, 3, 4])));
        assert_eq!(parse_ip_literal(b"1.2.3"), None);
        assert_eq!(parse_ip_literal(b"256.0.0.1"), None);
        assert_eq!(parse_ip_literal(b"1.2.3.4.5"), None);
        assert_eq!(parse_ip_literal(b"hello"), None);
        let mut one = [0u8; 16];
        one[15] = 1;
        assert_eq!(parse_ip_literal(b"::1"), Some(Addr::V6(one)));
        assert_eq!(parse_ip_literal(b"::"), Some(Addr::V6([0u8; 16])));
        assert_eq!(
            parse_ip_literal(b"2606:4700:4700:0000:0000:0000:0000:1111"),
            parse_ip_literal(b"2606:4700:4700::1111")
        );
        assert_eq!(parse_ip_literal(b"fe80::1%en0"), None);
    }

    #[test]
    fn renders_addresses() {
        let mut buf = [0u8; 46];
        let len = render_addr(Addr::V4([127, 0, 0, 1]), &mut buf);
        assert_eq!(&buf[..len], b"127.0.0.1");
        let mut one = [0u8; 16];
        one[15] = 1;
        let len = render_addr(Addr::V6(one), &mut buf);
        assert_eq!(&buf[..len], b"0:0:0:0:0:0:0:1");
        let len = render_addr(
            Addr::V6([
                0x26, 0x06, 0x47, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11, 0x11,
            ]),
            &mut buf,
        );
        assert_eq!(&buf[..len], b"2606:4700:0:0:0:0:0:1111");
    }

    #[test]
    fn family_mapping_is_honest_about_unknown_values() {
        assert_eq!(Family::from_raw(af::INET), Family::Inet);
        assert_eq!(Family::from_raw(af::INET6), Family::Inet6);
        assert_eq!(Family::from_raw(af::UNIX), Family::Unix);
        assert_eq!(Family::from_raw(4242), Family::Other(4242));
        assert!(Family::Inet.is_internet() && Family::Inet6.is_internet());
        assert!(!Family::Unix.is_internet() && !Family::Other(4242).is_internet());
    }
}
