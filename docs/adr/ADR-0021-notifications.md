# ADR-0021: Native notification backends, payload privacy and non-authoritative delivery

- **Status:** accepted
- **Date:** 2026-10-11
- **Deciders:** EPIC E (desktop shell)
- **Spec:** §5.5, §14.5, §22(13)

## Context

§5.5 requires completion notices through macOS Notification Center and Windows
toasts "after OS permission/registration", explicitly **not** through SMTP, Bark
push or browser notification infrastructure, with an opt-in granular preference
and "privacy-safe generic text on lock screens". §14.5 requires notifications to
be displayed "without blocking jobs or relying on OS delivery as a source of
truth". §22(13) makes all of that a release gate: native notifications are
privacy safe and nonblocking, and all completion events remain visible in the
app regardless of OS permission.

Three decisions follow from that: which API each platform uses, what may appear
in a payload, and what a delivery result means.

## Decision

### Backend per platform

- **macOS** (`crates/notifications/src/macos.rs`): `UserNotifications.framework`
  via `objc2` + `objc2-user-notifications`. It is the only notification API
  Apple still supports and the only one exposing a real `authorizationStatus`,
  which §5.5 ("after OS permission") and §22(13) ("visible regardless of OS
  permission") both need. `mac-notification-sys`/`notify-rust` drive the
  deprecated `NSUserNotification` and cannot report permission truthfully, so
  they were rejected. Using the framework directly also gives us
  `targetContentIdentifier`, which is how a click opens the affected
  recording/job.
  `UNUserNotificationCenter` requires a bundled app, so the backend checks the
  main bundle identifier first and reports `Unsupported` for unbundled
  development binaries instead of raising an Objective-C exception. All
  Objective-C work runs inside one `spawn_blocking` closure, because ObjC
  objects and blocks are neither `Send` nor `Sync` and a completion handler must
  outlive the call.
- **Windows** (`crates/notifications/src/windows.rs`): `tauri-winrt-notification`
  over `Windows.UI.Notifications.ToastNotificationManager` — a real WinRT toast,
  maintained by the Tauri organisation whose runtime we already ship, depending
  only on the `windows` crate family. Windows has no runtime permission prompt:
  toasts require a registered **AppUserModelID** (installer shortcut carrying
  `System.AppUserModel.ID`, or `SetCurrentProcessExplicitAppUserModelID` for a
  portable build). The desktop crate supplies that AUMID; an empty one reports
  `Unsupported` rather than pretending to deliver.
- **Everything else**: `OsPermission::Unsupported` / `DeliveryOutcome::Unsupported`.

The crate declares no HTTP client, no mail/SMTP client and no push SDK, and
`deps_policy` fails the test suite if one ever appears in its manifest.

### Privacy rule

A notification payload may contain **counts, fixed category phrases and a typed
target id — nothing else**. This is enforced structurally rather than by review:
`privacy::build` is the only supported constructor for a `NotificationRequest`
and it takes a `NotificationContext` that has no `String`, `Path` or free-text
member anywhere. Every word reaching the OS is a `&'static str` in
`privacy.rs` or a formatted integer, so a recording title
(`/Users/rom/secret-merger.mp3`), a file path or a transcript excerpt cannot be
expressed as input, let alone displayed on a lock screen. A redaction test
checks every category's payload against an allowlist regex and against
adversarial leak strings.

### Delivery is not the state of record

`Dispatcher::notify` checks the master switch and per-category opt-in first
(`SuppressedByPreference`, and no OS call at all), then OS permission
(`SuppressedByOsPermission`, prompting once when the user has opted in and the
OS has not been asked yet), then delivers. It returns a `DeliveryOutcome` and
never an error; the backend call runs on its own task behind a timeout, so a
panicking or wedged notification stack surfaces as `Failed` instead of unwinding
or blocking the job completion path.

Callers persist the outcome next to the activity entry
(`notification_delivered`, `notification_suppressed_reason`) and never derive
application state from it. The committed job row and the in-app activity history
remain authoritative, which is what keeps every completion event visible when
notifications are disabled, denied or broken.

## Rejected alternatives

- **`mac-notification-sys` / `notify-rust` on macOS.** Rejected: they drive the deprecated `NSUserNotification` and cannot report OS permission truthfully, which §5.5 and §22(13) need.
- **Treating a delivered notification as authoritative job state.** Rejected: delivery is OS-controlled and lossy; the destination always refetches committed state.
- **Putting transcript or title text in notification payloads.** Rejected: payloads are visible on lock screens; only a target kind and id are carried.

## Consequences

- The desktop crate owns the Windows AUMID, the macOS activation delegate
  (reading `targetContentIdentifier` via
  `NotificationTarget::from_activation_token`) and the `set_notification_settings`
  command; this crate stays free of Tauri.
- The minimal default is "everything off": the master switch and all four
  categories are opt-in, so a fresh install posts nothing until onboarding or
  the settings pane records an explicit choice.
- Adding a fifth notice means adding a `NotificationContext` variant and a fixed
  phrase — a deliberate review point, not a new free-text call site.
- The Windows backend is type-checked for `x86_64-pc-windows-msvc` but has not
  been exercised on Windows hardware yet; §22(17) still requires a named
  real-hardware test before any Windows support claim.
