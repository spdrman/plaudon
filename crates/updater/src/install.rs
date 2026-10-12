//! Crash-safe install and verified rollback (SPEC v1.3 §16.5, §22 item 8,
//! §18.6A "Archive" row, §14.6 recovery tests).
//!
//! # Layout
//!
//! ```text
//! <root>/journal.json          atomically replaced, single source of truth
//! <root>/slots/a/manifest.json signed envelope that authorised slot a
//! <root>/slots/a/payload.bin   the package bytes installed in slot a
//! <root>/slots/b/...           the other slot
//! <root>/staging/...           the in-flight download, never launched
//! ```
//!
//! # Why A/B slots
//!
//! "Interruption at any step leaves a launchable application" cannot be met
//! by moving a directory into place: a multi-step move has no atomic point.
//! With two slots the only mutation that changes what gets launched is a
//! **single** `rename(2)` of the journal, which POSIX makes atomic. Every
//! other step writes into the slot that is *not* currently active, so a crash
//! anywhere before the commit leaves the previous installation bit-identical,
//! and a crash anywhere after it leaves the new one fully written.
//!
//! # State machine
//!
//! `stage → verify → swap → commit`, with [`PendingState`] recorded in the
//! journal after each step so a restart can tell how far the attempt got.
//! Recovery on [`Installer::open`] discards any pending attempt: the staged
//! bytes are untrusted until committed, so there is nothing to resume and
//! nothing to repair.
//!
//! # Rollback
//!
//! [`Installer::rollback`] runs the *same* [`UpdateVerifier`] calls, in the
//! same order, over the retained slot's own manifest and payload (§16.5:
//! "rollback must not weaken signature checking"). A tampered retained
//! payload is rejected exactly as a tampered download is. The
//! `minimum_supported_version` floor is waived only when the retained
//! artifact is byte-for-byte the one the journal recorded as previously
//! verified; anything else is subject to the floor, closing the "swap the
//! rollback artifact and downgrade" hole.

use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::UpdateError;
use crate::manifest::{Sha256Digest, Target, Version};
use crate::verify::{check_forward_progress, check_version_policy, sha256, UpdateVerifier};

/// Journal schema version understood by this build.
pub const JOURNAL_SCHEMA_VERSION: u32 = 1;

const JOURNAL_FILE: &str = "journal.json";
const JOURNAL_TMP_FILE: &str = "journal.json.tmp";
const SLOTS_DIR: &str = "slots";
const STAGING_DIR: &str = "staging";
const MANIFEST_FILE: &str = "manifest.json";
const PAYLOAD_FILE: &str = "payload.bin";

/// One of the two install slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    /// First slot.
    A,
    /// Second slot.
    B,
}

impl Slot {
    /// The other slot; the target of the next install.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    /// Directory name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the journal knows about an installed slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotRecord {
    /// Which slot holds the payload.
    pub slot: Slot,
    /// Version installed there.
    pub version: Version,
    /// SHA-256 of the payload that was verified before it was written.
    pub package_sha256: Sha256Digest,
}

/// How far an in-flight attempt got before the process stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingState {
    /// Bytes written to `staging/`, nothing verified yet.
    Staged,
    /// Manifest and package passed the §16.5 verification path.
    Verified,
    /// Verified bytes written into the inactive slot; not yet active.
    Swapped,
}

/// An install attempt that has not been committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRecord {
    /// Slot the attempt targets. Never the active slot.
    pub slot: Slot,
    /// How far the attempt got.
    pub state: PendingState,
}

/// The atomically replaced record of what is installed (§13.3A journal
/// discipline, applied to the updater).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    /// Journal schema version.
    pub schema_version: u32,
    /// The slot that is launched.
    pub active: SlotRecord,
    /// The retained previous version, if one exists and is still intact.
    pub previous: Option<SlotRecord>,
    /// Highest `minimum_supported_version` this installation has committed
    /// to. Monotonic: a committed release can raise it, nothing lowers it.
    pub floor: Version,
    /// An uncommitted attempt, cleared by recovery.
    pub pending: Option<PendingRecord>,
}

