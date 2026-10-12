//! Versioned adapter for the official PLAUD Cloud CLI (spec 2.1, 7.1-7.3).
//!
//! Everything here is written against assumptions we have *not* been allowed
//! to make. Spec 2.1 explicitly forbids assuming JSON mode, stable tabular
//! output, a library API, documented exit codes, pagination, refresh-token
//! access, a stable download URL format or redistribution rights. So the
//! adapter:
//!
//! * refuses to run an executable the user did not point us at through the
//!   OS file picker, and never routes an argument through a shell;
//! * treats the CLI's stdout as an untrusted, version-locked format and
//!   fails closed on an unrecognized version;
//! * maps every observed failure onto the stable spec 7.6 codes and never
//!   surfaces vendor text;
//! * applies a hard SSRF, redirect, scheme, host and size policy to any
//!   download URL the CLI yields, and resumes only when range semantics plus
//!   a strong validator were actually observed.

pub mod download;
pub mod invocation;
pub mod parser;
pub mod state;

pub use download::{DownloadPolicy, DownloadRejection, RangeSupport, UrlContract};
pub use invocation::{CliInvocation, InvocationError, PLAUD_SUBCOMMANDS};
pub use parser::{CliVersion, ParsedListing, ParserError, VersionedParser};
pub use state::{ConnectorEvent, ConnectorState};

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-source-plaud-cli";

/// Version of the inter-module contract exposed by this crate (spec 6.6).
pub const CONTRACT_VERSION: u32 = 1;
