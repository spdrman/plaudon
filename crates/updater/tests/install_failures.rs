//! Failure injection for the install/rollback state machine (SPEC v1.3
//! §16.5, §18.5, §18.6A "Archive" row, §14.6 recovery tests).
//!
//! The happy path and the twelve interruption points are owned by the inline
//! suite in `src/install.rs`. This suite owns everything that goes wrong
//! *around* them: an unreadable or self-contradictory journal, a slot whose
//! bytes were removed or rewritten behind our back, a filesystem that refuses
//! writes, and a rollback whose retained artifact is missing, unsigned or
//! foreign. The invariant asserted after every single one is the same:
//! whatever was launchable before the failure is still launchable after it.

mod support;

use std::fs;
use std::path::Path;

use plaudon_updater::error::{UpdateError, VerifyError};
use plaudon_updater::install::{
    Installer, InterruptHook as _, InterruptPoint, Journal, NoInterrupt, PendingRecord,
    PendingState, Slot,
};
use plaudon_updater::manifest::Version;
use plaudon_updater::testkeys::{
    build_artifact, package_bytes, test_signing_key, FixtureFlaw, BASE_FIXTURE_NAME,
    FIXTURE_TARGET, TEST_KEY_ID,
};

use support::{bootstrapped, load, manifest_for, signed_envelope, verifier};

const V1: Version = Version::new(1, 0, 0);
const V2: Version = Version::new(1, 1, 0);

fn slot_dir(root: &Path, slot: Slot) -> std::path::PathBuf {
    root.join("slots").join(slot.as_str())
}

fn journal_path(root: &Path) -> std::path::PathBuf {
    root.join("journal.json")
}

fn read_journal(root: &Path) -> Journal {
    serde_json::from_slice(&fs::read(journal_path(root)).expect("journal readable"))
        .expect("journal parses")
}

/// Bootstrap v1.0.0 and install v1.1.0 on top, leaving v1.0.0 retained.
fn updated(root: &Path) -> Installer {
    let mut installer = bootstrapped(root, verifier());
    let update = load(FixtureFlaw::None.fixture_name());
    installer
        .install(update.envelope.as_bytes(), &update.package)
        .expect("the valid fixture installs");
    assert_eq!(installer.installed_version(), V2);
    assert_eq!(installer.retained_version(), Some(V1));
    installer
}

// ---- Slot and hook surface ---------------------------------------------

#[test]
fn the_two_slots_are_each_other_s_other() {
    assert_eq!(Slot::A.other(), Slot::B);
    assert_eq!(Slot::B.other(), Slot::A);
    assert_eq!(Slot::A.other().other(), Slot::A);
    assert_eq!(Slot::A.as_str(), "a");
    assert_eq!(Slot::B.as_str(), "b");
    assert_eq!(Slot::A.to_string(), "a");
    assert_eq!(Slot::B.to_string(), "b");
    assert_ne!(Slot::A, Slot::B);
    assert_eq!(
        serde_json::to_string(&Slot::B).expect("encodes"),
        "\"b\"",
        "the journal spelling is the directory name"
    );
    assert_eq!(
        serde_json::from_str::<Slot>("\"a\"").expect("decodes"),
        Slot::A
    );
}

#[test]
fn the_production_hook_never_interrupts_and_every_point_is_classified() {
    let hook = NoInterrupt;
    let mut rollback_points = 0;
    for point in InterruptPoint::ALL {
        assert!(
            hook.should_continue(*point),
            "production must never interrupt at {point}"
        );
        if point.is_rollback() {
            rollback_points += 1;
        }
    }
    assert_eq!(InterruptPoint::ALL.len(), 12);
    assert_eq!(rollback_points, 4, "four of the points belong to rollback");
    assert!(!InterruptPoint::BeforeStage.is_rollback());
    assert!(!InterruptPoint::AfterCommit.is_rollback());
    assert!(InterruptPoint::BeforeRollbackVerify.is_rollback());
    assert!(InterruptPoint::AfterRollbackCommit.is_rollback());
}

// ---- Bootstrap ----------------------------------------------------------