/// Where the state machine can be interrupted.
///
/// The chaos suite walks [`InterruptPoint::ALL`] and asserts that after every
/// single one the installation re-opens launchable (§18.6A "Archive" row:
/// "crash before/after rename ... every completed record has durable bytes").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptPoint {
    /// Before anything is written to `staging/`.
    BeforeStage,
    /// After `staging/` is populated and journalled.
    AfterStage,
    /// Before the signature and digest checks run.
    BeforeVerify,
    /// After verification succeeded and was journalled.
    AfterVerify,
    /// Before the verified bytes are written into the inactive slot.
    BeforeSwap,
    /// After the inactive slot is complete and journalled.
    AfterSwap,
    /// Before the single atomic journal rename that flips the active slot.
    BeforeCommit,
    /// After the flip.
    AfterCommit,
    /// Before rollback re-verifies the retained artifact.
    BeforeRollbackVerify,
    /// After rollback's verification succeeded.
    AfterRollbackVerify,
    /// Before the atomic journal rename that restores the previous slot.
    BeforeRollbackCommit,
    /// After the restore.
    AfterRollbackCommit,
}

impl InterruptPoint {
    /// Every interruption point, in state-machine order.
    pub const ALL: &'static [Self] = &[
        Self::BeforeStage,
        Self::AfterStage,
        Self::BeforeVerify,
        Self::AfterVerify,
        Self::BeforeSwap,
        Self::AfterSwap,
        Self::BeforeCommit,
        Self::AfterCommit,
        Self::BeforeRollbackVerify,
        Self::AfterRollbackVerify,
        Self::BeforeRollbackCommit,
        Self::AfterRollbackCommit,
    ];

    /// `true` for points reached by [`Installer::rollback`].
    #[must_use]
    pub const fn is_rollback(self) -> bool {
        matches!(
            self,
            Self::BeforeRollbackVerify
                | Self::AfterRollbackVerify
                | Self::BeforeRollbackCommit
                | Self::AfterRollbackCommit
        )
    }
}

impl fmt::Display for InterruptPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::BeforeStage => "before_stage",
            Self::AfterStage => "after_stage",
            Self::BeforeVerify => "before_verify",
            Self::AfterVerify => "after_verify",
            Self::BeforeSwap => "before_swap",
            Self::AfterSwap => "after_swap",
            Self::BeforeCommit => "before_commit",
            Self::AfterCommit => "after_commit",
            Self::BeforeRollbackVerify => "before_rollback_verify",
            Self::AfterRollbackVerify => "after_rollback_verify",
            Self::BeforeRollbackCommit => "before_rollback_commit",
            Self::AfterRollbackCommit => "after_rollback_commit",
        };
        f.write_str(name)
    }
}

/// Deterministic interruption seam for the chaos suite (§14.6).
///
/// Returning `false` models the process dying at that point: the installer
/// returns [`UpdateError::Interrupted`] immediately and performs **no**
/// cleanup, exactly as a `SIGKILL` would. Production wires
/// [`NoInterrupt`].
pub trait InterruptHook: fmt::Debug + Send + Sync {
    /// `true` to continue, `false` to simulate death at `point`.
    fn should_continue(&self, point: InterruptPoint) -> bool;
}

/// The production hook: never interrupts.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoInterrupt;

impl InterruptHook for NoInterrupt {
    fn should_continue(&self, _point: InterruptPoint) -> bool {
        true
    }
}

/// Dies at exactly one point. Used by the chaos suite.
#[derive(Debug, Clone, Copy)]
pub struct InterruptAt(pub InterruptPoint);

impl InterruptHook for InterruptAt {
    fn should_continue(&self, point: InterruptPoint) -> bool {
        point != self.0
    }
}

/// What an operator or the launcher needs to know to start the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchableState {
    /// Slot to launch.
    pub slot: Slot,
    /// Version in that slot.
    pub version: Version,
    /// Absolute path of the payload to execute.
    pub payload: PathBuf,
}

/// The install/rollback state machine.
#[derive(Debug)]
pub struct Installer {
    root: PathBuf,
    journal: Journal,
    verifier: Arc<dyn UpdateVerifier>,
    hook: Arc<dyn InterruptHook>,
    host: Target,
}

impl Installer {
    /// Register the shipped build as the initial active slot (§16.5).
    ///
    /// The shipped artifact goes through the identical verification path as
    /// any later update: an installation that was never verified could not
    /// honestly be rolled back *to*. `host` is the target this build was
    /// produced for — passed in rather than inferred so that a wrong-target
    /// manifest is rejected identically on every machine the suite runs on
    /// (§22 item 17).
    pub fn bootstrap(
        root: impl Into<PathBuf>,
        verifier: Arc<dyn UpdateVerifier>,
        host: Target,
        envelope: &[u8],
        package: &[u8],
    ) -> Result<Self, UpdateError> {
        let root = root.into();
        let verified = verifier.verify_manifest(envelope)?;
        verified.document().check_target(host)?;
        verifier.verify_package(&verified, package)?;

        fs::create_dir_all(root.join(SLOTS_DIR)).map_err(|e| UpdateError::io("create root", e))?;
        write_slot(&root, Slot::A, envelope, package)?;
        let journal = Journal {
            schema_version: JOURNAL_SCHEMA_VERSION,
            active: SlotRecord {
                slot: Slot::A,
                version: verified.version(),
                package_sha256: sha256(package),
            },
            previous: None,
            floor: verified.document().minimum_supported_version,
            pending: None,
        };
        write_journal(&root, &journal)?;
        Ok(Self {
            root,
            journal,
            verifier,
            hook: Arc::new(NoInterrupt),
            host,
        })
    }

