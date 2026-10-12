//! Watched-folder source with stable-file completion barriers (§4.3, §7.10,
//! App. A.2, App. A.4).
//!
//! # The one idea in this crate
//!
//! A filesystem event is a rumour. Everything here exists to turn rumours into
//! a decision that is safe to act on:
//!
//! ```text
//!  notify / inventory sweep  ──hints──▶  CoalescingQueue  ──one per path──▶
//!      CompletionBarrier  ──ready──▶  ImportService (§13.3A durable import)
//!                         ──defer──▶  App. A.2 "still changing" / "placeholder"
//! ```
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`config`] | the folder, canonicalised through `plaudon_security::fsguard` |
//! | [`barrier`] | quiescence, locality, readability, sniff, disk, digest |
//! | [`placeholder`] | cloud-drive placeholder detection, as a trait |
//! | [`queue`] | debounce and coalescing, keyed by canonical path |
//! | [`watcher`] | `notify` hints, plus a scripted source for fixtures |
//! | [`inventory`] | the bounded sweep that recovers dropped events |
//! | [`service`] | the use case: import, provenance, metrics, folder rules |
//!
//! # Guarantees
//!
//! * A source file is never renamed, moved, truncated or deleted — including
//!   as failure cleanup (§4.3(4), App. A.4). Byte identity is re-checked after
//!   every import and recorded as a §17.4 metric.
//! * A cloud placeholder is a retryable status, never a recording (§7.10).
//! * A vanished file marks the association unavailable; the archived recording
//!   and its derivatives stay (§4.3(6)).
//! * `ask_before_cloud` refuses background dispatch without an explicit valid
//!   consent grant, through `plaudon_security::consent` (§4.3(7), §16.3).
//!
//! # Non-guarantees
//!
//! Detection of a cloud placeholder is a heuristic over what the platform
//! exposes; a sync client that hides eviction entirely will be noticed only by
//! the read probe failing. Quiescence is evidence, not proof: a writer that
//! pauses for longer than the window and resumes can still be caught
//! mid-write, which is why the digest is re-validated against a second stat
//! and discarded if anything moved.

pub mod barrier;
pub mod clock;
pub mod config;
pub mod error;
pub mod fsmeta;
pub mod inventory;
pub mod placeholder;
pub mod queue;
pub mod service;
pub mod sniff;
pub mod watcher;

pub use barrier::{BarrierOutcome, BarrierPolicy, CompletionBarrier, DeferReason, ReadyCandidate};
pub use clock::{Clock, ManualClock, SystemClock};
pub use config::{FolderCaps, FolderRule, WatchedFolderConfig};
pub use error::{FolderError, FolderResult};
pub use fsmeta::{stable_file_id, DiskSpaceProbe, FixedDiskSpaceProbe, StableFileId};
pub use inventory::{sweep, InventoryReport};
pub use placeholder::{
    OsPlaceholderProbe, PlaceholderProbe, PlaceholderReason, PlaceholderState,
    ScriptedPlaceholderProbe,
};
pub use queue::{CoalescingQueue, OfferOutcome, PendingCandidate};
pub use service::{
    CandidateOutcome, CloudDispatchRefusal, LocalWorkSink, NoLocalWork, QueueingLocalWorkSink,
    WatchedFolderService,
};
pub use sniff::{sniff as sniff_container, MediaContainer};
pub use watcher::{FsEventSource, FsHint, NotifyWatcher, ScriptedEventSource};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-source-folder";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
///
/// Capability and contract payloads are versioned independently of the
/// application release, so consumers can detect an unknown contract instead of
/// assuming an unavailable feature is present.
pub const CONTRACT_VERSION: u32 = 1;