#[test]
fn bootstrapping_from_a_hostile_artifact_creates_no_installation() {
    for flaw in [
        FixtureFlaw::Unsigned,
        FixtureFlaw::WrongKey,
        FixtureFlaw::TruncatedPackage,
        FixtureFlaw::TamperedPackageByte,
        FixtureFlaw::TamperedManifestField,
        FixtureFlaw::WrongTarget,
    ] {
        let root = tempfile::tempdir().expect("tempdir");
        let hostile = load(flaw.fixture_name());
        let error = Installer::bootstrap(
            root.path(),
            verifier(),
            FIXTURE_TARGET,
            hostile.envelope.as_bytes(),
            &hostile.package,
        )
        .err()
        .unwrap_or_else(|| panic!("{} must not bootstrap", flaw.fixture_name()));
        assert!(
            matches!(error, UpdateError::Verify(_)),
            "{} gave {error:?}",
            flaw.fixture_name()
        );
        assert!(
            !journal_path(root.path()).exists(),
            "{}: a refused bootstrap must leave no journal",
            flaw.fixture_name()
        );
        assert!(
            !root.path().join("slots").exists(),
            "{}: a refused bootstrap must write no slot",
            flaw.fixture_name()
        );
    }
}

#[test]
fn bootstrapping_reports_the_specific_check_that_failed() {
    let root = tempfile::tempdir().expect("tempdir");
    let unsigned = load(FixtureFlaw::Unsigned.fixture_name());
    assert!(matches!(
        Installer::bootstrap(
            root.path(),
            verifier(),
            FIXTURE_TARGET,
            unsigned.envelope.as_bytes(),
            &unsigned.package,
        ),
        Err(UpdateError::Verify(VerifyError::UnsignedManifest))
    ));

    let foreign = load(FixtureFlaw::WrongTarget.fixture_name());
    assert!(matches!(
        Installer::bootstrap(
            root.path(),
            verifier(),
            FIXTURE_TARGET,
            foreign.envelope.as_bytes(),
            &foreign.package,
        ),
        Err(UpdateError::Verify(VerifyError::UnsupportedTarget { .. }))
    ));

    let truncated = load(FixtureFlaw::TruncatedPackage.fixture_name());
    assert!(matches!(
        Installer::bootstrap(
            root.path(),
            verifier(),
            FIXTURE_TARGET,
            truncated.envelope.as_bytes(),
            &truncated.package,
        ),
        Err(UpdateError::Verify(VerifyError::TruncatedPackage { .. }))
    ));
}

#[test]
fn bootstrapping_under_a_path_that_is_a_file_is_an_io_failure() {
    let parent = tempfile::tempdir().expect("tempdir");
    let blocker = parent.path().join("not-a-directory");
    fs::write(&blocker, b"in the way").expect("write blocker");
    let base = load(BASE_FIXTURE_NAME);

    let error = Installer::bootstrap(
        blocker.join("install"),
        verifier(),
        FIXTURE_TARGET,
        base.envelope.as_bytes(),
        &base.package,
    )
    .expect_err("the root cannot be created");
    assert!(
        matches!(&error, UpdateError::Io { context, .. } if *context == "create root"),
        "got {error:?}"
    );
    assert!(error.is_retryable(), "a filesystem failure may be retried");
}

#[test]
fn a_fresh_bootstrap_has_no_rollback_target() {
    let root = tempfile::tempdir().expect("tempdir");
    let installer = bootstrapped(root.path(), verifier());
    assert_eq!(installer.root(), root.path());
    assert_eq!(installer.installed_version(), V1);
    assert_eq!(installer.retained_version(), None);
    assert_eq!(installer.journal().active.slot, Slot::A);
    assert_eq!(installer.journal().floor, V1);
    assert!(installer.journal().previous.is_none());
    assert!(installer.journal().pending.is_none());
    assert_eq!(installer.journal(), &read_journal(root.path()));
}

// ---- Journal recovery ---------------------------------------------------

#[test]
fn an_unreadable_or_unparsable_journal_is_reported_as_corrupt() {
    // No journal at all.
    let empty = tempfile::tempdir().expect("tempdir");
    assert!(matches!(
        Installer::open(empty.path(), verifier(), FIXTURE_TARGET),
        Err(UpdateError::JournalCorrupt(_))
    ));

    // A journal that is not JSON, and one that is JSON of the wrong shape.
    for body in [
        b"".as_slice(),
        b"not json at all".as_slice(),
        b"[]".as_slice(),
        br#"{"schema_version":1}"#.as_slice(),
    ] {
        let root = tempfile::tempdir().expect("tempdir");
        bootstrapped(root.path(), verifier());
        fs::write(journal_path(root.path()), body).expect("write journal");
        let error = Installer::open(root.path(), verifier(), FIXTURE_TARGET)
            .expect_err("a broken journal must not open");
        assert!(
            matches!(error, UpdateError::JournalCorrupt(_)),
            "{} gave {error:?}",
            String::from_utf8_lossy(body)
        );
    }
}

