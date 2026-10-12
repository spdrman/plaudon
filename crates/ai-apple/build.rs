//! Builds and links the macOS-only Swift bridge (task H1.2, spec 11.1, 11.6).
//!
//! Three rules, in order of importance:
//!
//! 1. **A Windows or Linux build links nothing Apple.** Without the
//!    `apple-bridge` feature this script emits nothing at all, and the crate
//!    compiles to pure Rust whose only Apple content is a capability mapping
//!    that always answers "unsupported". `scripts/check-no-apple-symbols.sh`
//!    verifies the result by inspecting the binary.
//! 2. **A missing Swift toolchain is a loud failure.** Spec 11.6 and the task
//!    acceptance both forbid a silently stubbed bridge: a build that cannot
//!    produce the library must not produce a library-shaped nothing.
//! 3. **The deployment target is pinned low and the new frameworks are weak.**
//!    `Speech` and `FoundationModels` exist only from macOS 26, but the
//!    product supports older Macs through whisper.cpp. Weak-linking plus the
//!    `@available` guards inside the Swift source means a macOS 13 host loads
//!    the binary and the probe answers `platform_unsupported`, instead of
//!    dyld refusing to launch the application.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Lowest macOS the produced binary must load on.
///
/// Not the lowest macOS that has Apple AI: that is 26. This is the floor for
/// *loading*, which is what weak linking buys.
const DEFAULT_DEPLOYMENT_TARGET: &str = "13.0";

/// Frameworks that exist on every supported macOS and are linked normally.
const REQUIRED_FRAMEWORKS: [&str; 3] = ["Foundation", "AVFoundation", "CoreMedia"];

/// Frameworks newer than the deployment target, linked weakly and probed at
/// runtime.
const WEAK_FRAMEWORKS: [&str; 2] = ["Speech", "FoundationModels"];

fn main() {
    let manifest = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let bridge_root = manifest
        .parent()
        .and_then(Path::parent)
        .expect("crates/ai-apple has a workspace root")
        .join("native/apple-bridge");

    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rerun-if-changed={}",
        bridge_root.join("Sources").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        bridge_root.join("include/plaudon_apple_bridge.h").display()
    );
    println!("cargo:rerun-if-env-changed=PLAUDON_MACOS_DEPLOYMENT_TARGET");
    println!("cargo:rerun-if-env-changed=PLAUDON_APPLE_BRIDGE_LIB_DIR");

    if std::env::var_os("CARGO_FEATURE_APPLE_BRIDGE").is_none() {
        // The default build. Nothing Apple is referenced, searched for or
        // emitted, on any platform.
        return;
    }

    let target_os = env("CARGO_CFG_TARGET_OS");
    assert!(
        target_os == "macos",
        "the `apple-bridge` feature builds a Swift library and can only be \
         enabled for a macOS target; this build targets `{target_os}`. Build \
         without the feature, or set --target to an *-apple-darwin triple."
    );

    let out_dir = PathBuf::from(env("OUT_DIR"));
    let lib_dir = match std::env::var_os("PLAUDON_APPLE_BRIDGE_LIB_DIR") {
        // A prebuilt library, for CI jobs that compile Swift once and for
        // reproducible release builds.
        Some(dir) => {
            let dir = PathBuf::from(dir);
            assert!(
                dir.join("libPlaudonAppleBridge.a").is_file(),
                "PLAUDON_APPLE_BRIDGE_LIB_DIR is set to `{}`, which does not \
                 contain libPlaudonAppleBridge.a. Run \
                 scripts/build-apple-bridge.sh, or unset the variable to build \
                 from source.",
                dir.display()
            );
            dir
        }
        None => compile_swift(&bridge_root, &out_dir),
    };

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=PlaudonAppleBridge");

    // The Swift runtime lives beside the OS, not in the bundle. Without this
    // search path and rpath the binary cannot resolve
    // `libswift_Concurrency.dylib` and fails to launch (recorded during A1.3).
    println!("cargo:rustc-link-search=native=/usr/lib/swift");
    for framework in REQUIRED_FRAMEWORKS {
        println!("cargo:rustc-link-lib=framework={framework}");
    }

    // Weak framework references and the Swift rpath are linker arguments
    // rather than link-lib entries, and Cargo applies those only to targets
    // this package itself links. A downstream binary (the Tauri application)
    // emits the same flags from its own build script, reading them from the
    // `DEP_PLAUDON_AI_APPLE_LINK_ARGS` metadata published below rather than
    // restating the list.
    for kind in [""] {
        println!("cargo:rustc-link-arg{kind}=-Wl,-rpath,/usr/lib/swift");
        for framework in WEAK_FRAMEWORKS {
            println!("cargo:rustc-link-arg{kind}=-Wl,-weak_framework,{framework}");
        }
    }

    // Consumed by a dependent package's build script as
    // `DEP_PLAUDON_AI_APPLE_LINK_ARGS` (requires `links` in Cargo.toml).
    println!(
        "cargo:link_args={}",
        WEAK_FRAMEWORKS
            .iter()
            .map(|f| format!("-Wl,-weak_framework,{f}"))
            .chain(std::iter::once("-Wl,-rpath,/usr/lib/swift".to_owned()))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!("cargo:lib_dir={}", lib_dir.display());
}

