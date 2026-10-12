//! `plaudon-security` — the Rust-owned security policy boundary.
//!
//! Spec references: §6.3A (trust and process boundaries), §16 (security and
//! privacy specification), App. B.7 (local-only security and privacy design).
//!
//! This crate owns the five controls that the rest of the application is not
//! allowed to re-implement:
//!
//! | Module | Control | Spec |
//! |---|---|---|
//! | [`outbound`] | the single egress gate and Local-only Mode | §16.2A |
//! | [`local_only`] | persisted Local-only state, route/destination override, cloud-folder rule | §1.2(15), App. B.7 |
//! | [`secrets`] | OS credential storage, no plaintext fallback | §16.4 |
//! | [`consent`] | scoped consent grants and the dispatch preflight | §16.3 |
//! | [`fsguard`] | path canonicalization, private temp dirs, archive entries | §16.5, §7.4 |
//! | [`sandbox`] | out-of-process isolation for decoders and inference helpers | §6.3A |
//! | [`response`] | secret-shape scanning of serialized command results | §6.3A, §15.4 |
//! | [`domain_map`] | the one bridge from these errors to `plaudon_domain::error::AppError` | §15.4 |
//!
//! # Boundaries
//!
//! * The crate has **no HTTP client**. It owns [`outbound::EgressTicket`],
//!   which an HTTP transport must accept and cannot construct.
//! * The crate has **no database**. Persistence stores opaque references only.
//! * The crate does not depend on a UI toolkit, on any vendor SDK, or on the
//!   domain crate, so it is usable from tests, fuzz targets and helper binaries.
//!
//! # What this crate does not promise
//!
//! See [`outbound::NOT_GUARANTEED`]. In short: we control traffic this process
//! initiates, not the operating system, other applications, platform stores, or
//! the radio-layer confidentiality of a legacy BLE session.

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_debug_implementations, rust_2018_idioms)]

pub mod consent;
pub mod domain_map;
pub mod error;
pub mod fsguard;
pub mod local_only;
pub mod outbound;
pub mod response;
pub mod sandbox;
pub mod secrets;

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-security";

pub use error::{ErrorAction, ErrorCode, ErrorEnvelopeParts, SecurityError};