#[test]
fn a_journal_from_another_schema_version_is_refused_by_number() {
    for version in [0, 2, 99] {
        let root = tempfile::tempdir().expect("tempdir");
        bootstrapped(root.path(), verifier());
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(journal_path(root.path())).expect("read"))
                .expect("parse");
        value["schema_version"] = serde_json::json!(version);
        fs::write(journal_path(root.path()), value.to_string()).expect("write");

        let error = Installer::open(root.path(), verifier(), FIXTURE_TARGET)
            .expect_err("a foreign schema version must not open");
        assert!(
            matches!(&error, UpdateError::JournalCorrupt(detail)
                if detail.contains(&format!("schema_version {version}"))),
            "got {error:?}"
        );
    }
}

#[test]
fn a_journal_carrying_an_unknown_member_is_refused() {
    let root = tempfile::tempdir().expect("tempdir");
    bootstrapped(root.path(), verifier());
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(journal_path(root.path())).expect("read")).expect("parse");
    value["rollout_ring"] = serde_json::json!("canary");
    fs::write(journal_path(root.path()), value.to_string()).expect("write");

    let error = Installer::open(root.path(), verifier(), FIXTURE_TARGET)
        .expect_err("an unknown journal member must not open");
    assert!(
        matches!(&error, UpdateError::JournalCorrupt(detail) if detail.contains("rollout_ring")),
        "got {error:?}"
    );
}

#[test]
fn a_journal_that_retains_the_active_slot_is_refused() {
    let root = tempfile::tempdir().expect("tempdir");
    let installer = updated(root.path());
    let mut journal = installer.journal().clone();
    assert_ne!(
        journal.active.slot,
        journal.previous.expect("retained").slot
    );
    journal.previous = Some(journal.active);
    fs::write(
        journal_path(root.path()),
        serde_json::to_vec(&journal).expect("encode"),
    )
    .expect("write");

    let error = Installer::open(root.path(), verifier(), FIXTURE_TARGET)
        .expect_err("one slot cannot be both active and retained");
    assert!(
        matches!(&error, UpdateError::JournalCorrupt(detail)
            if detail.contains("same slot")),
        "got {error:?}"
    );
}

#[test]
fn opening_discards_a_pending_attempt_and_its_staged_bytes() {
    let root = tempfile::tempdir().expect("tempdir");
    let installer = bootstrapped(root.path(), verifier());
    let mut journal = installer.journal().clone();
    journal.pending = Some(PendingRecord {
        slot: Slot::B,
        state: PendingState::Swapped,
    });
    fs::write(
        journal_path(root.path()),
        serde_json::to_vec(&journal).expect("encode"),
    )
    .expect("write");
    let staging = root.path().join("staging");
    fs::create_dir_all(&staging).expect("create staging");
    fs::write(staging.join("payload.bin"), b"untrusted").expect("stage bytes");

    let reopened = Installer::open(root.path(), verifier(), FIXTURE_TARGET).expect("reopen");
    assert!(
        reopened.journal().pending.is_none(),
        "recovery discards the attempt rather than resuming it"
    );
    assert!(!staging.exists(), "staged bytes are untrusted and removed");
    assert_eq!(reopened.installed_version(), V1);
    assert_eq!(reopened.launchable().expect("launchable").version, V1);
    // The discard is durable: a second open sees the cleared journal.
    assert!(read_journal(root.path()).pending.is_none());
}

#[test]
fn a_journal_round_trips_through_its_own_encoding() {
    let root = tempfile::tempdir().expect("tempdir");
    let installer = updated(root.path());
    let journal = installer.journal().clone();
    let encoded = serde_json::to_vec(&journal).expect("encodes");
    let decoded: Journal = serde_json::from_slice(&encoded).expect("decodes");
    assert_eq!(decoded, journal);
    assert_eq!(decoded.active.version, V2);
    assert_eq!(decoded.previous.expect("retained").version, V1);
    assert_eq!(decoded.pending, None);
}

