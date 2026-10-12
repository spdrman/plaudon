//! D2.2 acceptance: the shared hostile-path corpus, and a lint that keeps
//! `std::fs` confined to `fsguard`.
//!
//! Spec §18.5 ("test hostile filenames and symlinks"), §16.5, §13.4, §13.5.
//!
//! The corpus itself is `tests/fixtures/hostile-paths/names.txt`, a plain data
//! file shared with the D2.4 fuzz target. The *expected verdict* for every line
//! is checked in here, in [`expected_verdicts`]; a corpus line with no verdict
//! fails the suite, so adding a hostile name forces a deliberate decision about
//! what must happen to it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use plaudon_security::fsguard::{
    canonicalize_and_validate, check_component, is_invisible_or_bidi, is_windows_device_name,
    sanitize_filename, validate_archive_entry, ArchiveEntryMeta, ArchivePolicy, PathPolicy,
    PathRejection, DEFAULT_MAX_COMPONENT_LEN,
};

const CORPUS: &str = include_str!("../../../tests/fixtures/hostile-paths/names.txt");

/// Longest path the managed archive ever constructs. The library default is the
/// Windows extended-length ceiling (32767); a real managed root uses something
/// far smaller, and the corpus verdicts are stated against this value so they
/// are identical on every platform.
const MANAGED_MAX_PATH_LEN: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Must be refused.
    Reject,
    /// Must be accepted, and must resolve inside the permitted root. Some of
    /// these look hostile and are inert: we never percent-decode a path, and a
    /// backslash is a directory separator, not part of a name.
    Accept,
}

use Verdict::{Accept, Reject};

/// `(corpus line, verdict for canonicalize_and_validate, verdict for
/// validate_archive_entry)`.
fn expected_verdicts() -> Vec<(String, Verdict, Verdict)> {
    let mut table = owned(&structural_verdicts());
    table.extend(owned(&name_rule_verdicts()));

    // Overlong component (300 bytes) and overlong path (5000 bytes).
    table.push(("a".repeat(300), Reject, Reject));
    table.push(("b/".repeat(2499) + "bb", Reject, Reject));
    table
}

fn owned(rows: &[(&str, Verdict, Verdict)]) -> Vec<(String, Verdict, Verdict)> {
    rows.iter()
        .map(|(line, path, archive)| ((*line).to_owned(), *path, *archive))
        .collect()
}

/// Lines whose verdict follows from the *shape* of the path: traversal,
/// anchoring, and the Windows namespaces.
fn structural_verdicts() -> Vec<(&'static str, Verdict, Verdict)> {
    vec![
        // relative traversal
        ("..", Reject, Reject),
        ("../secret.txt", Reject, Reject),
        ("../../../../etc/passwd", Reject, Reject),
        ("..\\\\windows\\\\system32\\\\config", Reject, Reject),
        ("..\\\\..\\\\..\\\\boot.ini", Reject, Reject),
        ("....//etc/passwd", Reject, Reject),
        ("media/../../etc/passwd", Reject, Reject),
        ("media/./../../etc/passwd", Reject, Reject),
        ("a/b/../../../outside.txt", Reject, Reject),
        (".", Reject, Reject),
        ("./media/recording.m4a", Reject, Reject),
        ("media/.", Reject, Reject),
        ("media/..", Reject, Reject),
        // percent-encoded traversal is inert: nothing in plaudon ever decodes a
        // path, so these are ordinary (ugly) single components.
        ("%2e%2e%2fetc%2fpasswd", Accept, Accept),
        ("..%2f..%2fetc%2fpasswd", Accept, Accept),
        ("%252e%252e%252fetc", Accept, Accept),
        // absolute
        ("/etc/passwd", Reject, Reject),
        ("/", Reject, Reject),
        (
            "/Users/someone/Library/Keychains/login.keychain-db",
            Reject,
            Reject,
        ),
        (
            "C:\\\\Windows\\\\System32\\\\drivers\\\\etc\\\\hosts",
            Reject,
            Reject,
        ),
        ("C:/Windows/System32/x", Reject, Reject),
        ("c:\\\\x", Reject, Reject),
        ("\\\\etc\\\\passwd", Reject, Reject),
        // UNC and device namespace
        ("\\\\\\\\server\\\\share\\\\x", Reject, Reject),
        ("\\\\\\\\server\\\\share", Reject, Reject),
        ("\\\\\\\\?\\\\C:\\\\x", Reject, Reject),
        ("\\\\\\\\?\\\\UNC\\\\server\\\\share\\\\x", Reject, Reject),
        ("\\\\\\\\.\\\\COM1", Reject, Reject),
        ("\\\\\\\\.\\\\PhysicalDrive0", Reject, Reject),
    ]
}

