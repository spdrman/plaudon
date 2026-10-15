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
//! `plaudon_platform::associations::windows_registry_script` generates. A
//! `.msi` cannot import a `.reg` file whose paths are only known at install
//! time, so the keys are written twice — once as the committed `.reg`
//! artifact, once as `RegistryValue` rows in the WiX source. This test is
//! what stops the two from drifting.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use plaudon_platform::associations;

/// The installed executable the WiX source substitutes for `%EXEPATH%`.
///
/// `[#FileId]` is Windows Installer's formatted reference to the full path a
/// file is installed to, which is the only thing either installer could use:
/// the install directory is not known when the package is built.
const INSTALLED_EXECUTABLE: &str = "[#PlaudonEgui]";

fn packaging_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the repository root")
        .to_path_buf()
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

/// The attributes of one XML element, in source order.
///
/// A real parser is unnecessary and would hide what is being asserted: the
/// installer definition is a flat list of self-closing elements, and what
/// matters is every attribute of every `RegistryValue`, `RegistryKey` and
/// `Shortcut` exactly as written.
fn attributes(element: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = element;
    while let Some(equals) = rest.find('=') {
        let name = rest[..equals]
            .rsplit(|c: char| c.is_whitespace())
            .next()
            .unwrap_or_default()
            .to_owned();
        let after = &rest[equals + 1..];
        let Some(open) = after.find('"') else { break };
        let Some(close) = after[open + 1..].find('"') else {
            break;
        };
        let value = &after[open + 1..open + 1 + close];
        if !name.is_empty() {
            out.push((name, unescape_xml(value)));
        }
        rest = &after[open + 1 + close + 1..];
    }
    out
}

