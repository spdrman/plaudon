//! Optional narrow local automation CLI. No public HTTP daemon.
//!
//! Spec 14.5 permits "an optional, explicitly enabled local scripting bridge"
//! exposing `list`, `export`, `process` and `jobs status`, and prohibits an
//! unauthenticated loopback REST server in V1. App. A.5 adds that automation
//! is restricted to the current signed-in OS user and that cost-bearing work
//! needs explicit cloud consent.
//!
//! What that means in this crate:
//!
//! * There is **no network code at all**. This crate depends on no HTTP
//!   client, no server, no socket library, and defines no `serve`, `daemon`
//!   or `listen` command. The prohibition is structural, not a default.
//! * The bridge is off until a user enables it, and the OS user who enabled
//!   it is recorded ([`bridge`]).
//! * Consent and Local-only Mode are read from the same rows the GUI writes
//!   ([`policy`]), so revoking consent in the app revokes it for scripts.
//! * Commands delegate to `plaudon_app_core` and `plaudon_jobs`
//!   ([`commands`]); there is no second implementation of anything the GUI
//!   does.
//! * All output is one JSON envelope ([`output`]) built from typed values, so
//!   a secret, a signed URL or a transcript excerpt has no path into stdout.

pub mod bridge;
pub mod cli;
pub mod commands;
pub mod output;
pub mod policy;

pub use bridge::{BridgeRefusal, BridgeState, BridgeStateStore};
pub use cli::{Cli, Command};
pub use commands::run;
pub use output::Envelope;
pub use policy::{Destination, EffectivePolicy, PolicyRefusal};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-automation";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;

/// Default library file name inside the application data directory.
pub const DEFAULT_LIBRARY_FILE: &str = "library.sqlite";