/// Lines whose verdict follows from a *naming* rule: reserved device names and
/// characters, invisible characters, normalisation and length.
fn name_rule_verdicts() -> Vec<(&'static str, Verdict, Verdict)> {
    vec![
        // Windows reserved device names
        ("CON", Reject, Reject),
        ("PRN", Reject, Reject),
        ("AUX", Reject, Reject),
        ("NUL", Reject, Reject),
        ("COM1", Reject, Reject),
        ("COM9", Reject, Reject),
        ("LPT1", Reject, Reject),
        ("LPT9", Reject, Reject),
        ("CoN.TXT", Reject, Reject),
        ("nul.mp3", Reject, Reject),
        ("com1.tar.gz", Reject, Reject),
        ("lpt3.", Reject, Reject),
        ("aux.wav", Reject, Reject),
        ("media/CON/notes.txt", Reject, Reject),
        // trailing dot / leading or trailing space
        ("report.", Reject, Reject),
        ("report ", Reject, Reject),
        ("notes...", Reject, Reject),
        ("notes   ", Reject, Reject),
        ("folder /recording.m4a", Reject, Reject),
        ("folder./recording.m4a", Reject, Reject),
        (" leading-space.txt", Reject, Reject),
        // alternate data streams and reserved characters
        ("file.txt:hidden", Reject, Reject),
        ("file.txt:$DATA", Reject, Reject),
        ("notes.m4a:Zone.Identifier", Reject, Reject),
        ("C:file.txt", Reject, Reject),
        ("what?.txt", Reject, Reject),
        ("a<b>c.txt", Reject, Reject),
        ("pipe|name.mp3", Reject, Reject),
        ("quote\".txt", Reject, Reject),
        ("star*.wav", Reject, Reject),
        // A backslash is a separator on Windows, so this is the two-component
        // path `back/slash.txt` and stays inside the root. As a single
        // *component* it is refused; `sanitize_filename` replaces the slash.
        ("back\\\\slash.txt", Accept, Accept),
        // control characters
        ("bell\\u{7}.txt", Reject, Reject),
        ("tab\\there.txt", Reject, Reject),
        ("line\\nbreak.txt", Reject, Reject),
        ("cr\\rreturn.txt", Reject, Reject),
        ("nul\\0byte.txt", Reject, Reject),
        ("del\\u{7F}.txt", Reject, Reject),
        ("esc\\u{1B}[31m.txt", Reject, Reject),
        // bidi, isolates, zero-width and other invisibles
        ("exploit\\u{202E}gnp.exe", Reject, Reject),
        ("invoice\\u{202D}txt.exe", Reject, Reject),
        ("\\u{202A}embedded.txt", Reject, Reject),
        ("isolate\\u{2066}spoof\\u{2069}.txt", Reject, Reject),
        ("rtl\\u{2067}name.m4a", Reject, Reject),
        ("zero\\u{200B}width.txt", Reject, Reject),
        ("joiner\\u{200D}name.txt", Reject, Reject),
        ("bom\\u{FEFF}file.txt", Reject, Reject),
        ("mark\\u{200E}name.txt", Reject, Reject),
        ("soft\\u{00AD}hyphen.txt", Reject, Reject),
        // Confusables are legitimate characters. They are accepted and flagged
        // by `SanitizedName::mixed_scripts`, never rewritten.
        ("p\\u{430}ypal-invoice.pdf", Accept, Accept),
        ("\\u{430}ccount.m4a", Accept, Accept),
        ("micr\\u{43E}soft-update.exe", Accept, Accept),
        // NFC / NFD pairs
        ("caf\\u{E9}.txt", Accept, Accept),
        ("cafe\\u{301}.txt", Accept, Accept),
        ("\\u{1E9B}\\u{323}.txt", Accept, Accept),
        ("\\u{212B}ngstrom.txt", Accept, Accept),
        ("\\u{C5}ngstrom.txt", Accept, Accept),
        // legitimate names
        ("Meeting Notes 2026-03-04.m4a", Accept, Accept),
        ("standup_recording.wav", Accept, Accept),
        ("project.v2.final.md", Accept, Accept),
        ("r\\u{E9}sum\\u{E9}.pdf", Accept, Accept),
        ("\\u{4F1A}\\u{8B70}\\u{30E1}\\u{30E2}.m4a", Accept, Accept),
        (
            "\\u{41F}\\u{440}\\u{438}\\u{432}\\u{435}\\u{442} \\u{43C}\\u{438}\\u{440}.mp3",
            Accept,
            Accept,
        ),
        ("media/sha256/ab/abcdef0123456789.m4a", Accept, Accept),
        ("meeting \\u{1F399}\\u{FE0F} notes.m4a", Accept, Accept),
        ("a\\u{301}\\u{302}\\u{303}-combining.txt", Accept, Accept),
        ("1099-form (final).pdf", Accept, Accept),
        ("-rf.txt", Accept, Accept),
    ]
}

