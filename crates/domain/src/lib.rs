//! Core domain identities, media identity, transcripts, capability and error taxonomy.
//!
//! This crate is the root of the dependency graph described by spec 6.3(1):
//! it has no mandatory dependency on Tauri, PLAUD, OpenRouter or Apple, and no
//! network, database or platform dependency. The workspace contains a
//! dependency-boundary test that fails the build if that changes.

pub mod anchor;
pub mod capability;
pub mod claim;
pub mod error;
pub mod ids;
pub mod locale;
pub mod media;
pub mod model;
pub mod money;
pub mod output;
pub mod recording;
pub mod source;
pub mod time;
pub mod timerange;
pub mod transcript;

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-domain";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
