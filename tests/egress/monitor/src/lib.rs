//! `plaudon-egress-monitor` — D2.3 network-deny observation library (spec
//! §16.2A, §1.5, §18.6A "Local-only", §22(15), §24).
//!
//! This is a `cdylib` that is injected into a child process and intercepts the
//! libc entry points through which *any* internet traffic from that process
//! must pass. Every intercepted call is appended as one JSON line to the file
//! named by `PLAUDON_EGRESS_LOG`; every non-loopback internet destination is
//! refused with `EPERM` (or `EAI_FAIL` for resolver calls).
//!
//! The point is evidence, not protection: the application's own
//! `plaudon_security::outbound` gate is what refuses egress in production. This
//! library independently *observes* whether anything got past it, so
//! "Local-only Mode makes no app-initiated requests" is a measured statement
//! rather than a claim about our own code.
//!
//! # What this proves, and what it cannot
//!
//! * It observes **this process** (and children that inherit the injection and
//!   are not restricted binaries). It says nothing about the operating system,
//!   other applications, platform stores, or a cloud-sync client acting on a
//!   user-selected folder.
//! * `DYLD_INSERT_LIBRARIES` is stripped by dyld for hardened-runtime, setuid
//!   and platform binaries; `LD_PRELOAD` is ignored for setuid and static
//!   binaries. Absence of a `monitor_ready` line therefore means "not
//!   observed", which the harness treats as a **failure**, never as a pass.
//! * `socket(2)` is recorded but never refused: creating a socket transmits
//!   nothing, and the destination is not known until `connect`/`sendto`.
//! * Sends on an already-connected socket are not intercepted; the `connect`
//!   that would have to precede them is.
//! * `exec` is recorded, not blocked. "The app never launches the cloud CLI"
//!   is proved by the absence of an `exec` record, not by this library
//!   preventing one.
//!
//! # Trace line schema
//!
//! ```json
//! {"ts_unix":1760000000,"call":"connect","family":"inet",
//!  "host_or_ip":"203.0.113.1","port":443,"denied":true}
//! ```
//!
//! `call` is one of `monitor_ready`, `socket`, `connect`, `connectx`,
//! `sendto`, `getaddrinfo`, `gethostbyname`, `res_query`, `exec`. `exec` lines
//! carry an extra `program` field.

// Interposing libc is inherently `unsafe`; every block below carries a safety
// comment, and no interposed function can unwind (they are `extern "C"`).
#![allow(unsafe_code)]
#![warn(missing_debug_implementations, rust_2018_idioms)]

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-egress-monitor";

pub mod wire;

#[cfg(all(feature = "interpose", any(target_os = "macos", target_os = "linux")))]
pub mod emit;
#[cfg(all(feature = "interpose", any(target_os = "macos", target_os = "linux")))]
mod platform;
#[cfg(all(feature = "interpose", any(target_os = "macos", target_os = "linux")))]
mod trace;

#[cfg(all(feature = "interpose", target_os = "macos"))]
mod interpose_apple;
#[cfg(all(feature = "interpose", target_os = "linux"))]
mod interpose_linux;

/// How this build of the monitor hooks libc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorStatus {
    /// macOS: `__DATA,__interpose` records, injected with `DYLD_INSERT_LIBRARIES`.
    DyldInterpose,
    /// Linux: exported symbol overrides, injected with `LD_PRELOAD`.
    LdPreload,
    /// No interception mechanism is implemented for this target. The crate
    /// still builds so the harness links, but the harness must fail loudly
    /// instead of reporting a vacuous pass.
    Unsupported,
}

impl MonitorStatus {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            MonitorStatus::DyldInterpose => "dyld-interpose",
            MonitorStatus::LdPreload => "ld-preload",
            MonitorStatus::Unsupported => "unsupported",
        }
    }

    #[must_use]
    pub const fn is_supported(self) -> bool {
        !matches!(self, MonitorStatus::Unsupported)
    }

    /// Name of the environment variable that injects this monitor, if any.
    #[must_use]
    pub const fn injection_env(self) -> Option<&'static str> {
        match self {
            MonitorStatus::DyldInterpose => Some("DYLD_INSERT_LIBRARIES"),
            MonitorStatus::LdPreload => Some("LD_PRELOAD"),
            MonitorStatus::Unsupported => None,
        }
    }
}

/// The interception mechanism this *platform* uses. It describes the platform,
/// not whether this particular build installed the hooks — see
/// [`HOOKS_INSTALLED`].
#[cfg(target_os = "macos")]
pub const STATUS: MonitorStatus = MonitorStatus::DyldInterpose;
/// The interception mechanism this platform uses.
#[cfg(target_os = "linux")]
pub const STATUS: MonitorStatus = MonitorStatus::LdPreload;
/// No interception mechanism exists for this platform.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub const STATUS: MonitorStatus = MonitorStatus::Unsupported;

/// Whether this build actually installs the libc hooks.
///
/// The hooks live behind the non-default `interpose` feature so that linking
/// this crate as a plain library — which the harness does, for the wire-format
/// helpers — cannot accidentally interpose the *harness's* own libc calls.
/// Build the injected artifact with
/// `cargo build -p plaudon-egress-monitor --features interpose`.
pub const HOOKS_INSTALLED: bool = cfg!(feature = "interpose") && STATUS.is_supported();

/// Environment variable naming the trace file the monitor appends to.
pub const LOG_ENV_NAME: &str = "PLAUDON_EGRESS_LOG";

/// Opt-out used by CI on platforms where no interception mechanism exists.
/// Without it, an unsupported platform is a test failure, not a skip.
pub const ALLOW_UNSUPPORTED_ENV: &str = "PLAUDON_EGRESS_ALLOW_UNSUPPORTED";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_matches_manifest() {
        assert_eq!(PACKAGE_NAME, env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn status_is_supported_on_ci_platforms() {
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(STATUS.is_supported());
            assert!(STATUS.injection_env().is_some());
        } else {
            assert_eq!(STATUS, MonitorStatus::Unsupported);
            assert_eq!(STATUS.injection_env(), None);
        }
    }

    #[test]
    fn log_env_name_matches_the_bytes_used_by_the_emitter() {
        #[cfg(all(feature = "interpose", any(target_os = "macos", target_os = "linux")))]
        {
            let bytes = crate::emit::LOG_ENV;
            assert_eq!(bytes.last(), Some(&0));
            assert_eq!(&bytes[..bytes.len() - 1], LOG_ENV_NAME.as_bytes());
        }
    }

    #[test]
    fn hooks_are_only_installed_in_an_interposing_build() {
        assert_eq!(
            HOOKS_INSTALLED,
            cfg!(feature = "interpose") && STATUS.is_supported()
        );
    }
}
