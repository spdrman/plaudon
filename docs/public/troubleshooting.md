# Troubleshooting

Each entry names what you will see, why it happens and what to do. If a step
cannot be completed the application tells you which one failed; it never
reports success for work that did not happen.

## First run

**macOS refuses to open the application: "Plaudon.app" Not Opened.** The
published build is ad-hoc signed and not notarized, so Gatekeeper blocks the
first launch. Click **Done** (never *Move to Trash*) and follow
[Install and open Plaudon on macOS](./install-macos.md#5-allow-it-once-in-system-settings).

**The application opens with an empty library.** That is the expected first
state. Drag an audio file onto the window, or use Import, to add a recording.
Local-only Mode is on by default, so nothing is sent anywhere.

**Transcription is unavailable and offers to download a model.** Local
transcription needs an offline model artifact, which is not bundled. Choose a
model; the licence and the download size are shown first. Downloading requires
leaving Local-only Mode, and that is stated on the prompt.

**A model download was interrupted.** Partial downloads are discarded, not
resumed into a corrupt file. Start it again; a completed artifact is verified
against its published checksum before it is used.

## Import

**"Unrecognised container" or "format mismatch".** The file content does not
match its extension, or the format is not one the decoder supports. The message
names what the file actually is. Convert it, or import the original.

**A file imported once is reported as a duplicate.** Deduplication works on the
actual bytes, so the same audio under a different name is recognised. You are
asked what to do rather than having a second copy created silently.

**The source file is unchanged after import.** That is deliberate: an import
copies, and never moves, renames, modifies or deletes your file.

## PLAUD account source (optional)

**"The PLAUD command-line tool was not found."** The official tool is installed
by you, not by this application. Install it, then reconnect the source. Manual
import keeps working either way; nothing in the product requires that tool.

**"This version of the tool is not supported."** The application only drives
versions whose output contract has been validated. An unvalidated version fails
closed rather than guessing at its output. Use a validated version, or import
your recordings manually.

**Login fails with a Google or Apple PLAUD account.** Whether the official tool
supports those sign-in methods is outside our control. There is no token-paste
workaround and none will be offered. Use manual import.

**Your PLAUD session expired mid-import.** Queued work pauses and asks you to
re-authenticate. Nothing is dropped and nothing is marked complete that did not
complete.

## Direct device access (optional)

**The device is not listed.** Only the hardware, firmware and host-OS
combinations in the [compatibility matrix](./compatibility.md) are offered. An
unlisted combination is not shown as connectable, because we have no evidence it
works.

**macOS asks for Bluetooth permission, or you denied it.** Grant it in System
Settings under Privacy & Security → Bluetooth, or from **Settings → Privacy &
Security permissions** in Plaudon, which opens that pane for you (see
[Install on macOS](./install-macos.md#7-privacy--security-permissions)). Until
then, discovery is disabled with that reason shown; the application never
attempts access without permission.

**Only some recordings appear.** A device that has already synchronised with the
PLAUD phone app may offer only the recordings it has not yet transferred. The
application reports the listing as incomplete rather than presenting it as your
full history.

**The firmware uses the newer encrypted mode.** That mode is refused rather than
downgraded to the legacy protocol. The status explains this; there is no setting
that weakens it.

## Cloud AI (optional)

**"Local-only Mode blocked this."** Cloud work requires leaving Local-only Mode
and granting scoped consent. Both are explicit, both are reversible, and
revoking consent stops further dispatch immediately.

**A request timed out after it was sent.** The outcome is unknown and may have
been billed, so nothing is retried automatically. You decide whether to try
again.

**"This model is incompatible with this request style."** The model does not
accept the request family that recording needs. It is marked incompatible and
nothing is uploaded on a guess.

## Notifications

**No notification appeared.** If the OS permission is denied or notifications
are off, the event still lands in the in-app activity list, and the activity
entry records why nothing was delivered. Notification text never includes a file
path or key material.

## Backup and restore

**Where is my data?** In the application data directory for your platform; the
exact path is shown in Settings. It is stored unencrypted, so use full-disk
encryption if that matters to you.

**Opening a different library for one run.** Set `PLAUDON_DATA_DIR` to an
absolute path before launching. Everything that lives in the library — the
database, the media archive, the logs, the waveform cache and the window
state — follows the override, so the installed library is not touched at all.
The path must be absolute; a relative or empty value is refused with an error
rather than guessed at. This is what a support session or a release smoke test
uses.

**Restoring an archive refused to proceed.** An archive is validated before
anything is written: path traversal, duplicate entries, decompression bombs,
checksum mismatches and unknown schema versions are all rejected, and your
existing library is left untouched. See [backup and restore](./backup-and-restore.md).

**Moving to a new machine.** Export a portable backup, copy it across, and
restore it into a clean install. Media hashes and structured records are
compared against the archive manifest, and any mismatch is reported rather than
quietly accepted.

## Updates

**"Update checks are off in Local-only Mode."** That is the mode working as
described: no update request is made at all. Turn Local-only Mode off to allow
checks.

**An update failed to apply.** The previous version stays installed and
launchable. Updates and rollbacks are both signature-verified with the same
strictness; an unsigned, re-signed or altered package is refused.