// ---- Launchability ------------------------------------------------------

#[test]
fn a_slot_missing_either_of_its_two_files_is_incomplete() {
    for missing in ["manifest.json", "payload.bin"] {
        let root = tempfile::tempdir().expect("tempdir");
        let installer = bootstrapped(root.path(), verifier());
        fs::remove_file(slot_dir(root.path(), Slot::A).join(missing)).expect("remove");

        let error = installer
            .launchable()
            .expect_err("a slot missing one of its two files is not launchable");
        assert!(
            matches!(&error, UpdateError::SlotIncomplete { slot } if slot == "a"),
            "removing {missing} gave {error:?}"
        );
    }
}

#[test]
fn a_slot_whose_payload_was_rewritten_is_incomplete() {
    let root = tempfile::tempdir().expect("tempdir");
    let installer = bootstrapped(root.path(), verifier());
    let payload = slot_dir(root.path(), Slot::A).join("payload.bin");

    // Same length, different bytes: only the digest can catch this.
    let mut bytes = fs::read(&payload).expect("read payload");
    bytes[0] ^= 0xff;
    fs::write(&payload, &bytes).expect("rewrite payload");

    let error = installer
        .launchable()
        .expect_err("the digest must be checked");
    assert!(
        matches!(&error, UpdateError::SlotIncomplete { slot } if slot == "a"),
        "got {error:?}"
    );
    assert!(error.is_retryable());
}

#[test]
fn an_empty_payload_file_is_incomplete() {
    let root = tempfile::tempdir().expect("tempdir");
    let installer = bootstrapped(root.path(), verifier());
    fs::write(slot_dir(root.path(), Slot::A).join("payload.bin"), b"").expect("truncate");
    assert!(matches!(
        installer.launchable(),
        Err(UpdateError::SlotIncomplete { .. })
    ));
}

#[cfg(unix)]
#[test]
fn an_unreadable_payload_is_an_io_failure_not_a_digest_failure() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().expect("tempdir");
    let installer = bootstrapped(root.path(), verifier());
    let payload = slot_dir(root.path(), Slot::A).join("payload.bin");
    fs::set_permissions(&payload, fs::Permissions::from_mode(0o000)).expect("chmod");

    let outcome = installer.launchable();
    // A process running as root can read the file anyway; the assertion is
    // about the failure *classification*, so only check it when the denial
    // actually happened.
    if let Err(error) = outcome {
        assert!(
            matches!(&error, UpdateError::Io { context, .. } if *context == "read active payload"),
            "got {error:?}"
        );
    }
    fs::set_permissions(&payload, fs::Permissions::from_mode(0o644)).expect("restore");
    assert_eq!(installer.launchable().expect("readable again").version, V1);
}

// ---- Install-time filesystem failures -----------------------------------

#[test]
fn a_staging_path_that_is_not_a_directory_is_an_io_failure() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = bootstrapped(root.path(), verifier());
    // Something took the staging name. Removing it is not a `NotFound`, so
    // the error must surface rather than be swallowed.
    fs::write(root.path().join("staging"), b"not a directory").expect("write blocker");

    let update = load(FixtureFlaw::None.fixture_name());
    let error = installer
        .install(update.envelope.as_bytes(), &update.package)
        .expect_err("staging cannot be prepared");
    assert!(
        matches!(&error, UpdateError::Io { context, .. } if *context == "clear staging"),
        "got {error:?}"
    );
    assert_eq!(installer.installed_version(), V1);
    assert_eq!(installer.launchable().expect("launchable").version, V1);
}

#[cfg(unix)]
#[test]
fn an_unwritable_root_leaves_the_previous_installation_launchable() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = bootstrapped(root.path(), verifier());
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o555)).expect("chmod");

    let update = load(FixtureFlaw::None.fixture_name());
    let outcome = installer.install(update.envelope.as_bytes(), &update.package);
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).expect("restore");

    if let Err(error) = outcome {
        assert!(
            matches!(error, UpdateError::Io { .. }),
            "a read-only root must fail as a filesystem error, got {error:?}"
        );
    }
    // Whatever happened, v1 is still there and still launchable.
    let reopened = Installer::open(root.path(), verifier(), FIXTURE_TARGET).expect("reopen");
    assert!(reopened.launchable().is_ok());
    assert!(reopened.journal().pending.is_none());
}

