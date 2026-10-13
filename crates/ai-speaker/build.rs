//! Link directives the vendored ONNX Runtime needs, and the error message a
//! developer gets when it is not vendored yet (ADR-0081 §3, issue #283).
//!
//! `ort-sys` emits its own link directives for the archive **it** downloads.
//! This project does not use that path: `download-binaries` is off, so
//! `ORT_LIB_LOCATION` must point at the archive `scripts/fetch-onnxruntime.sh`
//! vendored, and `ort-sys` then links it as a user-provided build. That mode
//! deliberately emits no execution-provider link directives, which is correct
//! for every provider except the ones the upstream build was compiled with:
//!
//! * macOS arm64 — built with CoreML. `ort-sys` links `Foundation` and
//!   `CoreML` for every Apple target already, so nothing is needed here.
//!   Verified by linking and running on this project's reference machine.
//! * Windows x64 — built with DirectML. The DirectML and Direct3D 12 import
//!   libraries are needed, and `ort-sys` emits them only on its download path,
//!   so this build script emits them for the vendored one.
//!
//! **The Windows branch has never been executed on Windows by this project**
//! (`crates/ai-speaker/src/runtime.rs`, `docs/release/voice-profiles-evidence.md`):
//! no Windows x86-64 machine exists in this project's tooling. It is written
//! from `ort-sys`'s own directives for the same archive, and the first Windows
//! build is what will confirm or correct it.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=ORT_LIB_LOCATION");
    println!("cargo:rerun-if-env-changed=ORT_LIB_PATH");

    // The feature is off in every shipped configuration; without it this crate
    // links no native code at all.
    if std::env::var_os("CARGO_FEATURE_ONNX").is_none() {
        return;
    }

    if std::env::var_os("ORT_LIB_LOCATION").is_none() && std::env::var_os("ORT_LIB_PATH").is_none()
    {
        println!(
            "cargo:warning=ORT_LIB_LOCATION is not set. The `onnx` feature links a vendored ONNX \
             Runtime and this build will fail at the link step. Run \
             `scripts/fetch-onnxruntime.sh` once and export the path it prints."
        );
    }

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // The vendored Windows archive is upstream's DirectML build; these are
        // the libraries `ort-sys` links for it on its own download path.
        for lib in ["DirectML", "dxguid", "dxcore", "dxgi", "d3d12"] {
            println!("cargo:rustc-link-lib={lib}");
        }
    }
}