/// Compile the Swift sources into `out_dir`, returning the directory holding
/// the static library.
fn compile_swift(bridge_root: &Path, out_dir: &Path) -> PathBuf {
    let swiftc = locate_swiftc();
    let arch = match env("CARGO_CFG_TARGET_ARCH").as_str() {
        "aarch64" => "arm64".to_owned(),
        other => other.to_owned(),
    };
    let deployment_target = std::env::var("PLAUDON_MACOS_DEPLOYMENT_TARGET")
        .unwrap_or_else(|_| DEFAULT_DEPLOYMENT_TARGET.to_owned());
    let target = format!("{arch}-apple-macosx{deployment_target}");

    let sources = swift_sources(&bridge_root.join("Sources/PlaudonAppleBridge"));
    assert!(
        !sources.is_empty(),
        "no Swift sources under {}; the bridge cannot be built",
        bridge_root.display()
    );

    let library = out_dir.join("libPlaudonAppleBridge.a");
    let mut command = Command::new(&swiftc);
    // Cargo exports its own dynamic-library search path so build scripts can
    // load proc-macro dependencies. Inherited by `swift-frontend` it shadows
    // the Swift runtime and the driver fails with "unable to load standard
    // library for target". The compiler needs none of it.
    command.env_remove("DYLD_FALLBACK_LIBRARY_PATH");
    command.env_remove("DYLD_LIBRARY_PATH");
    if let Some(sdk) = macos_sdk_path() {
        // Pinned explicitly so the build does not depend on whatever SDKROOT
        // the surrounding shell happened to export.
        command.arg("-sdk").arg(sdk);
    }
    command
        .arg("-emit-library")
        .arg("-static")
        .arg("-O")
        .arg("-target")
        .arg(&target)
        .arg("-module-name")
        .arg("PlaudonAppleBridge")
        .arg("-emit-module")
        .arg("-emit-module-path")
        .arg(out_dir.join("PlaudonAppleBridge.swiftmodule"))
        .arg("-o")
        .arg(&library)
        .args(&sources);

    let output = command.output().unwrap_or_else(|error| {
        panic!(
            "failed to run `{}`: {error}. Install the Xcode command line tools \
             (`xcode-select --install`) or build without the `apple-bridge` \
             feature.",
            swiftc.display()
        )
    });
    assert!(
        output.status.success(),
        "swiftc failed building the Apple bridge for {target}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        library.is_file(),
        "swiftc reported success but produced no library at {}",
        library.display()
    );
    out_dir.to_path_buf()
}

/// The macOS SDK `xcrun` resolves, when it can.
fn macos_sdk_path() -> Option<PathBuf> {
    let output = Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Every `.swift` file in the module, sorted so the build is reproducible.
fn swift_sources(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut sources: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "swift"))
        .collect();
    sources.sort();
    sources
}

/// Find a Swift compiler, or fail with an instruction the reader can act on.
fn locate_swiftc() -> PathBuf {
    if let Some(explicit) = std::env::var_os("PLAUDON_SWIFTC") {
        return PathBuf::from(explicit);
    }
    if let Ok(output) = Command::new("xcrun").args(["-f", "swiftc"]).output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !path.is_empty() {
                return PathBuf::from(path);
            }
        }
    }
    if Command::new("swiftc")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        return PathBuf::from("swiftc");
    }
    panic!(
        "the `apple-bridge` feature needs a Swift compiler and none was found. \
         Install the Xcode command line tools with `xcode-select --install`, \
         set PLAUDON_SWIFTC to a swiftc binary, point \
         PLAUDON_APPLE_BRIDGE_LIB_DIR at a prebuilt libPlaudonAppleBridge.a, \
         or build without the feature. A stubbed bridge is not produced: an \
         application that silently reports Apple AI as unavailable when the \
         build was simply misconfigured is worse than a failed build."
    );
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("cargo must set {key}"))
}
