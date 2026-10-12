//! SQLite migrations, repositories and the content-addressed media archive.
//!
//! Three things in this crate are load-bearing for data integrity:
//!
//! * [`migrations`] applies versioned SQL files, records a SHA-256 per applied
//!   migration and takes a verified backup before any upgrade, so a tampered,
//!   reordered or failed migration cannot leave an unusable library.
//! * [`archive`] derives every stored path from content identity alone, so a
//!   hostile original filename can never influence where bytes land.
//! * [`journal`] implements the spec 13.3A durable import order and the
//!   startup reconciler, because a SQLite transaction cannot atomically commit
//!   a filesystem rename.

pub mod archive;
pub mod backups;
pub mod db;
pub mod deletion;
pub mod device;
pub mod error;
pub mod intelligence;
pub mod journal;
pub mod migrations;
pub mod repo;
pub mod search;
pub mod speakers;
pub mod tools;
pub mod transcript;

pub use db::{begin_write, Database};
pub use error::{PersistenceError, PersistenceResult};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-persistence";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
