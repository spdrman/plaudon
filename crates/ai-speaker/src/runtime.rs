//! The ONNX Runtime binaries the `onnx` feature links, as facts (ADR-0081 §3).
//!
//! `ort`'s `download-binaries` feature fetches a prebuilt ONNX Runtime over
//! TLS *from the build script*. A release build that reaches a third-party CDN
//! while it compiles has no reproducible input and no reviewable provenance,
//! so this crate does not use that feature at all: `ort-sys` is built with
//! `ORT_LIB_LOCATION` pointing at an archive this project fetched, verified and
//! recorded here.
//!
//! The records below are the same bytes `download-binaries` would have
//! fetched — the URLs and digests are `ort-sys`'s own `build/download/dist.tsv`
//! for the pinned `ort` version — which is deliberate: the build no longer
//! downloads anything, and the artifact it links is still the upstream one,
//! verified against the upstream table rather than against a value we invented.
//!
//! Nothing here downloads anything. `scripts/fetch-onnxruntime.sh` (and
//! `scripts/fetch-onnxruntime.ps1` on Windows) is the acquisition half, and
//! `plaudon-integration-tests --test licensing_register` fails when the script,
//! `docs/licensing/third-party-register.md`, `NOTICE` and this module stop
//! agreeing.
//!
//! # Evidence, not assumption
//!
//! [`ArchiveEvidence`] is a field rather than a sentence in a document because
//! the two targets are not in the same state: the macOS arm64 archive has been
//! fetched, verified, extracted and *linked* into a working binary on this
//! project's reference machine; the Windows x64 archive has been fetched,
//! verified and listed, and nothing more, because no Windows x86-64 machine
//! exists in this project's tooling (`docs/PRECOMMIT.md` §3,
//! `docs/release/voice-profiles-evidence.md`). A build that claimed otherwise
//! would be claiming a measurement nobody took.

use std::fmt;

/// Version of ONNX Runtime every record below carries.
pub const ONNXRUNTIME_VERSION: &str = "1.31.0";

/// Directory, relative to the repository root, the fetch script extracts into.
///
/// It is **not** committed: the extracted static library is 83 MB on macOS and
/// 343 MB on Windows, and a repository is not a binary mirror. The digest is
/// what is committed.
pub const VENDOR_DIR: &str = "vendor/onnxruntime";

/// How far a record has actually been taken on a real machine of that target.
///
/// There is no third variant for "should work": a target is either one we
/// linked or one we only inspected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveEvidence {
    /// Fetched, digest-verified, extracted, and linked into a binary that ran.
    Linked {
        /// Machine the link and the run happened on.
        host: &'static str,
        /// Date of that run, ISO-8601.
        date: &'static str,
    },
    /// Fetched, digest-verified and its member files listed — nothing else.
    ///
    /// The archive is known-good bytes; whether `ort-sys` links *this* archive
    /// into a working binary on that target is not known to this project yet.
    FetchedAndListed {
        /// Machine the fetch and the listing happened on.
        host: &'static str,
        /// Date of that fetch, ISO-8601.
        date: &'static str,
        /// What stands between this record and [`ArchiveEvidence::Linked`].
        outstanding: &'static str,
    },
}

impl ArchiveEvidence {
    /// `true` only when a binary was actually linked against the archive.
    #[must_use]
    pub const fn is_linked(&self) -> bool {
        matches!(self, Self::Linked { .. })
    }
}

/// One vendored ONNX Runtime distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeArchive {
    /// Rust target triple this archive is for.
    pub target: &'static str,
    /// ONNX Runtime version it contains.
    pub onnxruntime_version: &'static str,
    /// Execution providers the upstream build was compiled with. It is part of
    /// the identity of the artifact: the Windows build carries DirectML, which
    /// is why linking it needs the Direct3D import libraries.
    pub feature_set: &'static str,
    /// Exact URL the bytes are fetched from.
    pub url: &'static str,
    /// Lowercase hexadecimal SHA-256 of the fetched archive.
    pub sha256_hex: &'static str,
    /// Exact size in bytes of the fetched archive.
    pub byte_length: u64,
    /// Name of the static library inside the archive, which is what
    /// `ORT_LIB_LOCATION` must end up pointing at the directory of.
    pub library_file_name: &'static str,
    /// Size in bytes of that library once extracted.
    pub library_byte_length: u64,
    /// Every other member of the archive, by name.
    pub other_members: &'static [&'static str],
    /// SPDX identifier of the licence the ONNX Runtime binary ships under.
    pub licence_spdx: &'static str,
    /// Copyright holder named in that licence.
    pub licence_holder: &'static str,
    /// How far this record has been taken on a real machine.
    pub evidence: ArchiveEvidence,
}

