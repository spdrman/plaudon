//! Versioned adapter for the official PLAUD Cloud CLI (spec 2.1, 7.1-7.3, 7.6).
//!
//! Everything here is written against assumptions we have *not* been allowed
//! to make. Spec 2.1 explicitly forbids assuming JSON mode, stable tabular
//! output, a library API, documented exit codes, pagination, refresh-token
//! access, a stable download URL format or redistribution rights. So the
//! adapter:
//!
//! * searches a fixed candidate list for a constant executable leaf name, and
//!   never auto-approves an arbitrary user-supplied binary ([`discovery`]);
//! * fails closed on any release outside the committed compatibility table,
//!   which is currently empty because no release has been exercised
//!   ([`compatibility`]);
//! * launches the tool only through the security crate's egress gate, with an
//!   argument array, a scrubbed environment, a timeout and output caps, and
//!   never in Local-only Mode ([`runner`]);
//! * treats the CLI's stdout as an untrusted, version-locked format and
//!   refuses a partial or duplicated listing rather than showing it as
//!   complete ([`parser`]);
//! * maps every observed failure onto the stable spec 7.6 codes and renders
//!   only copy we wrote ourselves ([`errors`], [`diagnostics`]);
//! * applies a hard SSRF, redirect, scheme, host, size and content-type policy
//!   to any download URL the CLI yields, streams it to a `.part` file, verifies
//!   it and commits it atomically, and resumes only when range semantics plus
//!   a strong validator were actually observed ([`download`], [`transport`],
//!   [`acquire`]).
//!
//! # What this crate never does
//!
//! It never calls a PLAUD HTTP endpoint directly, never reads the vendor
//! tool's private session storage, never asks a user to paste a bearer token
//! or a browser cookie, and never persists or logs a signed URL.

pub mod acquire;
pub mod compatibility;
pub mod connector;
pub mod diagnostics;
pub mod discovery;
pub mod download;
pub mod errors;
pub mod installed;
pub mod invocation;
pub mod parser;
pub mod runner;
pub mod state;
pub mod transport;

pub use acquire::{AcquireLimits, AcquisitionPipeline, AcquisitionReport, SignedUrlProvider};
pub use compatibility::{
    CliCompatibilityRow, CliCompatibilityTable, CliEvidenceLevel, PaginationEvidence, TableError,
    VersionVerdict,
};
pub use connector::{ListingResult, PlaudCloudCliConnector};
pub use diagnostics::{ConnectorDiagnostics, DisconnectCopy, NextAction};
pub use discovery::{CliDiscovery, DiscoveredCli, DiscoveryError, DiscoveryOrigin, UserApproval};
pub use download::{
    DownloadPolicy, DownloadRejection, RangeSupport, UrlContract, OBSERVED_AUDIO_HOSTS,
};
pub use invocation::{
    CliCommand, FilesPaging, MAX_PAGE_NUMBER, MAX_PAGE_SIZE, PLAUD_SUBCOMMANDS,
    SUPPORTED_CURSOR_FLAGS, SUPPORTED_PAGE_FLAGS, SUPPORTED_PAGE_SIZE_FLAGS,
};
pub use parser::{
    CliVersion, ListingCompleteness, ListingFormat, ParsedListing, ParsedListingPage, ParserError,
    VersionedParser,
};
pub use runner::{CliOutput, CliRunner, RunError, RunLimits, UserInitiated};
pub use state::{ConnectorEvent, ConnectorState, ConnectorStatus};
pub use transport::{HttpFetcher, ReqwestFetcher, TransportError};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-source-plaud-cli";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
