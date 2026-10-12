//! Signed application updates with verified rollback
//! (SPEC v1.3 §2.4, §16.5, §18.5, §22 items 8 and 15).
//!
//! §2.4 commits us to inspecting and enforcing update validation ourselves
//! rather than trusting a framework default; §16.5 requires that "updater
//! manifests and packages must pass cryptographic verification" and that
//! "rollback must not weaken signature checking"; §22 item 15 requires that
//! Local-only Mode makes no app-initiated update-check request at all.
//!
//! The crate is split so each of those sentences has exactly one owner:
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`config`] | the one place the endpoint and pinned keys live |
//! | [`manifest`] | the signed envelope and its inner document |
//! | [`verify`] | ed25519 + SHA-256, the only verification path |
//! | [`install`] | crash-safe A/B install, commit and verified rollback |
//! | [`policy`] | the Local-only gate in front of any transport |
//! | [`error`] | typed rejections mapped onto the §15.4 taxonomy |
//! | [`testkeys`] | clearly-marked test-only keys and fixture generation |
//!
//! # Usage sketch
//!
//! ```no_run
//! use std::sync::Arc;
//! use plaudon_updater::install::Installer;
//! use plaudon_updater::policy::{EgressPolicy, UpdateCheck, UpdateChecker, UpdateTransport};
//! use plaudon_updater::verify::Ed25519Verifier;
//!
//! # fn demo<T: UpdateTransport>(transport: T, root: &std::path::Path)
//! # -> Result<(), plaudon_updater::error::UpdateError> {
//! let verifier = Arc::new(Ed25519Verifier::embedded());
//! let mut installer = Installer::open(root, verifier.clone())?;
//!
//! let checker = UpdateChecker::new(
//!     transport,
//!     Ed25519Verifier::embedded(),
//!     EgressPolicy::LocalOnly,
//!     installer.installed_version(),
//!     installer.journal().floor,
//! );
//!
//! match checker.check_for_update()? {
//!     UpdateCheck::Suppressed { reason } => println!("{}", reason.explanation()),
//!     UpdateCheck::UpToDate { .. } => {}
//!     UpdateCheck::Available(manifest) => {
//!         let package = checker.download_package(&manifest)?;
//!         let envelope = transport_envelope_bytes();
//!         installer.install(&envelope, &package)?;
//!     }
//! }
//! # Ok(())
//! # }
//! # fn transport_envelope_bytes() -> Vec<u8> { Vec::new() }
//! ```

pub mod config;
pub mod error;
pub mod install;
pub mod manifest;
pub mod policy;
pub mod testkeys;
pub mod verify;