// ---------------------------------------------------------------------------
// corpus loading
// ---------------------------------------------------------------------------

/// Decode one corpus line. The grammar is documented at the top of the corpus
/// file; an unknown escape is a hard error so a typo cannot silently become a
/// different (weaker) test case.
fn decode_corpus_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('u') => {
                assert_eq!(
                    chars.next(),
                    Some('{'),
                    "\\u must be followed by {{ in {line:?}"
                );
                let mut hex = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(digit) => hex.push(digit),
                        None => panic!("unterminated \\u{{...}} in {line:?}"),
                    }
                }
                let code = u32::from_str_radix(&hex, 16)
                    .unwrap_or_else(|_| panic!("bad \\u{{{hex}}} in {line:?}"));
                out.push(
                    char::from_u32(code)
                        .unwrap_or_else(|| panic!("not a scalar value: \\u{{{hex}}}")),
                );
            }
            other => panic!("unknown escape \\{other:?} in corpus line {line:?}"),
        }
    }
    out
}

/// Every data line of the corpus, in file order, still escaped.
fn corpus_lines() -> Vec<&'static str> {
    CORPUS
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

#[test]
fn corpus_is_well_formed_and_every_line_has_a_checked_in_verdict() {
    let lines = corpus_lines();
    assert!(
        lines.len() >= 90,
        "corpus shrank unexpectedly: {} lines",
        lines.len()
    );

    let unique: BTreeSet<&str> = lines.iter().copied().collect();
    assert_eq!(
        unique.len(),
        lines.len(),
        "the corpus contains a duplicate line"
    );

    let verdicts: BTreeMap<String, (Verdict, Verdict)> = expected_verdicts()
        .into_iter()
        .map(|(line, path, archive)| (line, (path, archive)))
        .collect();

    let missing: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| !verdicts.contains_key(*line))
        .collect();
    assert!(
        missing.is_empty(),
        "corpus lines without a checked-in verdict: {missing:?}"
    );

    let stale: Vec<&String> = verdicts
        .keys()
        .filter(|line| !unique.contains(line.as_str()))
        .collect();
    assert!(
        stale.is_empty(),
        "verdicts for lines that are no longer in the corpus: {stale:?}"
    );

    // Decoding must succeed for every line, and must actually decode.
    assert_eq!(decode_corpus_line("a\\u{202E}b"), "a\u{202E}b");
    assert_eq!(decode_corpus_line("x\\\\y"), "x\\y");
    assert_eq!(decode_corpus_line("n\\0"), "n\0");
    for line in &lines {
        let _ = decode_corpus_line(line);
    }
}

