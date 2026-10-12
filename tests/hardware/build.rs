//! Re-emit the Apple bridge's linker flags for this package's test binaries.
//!
//! Cargo applies `rustc-link-arg` only to targets of the package that emitted
//! it, so a dependent that links the Swift static library has to repeat the
//! flags. `crates/ai-apple` publishes them as `links` metadata precisely so
//! the list is stated once; this script copies them across.
//!
//! Without the `/usr/lib/swift` rpath the test binary cannot resolve
//! `libswift_Concurrency.dylib` and aborts at launch, and without the weak
//! framework references it would hard-require Speech and FoundationModels.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // `DEP_<LINKS>_LINK_ARGS`, where `<LINKS>` is the `links` key of
    // `plaudon-ai-apple` uppercased. Discovered rather than spelled out, so a
    // rename of that key does not silently stop the flags from arriving.
    let Some((_, flags)) =
        std::env::vars().find(|(key, _)| key.starts_with("DEP_") && key.ends_with("_LINK_ARGS"))
    else {
        return;
    };
    for flag in flags.split_whitespace() {
        // The `-tests` form covers `tests/*.rs`; the plain form is not
        // available here because this package has no linked target of its
        // own beyond them.
        println!("cargo:rustc-link-arg-tests={flag}");
    }
}