#[test]
fn a_journal_temporary_file_that_cannot_be_written_stops_the_install() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = bootstrapped(root.path(), verifier());
    // Occupy the journal's temporary name with a directory: the durable
    // write cannot create its file, so the attempt must stop before the
    // rename that would flip the active slot.
    fs::create_dir(root.path().join("journal.json.tmp")).expect("occupy tmp name");

    let update = load(FixtureFlaw::None.fixture_name());
    let error = installer
        .install(update.envelope.as_bytes(), &update.package)
        .expect_err("the journal cannot be written");
    assert!(
        matches!(&error, UpdateError::Io { context, .. } if *context == "write journal"),
        "got {error:?}"
    );

    // The commit never happened, so the previous installation is intact.
    assert_eq!(read_journal(root.path()).active.version, V1);
    fs::remove_dir(root.path().join("journal.json.tmp")).expect("free tmp name");
    let reopened = Installer::open(root.path(), verifier(), FIXTURE_TARGET).expect("reopen");
    assert_eq!(reopened.installed_version(), V1);
    assert_eq!(reopened.launchable().expect("launchable").version, V1);
    assert!(reopened.journal().pending.is_none());
}

#[test]
fn a_journal_that_cannot_be_renamed_into_place_fails_the_bootstrap() {
    let root = tempfile::tempdir().expect("tempdir");
    // The journal's live name is a non-empty directory, so the atomic
    // rename — the one step that changes what gets launched — fails.
    let blocked = root.path().join("journal.json");
    fs::create_dir(&blocked).expect("occupy journal name");
    fs::write(blocked.join("occupant"), b"x").expect("make it non-empty");

    let base = load(BASE_FIXTURE_NAME);
    let error = Installer::bootstrap(
        root.path(),
        verifier(),
        FIXTURE_TARGET,
        base.envelope.as_bytes(),
        &base.package,
    )
    .expect_err("the journal cannot be committed");
    assert!(
        matches!(&error, UpdateError::Io { context, .. } if *context == "commit journal"),
        "got {error:?}"
    );
    assert!(
        matches!(
            Installer::open(root.path(), verifier(), FIXTURE_TARGET),
            Err(UpdateError::JournalCorrupt(_))
        ),
        "a failed commit leaves no usable installation"
    );
}

#[test]
fn an_unusable_target_slot_stops_the_swap_after_verification() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = bootstrapped(root.path(), verifier());
    // The inactive slot's directory name is taken by a plain file, so the
    // swap cannot clear it. Verification has already passed at that point,
    // which is exactly the window the A/B layout has to survive.
    fs::write(slot_dir(root.path(), Slot::B), b"not a directory").expect("occupy slot b");

    let update = load(FixtureFlaw::None.fixture_name());
    let error = installer
        .install(update.envelope.as_bytes(), &update.package)
        .expect_err("the target slot cannot be written");
    assert!(
        matches!(&error, UpdateError::Io { context, .. } if *context == "clear slot"),
        "got {error:?}"
    );

    assert_eq!(installer.installed_version(), V1);
    assert!(installer.journal().pending.is_none());
    assert!(!root.path().join("staging").exists());
    assert_eq!(installer.launchable().expect("launchable").version, V1);
    assert_eq!(read_journal(root.path()).active.version, V1);
}

// ---- Install-time rejections --------------------------------------------

#[test]
fn reinstalling_the_installed_version_is_not_forward_progress() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = bootstrapped(root.path(), verifier());
    let same = load(BASE_FIXTURE_NAME);

    let error = installer
        .install(same.envelope.as_bytes(), &same.package)
        .expect_err("installing 1.0.0 over 1.0.0 is not an update");
    assert!(
        matches!(
            error,
            UpdateError::Verify(VerifyError::DowngradeBlocked { offered, installed, .. })
                if offered == V1 && installed == V1
        ),
        "got {error:?}"
    );
    assert!(installer.journal().pending.is_none());
    assert!(!root.path().join("staging").exists());
    assert!(!slot_dir(root.path(), Slot::B).exists());
}