    /// Open an existing installation and run recovery (§14.6).
    ///
    /// Recovery is deliberately a *discard*, not a resume: staged bytes carry
    /// no proof that verification completed, so continuing from
    /// [`PendingState::Swapped`] would mean activating a slot whose check
    /// may never have run.
    pub fn open(
        root: impl Into<PathBuf>,
        verifier: Arc<dyn UpdateVerifier>,
        host: Target,
    ) -> Result<Self, UpdateError> {
        let root = root.into();
        let journal = read_journal(&root)?;
        let mut installer = Self {
            root,
            journal,
            verifier,
            hook: Arc::new(NoInterrupt),
            host,
        };
        installer.recover()?;
        Ok(installer)
    }

    /// Install the chaos hook. Production callers never need this.
    #[must_use]
    pub fn with_interrupt_hook(mut self, hook: Arc<dyn InterruptHook>) -> Self {
        self.hook = hook;
        self
    }

    /// The journal as last written.
    #[must_use]
    pub const fn journal(&self) -> &Journal {
        &self.journal
    }

    /// The installation root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The version that would start now.
    #[must_use]
    pub const fn installed_version(&self) -> Version {
        self.journal.active.version
    }

    /// The retained previous version, if any.
    #[must_use]
    pub fn retained_version(&self) -> Option<Version> {
        self.journal.previous.map(|p| p.version)
    }

    /// Resolve and re-check what the launcher would start.
    ///
    /// This is the assertion the chaos suite makes after every simulated
    /// kill: the active slot must exist, carry its signed manifest, and hash
    /// to the digest the journal recorded.
    pub fn launchable(&self) -> Result<LaunchableState, UpdateError> {
        let record = self.journal.active;
        let dir = slot_dir(&self.root, record.slot);
        let payload_path = dir.join(PAYLOAD_FILE);
        if !dir.join(MANIFEST_FILE).is_file() || !payload_path.is_file() {
            return Err(UpdateError::SlotIncomplete {
                slot: record.slot.to_string(),
            });
        }
        let bytes =
            fs::read(&payload_path).map_err(|e| UpdateError::io("read active payload", e))?;
        if sha256(&bytes) != record.package_sha256 {
            return Err(UpdateError::SlotIncomplete {
                slot: record.slot.to_string(),
            });
        }
        Ok(LaunchableState {
            slot: record.slot,
            version: record.version,
            payload: payload_path,
        })
    }

    /// Run the forward update: stage, verify, swap, commit.
    ///
    /// On any rejection the staging area is removed and the journal returns
    /// to a pending-free state with the active slot untouched — "no partial
    /// install on any rejection" (§16.5).
    pub fn install(&mut self, envelope: &[u8], package: &[u8]) -> Result<Version, UpdateError> {
        self.interrupt(InterruptPoint::BeforeStage)?;
        let target = self.journal.active.slot.other();

        match self.try_install(envelope, package, target) {
            Ok(version) => Ok(version),
            Err(UpdateError::Interrupted(point)) => Err(UpdateError::Interrupted(point)),
            Err(other) => {
                self.abandon_pending()?;
                Err(other)
            }
        }
    }