impl RuntimeArchive {
    /// File name the archive is stored under by the fetch script.
    #[must_use]
    pub fn archive_file_name(&self) -> &'static str {
        // The last path segment of the URL; every dist URL has one.
        self.url.rsplit('/').next().unwrap_or(self.url)
    }

    /// Path, relative to the repository root, the library is extracted to.
    #[must_use]
    pub fn vendor_dir(&self) -> String {
        format!("{VENDOR_DIR}/{}", self.target)
    }
}

impl fmt::Display for RuntimeArchive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ONNX Runtime {} for {} ({})",
            self.onnxruntime_version, self.target, self.feature_set
        )
    }
}

/// macOS arm64: the archive this project builds and measures against.
pub const MACOS_ARM64: RuntimeArchive = RuntimeArchive {
    target: "aarch64-apple-darwin",
    onnxruntime_version: ONNXRUNTIME_VERSION,
    feature_set: "coreml",
    url: "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.31.0/aarch64-apple-darwin+coreml.tar.lzma2",
    sha256_hex: "4033efca0e6c1606bd2ec3870d4cb19d0dba0ad0160dbfb616e66cae969ed68c",
    byte_length: 9_687_276,
    library_file_name: "libonnxruntime.a",
    library_byte_length: 83_546_072,
    other_members: &[],
    licence_spdx: "MIT",
    licence_holder: "Microsoft Corporation",
    evidence: ArchiveEvidence::Linked {
        host: "Apple M5, macOS 27.0.1, Rust 1.92.0",
        date: "2026-10-12",
    },
};

/// Windows x64: fetched and verified, never linked here.
///
/// The upstream build carries the DirectML execution provider, so linking it
/// needs `DirectML.lib`, `dxguid`, `dxcore`, `dxgi` and `d3d12` from the
/// Windows SDK. `ort-sys` emits those link directives only on its own download
/// path, so `build.rs` in this crate emits them for the vendored path. That
/// build script has never been run on Windows by this project, which is what
/// `outstanding` says.
pub const WINDOWS_X64: RuntimeArchive = RuntimeArchive {
    target: "x86_64-pc-windows-msvc",
    onnxruntime_version: ONNXRUNTIME_VERSION,
    feature_set: "directml",
    url: "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.31.0/x86_64-pc-windows-msvc+directml.tar.lzma2",
    sha256_hex: "f99aaef23f12986abb05924c944d8433521263ad756b462df762a5a982c98405",
    byte_length: 32_457_673,
    library_file_name: "onnxruntime.lib",
    library_byte_length: 359_948_106,
    other_members: &["DirectML.dll"],
    licence_spdx: "MIT",
    licence_holder: "Microsoft Corporation",
    evidence: ArchiveEvidence::FetchedAndListed {
        host: "Apple M5, macOS 27.0.1 (the archive was fetched and verified here, not used here)",
        date: "2026-10-12",
        outstanding: "on a Windows 11 x64 machine with the pinned toolchain: run \
                      scripts/fetch-onnxruntime.ps1, then \
                      `cargo build --offline -p plaudon-ai-speaker --features onnx` with \
                      ORT_LIB_LOCATION set, and record whether the Direct3D/DirectML link \
                      libraries build.rs adds are sufficient",
    },
};

/// Every vendored runtime, in display order.
pub const VENDORED_RUNTIMES: &[RuntimeArchive] = &[MACOS_ARM64, WINDOWS_X64];

/// The record for a Rust target triple, if this project vendors one.
#[must_use]
pub fn for_target(target: &str) -> Option<&'static RuntimeArchive> {
    VENDORED_RUNTIMES
        .iter()
        .find(|archive| archive.target == target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_record_names_an_archive_file_and_a_vendor_directory() {
        for archive in VENDORED_RUNTIMES {
            assert!(
                archive.archive_file_name().ends_with(".tar.lzma2"),
                "{archive} is not the `.tar.lzma2` dist `ort-sys` publishes"
            );
            assert_eq!(
                archive.vendor_dir(),
                format!("vendor/onnxruntime/{}", archive.target)
            );
            assert_eq!(archive.sha256_hex.len(), 64);
            assert!(archive
                .sha256_hex
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            assert!(archive.url.starts_with("https://"));
        }
    }

    #[test]
    fn the_targets_are_the_two_reference_platforms_and_nothing_else() {
        let targets: Vec<&str> = VENDORED_RUNTIMES.iter().map(|a| a.target).collect();
        assert_eq!(targets, ["aarch64-apple-darwin", "x86_64-pc-windows-msvc"]);
        assert!(for_target("x86_64-unknown-linux-gnu").is_none());
        assert_eq!(for_target("aarch64-apple-darwin"), Some(&MACOS_ARM64));
    }

    #[test]
    fn the_windows_archive_does_not_claim_a_link_nobody_performed() {
        assert!(MACOS_ARM64.evidence.is_linked());
        let ArchiveEvidence::FetchedAndListed { outstanding, .. } = WINDOWS_X64.evidence else {
            panic!("the Windows record claims a link that no machine in this project performed");
        };
        assert!(outstanding.contains("Windows 11 x64"));
    }
}