// ---------------------------------------------------------------------------
// the table test
// ---------------------------------------------------------------------------

#[test]
fn every_corpus_name_gets_its_checked_in_path_verdict() {
    let root = tempfile::tempdir().expect("temp root");
    let canonical_root = fs::canonicalize(root.path()).expect("canonical root");
    let policy =
        PathPolicy::for_roots([root.path().to_path_buf()]).with_max_path_len(MANAGED_MAX_PATH_LEN);

    for (line, expected, _) in expected_verdicts() {
        let decoded = decode_corpus_line(&line);
        let outcome = canonicalize_and_validate(&policy, Path::new(&decoded));
        match (expected, &outcome) {
            (Reject, Ok(accepted)) => panic!(
                "corpus line {line:?} must be rejected, was accepted as {}",
                accepted.as_path().display()
            ),
            (Accept, Err(error)) => {
                panic!("corpus line {line:?} must be accepted, was rejected: {error}")
            }
            (Accept, Ok(accepted)) => assert!(
                accepted.as_path().starts_with(&canonical_root),
                "corpus line {line:?} escaped the root: {}",
                accepted.as_path().display()
            ),
            (Reject, Err(_)) => {}
        }
    }
}

#[test]
fn every_corpus_name_gets_its_checked_in_archive_verdict() {
    let destination = tempfile::tempdir().expect("destination");
    let before = directory_entries(destination.path());

    for (line, _, expected) in expected_verdicts() {
        let decoded = decode_corpus_line(&line);
        // A fresh policy per entry: this test is about one entry's rules, not
        // about the duplicate/bomb accumulators (covered in the unit tests).
        let mut policy =
            ArchivePolicy::new(destination.path()).with_max_path_len(MANAGED_MAX_PATH_LEN);
        let outcome =
            validate_archive_entry(&ArchiveEntryMeta::file(decoded, 1024, 512), &mut policy);
        match (expected, &outcome) {
            (Reject, Ok(entry)) => panic!(
                "corpus line {line:?} must be rejected, was accepted as {}",
                entry.relative_path().display()
            ),
            (Accept, Err(error)) => {
                panic!("corpus line {line:?} must be accepted, was rejected: {error}")
            }
            (Accept, Ok(entry)) => {
                let target = destination.path().join(entry.relative_path());
                assert!(
                    target.starts_with(destination.path()),
                    "corpus line {line:?} escaped the destination"
                );
                assert!(!entry.relative_path().is_absolute());
            }
            (Reject, Err(_)) => {}
        }
    }

    assert_eq!(
        directory_entries(destination.path()),
        before,
        "archive validation must not touch the destination"
    );
}

