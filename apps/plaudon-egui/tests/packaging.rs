//! What the packaging inputs promise, asserted without a packager.
//!
//! `scripts/package-macos-egui.sh` and `scripts/package-windows-egui.ps1`
//! each run on one operating system and neither can run in CI's Linux legs,
//! so the facts that decide whether an installed Plaudon works — the bundle
//! identifier that picks the library directory (migration-plan risk R7), the
//! entitlement set, the registry claim, the icon the OS actually gets — are
//! asserted here, from the committed inputs, on every platform.
//!
//! The one rule these tests exist to enforce: the Windows installer's
//! registry writes are the *same* claim that
//! `plaudon_platform::associations::windows_registry_script` generates. NSIS
//! cannot `reg import` a file whose paths are only known at install time, so
//! the keys are written twice — once as the committed `.reg` artifact, once
//! as `WriteRegStr` lines. This test is what stops the two from drifting.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use plaudon_platform::associations;

/// The installed executable the NSIS script substitutes for `%EXEPATH%`.
const INSTALLED_EXECUTABLE: &str = r"$INSTDIR\plaudon-egui.exe";

fn packaging_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging")
}

fn read(relative: &str) -> String {
    let path = packaging_dir().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

// ---------------------------------------------------------------- macOS ----

/// The value of a `<key>`/`<string>` pair in a flat plist dictionary.
fn plist_string(plist: &str, key: &str) -> Option<String> {
    let marker = format!("<key>{key}</key>");
    let after = plist.split_once(&marker)?.1;
    let open = after.find("<string>")? + "<string>".len();
    let close = after[open..].find("</string>")?;
    Some(after[open..open + close].to_owned())
}

fn plist_has_key(plist: &str, key: &str) -> bool {
    plist.contains(&format!("<key>{key}</key>"))
}

#[test]
fn the_bundle_identifier_is_the_one_the_library_lives_under() {
    let plist = read("macos/Info.plist");
    assert_eq!(
        plist_string(&plist, "CFBundleIdentifier").as_deref(),
        Some(plaudon_app::runtime::APPLICATION_IDENTIFIER),
        "the bundle identifier decides the data directory (~/Library/Application Support/<id>); \
         changing it orphans every existing library (migration plan R7)"
    );
    assert_eq!(
        plist_string(&plist, "CFBundleIdentifier").as_deref(),
        Some(associations::BUNDLE_IDENTIFIER),
        "Launch Services registers the file association under this identifier"
    );
}

#[test]
fn the_bundle_runs_the_binary_cargo_builds() {
    let plist = read("macos/Info.plist");
    assert_eq!(
        plist_string(&plist, "CFBundleExecutable").as_deref(),
        Some("plaudon-egui"),
        "CFBundleExecutable must name the file in Contents/MacOS"
    );
    assert_eq!(
        plist_string(&plist, "CFBundleIconFile").as_deref(),
        Some("plaudon"),
        "CFBundleIconFile must name the .icns the packaging script writes"
    );
}

#[test]
fn the_packaging_script_still_has_every_placeholder_to_substitute() {
    let plist = read("macos/Info.plist");
    for token in [
        "@SHORT_VERSION@",
        "@BUNDLE_VERSION@",
        "@MINIMUM_SYSTEM_VERSION@",
    ] {
        assert!(
            plist.contains(token),
            "{token} is gone from the template, so package-macos-egui.sh would ship a literal"
        );
    }
}

#[test]
fn the_usage_descriptions_cover_every_prompt_the_product_can_trigger() {
    let plist = read("macos/Info.plist");
    // Bluetooth: the prompt only appears for a bundle that declares this
    // (docs/hardware/macos-apple-silicon.md). Speech: Apple's recogniser
    // refuses an unbundled process (docs/spikes/A1.3-apple-availability.md).
    // Removable volumes: the legacy USB import reads a mounted device volume
    // (crates/plaud-usb).
    for key in [
        "NSBluetoothAlwaysUsageDescription",
        "NSSpeechRecognitionUsageDescription",
        "NSRemovableVolumesUsageDescription",
    ] {
        assert!(plist_has_key(&plist, key), "Info.plist is missing {key}");
        let text = plist_string(&plist, key).expect("a usage description is a string");
        assert!(
            text.len() > 30,
            "{key} must say what the permission is used for, not just that it is wanted"
        );
    }
}

#[test]
fn the_bundle_never_asks_to_capture() {
    let plist = read("macos/Info.plist");
    for key in [
        "NSMicrophoneUsageDescription",
        "NSCameraUsageDescription",
        "NSScreenCaptureUsageDescription",
    ] {
        assert!(
            !plist_has_key(&plist, key),
            "{key} would ask for a capture permission; Plaudon imports recordings, it never makes them"
        );
    }
}

#[test]
fn the_associations_are_contributed_by_the_generator_not_copied() {
    let plist = read("macos/Info.plist");
    for key in [
        "CFBundleURLTypes",
        "CFBundleDocumentTypes",
        "UTExportedTypeDeclarations",
    ] {
        assert!(
            !plist_has_key(&plist, key),
            "{key} belongs to plaudon_platform::associations and is merged in at package time; \
             a second copy here is a second source of truth"
        );
    }
    // And the generated fragment really does carry them.
    let fragment = associations::macos_info_plist_fragment();
    for key in [
        "CFBundleURLTypes",
        "CFBundleDocumentTypes",
        "UTExportedTypeDeclarations",
    ] {
        assert!(fragment.contains(&format!("<key>{key}</key>")));
    }
}

#[test]
fn the_entitlements_are_exactly_the_five_privileges_the_product_uses() {
    let entitlements = read("macos/entitlements.plist");
    let expected = [
        "com.apple.security.app-sandbox",
        "com.apple.security.files.user-selected.read-write",
        "com.apple.security.network.client",
        "com.apple.security.device.bluetooth",
        "com.apple.security.device.usb",
    ];
    let found: BTreeSet<String> = entitlements
        .match_indices("<key>")
        .filter_map(|(at, _)| {
            let rest = &entitlements[at + "<key>".len()..];
            rest.find("</key>").map(|end| rest[..end].to_owned())
        })
        .collect();
    assert_eq!(
        found,
        expected.iter().map(|key| (*key).to_owned()).collect(),
        "the entitlement set is a reviewed list; adding one is a privilege decision"
    );
}

#[test]
fn no_hardened_runtime_escape_hatch_is_requested() {
    let entitlements = read("macos/entitlements.plist");
    for key in [
        "com.apple.security.cs.allow-jit",
        "com.apple.security.cs.allow-unsigned-executable-memory",
        "com.apple.security.cs.disable-library-validation",
        "com.apple.security.cs.allow-dyld-environment-variables",
        "com.apple.security.cs.debugger",
        "com.apple.security.get-task-allow",
        "com.apple.security.network.server",
    ] {
        assert!(
            !entitlements.contains(key),
            "{key} disarms the hardened runtime and must never ship (spec 16.5)"
        );
    }
}

// -------------------------------------------------------------- Windows ----

/// `!define NAME "value"` pairs, so `${NAME}` can be expanded.
fn nsis_defines(script: &str) -> Vec<(String, String)> {
    script
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("!define "))
        .filter_map(|rest| {
            let mut tokens = nsis_tokens(rest);
            let name = tokens.next()?;
            let value = tokens.next().unwrap_or_default();
            Some((name, value))
        })
        .collect()
}

