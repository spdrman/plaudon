//! File associations and the `plaudon://` URL scheme, as data and as artifacts.
//!
//! Plaudon claims exactly two things from the operating system:
//!
//! * the `.plaudon` portable-archive extension (the backup/restore format), and
//! * the `plaudon://` URL scheme, which is how a notification click, a browser
//!   link or a second launch asks the running app to open something
//!   ([`crate::activation`]).
//!
//! The registration itself is **install-time work, not runtime work**. On
//! macOS the claim is a static part of `Info.plist` inside the `.app` bundle;
//! Launch Services reads it when the bundle is installed. On Windows the claim
//! lives under `HKEY_CURRENT_USER\Software\Classes` and is written by the
//! **installer** (and removed by the uninstaller) — the running application
//! never writes those keys, because an app that silently re-claims extensions
//! every time it starts is a thing users rightly complain about, and because
//! writing them would need elevation on a per-machine install.
//!
//! So this module generates the two artifacts the packaging task (EPIC O2.5)
//! has to ship, and a golden test keeps the committed copies under
//! `crates/platform/resources/` equal to what the generators produce:
//!
//! | Artifact | Generator | Consumed by |
//! | --- | --- | --- |
//! | `resources/Info.associations.plist` | [`macos_info_plist_fragment`] | merged into the bundle's `Info.plist` |
//! | `resources/file-associations.reg` | [`windows_registry_script`] | imported by the Windows installer |
//!
//! The `.reg` file is deliberately pure ASCII so that it is valid whether the
//! installer feeds it to `reg import` as written or converts it to UTF-16LE
//! first; it carries the [`EXECUTABLE_PLACEHOLDER`] rather than a path, and the
//! installer substitutes the real executable through
//! [`windows_registry_script_with_executable`].

use std::fmt::Write as _;

/// Extension of the portable archive, without a leading dot.
pub const ARCHIVE_EXTENSION: &str = "plaudon";

/// Uniform Type Identifier exported for the portable archive on macOS.
pub const ARCHIVE_UTI: &str = "app.plaudon.archive";

/// MIME type exported for the portable archive.
pub const ARCHIVE_MIME_TYPE: &str = "application/vnd.plaudon.archive";

/// Human-readable name of the archive document type.
pub const ARCHIVE_TYPE_NAME: &str = "Plaudon portable archive";

/// URL scheme Plaudon claims, without the `://`.
pub const URL_SCHEME: &str = "plaudon";

/// Bundle/application identifier the claim is registered under.
///
/// The same identifier as the data directory, so an installation keeps its
/// Launch Services registration across the Tauri-to-egui cutover.
pub const BUNDLE_IDENTIFIER: &str = plaudon_app::runtime::APPLICATION_IDENTIFIER;

/// Windows ProgID of the archive document type.
pub const WINDOWS_PROG_ID: &str = "Plaudon.Archive";

/// Token the installer replaces with the absolute path of the installed
/// executable before importing the registry script.
pub const EXECUTABLE_PLACEHOLDER: &str = "%EXEPATH%";

/// Committed copy of the macOS fragment, relative to the crate root.
pub const MACOS_ARTIFACT_PATH: &str = "crates/platform/resources/Info.associations.plist";

/// Committed copy of the Windows registry script, relative to the repository root.
pub const WINDOWS_ARTIFACT_PATH: &str = "crates/platform/resources/file-associations.reg";