#[test]
fn sanitize_filename_makes_every_corpus_name_safe_to_put_on_disk() {
    let root = tempfile::tempdir().expect("temp root");
    let canonical_root = fs::canonicalize(root.path()).expect("canonical root");
    let policy = PathPolicy::for_roots([root.path().to_path_buf()]);

    for line in corpus_lines() {
        let decoded = decode_corpus_line(line);
        let sanitized = sanitize_filename(&decoded);

        let on_disk = sanitized.on_disk();
        assert!(!on_disk.is_empty(), "{line:?} sanitised to nothing");
        check_component(on_disk, DEFAULT_MAX_COMPONENT_LEN).unwrap_or_else(|error| {
            panic!("{line:?} -> {on_disk:?} is not a safe component: {error}")
        });
        assert!(!is_windows_device_name(on_disk), "{line:?} -> {on_disk:?}");
        assert!(on_disk.len() <= 200, "{line:?} -> {} bytes", on_disk.len());
        assert!(
            !on_disk.contains('/') && !on_disk.contains('\\'),
            "{line:?} -> {on_disk:?}"
        );

        // The sanitised name is usable as a single component inside the root.
        let validated = canonicalize_and_validate(&policy, Path::new(on_disk))
            .unwrap_or_else(|error| panic!("{line:?} -> {on_disk:?} failed validation: {error}"));
        assert!(validated.as_path().starts_with(&canonical_root));

        // Sanitising twice changes nothing more.
        assert_eq!(
            sanitize_filename(on_disk).on_disk(),
            on_disk,
            "not a fixed point: {line:?}"
        );

        // The display form can never carry a spoofing character.
        let display = sanitized.display();
        assert!(!display.is_empty(), "{line:?} has an empty display name");
        assert!(
            !display
                .chars()
                .any(|ch| ch.is_control() || is_invisible_or_bidi(ch)),
            "{line:?} -> display {display:?} is not safe"
        );
    }
}

#[test]
fn nfc_and_nfd_corpus_pairs_collapse_to_the_same_on_disk_name() {
    let composed = sanitize_filename(&decode_corpus_line("caf\\u{E9}.txt"));
    let decomposed = sanitize_filename(&decode_corpus_line("cafe\\u{301}.txt"));
    assert_eq!(composed.on_disk(), decomposed.on_disk());

    let sign = sanitize_filename(&decode_corpus_line("\\u{212B}ngstrom.txt"));
    let letter = sanitize_filename(&decode_corpus_line("\\u{C5}ngstrom.txt"));
    assert_eq!(sign.on_disk(), letter.on_disk());
}

#[test]
fn confusable_corpus_names_are_accepted_but_flagged() {
    for line in [
        "p\\u{430}ypal-invoice.pdf",
        "\\u{430}ccount.m4a",
        "micr\\u{43E}soft-update.exe",
    ] {
        let sanitized = sanitize_filename(&decode_corpus_line(line));
        assert!(
            sanitized.mixed_scripts(),
            "{line:?} should be flagged as mixed-script"
        );
    }
    for line in [
        "Meeting Notes 2026-03-04.m4a",
        "\\u{41F}\\u{440}\\u{438}\\u{432}\\u{435}\\u{442} \\u{43C}\\u{438}\\u{440}.mp3",
    ] {
        let sanitized = sanitize_filename(&decode_corpus_line(line));
        assert!(!sanitized.mixed_scripts(), "{line:?} should not be flagged");
    }
}

#[test]
fn overlong_names_are_refused_for_the_documented_reason() {
    let root = tempfile::tempdir().expect("temp root");
    let policy =
        PathPolicy::for_roots([root.path().to_path_buf()]).with_max_path_len(MANAGED_MAX_PATH_LEN);

    let long_component = "a".repeat(300);
    assert_eq!(
        canonicalize_and_validate(&policy, Path::new(&long_component))
            .unwrap_err()
            .reason(),
        PathRejection::ComponentTooLong
    );

    let long_path = "b/".repeat(2499) + "bb";
    assert_eq!(long_path.len(), 5000);
    assert_eq!(
        canonicalize_and_validate(&policy, Path::new(&long_path))
            .unwrap_err()
            .reason(),
        PathRejection::PathTooLong
    );

    // The same path under a portable-backup policy is refused earlier, for
    // legacy Windows `MAX_PATH` (§13.6 macOS <-> Windows round trip).
    let portable =
        PathPolicy::for_roots([root.path().to_path_buf()]).with_windows_short_path_compatible(true);
    assert_eq!(
        canonicalize_and_validate(&portable, Path::new(&long_path))
            .unwrap_err()
            .reason(),
        PathRejection::NotWindowsShortPathCompatible
    );
}