fn expand(value: &str, defines: &[(String, String)]) -> String {
    let mut out = value.to_owned();
    // Defines may refer to earlier defines, so iterate until stable.
    for _ in 0..4 {
        let mut next = out.clone();
        for (name, replacement) in defines {
            next = next.replace(&format!("${{{name}}}"), replacement);
        }
        if next == out {
            break;
        }
        out = next;
    }
    out
}

/// Split an NSIS argument list, honouring `"…"`, `'…'` and the `$\"` escape.
fn nsis_tokens(line: &str) -> impl Iterator<Item = String> + '_ {
    let mut chars = line.chars().peekable();
    std::iter::from_fn(move || {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let first = *chars.peek()?;
        let mut token = String::new();
        if first == '"' || first == '\'' {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '$' && chars.peek() == Some(&'\\') {
                    chars.next();
                    if let Some(escaped) = chars.next() {
                        token.push(escaped);
                    }
                } else if c == first {
                    break;
                } else {
                    token.push(c);
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                token.push(c);
                chars.next();
            }
        }
        Some(token)
    })
}

/// Every `WriteRegStr HKCU "key" "name" "data"` as (key, name, data).
///
/// `defines` comes from the whole file, because a section parsed on its own
/// cannot see the `!define` block above it.
fn nsis_registry_writes(
    script: &str,
    defines: &[(String, String)],
) -> BTreeSet<(String, String, String)> {
    script
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("WriteRegStr "))
        .map(|rest| {
            let tokens: Vec<String> = nsis_tokens(rest).collect();
            let [hive, key, name, data] = tokens.as_slice() else {
                panic!("WriteRegStr takes four arguments: {rest}");
            };
            assert_eq!(hive, "HKCU", "the installer is per-user: {rest}");
            (
                expand(key, defines),
                expand(name, defines),
                expand(data, defines),
            )
        })
        .collect()
}