/// The `CFBundleDocumentTypes`, `UTExportedTypeDeclarations` and
/// `CFBundleURLTypes` claims, as a standalone property list.
///
/// A complete `plist` document rather than a bare key/value run: packaging can
/// then merge it with `/usr/libexec/PlistBuddy -c "Merge …"` or parse it with
/// `plutil`, which is not possible with a fragment that is not valid XML on
/// its own. The three top-level keys are copied verbatim into the bundle's
/// `Info.plist`.
#[must_use]
pub fn macos_info_plist_fragment() -> String {
    let mut out = String::with_capacity(2048);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
    );
    out.push_str("<plist version=\"1.0\">\n<dict>\n");

    out.push_str("\t<key>CFBundleDocumentTypes</key>\n\t<array>\n\t\t<dict>\n");
    let _ = writeln!(
        out,
        "\t\t\t<key>CFBundleTypeName</key>\n\t\t\t<string>{ARCHIVE_TYPE_NAME}</string>"
    );
    out.push_str("\t\t\t<key>CFBundleTypeRole</key>\n\t\t\t<string>Editor</string>\n");
    out.push_str("\t\t\t<key>LSHandlerRank</key>\n\t\t\t<string>Owner</string>\n");
    let _ = writeln!(
        out,
        "\t\t\t<key>LSItemContentTypes</key>\n\t\t\t<array>\n\t\t\t\t<string>{ARCHIVE_UTI}</string>\n\t\t\t</array>"
    );
    out.push_str("\t\t</dict>\n\t</array>\n");

    out.push_str("\t<key>UTExportedTypeDeclarations</key>\n\t<array>\n\t\t<dict>\n");
    let _ = writeln!(
        out,
        "\t\t\t<key>UTTypeIdentifier</key>\n\t\t\t<string>{ARCHIVE_UTI}</string>"
    );
    let _ = writeln!(
        out,
        "\t\t\t<key>UTTypeDescription</key>\n\t\t\t<string>{ARCHIVE_TYPE_NAME}</string>"
    );
    out.push_str(
        "\t\t\t<key>UTTypeConformsTo</key>\n\t\t\t<array>\n\t\t\t\t<string>public.data</string>\n\t\t\t</array>\n",
    );
    out.push_str("\t\t\t<key>UTTypeTagSpecification</key>\n\t\t\t<dict>\n");
    let _ = writeln!(
        out,
        "\t\t\t\t<key>public.filename-extension</key>\n\t\t\t\t<array>\n\t\t\t\t\t<string>{ARCHIVE_EXTENSION}</string>\n\t\t\t\t</array>"
    );
    let _ = writeln!(
        out,
        "\t\t\t\t<key>public.mime-type</key>\n\t\t\t\t<array>\n\t\t\t\t\t<string>{ARCHIVE_MIME_TYPE}</string>\n\t\t\t\t</array>"
    );
    out.push_str("\t\t\t</dict>\n\t\t</dict>\n\t</array>\n");

    out.push_str("\t<key>CFBundleURLTypes</key>\n\t<array>\n\t\t<dict>\n");
    let _ = writeln!(
        out,
        "\t\t\t<key>CFBundleURLName</key>\n\t\t\t<string>{BUNDLE_IDENTIFIER}</string>"
    );
    out.push_str("\t\t\t<key>CFBundleTypeRole</key>\n\t\t\t<string>Viewer</string>\n");
    let _ = writeln!(
        out,
        "\t\t\t<key>CFBundleURLSchemes</key>\n\t\t\t<array>\n\t\t\t\t<string>{URL_SCHEME}</string>\n\t\t\t</array>"
    );
    out.push_str("\t\t</dict>\n\t</array>\n");

    out.push_str("</dict>\n</plist>\n");
    out
}