    fn try_install(
        &mut self,
        envelope: &[u8],
        package: &[u8],
        target: Slot,
    ) -> Result<Version, UpdateError> {
        // ---- stage -------------------------------------------------------
        let staging = self.root.join(STAGING_DIR);
        remove_dir_if_present(&staging, "clear staging")?;
        fs::create_dir_all(&staging).map_err(|e| UpdateError::io("create staging", e))?;
        write_file_durable(&staging.join(MANIFEST_FILE), envelope, "stage manifest")?;
        write_file_durable(&staging.join(PAYLOAD_FILE), package, "stage package")?;
        self.journal.pending = Some(PendingRecord {
            slot: target,
            state: PendingState::Staged,
        });
        write_journal(&self.root, &self.journal)?;
        self.interrupt(InterruptPoint::AfterStage)?;

        // ---- verify ------------------------------------------------------
        self.interrupt(InterruptPoint::BeforeVerify)?;
        let verified = self.verifier.verify_manifest(envelope)?;
        verified.document().check_target(self.host)?;
        check_forward_progress(
            verified.document(),
            self.journal.active.version,
            self.journal.floor,
        )?;
        self.verifier.verify_package(&verified, package)?;
        self.journal.pending = Some(PendingRecord {
            slot: target,
            state: PendingState::Verified,
        });
        write_journal(&self.root, &self.journal)?;
        self.interrupt(InterruptPoint::AfterVerify)?;

        // ---- swap --------------------------------------------------------
        self.interrupt(InterruptPoint::BeforeSwap)?;
        // The target slot may currently be the retained previous version. We
        // are about to overwrite it, so stop claiming it first: a journal
        // that advertises a rollback target we have already destroyed is the
        // one way this layout could strand a user.
        if self.journal.previous.is_some_and(|p| p.slot == target) {
            self.journal.previous = None;
            write_journal(&self.root, &self.journal)?;
        }
        write_slot(&self.root, target, envelope, package)?;
        self.journal.pending = Some(PendingRecord {
            slot: target,
            state: PendingState::Swapped,
        });
        write_journal(&self.root, &self.journal)?;
        self.interrupt(InterruptPoint::AfterSwap)?;

        // ---- commit ------------------------------------------------------
        self.interrupt(InterruptPoint::BeforeCommit)?;
        let previous = self.journal.active;
        self.journal.previous = Some(previous);
        self.journal.active = SlotRecord {
            slot: target,
            version: verified.version(),
            package_sha256: sha256(package),
        };
        self.journal.floor = self
            .journal
            .floor
            .max(verified.document().minimum_supported_version);
        self.journal.pending = None;
        write_journal(&self.root, &self.journal)?;
        self.interrupt(InterruptPoint::AfterCommit)?;

        remove_dir_if_present(&staging, "clear staging")?;
        Ok(verified.version())
    }

    /// Return to the retained previous version, re-running the identical
    /// verification path (§16.5).
    ///
    /// The retained slot's own signed manifest and payload are verified again
    /// from disk. A tampered payload, a tampered manifest or a substituted
    /// artifact is rejected and the active slot does not move. The
    /// `minimum_supported_version` floor is waived only when the artifact is
    /// exactly the one the journal recorded as previously verified.
    pub fn rollback(&mut self) -> Result<Version, UpdateError> {
        let previous = self
            .journal
            .previous
            .ok_or(UpdateError::NoRetainedVersion)?;

        self.interrupt(InterruptPoint::BeforeRollbackVerify)?;
        let dir = slot_dir(&self.root, previous.slot);
        let envelope = fs::read(dir.join(MANIFEST_FILE))
            .map_err(|e| UpdateError::io("read retained manifest", e))?;
        let package = fs::read(dir.join(PAYLOAD_FILE))
            .map_err(|e| UpdateError::io("read retained package", e))?;

        let verified = self.verifier.verify_manifest(&envelope)?;
        verified.document().check_target(self.host)?;
        self.verifier.verify_package(&verified, &package)?;

        // Is this the artifact we ourselves verified and committed earlier?
        // Identity is the recorded version *and* digest; either one differing
        // means the retained slot was substituted behind our back.
        let is_retained_artifact =
            verified.version() == previous.version && sha256(&package) == previous.package_sha256;
        check_version_policy(
            verified.document(),
            self.journal.active.version,
            self.journal.floor,
            is_retained_artifact,
        )?;
        self.interrupt(InterruptPoint::AfterRollbackVerify)?;

        self.interrupt(InterruptPoint::BeforeRollbackCommit)?;
        let demoted = self.journal.active;
        self.journal.active = previous;
        // The slot we just left is intact and was verified on the way in, so
        // it becomes the new rollback target; rolling back twice toggles.
        self.journal.previous = Some(demoted);
        self.journal.pending = None;
        write_journal(&self.root, &self.journal)?;
        self.interrupt(InterruptPoint::AfterRollbackCommit)?;
        Ok(previous.version)
    }

    fn interrupt(&self, point: InterruptPoint) -> Result<(), UpdateError> {
        if self.hook.should_continue(point) {
            Ok(())
        } else {
            Err(UpdateError::Interrupted(point))
        }
    }

