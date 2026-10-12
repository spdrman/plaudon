//! Re-emit the Apple bridge's linker flags for this package's own targets.
//!
//! Cargo applies `rustc-link-arg` only to targets of the package that emitted
//! it, so a dependent that links the Swift static library has to repeat the
//! flags; `crates/ai-apple` publishes them as `links` metadata precisely so
//! the list is written once. `apps/plaudon-egui` and `tests/hardware` already
//! do this for the application binary and the hardware suite — this package's
//! test binaries need it for the same reason.
//!
//! Without the `/usr/lib/swift` rpath, `cargo test -p plaudon-app
//! --all-features` links a binary dyld refuses to start (`Library not loaded:
//! @rpath/libswift_Concurrency.dylib`), and without the weak framework
//! references it would hard-require Speech and FoundationModels on Macs that
//! have neither.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // `DEP_<LINKS>_LINK_ARGS`, where `<LINKS>` is the `links` key of
    // `plaudon-ai-apple` uppercased, and which Cargo sets only for a direct
    // dependent and only while the `apple-bridge` feature is on. Discovered
    // rather than spelled out, so a rename of that key does not silently stop
    // the flags from arriving.
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
