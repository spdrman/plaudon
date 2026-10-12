//! Tauri codegen, plus the Apple bridge's linker flags.
//!
//! Cargo applies `rustc-link-arg` only to targets of the package that emitted
//! it, so the binary that actually links the Swift static library has to
//! restate the flags. `crates/ai-apple` publishes them as `links` metadata so
//! the list is written once; this copies them onto the application binary.
//!
//! Without the `/usr/lib/swift` rpath the bundle cannot resolve
//! `libswift_Concurrency.dylib` and fails to launch; without the weak
//! framework references it would hard-require Speech and FoundationModels and
//! dyld would refuse to start the application on a Mac older than macOS 26.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // `DEP_<LINKS>_LINK_ARGS` from `plaudon-ai-apple`, present only when its
    // `apple-bridge` feature is on. Discovered rather than spelled out, so a
    // rename of the `links` key cannot silently drop the flags.
    if let Some((_, flags)) =
        std::env::vars().find(|(key, _)| key.starts_with("DEP_") && key.ends_with("_LINK_ARGS"))
    {
        for flag in flags.split_whitespace() {
            // The plain form covers the library's own unit-test binary,
            // which the `-bins` and `-tests` forms do not.
            println!("cargo:rustc-link-arg={flag}");
        }
    }

    tauri_build::build();
}
