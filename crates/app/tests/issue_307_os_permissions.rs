//! Issue #307: the permission model behind the guided macOS Privacy &
//! Security setup, driven through the fake provider.
//!
//! Nothing here touches the real operating system: every status comes from
//! [`FakePermissionProvider`], and the assertions are about what the model
//! claims, what it asks for and what it never asks for.

use plaudon_app::dto::Platform;
use plaudon_app::os_permissions::{
    FakeCall, FakePermissionProvider, OsPermissionKind, OsPermissionProvider, OsPermissionStatus,
    PermissionSnapshot,
};

const MAC_KINDS: [OsPermissionKind; 4] = [
    OsPermissionKind::Bluetooth,
    OsPermissionKind::Notifications,
    OsPermissionKind::RemovableVolumes,
    OsPermissionKind::FilesAndFolders,
];

#[test]
fn issue_307_macos_lists_exactly_the_permissions_a_feature_needs() {
    assert_eq!(OsPermissionKind::for_platform(Platform::Macos), MAC_KINDS);
    let titles: Vec<&str> = MAC_KINDS.iter().map(|kind| kind.guide().title).collect();
    assert_eq!(
        titles,
        [
            "Bluetooth",
            "Notifications",
            "Removable Volumes",
            "Files and Folders"
        ]
    );
}

#[test]
fn issue_307_full_disk_access_and_speech_recognition_are_not_requested() {
    for kind in OsPermissionKind::ALL {
        let guide = kind.guide();
        for text in [guide.title, guide.needed_for, guide.ask_note]
            .into_iter()
            .chain(guide.steps.iter().copied())
        {
            assert!(
                !text.contains("Full Disk Access"),
                "{kind:?} sends the user to Full Disk Access: {text:?}"
            );
            assert!(
                !text.contains("Speech Recognition"),
                "{kind:?} asks for Speech Recognition: {text:?}"
            );
        }
    }
}

#[test]
fn issue_307_windows_and_linux_have_no_privacy_permissions() {
    assert_eq!(OsPermissionKind::for_platform(Platform::Windows), &[] as &[OsPermissionKind]);
    assert_eq!(OsPermissionKind::for_platform(Platform::Linux), &[] as &[OsPermissionKind]);
}

#[test]
fn issue_307_every_permission_says_what_needs_it_what_happens_without_it_and_how_to_grant_it() {
    for kind in MAC_KINDS {
        let guide = kind.guide();
        assert!(!guide.needed_for.is_empty(), "{kind:?}: what needs it?");
        assert!(
            !guide.without_it.is_empty(),
            "{kind:?}: what happens without it?"
        );
        assert!(
            !guide.ask_note.is_empty(),
            "{kind:?}: how does asking work?"
        );
        assert!(
            guide.optional,
            "{kind:?}: Plaudon works without every one of these permissions"
        );
        assert!(
            guide.steps.len() >= 3,
            "{kind:?}: the manual steps are too thin: {:?}",
            guide.steps
        );
        assert!(
            guide
                .steps
                .iter()
                .any(|step| step.contains("Open System Settings")),
            "{kind:?}: the steps never point at the button that opens the pane"
        );
    }
}

#[test]
fn issue_307_ask_macos_exists_only_where_macos_has_an_api() {
    assert!(OsPermissionKind::Bluetooth.guide().can_ask);
    assert!(OsPermissionKind::Notifications.guide().can_ask);
    assert!(!OsPermissionKind::RemovableVolumes.guide().can_ask);
    assert!(!OsPermissionKind::FilesAndFolders.guide().can_ask);
}

#[test]
fn issue_307_status_words_are_distinct_and_never_call_not_asked_a_refusal() {
    let statuses = [
        (OsPermissionStatus::Granted, "Granted"),
        (OsPermissionStatus::Denied, "Denied"),
        (OsPermissionStatus::NotDetermined, "Not asked yet"),
        (OsPermissionStatus::Restricted, "Restricted"),
        (OsPermissionStatus::NotApplicable, "Not applicable"),
        (OsPermissionStatus::NotReported, "Not reported by macOS"),
    ];
    let mut glyphs = Vec::new();
    for (status, word) in statuses {
        let (glyph, label) = status.label();
        assert_eq!(label, word, "{status:?}");
        assert!(!glyph.is_empty(), "{status:?} has no glyph");
        glyphs.push(glyph);
    }
    assert_ne!(
        OsPermissionStatus::NotDetermined.label().0,
        OsPermissionStatus::Denied.label().0,
        "never asked must not look like a refusal"
    );
}

