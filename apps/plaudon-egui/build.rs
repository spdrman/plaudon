//! Link arguments the final binary cannot inherit, and the Windows resources.
//!
//! # Apple bridge link flags
//!
//! Cargo applies `rustc-link-arg` only to targets of the package that emitted
//! it, so the binary that actually links the Swift static library has to
//! restate the flags. `crates/ai-apple` publishes them as `links` metadata so
//! the list is written once; this copies them onto the application binary,
//! exactly as `apps/desktop/src-tauri/build.rs` did for the Tauri shell.
//!
//! Without the `/usr/lib/swift` rpath the bundle cannot resolve
//! `libswift_Concurrency.dylib` and fails to launch; without the weak
//! framework references it would hard-require Speech and FoundationModels and
//! dyld would refuse to start the application on a Mac older than macOS 26.
//!
//! # Windows resources
//!
//! A Windows GUI application carries its icon, its version information and
//! its side-by-side manifest (DPI awareness, long paths, no elevation) as
//! linked resources. They are compiled from `packaging/windows/
//! plaudon-egui.rc` with whichever resource compiler the host has —
//! `llvm-rc` (the LLVM distribution and cargo-xwin) or MSVC's `rc.exe` — and
//! linked into the binary. `scripts/package-windows-egui.ps1` refuses to
//! package an executable whose version information is missing, so a build
//! that silently lost its resources cannot become an installer.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    forward_apple_bridge_link_args();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_resources();
    }
}

/// `DEP_<LINKS>_LINK_ARGS` from `plaudon-ai-apple`, present only when its
/// `apple-bridge` feature is on. Discovered rather than spelled out, so a
/// rename of the `links` key cannot silently drop the flags.
fn forward_apple_bridge_link_args() {
    let Some((_, flags)) =
        std::env::vars().find(|(key, _)| key.starts_with("DEP_") && key.ends_with("_LINK_ARGS"))
    else {
        return;
    };
    for flag in flags.split_whitespace() {
        // The plain form covers the library's own unit-test binary, which the
        // `-bins` and `-tests` forms do not.
        println!("cargo:rustc-link-arg={flag}");
    }
}

fn embed_windows_resources() {
    let packaging = Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/windows");
    let script = packaging.join("plaudon-egui.rc");
    println!("cargo:rerun-if-changed={}", script.display());
    println!(
        "cargo:rerun-if-changed={}",
        packaging.join("plaudon-egui.exe.manifest").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        packaging.join("plaudon.ico").display()
    );
    println!("cargo:rerun-if-env-changed=RC");

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"))
        .join("plaudon-egui.res");
    let version = env!("CARGO_PKG_VERSION");
    // VERSIONINFO's numeric fields take four comma-separated integers, so the
    // same version is passed twice: once as the display string, once in the
    // binary form `major,minor,patch,0`.
    let numeric = {
        let mut parts: Vec<&str> = version.split('.').collect();
        parts.resize(4, "0");
        parts.join(",")
    };

    // `RC` wins, then `llvm-rc` (LLVM, and what cargo-xwin cross-builds
    // with), then MSVC's `rc.exe`. Candidates are tried by *compiling*, not
    // by probing with a help flag: `llvm-rc /?` already exits non-zero, so a
    // probe would have to encode each tool's idea of a usage error. A
    // candidate that is absent is skipped; one that is present and fails is a
    // real error, because the alternative is a release binary with no icon,
    // no version block and no manifest.
    let mut failures = Vec::new();
    for compiler in resource_compilers() {
        let result = Command::new(&compiler)
            .arg(format!("/DPLAUDON_VERSION=\"{version}\""))
            .arg(format!("/DPLAUDON_VERSION_COMMA={numeric}"))
            .arg("/fo")
            .arg(&out)
            .arg(&script)
            .current_dir(&packaging)
            .output();
        match result {
            Ok(output) if output.status.success() => {
                println!("cargo:rustc-link-arg-bins={}", out.display());
                return;
            }
            Ok(output) => failures.push(format!(
                "{} failed with {}: {}",
                compiler.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                failures.push(format!("{} could not be run: {error}", compiler.display()));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "the Windows resources could not be compiled:\n  {}",
        failures.join("\n  ")
    );
    println!(
        "cargo:warning=no resource compiler (llvm-rc or rc.exe) was found: the Windows binary is \
         being built without its icon, version information and manifest. Set RC to the resource \
         compiler; scripts/package-windows-egui.ps1 will refuse this binary."
    );
}

fn resource_compilers() -> Vec<PathBuf> {
    std::env::var_os("RC")
        .map(PathBuf::from)
        .into_iter()
        .chain(["llvm-rc", "rc.exe"].into_iter().map(PathBuf::from))
        .collect()
}
