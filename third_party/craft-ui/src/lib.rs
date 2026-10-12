//! Vendored egui layout primitives from `storytold/craft-libs`.
//!
//! Plaudon's dependency policy denies git sources (`deny.toml`,
//! `[sources] unknown-git = "deny"`) and `craft-ui` is not published to
//! crates.io, so the two pieces Plaudon actually reuses are vendored here at a
//! pinned revision instead of tracked: the split geometry and the
//! [`layout::ResizeHandle`], which is the strongest accessibility asset found
//! in the reference applications (`Role::Splitter`, numeric value/min/max/step,
//! `Increment`/`Decrement`/`SetValue`, arrow keys with focus lock).
//!
//! Provenance, licence terms and the exact upstream revision are recorded in
//! `third_party/craft-ui/README.md`, `NOTICE`, `LICENSE-MIT` and
//! `LICENSE-APACHE`, and in `docs/licensing/third-party-register.md`. Every
//! source file carries a header naming the upstream path, the revision and
//! what was changed.
//!
//! This crate is a snapshot, not an upstream to track: do not add features
//! here that belong in Plaudon's own UI crates.

pub mod layout;
