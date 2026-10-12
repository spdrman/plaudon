//! The validated command layer: one function per published command.
//!
//! Spec §15.1: no generic shell execution, no HTTP proxy and no generic
//! filesystem command is exposed to the user interface. Every command takes
//! one `request` struct, validates it in Rust and returns either a typed
//! response or the §15.4 error envelope. [`crate::command`] turns these
//! functions into the typed [`crate::command::AppCommand`] surface; the
//! allowlist lives there and is pinned by its tests.

pub mod archive;
pub mod cloud;
pub mod devices;
pub mod import;
pub mod jobs;
pub mod library;
pub mod media;
pub mod models;
pub mod outputs;
pub mod search;
pub mod settings;
pub mod sources;
pub mod speakers;
pub mod templates;
pub mod tools;
pub mod transcript;
pub mod update;