/// The install-time registry script, carrying [`EXECUTABLE_PLACEHOLDER`].
///
/// Everything is written under `HKEY_CURRENT_USER\Software\Classes`, which is
/// the per-user hive: a per-user install needs no elevation, and an
/// uninstaller can remove exactly what it wrote. `OpenWithProgids` is used
/// rather than a hard default so Windows' own "choose a default app" flow
/// stays in charge of what actually opens a `.plaudon` file.
#[must_use]
pub fn windows_registry_script() -> String {
    let exe = EXECUTABLE_PLACEHOLDER;
    let mut out = String::with_capacity(1024);
    out.push_str("Windows Registry Editor Version 5.00\r\n\r\n");
    out.push_str("; Plaudon file associations. Written at install time by the\r\n");
    out.push_str("; installer and removed by the uninstaller; the running\r\n");
    out.push_str("; application never writes these keys.\r\n");
    out.push_str(
        "; Generated by plaudon-platform; edit crates/platform/src/associations.rs,\r\n; not this file.\r\n\r\n",
    );

    let _ = write!(
        out,
        "[HKEY_CURRENT_USER\\Software\\Classes\\.{ARCHIVE_EXTENSION}]\r\n@=\"{WINDOWS_PROG_ID}\"\r\n\r\n"
    );
    let _ = write!(
        out,
        "[HKEY_CURRENT_USER\\Software\\Classes\\.{ARCHIVE_EXTENSION}\\OpenWithProgids]\r\n\"{WINDOWS_PROG_ID}\"=\"\"\r\n\r\n"
    );
    let _ = write!(
        out,
        "[HKEY_CURRENT_USER\\Software\\Classes\\{WINDOWS_PROG_ID}]\r\n@=\"{ARCHIVE_TYPE_NAME}\"\r\n\r\n"
    );
    let _ = write!(
        out,
        "[HKEY_CURRENT_USER\\Software\\Classes\\{WINDOWS_PROG_ID}\\DefaultIcon]\r\n@=\"\\\"{}\\\",0\"\r\n\r\n",
        reg_escape(exe)
    );
    let _ = write!(
        out,
        "[HKEY_CURRENT_USER\\Software\\Classes\\{WINDOWS_PROG_ID}\\shell\\open\\command]\r\n@=\"\\\"{}\\\" \\\"%1\\\"\"\r\n\r\n",
        reg_escape(exe)
    );
    let _ = write!(
        out,
        "[HKEY_CURRENT_USER\\Software\\Classes\\{URL_SCHEME}]\r\n@=\"URL:Plaudon Protocol\"\r\n\"URL Protocol\"=\"\"\r\n\r\n"
    );
    let _ = write!(
        out,
        "[HKEY_CURRENT_USER\\Software\\Classes\\{URL_SCHEME}\\DefaultIcon]\r\n@=\"\\\"{}\\\",0\"\r\n\r\n",
        reg_escape(exe)
    );
    let _ = write!(
        out,
        "[HKEY_CURRENT_USER\\Software\\Classes\\{URL_SCHEME}\\shell\\open\\command]\r\n@=\"\\\"{}\\\" \\\"%1\\\"\"\r\n",
        reg_escape(exe)
    );
    out
}

/// The registry script with the placeholder replaced by a real executable path.
///
/// The path is escaped for the `.reg` string syntax, so a normal Windows path
/// (`C:\Program Files\Plaudon\plaudon.exe`) survives the round trip. This is
/// the function an installer build step calls; the committed artifact keeps
/// the placeholder because the install location is not known at build time.
#[must_use]
pub fn windows_registry_script_with_executable(executable: &str) -> String {
    windows_registry_script().replace(&reg_escape(EXECUTABLE_PLACEHOLDER), &reg_escape(executable))
}