#[test]
fn issue_307_a_snapshot_reads_every_macos_permission_without_prompting() {
    let fake = FakePermissionProvider::new();
    fake.set_status(OsPermissionKind::Bluetooth, OsPermissionStatus::Denied);
    fake.set_status(OsPermissionKind::Notifications, OsPermissionStatus::Granted);
    fake.set_status(
        OsPermissionKind::RemovableVolumes,
        OsPermissionStatus::NotReported,
    );
    fake.set_status(
        OsPermissionKind::FilesAndFolders,
        OsPermissionStatus::NotReported,
    );

    let snapshot = PermissionSnapshot::read(&fake, Platform::Macos);

    assert_eq!(
        snapshot.status(OsPermissionKind::Bluetooth),
        Some(OsPermissionStatus::Denied)
    );
    assert_eq!(
        snapshot.status(OsPermissionKind::Notifications),
        Some(OsPermissionStatus::Granted)
    );
    assert_eq!(
        snapshot.status(OsPermissionKind::RemovableVolumes),
        Some(OsPermissionStatus::NotReported)
    );
    assert_eq!(snapshot.iter().count(), 4);
    assert!(
        fake.calls()
            .iter()
            .all(|call| matches!(call, FakeCall::Status(_))),
        "reading a status must never prompt or open anything: {:?}",
        fake.calls()
    );
}

#[test]
fn issue_307_a_snapshot_off_macos_reads_nothing() {
    let fake = FakePermissionProvider::new();
    let snapshot = PermissionSnapshot::read(&fake, Platform::Windows);
    assert_eq!(snapshot.iter().count(), 0);
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
}

#[test]
fn issue_307_an_unset_fake_status_is_not_asked_yet() {
    let fake = FakePermissionProvider::new();
    assert_eq!(
        fake.status(OsPermissionKind::Bluetooth),
        OsPermissionStatus::NotDetermined
    );
}

#[test]
fn issue_307_answering_the_prompt_changes_what_is_read_afterwards() {
    let fake = FakePermissionProvider::new();
    fake.answer_prompt_with(OsPermissionKind::Bluetooth, OsPermissionStatus::Granted);

    assert_eq!(
        fake.request(OsPermissionKind::Bluetooth),
        OsPermissionStatus::Granted
    );
    assert_eq!(
        fake.status(OsPermissionKind::Bluetooth),
        OsPermissionStatus::Granted
    );
    assert_eq!(
        fake.calls(),
        [
            FakeCall::Request(OsPermissionKind::Bluetooth),
            FakeCall::Status(OsPermissionKind::Bluetooth)
        ]
    );
}

#[test]
fn issue_307_asking_for_a_permission_macos_cannot_ask_about_shows_nothing() {
    let fake = FakePermissionProvider::new();
    fake.set_status(
        OsPermissionKind::FilesAndFolders,
        OsPermissionStatus::NotReported,
    );
    fake.answer_prompt_with(
        OsPermissionKind::FilesAndFolders,
        OsPermissionStatus::Granted,
    );
    assert_eq!(
        fake.request(OsPermissionKind::FilesAndFolders),
        OsPermissionStatus::NotReported,
        "there is no prompt to answer, so nothing can change"
    );
}

#[test]
fn issue_307_a_failed_settings_open_comes_back_as_a_sentence() {
    let fake = FakePermissionProvider::new();
    fake.fail_open_settings("System Settings could not be opened.");
    let error = fake
        .open_settings(OsPermissionKind::Bluetooth)
        .expect_err("scripted to fail");
    assert_eq!(error.0, "System Settings could not be opened.");
}
