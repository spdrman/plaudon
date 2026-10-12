# Release notes

## 0.1.0 — first release candidate

Plaudon is an independent, local-first desktop application for importing,
transcribing and reviewing your own recordings. It is not produced by,
affiliated with or approved by PLAUD, OpenRouter or Apple.

### What this release does

- **Import** audio from your own folders by drag-and-drop or file picker, on
  macOS and Windows. Imports copy; your source file is never moved, altered or
  deleted. Identical bytes are recognised as duplicates by content, not by name.
- **Transcribe locally** with whisper.cpp and an offline model artifact you
  choose and download. Timestamps are either produced by the engine or derived
  from the audio, and each segment says which; where no alignment exists, the
  interface offers no seek affordance rather than inventing a time.
- **Review** a transcript with search, passage copy and immutable revisions:
  correcting a transcript creates a new revision and the previous one stays
  readable. Editing against a stale revision is refused rather than silently
  overwriting someone else's change.
- **Export** a transcript as text, Markdown, JSON or captions.
- **See the work** in a job list with persisted states and a corrective action
  for each failure code, plus native notifications and an in-app activity
  history that records why a notification was not delivered.
- **Stay local by default.** Local-only Mode is on at first launch; in that mode
  the application makes no app-initiated network request, including no update
  check. Cloud AI through your own OpenRouter key is opt-in, scoped per purpose
  and content type, and shows the model's price before dispatch.
- **Keyboard and screen-reader access** across the critical flows.

### What this release does not do

- **No direct PLAUD device support is published.** No hardware, firmware and
  host-OS combination has an archived evidence record, so every device route is
  published as `Unsupported` and is not offered. See the
  [compatibility matrix](./compatibility.md).
- **The optional PLAUD account connector is `Unsupported`.** No release of the
  official command-line tool has been validated against our output contract, so
  it fails closed. Manual import does not depend on it.
- **No quality or speed numbers are claimed.** Measurement protocols and the
  evaluation corpus are in place; thresholds are frozen in the release evidence
  bundle once baselines exist, and nothing is asserted before then.
- No recording capture, no account system of ours, no server component, no
  telemetry.

### Release artifacts and verification

Each release publishes a signed macOS bundle, a signed Windows installer and a
release evidence bundle containing build and toolchain hashes, the tested OS
builds, test results, the software bill of materials, the licence audit, the
database migration and restore results, egress traces from the Local-only tests,
and the compatibility records backing every published claim. The evidence
manifest is signed and every entry carries a SHA-256, so a published claim can
be checked.

Updates and rollbacks are both cryptographically verified against a pinned key;
a rollback never relaxes that check.

### Known limitations

- Audio copied from a device over Bluetooth, where that path becomes available,
  is a compressed derivative, not the original device recording.
- A legacy Bluetooth session may be unauthenticated at the radio layer even with
  all internet access disabled.
- Your library and any plaintext backup are stored unencrypted; use full-disk
  encryption if that matters.
- Cloud providers' retention is governed by their policies, not by this
  application; the consent prompt states retention before anything is sent.

### Reading

- [Privacy disclosure](./privacy-disclosure.md)
- [Vendor dependency matrix](./vendor-matrix.md)
- [Troubleshooting](./troubleshooting.md)
- [Backup and restore](./backup-and-restore.md)