    /// Drop an uncommitted attempt and its staged bytes.
    fn abandon_pending(&mut self) -> Result<(), UpdateError> {
        remove_dir_if_present(&self.root.join(STAGING_DIR), "clear staging")?;
        if self.journal.pending.take().is_some() {
            write_journal(&self.root, &self.journal)?;
        }
        Ok(())
    }

    fn recover(&mut self) -> Result<(), UpdateError> {
        if self.journal.pending.is_some() {
            tracing::warn!(
                active = %self.journal.active.slot,
                "discarding an uncommitted update attempt found at startup"
            );
        }
        self.abandon_pending()
    }
}

fn slot_dir(root: &Path, slot: Slot) -> PathBuf {
    root.join(SLOTS_DIR).join(slot.as_str())
}

fn write_slot(
    root: &Path,
    slot: Slot,
    envelope: &[u8],
    package: &[u8],
) -> Result<(), UpdateError> {
    let dir = slot_dir(root, slot);
    remove_dir_if_present(&dir, "clear slot")?;
    fs::create_dir_all(&dir).map_err(|e| UpdateError::io("create slot", e))?;
    // Payload first: a slot that has a manifest but no payload would look
    // more plausible to a careless reader than one that has neither.
    write_file_durable(&dir.join(PAYLOAD_FILE), package, "write slot payload")?;
    write_file_durable(&dir.join(MANIFEST_FILE), envelope, "write slot manifest")?;
    sync_dir(&dir);
    Ok(())
}

fn remove_dir_if_present(dir: &Path, context: &'static str) -> Result<(), UpdateError> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(UpdateError::io(context, e)),
    }
}

fn write_file_durable(path: &Path, bytes: &[u8], context: &'static str) -> Result<(), UpdateError> {
    let mut file = fs::File::create(path).map_err(|e| UpdateError::io(context, e))?;
    file.write_all(bytes).map_err(|e| UpdateError::io(context, e))?;
    file.sync_all().map_err(|e| UpdateError::io(context, e))?;
    Ok(())
}

/// Replace the journal atomically: write a temporary file, flush it to
/// stable storage, then `rename` over the live one. The rename is the only
/// moment at which the launched version changes.
fn write_journal(root: &Path, journal: &Journal) -> Result<(), UpdateError> {
    let bytes =
        serde_json::to_vec_pretty(journal).map_err(|source| UpdateError::Encoding { source })?;
    let tmp = root.join(JOURNAL_TMP_FILE);
    write_file_durable(&tmp, &bytes, "write journal")?;
    fs::rename(&tmp, root.join(JOURNAL_FILE)).map_err(|e| UpdateError::io("commit journal", e))?;
    sync_dir(root);
    Ok(())
}

fn read_journal(root: &Path) -> Result<Journal, UpdateError> {
    let bytes = fs::read(root.join(JOURNAL_FILE))
        .map_err(|e| UpdateError::JournalCorrupt(e.to_string()))?;
    let journal: Journal =
        serde_json::from_slice(&bytes).map_err(|e| UpdateError::JournalCorrupt(e.to_string()))?;
    if journal.schema_version != JOURNAL_SCHEMA_VERSION {
        return Err(UpdateError::JournalCorrupt(format!(
            "unsupported journal schema_version {}",
            journal.schema_version
        )));
    }
    if journal.previous.is_some_and(|p| p.slot == journal.active.slot) {
        return Err(UpdateError::JournalCorrupt(
            "active and previous name the same slot".to_owned(),
        ));
    }
    Ok(journal)
}

/// Flush a directory entry so a rename survives power loss.
///
/// Only Unix exposes directory handles; on other platforms the rename's own
/// durability is what the filesystem provides, which is why the layout never
/// depends on ordering *between* two renames.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_alternate() {
        assert_eq!(Slot::A.other(), Slot::B);
        assert_eq!(Slot::B.other(), Slot::A);
    }

    #[test]
    fn every_interrupt_point_renders_a_distinct_name() {
        let mut names: Vec<String> = InterruptPoint::ALL.iter().map(ToString::to_string).collect();
        names.sort_unstable();
        let total = names.len();
        names.dedup();
        assert_eq!(names.len(), total, "interrupt point names must be distinct");
        assert_eq!(total, 12);
    }

    #[test]
    fn interrupt_at_fires_once_only() {
        let hook = InterruptAt(InterruptPoint::BeforeCommit);
        assert!(!hook.should_continue(InterruptPoint::BeforeCommit));
        for point in InterruptPoint::ALL {
            if *point != InterruptPoint::BeforeCommit {
                assert!(hook.should_continue(*point));
            }
        }
    }
}