/// Every `DeleteRegKey HKCU "key"`.
fn nsis_registry_deletes(script: &str, defines: &[(String, String)]) -> BTreeSet<String> {
    script
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("DeleteRegKey "))
        .filter_map(|rest| {
            let tokens: Vec<String> = nsis_tokens(rest).collect();
            let [_hive, key] = tokens.as_slice() else {
                return None;
            };
            Some(expand(key, defines))
        })
        .collect()
}

/// The committed `.reg` claim, as (key under HKCU, value name, data).
fn generated_registry_claim() -> BTreeSet<(String, String, String)> {
    let script = associations::windows_registry_script_with_executable(INSTALLED_EXECUTABLE);
    let mut claim = BTreeSet::new();
    let mut key = String::new();
    for line in script.split("\r\n") {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with("Windows Registry Editor") {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section
                .strip_prefix("HKEY_CURRENT_USER\\")
                .expect("the claim is per-user")
                .clone_into(&mut key);
            continue;
        }
        let (name, data) = line.split_once('=').expect("a .reg value line");
        let name = if name == "@" {
            String::new()
        } else {
            unquote_reg(name)
        };
        claim.insert((key.clone(), name, unquote_reg(data)));
    }
    claim
}

/// Undo `.reg` string quoting: strip the quotes, unescape `\\` and `\"`.
fn unquote_reg(value: &str) -> String {
    let inner = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(escaped) = chars.next() {
                out.push(escaped);
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn the_installer_writes_exactly_the_generated_association_claim() {
    let script = read("windows/plaudon-egui.nsi");
    let written = nsis_registry_writes(&script, &nsis_defines(&script));
    let expected = generated_registry_claim();

    for triple in &expected {
        assert!(
            written.contains(triple),
            "the installer does not write {triple:?}; regenerate the WriteRegStr block from \
             plaudon_platform::associations::windows_registry_script"
        );
    }
    for triple in &written {
        if triple.0.starts_with("Software\\Classes") {
            assert!(
                expected.contains(triple),
                "the installer writes {triple:?}, which the association generator does not claim"
            );
        }
    }
    assert!(
        expected
            .iter()
            .any(|(key, _, _)| key == "plaudon" || key.starts_with("Software\\Classes\\plaudon")),
        "the plaudon:// protocol handler must be part of the claim"
    );
}

#[test]
fn the_uninstaller_removes_every_key_the_installer_wrote() {
    let script = read("windows/plaudon-egui.nsi");
    let (installer, uninstaller) = script
        .split_once("Section \"Uninstall\"")
        .expect("the script has an uninstall section");
    let defines = nsis_defines(&script);
    let written = nsis_registry_writes(installer, &defines);
    let deleted = nsis_registry_deletes(uninstaller, &defines);
    for (key, _, _) in &written {
        assert!(
            deleted
                .iter()
                .any(|removed| key == removed || key.starts_with(&format!("{removed}\\"))),
            "nothing in the uninstall section removes {key}"
        );
    }
}

#[test]
fn the_installer_is_per_user_and_leaves_the_library_alone() {
    let script = read("windows/plaudon-egui.nsi");
    assert!(
        script.contains("RequestExecutionLevel user"),
        "a per-user install must not ask for elevation"
    );
    assert!(
        !script.contains("HKLM") && !script.contains("HKEY_LOCAL_MACHINE"),
        "machine-wide keys would need elevation and would outlive the user's install"
    );
    assert!(
        !script.contains("$APPDATA\\app.plaudon.desktop"),
        "the uninstaller must never touch the library directory"
    );
    for forbidden in [
        "SetAutoStart",
        "$SMSTARTUP",
        "WriteRegStr HKCU \"Software\\Microsoft\\Windows\\CurrentVersion\\Run",
    ] {
        assert!(
            !script.contains(forbidden),
            "the installer must not add an autostart entry ({forbidden})"
        );
    }
}

#[test]
fn the_application_manifest_asks_for_no_privilege() {
    let manifest = read("windows/plaudon-egui.exe.manifest");
    assert!(manifest.contains(r#"level="asInvoker""#));
    assert!(manifest.contains(r#"uiAccess="false""#));
    assert!(!manifest.contains("requireAdministrator"));
    assert!(!manifest.contains("highestAvailable"));
    // The three behavioural settings the shell depends on.
    assert!(
        manifest.contains("PerMonitorV2"),
        "winit scales the viewport itself"
    );
    assert!(
        manifest.contains("longPathAware"),
        "export paths nest past MAX_PATH"
    );
    assert!(
        manifest.contains("UTF-8"),
        "the ANSI Win32 surface must agree with Rust strings"
    );
}

#[test]
fn the_resource_script_links_the_icon_and_the_manifest_that_exist() {
    let rc = read("windows/plaudon-egui.rc");
    assert!(
        rc.contains("plaudon.ico"),
        "the executable must carry its icon"
    );
    assert!(
        rc.contains("1 24 \"plaudon-egui.exe.manifest\""),
        "resource type 24 is RT_MANIFEST; the manifest must be linked at id 1"
    );
    assert!(
        rc.contains("VERSIONINFO"),
        "package-windows-egui.ps1 reads the version block back to prove the resources linked"
    );
    for referenced in ["plaudon.ico", "plaudon-egui.exe.manifest"] {
        assert!(
            packaging_dir().join("windows").join(referenced).exists(),
            "{referenced} is referenced by the .rc but not committed"
        );
    }
}

// ----------------------------------------------------------------- icon ----

#[test]
fn the_master_raster_is_a_1024_square() {
    let png = std::fs::read(packaging_dir().join("icon-1024.png")).expect("the committed master");
    assert_eq!(&png[1..4], b"PNG", "the master raster must be a PNG");
    let width = u32::from_be_bytes(png[16..20].try_into().expect("IHDR width"));
    let height = u32::from_be_bytes(png[20..24].try_into().expect("IHDR height"));
    assert_eq!(
        (width, height),
        (1024, 1024),
        "macOS scales 1024 down to every icns size"
    );
}

#[test]
fn the_windows_icon_carries_the_sizes_explorer_asks_for() {
    let ico =
        std::fs::read(packaging_dir().join("windows/plaudon.ico")).expect("the committed ico");
    assert_eq!(
        &ico[0..4],
        &[0, 0, 1, 0],
        "an .ico starts with reserved 0 and type 1"
    );
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    let sizes: BTreeSet<u32> = (0..count)
        .map(|index| {
            let entry = 6 + index * 16;
            // A zero byte means 256 pixels, which is why it is a byte at all.
            match u32::from(ico[entry]) {
                0 => 256,
                size => size,
            }
        })
        .collect();
    for required in [16_u32, 32, 48, 256] {
        assert!(
            sizes.contains(&required),
            "the .ico has no {required}px image: {sizes:?}"
        );
    }
}

// -------------------------------------------------------------- scripts ----

#[test]
fn both_packaging_scripts_fail_closed_without_credentials() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the repository root");
    let macos = std::fs::read_to_string(root.join("scripts/package-macos-egui.sh"))
        .expect("the macOS packaging script");
    let windows = std::fs::read_to_string(root.join("scripts/package-windows-egui.ps1"))
        .expect("the Windows packaging script");

    assert!(
        macos.contains("reason=missing_credential") && macos.contains("exit 3"),
        "an unsigned macOS bundle must exit 3, not 0"
    );
    assert!(
        windows.contains("reason=missing_credential") && windows.contains("exit 3"),
        "an unsigned Windows installer must exit 3, not 0"
    );
    assert!(
        macos.contains("NOTARY_STATUS=\"not_verified\""),
        "notarization starts not_verified and is only raised by a successful notarytool run"
    );
    assert!(
        windows.contains("notarization_status  = \"not_applicable\""),
        "there is no notarization on Windows; the report must say so rather than claim success"
    );
}