fn directory_entries(root: &Path) -> BTreeSet<PathBuf> {
    let mut found = BTreeSet::new();
    let Ok(entries) = fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        found.insert(entry.path());
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            found.extend(directory_entries(&entry.path()));
        }
    }
    found
}

// ---------------------------------------------------------------------------
// lint: direct std::fs use outside fsguard
// ---------------------------------------------------------------------------

/// Call sites outside `fsguard` that may use `std::fs` directly, with the
/// reason each one is safe. Anything not on this list fails the test.
///
/// The rule this enforces: a path that came from outside this process reaches
/// the filesystem only through [`canonicalize_and_validate`], which returns a
/// `ValidatedPath` that cannot be forged. A module that only ever touches paths
/// it built itself from compile-time constants is not part of that threat
/// model, and that is what an entry here asserts.
/// `#[cfg(test)]` modules (`*/tests.rs`) are outside the rule by construction:
/// they only ever touch directories they created themselves under the OS temp
/// directory, and they are not compiled into the application.
const STD_FS_ALLOWLIST: &[(&str, &str)] = &[
    (
        "src/bin/tauri_audit.rs",
        "developer-facing audit binary: reads the Tauri manifests and writes the \
         report to the paths named on its own command line, never an app path",
    ),
    (
        "src/bin/sandbox_probe.rs",
        "the sandbox probe helper: deliberately attempts reads and writes at the \
         paths its test driver sends it, so the suite can observe the OS refusing \
         them. Mediating those calls would remove the thing being measured",
    ),
    (
        "src/tauri_guard/audit.rs",
        "reads Tauri configuration, capability manifests and Rust sources from the \
         repository being audited; developer tooling over build inputs, not over \
         user data",
    ),
    (
        "src/sandbox/unix.rs",
        "reads /proc/<pid>/statm, a kernel pseudo-file whose path is built from an \
         integer process id and contains no externally supplied component",
    ),
];

#[test]
fn std_fs_is_confined_to_fsguard() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rust_files(&src, &mut files);
    assert!(
        files.len() > 3,
        "expected to find this crate's sources under {}",
        src.display()
    );

    let allowed: BTreeSet<&str> = STD_FS_ALLOWLIST.iter().map(|(path, _)| *path).collect();
    let mut offenders: Vec<String> = Vec::new();

    for file in files {
        let relative = file.strip_prefix(&src).map_or_else(
            |_| file.display().to_string(),
            |rest| format!("src/{}", rest.display()),
        );
        if relative == "src/fsguard.rs" || relative.starts_with("src/fsguard/") {
            continue;
        }
        if relative.ends_with("/tests.rs") || relative.contains("/tests/") {
            continue;
        }
        if allowed.contains(relative.as_str()) {
            continue;
        }
        let contents = fs::read_to_string(&file).expect("read source");
        for (number, line) in direct_fs_uses(&contents) {
            offenders.push(format!("{relative}:{number}: {line}"));
        }
    }

    assert!(
        offenders.is_empty(),
        "direct std::fs use outside fsguard:\n  {}\n\n\
         Route the path through `fsguard::canonicalize_and_validate` and take a \
         `ValidatedPath`, or add the call site to STD_FS_ALLOWLIST in this file \
         with the reason it is safe.",
        offenders.join("\n  ")
    );
}

/// Lines that reach for `std::fs` directly. Platform extension traits
/// (`std::os::unix::fs::PermissionsExt` and friends) are not filesystem access
/// and are not reported.
fn direct_fs_uses(contents: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let mut rest = line;
        while let Some(at) = rest.find("fs::") {
            let before = &rest[..at];
            let is_platform_extension =
                before.ends_with("os::unix::") || before.ends_with("os::windows::");
            if !is_platform_extension {
                found.push((index + 1, line.trim().to_owned()));
                break;
            }
            rest = &rest[at + "fs::".len()..];
        }
        if line.contains("use std::fs;") && found.last().map(|(n, _)| *n) != Some(index + 1) {
            found.push((index + 1, line.trim().to_owned()));
        }
    }
    found
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}