/// The predefined XML entities that can appear in an attribute value.
fn unescape_xml(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Every element of one name, as its attribute list.
///
/// `<Name …>` and `<Name …/>` both match; `<NameSomethingElse …>` does not.
fn elements(source: &str, name: &str) -> Vec<Vec<(String, String)>> {
    let open = format!("<{name}");
    let mut out = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find(&open) {
        let after = &rest[at + open.len()..];
        let boundary = after.chars().next();
        let end = after.find('>').expect("an element is closed");
        if boundary.is_some_and(|c| c.is_whitespace() || c == '/' || c == '>') {
            out.push(attributes(&after[..end]));
        }
        rest = &after[end..];
    }
    out
}

fn attribute(element: &[(String, String)], name: &str) -> Option<String> {
    element
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

/// Every `RegistryValue` row as (key under the hive, value name, data).
///
/// The hive is asserted here rather than returned: a per-user package writes
/// `HKCU` and nothing else, and a row that says otherwise is a bug this
/// function refuses to hand on to a comparison that might tolerate it.
fn wix_registry_writes(source: &str) -> BTreeSet<(String, String, String)> {
    elements(source, "RegistryValue")
        .iter()
        .map(|element| {
            let key = attribute(element, "Key").expect("a RegistryValue names its key");
            let root = attribute(element, "Root").expect("a RegistryValue names its hive");
            assert_eq!(root, "HKCU", "the installer is per-user: {key}");
            (
                key,
                attribute(element, "Name").unwrap_or_default(),
                attribute(element, "Value").unwrap_or_default(),
            )
        })
        .collect()
}

/// Every key the package deletes on uninstall (`ForceDeleteOnUninstall`).
fn wix_registry_deletes(source: &str) -> BTreeSet<String> {
    elements(source, "RegistryKey")
        .iter()
        .filter(|element| attribute(element, "ForceDeleteOnUninstall").as_deref() == Some("yes"))
        .map(|element| {
            let root = attribute(element, "Root").expect("a RegistryKey names its hive");
            assert_eq!(root, "HKCU", "the installer is per-user");
            attribute(element, "Key").expect("a RegistryKey names its key")
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
    let source = read("windows/plaudon-egui.wxs");
    let written = wix_registry_writes(&source);
    let expected = generated_registry_claim();

    for triple in &expected {
        assert!(
            written.contains(triple),
            "the installer does not write {triple:?}; regenerate the RegistryValue block from \
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
    // Windows Installer removes the *values* a component wrote when that
    // component is uninstalled, but it leaves the key itself behind if
    // anything else ever wrote to it. ForceDeleteOnUninstall is what removes
    // the key, and every key this package touches must be covered by one.
    let source = read("windows/plaudon-egui.wxs");
    let written = wix_registry_writes(&source);
    let deleted = wix_registry_deletes(&source);
    assert!(
        !deleted.is_empty(),
        "nothing in the package removes a registry key on uninstall"
    );
    for (key, _, _) in &written {
        assert!(
            deleted
                .iter()
                .any(|removed| key == removed || key.starts_with(&format!("{removed}\\"))),
            "nothing removes {key} on uninstall"
        );
    }
}

#[test]
fn the_installer_is_per_user_and_leaves_the_library_alone() {
    let source = read("windows/plaudon-egui.wxs");
    assert!(
        source.contains(r#"Scope="perUser""#),
        "a per-user install must not ask for elevation"
    );
    for forbidden in ["perMachine", "HKLM", "HKEY_LOCAL_MACHINE", "HKMU", "HKCR"] {
        assert!(
            !source.contains(forbidden),
            "machine-wide keys ({forbidden}) would need elevation and would outlive the user's \
             install"
        );
    }
    // HKCR and HKMU resolve to a per-user hive only as a side effect of the
    // install scope; every row in this package names the hive it means.
    for element in elements(&source, "RegistryValue")
        .iter()
        .chain(elements(&source, "RegistryKey").iter())
    {
        assert_eq!(
            attribute(element, "Root").as_deref(),
            Some("HKCU"),
            "every registry row names HKCU explicitly: {element:?}"
        );
    }
    // The library lives in the roaming application-data folder. Nothing in
    // the package may name it, and nothing in the package may delete a file
    // or a folder it did not install — those are the two ways an uninstall
    // becomes a data-loss bug.
    for forbidden in [
        r#"Id="AppDataFolder""#,
        "[AppDataFolder]",
        "<RemoveFile",
        "<RemoveFolder",
        "RemoveFolderEx",
    ] {
        assert!(
            !source.contains(forbidden),
            "the uninstaller must never touch the library directory ({forbidden})"
        );
    }
    for forbidden in [
        "StartupFolder",
        r"Software\Microsoft\Windows\CurrentVersion\Run",
        "ServiceInstall",
        "ScheduledTask",
        "<Environment",
    ] {
        assert!(
            !source.contains(forbidden),
            "the installer must not add an autostart, a service or a PATH edit ({forbidden})"
        );
    }
}

#[test]
fn the_package_upgrades_itself_rather_than_installing_beside_itself() {
    let source = read("windows/plaudon-egui.wxs");
    let package = elements(&source, "Package")
        .into_iter()
        .next()
        .expect("the installer defines a package");
    let upgrade_code = attribute(&package, "UpgradeCode").expect("the package has an upgrade code");
    assert_eq!(
        upgrade_code, "BB143690-5F22-43C6-9E83-75E7EE43F005",
        "the UpgradeCode is the identity of the product line; changing it makes the next release \
         install beside this one instead of replacing it"
    );
    assert!(
        !elements(&source, "MajorUpgrade").is_empty(),
        "without MajorUpgrade the previous version is never removed"
    );
    assert_eq!(
        attribute(&package, "Version").as_deref(),
        Some("$(var.Version)"),
        "the package version comes from the workspace version the packaging script reads"
    );
    // Component GUIDs are written out so that a component keeps its identity
    // across releases; a generated one can change under a rename and break
    // the upgrade.
    for component in elements(&source, "Component") {
        let id = attribute(&component, "Id").expect("a component is named");
        let guid = attribute(&component, "Guid")
            .unwrap_or_else(|| panic!("component {id} has no explicit GUID"));
        assert_eq!(
            guid.len(),
            36,
            "component {id} has a malformed GUID: {guid}"
        );
    }
}

#[test]
fn the_package_runs_no_code_of_its_own() {
    // A custom action is executable code inside an installer, which is the
    // thing a per-user package has no excuse for. The two property
    // assignments this package does make are type 51 rows: they set a string
    // and execute nothing.
    let source = read("windows/plaudon-egui.wxs");
    for forbidden in [
        "<CustomAction",
        "<Binary",
        "BinaryRef",
        "ExeCommand",
        "<InstallExecuteSequence",
    ] {
        assert!(
            !source.contains(forbidden),
            "{forbidden} introduces code or a sequence override into the installer"
        );
    }
    for element in elements(&source, "SetProperty")
        .iter()
        .chain(elements(&source, "SetDirectory").iter())
    {
        assert!(
            attribute(element, "Value").is_some(),
            "a property assignment sets a value and nothing else: {element:?}"
        );
    }
}

#[test]
fn the_shortcut_points_at_the_installed_executable() {
    let source = read("windows/plaudon-egui.wxs");
    let shortcut = elements(&source, "Shortcut")
        .into_iter()
        .next()
        .expect("the installer writes a Start Menu shortcut");
    assert_eq!(
        attribute(&shortcut, "Target").as_deref(),
        Some(INSTALLED_EXECUTABLE),
        "the shortcut must resolve to the file the package installs, not to a guessed path"
    );
    assert_eq!(
        attribute(&shortcut, "Directory").as_deref(),
        Some("ProgramMenuFolder"),
        "the shortcut belongs in the Start Menu and nowhere else"
    );
    assert_eq!(
        attribute(&shortcut, "Advertise").as_deref(),
        Some("no"),
        "an advertised shortcut makes the shell call the installer on every launch"
    );
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
fn the_resource_script_links_the_brand_icon_and_the_manifest_that_exist() {
    let rc = read("windows/plaudon-egui.rc");
    let icon_reference = "../../../../brand/kit/icons/desktop/plaudon.ico";
    assert!(
        rc.contains(icon_reference),
        "the executable must carry the brand kit's icon, not a copy of it"
    );
    assert!(
        rc.contains("1 24 \"plaudon-egui.exe.manifest\""),
        "resource type 24 is RT_MANIFEST; the manifest must be linked at id 1"
    );
    assert!(
        rc.contains("VERSIONINFO"),
        "package-windows-egui.ps1 reads the version block back to prove the resources linked"
    );
    // `build.rs` compiles the .rc with packaging/windows as the working
    // directory, so both paths are resolved from there.
    for referenced in [icon_reference, "plaudon-egui.exe.manifest"] {
        assert!(
            packaging_dir().join("windows").join(referenced).exists(),
            "{referenced} is referenced by the .rc but is not there"
        );
    }
}

// ----------------------------------------------------------------- icon ----
//
// `brand/kit/icons/desktop/` is the only place the application artwork lives:
// the Windows resource script links the kit's `.ico`, the macOS script copies
// the kit's `.icns`, and the window icon is the kit's 256px PNG embedded by
// `apps/plaudon-egui/src/icon.rs`. These tests assert what each platform
// needs of those files, and that no second copy has reappeared under
// `packaging/`.

fn brand_icons_dir() -> PathBuf {
    repo_root().join("brand/kit/icons/desktop")
}

#[test]
fn the_brand_kit_is_the_only_copy_of_the_application_icon() {
    for stale in [
        "icon.svg",
        "icon-1024.png",
        "windows/plaudon.ico",
        "icon-256.png",
    ] {
        assert!(
            !packaging_dir().join(stale).exists(),
            "packaging/{stale} is a second copy of the brand artwork; the kit is the source of truth"
        );
    }
    assert!(
        !repo_root().join("scripts/make-app-icon.sh").exists(),
        "the icons are exported by the brand kit, not regenerated from a packaging SVG"
    );
}

#[test]
fn the_master_raster_is_a_1024_square() {
    let png = std::fs::read(brand_icons_dir().join("plaudon-icon-1024.png"))
        .expect("the brand kit master");
    assert_eq!(&png[1..4], b"PNG", "the master raster must be a PNG");
    let width = u32::from_be_bytes(png[16..20].try_into().expect("IHDR width"));
    let height = u32::from_be_bytes(png[20..24].try_into().expect("IHDR height"));
    assert_eq!(
        (width, height),
        (1024, 1024),
        "1024 is the size every other export is cut from"
    );
}

#[test]
fn the_window_icon_is_the_brand_png_the_shell_embeds() {
    let png = std::fs::read(brand_icons_dir().join("plaudon-icon-256.png"))
        .expect("the brand kit window icon");
    assert_eq!(&png[1..4], b"PNG");
    let width = u32::from_be_bytes(png[16..20].try_into().expect("IHDR width"));
    let height = u32::from_be_bytes(png[20..24].try_into().expect("IHDR height"));
    assert_eq!((width, height), (256, 256), "src/icon.rs embeds this file");
}

#[test]
fn the_macos_bundle_icon_is_a_real_icns() {
    let icns = std::fs::read(brand_icons_dir().join("plaudon.icns")).expect("the brand kit icns");
    assert_eq!(
        &icns[0..4],
        b"icns",
        "an .icns starts with the 'icns' magic"
    );
    let declared = u32::from_be_bytes(icns[4..8].try_into().expect("icns length"));
    assert_eq!(
        declared as usize,
        icns.len(),
        "the header length must match the file; a truncated .icns gives a blank Dock tile"
    );
    // The script copies this file verbatim to Contents/Resources/plaudon.icns,
    // which is what CFBundleIconFile ("plaudon") names.
    let script = std::fs::read_to_string(repo_root().join("scripts/package-macos-egui.sh"))
        .expect("the macOS packaging script");
    assert!(
        script.contains("brand/kit/icons/desktop/plaudon.icns"),
        "the bundle icon must come from the brand kit"
    );
}

#[test]
fn the_windows_icon_carries_the_sizes_explorer_asks_for() {
    let ico = std::fs::read(brand_icons_dir().join("plaudon.ico")).expect("the brand kit ico");
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

#[test]
fn the_windows_installer_stages_the_brand_icon() {
    let script = std::fs::read_to_string(repo_root().join("scripts/package-windows-egui.ps1"))
        .expect("the Windows packaging script");
    assert!(
        script.contains("brand/kit/icons/desktop/plaudon.ico"),
        "the ARP icon and the Start menu shortcut must use the brand icon"
    );
    let wxs = read("windows/plaudon-egui.wxs");
    assert!(
        wxs.contains("<Icon Id=\"PlaudonIcon\" SourceFile=\"plaudon.ico\" />"),
        "WiX resolves the staged icon by name from the bind path"
    );
    assert!(
        wxs.contains("ARPPRODUCTICON"),
        "Add/Remove Programs must show the brand icon"
    );
}

// -------------------------------------------------------------- scripts ----

#[test]
fn both_packaging_scripts_fail_closed_without_credentials() {
    let root = repo_root();
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

// ------------------------------------------------- shipped radio (#301) ----
//
// The production radio sits behind the `hardware-scan` feature, which is off
// by default so CI and every headless build carry no Bluetooth stack. That is
// right for tests and wrong for the binaries people install: before #301 no
// packaging script and no release job turned it on, so every released build
// answered a scan with `SOURCE_UNAVAILABLE`.

fn read_repo(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The `PLAUDON_RELEASE_FEATURES` value of every job in the release workflow.
fn release_feature_sets() -> Vec<String> {
    read_repo(".github/workflows/release.yml")
        .lines()
        .filter_map(|line| line.trim().strip_prefix("PLAUDON_RELEASE_FEATURES:"))
        .map(|value| value.trim().trim_matches('"').to_owned())
        .collect()
}

fn names_feature(list: &str, feature: &str) -> bool {
    list.split([',', ' ']).any(|item| item.trim() == feature)
}

#[test]
fn issue_301_both_release_jobs_ship_the_real_radio() {
    let sets = release_feature_sets();
    assert_eq!(
        sets.len(),
        2,
        "the macOS and the Windows release jobs each name their features: {sets:?}"
    );
    for set in &sets {
        assert!(
            names_feature(set, "hardware-scan"),
            "a release job builds without `hardware-scan` ({set:?}), so the installed app \
             cannot scan"
        );
        assert!(
            names_feature(set, "local-whisper"),
            "the release job lost `local-whisper` on the way: {set:?}"
        );
    }
}

#[test]
fn issue_301_the_macos_packaging_script_always_builds_the_real_radio() {
    let script = read_repo("scripts/package-macos-egui.sh");
    assert!(
        script.contains("hardware-scan"),
        "scripts/package-macos-egui.sh must turn on `hardware-scan` itself, whatever \
         `--features` it is handed: a bundle built without it cannot scan"
    );
}

#[test]
fn issue_301_the_windows_packaging_script_always_builds_the_real_radio() {
    let script = read_repo("scripts/package-windows-egui.ps1");
    assert!(
        script.contains("hardware-scan"),
        "scripts/package-windows-egui.ps1 must turn on `hardware-scan` itself, whatever \
         `-Features` it is handed: an installer built without it cannot scan"
    );
}

#[test]
fn issue_301_default_and_test_builds_stay_free_of_a_bluetooth_stack() {
    // The other half of the bargain: shipping the radio must not leak it into
    // the default feature set, which is what every Linux and headless test
    // build compiles (a `btleplug` on Linux needs dbus).
    for manifest in [
        "apps/plaudon-egui/Cargo.toml",
        "crates/app/Cargo.toml",
        "crates/plaud-ble/Cargo.toml",
    ] {
        let text = read_repo(manifest);
        let default = text
            .lines()
            .find(|line| line.trim_start().starts_with("default ="))
            .unwrap_or("");
        assert!(
            !default.contains("hardware-scan"),
            "{manifest} turns `hardware-scan` on by default"
        );
    }
}