/// Escape a value for the double-quoted string syntax of a `.reg` file.
///
/// `regedit` treats `\` and `"` as escapes inside a value, so both have to be
/// doubled; nothing else is special. The surrounding quotes are *not* added
/// here, because a value is often a quoted path embedded in a longer value
/// (`"C:\\app.exe",0`). Control characters cannot appear in a Windows path and
/// are dropped rather than emitted raw, so a hostile argument can never
/// terminate the line and inject a second key.
#[must_use]
pub fn reg_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed artifacts, compiled in so the golden test needs no path
    /// resolution at run time.
    const COMMITTED_PLIST: &str = include_str!("../resources/Info.associations.plist");
    const COMMITTED_REG: &str = include_str!("../resources/file-associations.reg");

    #[test]
    fn the_committed_plist_is_what_the_generator_produces() {
        assert_eq!(
            macos_info_plist_fragment(),
            COMMITTED_PLIST,
            "regenerate {MACOS_ARTIFACT_PATH} from associations::macos_info_plist_fragment()"
        );
    }

    #[test]
    fn the_committed_registry_script_is_what_the_generator_produces() {
        assert_eq!(
            windows_registry_script(),
            COMMITTED_REG,
            "regenerate {WINDOWS_ARTIFACT_PATH} from associations::windows_registry_script()"
        );
    }

    #[test]
    fn the_plist_claims_both_the_document_type_and_the_url_scheme() {
        let plist = macos_info_plist_fragment();
        for expected in [
            "<key>CFBundleDocumentTypes</key>",
            "<key>UTExportedTypeDeclarations</key>",
            "<key>CFBundleURLTypes</key>",
            ARCHIVE_UTI,
            ARCHIVE_MIME_TYPE,
            ARCHIVE_EXTENSION,
            URL_SCHEME,
            BUNDLE_IDENTIFIER,
        ] {
            assert!(plist.contains(expected), "plist is missing {expected}");
        }
        assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"));
        assert!(plist.ends_with("</plist>\n"));
    }

    #[test]
    fn the_plist_tags_are_balanced() {
        let plist = macos_info_plist_fragment();
        for tag in ["dict", "array", "key", "string"] {
            let open = plist.matches(&format!("<{tag}>")).count();
            let close = plist.matches(&format!("</{tag}>")).count();
            assert_eq!(open, close, "unbalanced <{tag}> in the generated plist");
        }
    }

    #[test]
    fn the_registry_script_is_ascii_with_crlf_endings() {
        let reg = windows_registry_script();
        assert!(reg.is_ascii(), "the .reg artifact must stay pure ASCII");
        assert!(reg.starts_with("Windows Registry Editor Version 5.00\r\n"));
        for line in reg.split("\r\n") {
            assert!(
                !line.contains('\n'),
                "a bare LF leaked into the .reg artifact"
            );
        }
    }

    #[test]
    fn the_registry_script_claims_the_extension_the_progid_and_the_scheme() {
        let reg = windows_registry_script();
        for expected in [
            "[HKEY_CURRENT_USER\\Software\\Classes\\.plaudon]",
            "[HKEY_CURRENT_USER\\Software\\Classes\\.plaudon\\OpenWithProgids]",
            "[HKEY_CURRENT_USER\\Software\\Classes\\Plaudon.Archive\\shell\\open\\command]",
            "[HKEY_CURRENT_USER\\Software\\Classes\\plaudon]",
            "\"URL Protocol\"=\"\"",
            "[HKEY_CURRENT_USER\\Software\\Classes\\plaudon\\shell\\open\\command]",
            EXECUTABLE_PLACEHOLDER,
        ] {
            assert!(reg.contains(expected), ".reg is missing {expected}");
        }
        assert!(
            !reg.contains("HKEY_LOCAL_MACHINE"),
            "associations are per-user; nothing may be written to HKLM"
        );
    }

    #[test]
    fn substituting_the_executable_escapes_backslashes() {
        let script =
            windows_registry_script_with_executable("C:\\Program Files\\Plaudon\\plaudon.exe");
        assert!(
            script.contains(
                "@=\"\\\"C:\\\\Program Files\\\\Plaudon\\\\plaudon.exe\\\" \\\"%1\\\"\"\r\n"
            ),
            "the open command is not escaped as a .reg string: {script}"
        );
        assert!(!script.contains(EXECUTABLE_PLACEHOLDER));
        assert!(
            script.contains("\\\"%1\\\""),
            "the %1 argument placeholder must survive substitution"
        );
    }

    #[test]
    fn an_injected_quote_or_newline_cannot_escape_a_registry_value() {
        let hostile = reg_escape("C:\\x\"\r\n[HKEY_LOCAL_MACHINE\\Evil]");
        assert_eq!(hostile, "C:\\\\x\\\"[HKEY_LOCAL_MACHINE\\\\Evil]");
        assert!(!hostile.contains('\r'));
        assert!(!hostile.contains('\n'));
    }

    #[test]
    fn the_identifier_matches_the_application_layer() {
        assert_eq!(BUNDLE_IDENTIFIER, "app.plaudon.desktop");
    }
}