#[test]
fn an_older_release_cannot_be_installed_forward() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = updated(root.path());
    let older = build_artifact(FixtureFlaw::None, V1, V1);

    let error = installer
        .install(older.envelope.as_bytes(), &older.package)
        .expect_err("1.0.0 is not an update to 1.1.0");
    assert!(
        matches!(
            error,
            UpdateError::Verify(VerifyError::DowngradeBlocked { .. })
        ),
        "got {error:?}"
    );
    // The retained slot still holds the real v1 bytes, so rollback still works.
    assert_eq!(installer.installed_version(), V2);
    assert_eq!(installer.rollback().expect("rollback still works"), V1);
}

#[test]
fn a_rejected_install_clears_the_pending_record_it_wrote() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = bootstrapped(root.path(), verifier());
    let hostile = load(FixtureFlaw::TamperedPackageByte.fixture_name());

    assert!(installer
        .install(hostile.envelope.as_bytes(), &hostile.package)
        .is_err());
    // The in-memory journal and the on-disk one must agree that nothing is
    // pending: staging was written, journalled, then abandoned.
    assert!(installer.journal().pending.is_none());
    assert!(read_journal(root.path()).pending.is_none());
    assert!(!root.path().join("staging").exists());
}

// ---- Rollback failures --------------------------------------------------

#[test]
fn rollback_without_a_retained_version_is_refused_by_name() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = bootstrapped(root.path(), verifier());
    let error = installer.rollback().expect_err("nothing to roll back to");
    assert!(matches!(error, UpdateError::NoRetainedVersion));
    assert!(!error.is_retryable(), "there is still no retained version");
    assert_eq!(installer.installed_version(), V1);
}

#[test]
fn rollback_with_a_missing_retained_file_reports_which_one() {
    for (missing, context) in [
        ("manifest.json", "read retained manifest"),
        ("payload.bin", "read retained package"),
    ] {
        let root = tempfile::tempdir().expect("tempdir");
        let mut installer = updated(root.path());
        fs::remove_file(slot_dir(root.path(), Slot::A).join(missing)).expect("remove");

        let error = installer
            .rollback()
            .expect_err("the retained artifact is gone");
        assert!(
            matches!(&error, UpdateError::Io { context: got, source }
                if *got == context && source.kind() == std::io::ErrorKind::NotFound),
            "removing {missing} gave {error:?}"
        );
        assert_eq!(
            installer.installed_version(),
            V2,
            "a failed rollback must not move the active slot"
        );
        assert_eq!(installer.launchable().expect("launchable").version, V2);
    }
}

#[test]
fn rollback_re_runs_signature_verification_on_the_retained_manifest() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = updated(root.path());
    // Replace the retained manifest with an unsigned one. Its payload is
    // untouched, so only the signature check can refuse this.
    let unsigned = load(FixtureFlaw::Unsigned.fixture_name());
    fs::write(
        slot_dir(root.path(), Slot::A).join("manifest.json"),
        unsigned.envelope.as_bytes(),
    )
    .expect("write manifest");

    let error = installer.rollback().expect_err("rollback must re-verify");
    assert!(
        matches!(error, UpdateError::Verify(VerifyError::UnsignedManifest)),
        "got {error:?}"
    );
    assert_eq!(installer.installed_version(), V2, "the active slot held");
    assert_eq!(installer.launchable().expect("launchable").version, V2);
}

#[test]
fn rollback_refuses_a_retained_artifact_built_for_another_target() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = updated(root.path());
    let foreign = load(FixtureFlaw::WrongTarget.fixture_name());
    let dir = slot_dir(root.path(), Slot::A);
    fs::write(dir.join("manifest.json"), foreign.envelope.as_bytes()).expect("write manifest");
    fs::write(dir.join("payload.bin"), &foreign.package).expect("write payload");

    let error = installer.rollback().expect_err("foreign target");
    assert!(
        matches!(
            error,
            UpdateError::Verify(VerifyError::UnsupportedTarget { .. })
        ),
        "got {error:?}"
    );
    assert_eq!(installer.installed_version(), V2);
}

#[test]
fn rollback_recovers_an_installation_whose_active_slot_went_bad() {
    // The "failed launch probe" case: the new version is on disk and
    // committed, but its bytes no longer verify. Rollback is the documented
    // way out, and it must still re-run the full §16.5 path.
    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = updated(root.path());
    let active = slot_dir(root.path(), Slot::B).join("payload.bin");
    let mut bytes = fs::read(&active).expect("read active");
    bytes[0] ^= 0x5a;
    fs::write(&active, &bytes).expect("corrupt active");

    assert!(
        matches!(
            installer.launchable(),
            Err(UpdateError::SlotIncomplete { .. })
        ),
        "the launch probe must fail first"
    );
    assert_eq!(installer.rollback().expect("rollback"), V1);
    let state = installer.launchable().expect("v1 is launchable again");
    assert_eq!(state.version, V1);
    assert_eq!(state.slot, Slot::A);
    assert_eq!(
        fs::read(&state.payload).expect("payload"),
        package_bytes(V1)
    );
    // And the journal survived the restart.
    let reopened = Installer::open(root.path(), verifier(), FIXTURE_TARGET).expect("reopen");
    assert_eq!(reopened.installed_version(), V1);
    assert_eq!(reopened.retained_version(), Some(V2));
}

/// Install 1.1.0 with a `minimum_supported_version` of 1.1.0, which raises
/// the installation's floor above the retained 1.0.0.
fn updated_with_a_raised_floor(root: &Path) -> Installer {
    let mut installer = bootstrapped(root, verifier());
    let update = build_artifact(FixtureFlaw::None, V2, V2);
    installer
        .install(update.envelope.as_bytes(), &update.package)
        .expect("a release may raise the floor it commits to");
    assert_eq!(installer.journal().floor, V2, "the floor is monotonic");
    installer
}

#[test]
fn rollback_below_a_raised_floor_is_allowed_for_the_byte_identical_artifact() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = updated_with_a_raised_floor(root.path());
    // 1.0.0 now sits below the committed floor, and the waiver exists for
    // exactly this case: the artifact we ourselves verified on the way in.
    assert_eq!(
        installer.rollback().expect("the retained artifact returns"),
        V1
    );
    assert_eq!(installer.installed_version(), V1);
    assert_eq!(installer.launchable().expect("launchable").version, V1);
    assert_eq!(
        installer.journal().floor,
        V2,
        "rolling back never lowers the floor"
    );
}

#[test]
fn a_substituted_retained_artifact_of_the_same_version_does_not_inherit_the_waiver() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut installer = updated_with_a_raised_floor(root.path());

    // A validly signed 1.0.0 that is *not* the build we retained: same
    // version, different bytes. Identity is version **and** digest, so the
    // below-the-floor waiver must not apply to it.
    let package = b"plaudon-test-only substituted 1.0.0 build".to_vec();
    let manifest = manifest_for(V1, V1, FIXTURE_TARGET, &package, &test_signing_key());
    let envelope = signed_envelope(&manifest, &test_signing_key(), TEST_KEY_ID);
    let dir = slot_dir(root.path(), Slot::A);
    fs::write(dir.join("payload.bin"), &package).expect("write payload");
    fs::write(dir.join("manifest.json"), envelope.as_bytes()).expect("write manifest");

    let error = installer
        .rollback()
        .expect_err("a substituted artifact is subject to the floor");
    assert!(
        matches!(
            &error,
            UpdateError::Verify(VerifyError::DowngradeBlocked { offered, floor, .. })
                if *offered == V1 && *floor == V2
        ),
        "got {error:?}"
    );
    assert_eq!(installer.installed_version(), V2, "the active slot held");
    assert_eq!(installer.launchable().expect("launchable").version, V2);
}

#[test]
fn recovery_that_cannot_clear_the_staging_area_refuses_to_open() {
    let root = tempfile::tempdir().expect("tempdir");
    bootstrapped(root.path(), verifier());
    // A leftover file where the staging directory belongs: recovery cannot
    // discard the untrusted bytes, so opening fails loudly instead of
    // continuing with an unknown staging state.
    fs::write(root.path().join("staging"), b"not a directory").expect("write blocker");

    let error = Installer::open(root.path(), verifier(), FIXTURE_TARGET)
        .expect_err("recovery cannot complete");
    assert!(
        matches!(&error, UpdateError::Io { context, .. } if *context == "clear staging"),
        "got {error:?}"
    );

    // Clearing the obstruction restores a launchable installation.
    fs::remove_file(root.path().join("staging")).expect("remove blocker");
    let reopened = Installer::open(root.path(), verifier(), FIXTURE_TARGET).expect("reopen");
    assert_eq!(reopened.launchable().expect("launchable").version, V1);
}
