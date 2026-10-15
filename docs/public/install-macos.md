# Install and open Plaudon on macOS (unsigned build)

Plaudon's macOS artifact is **ad-hoc signed and not notarized**. macOS will refuse
to open it on the first attempt, and the bypass that used to work (right-click →
**Open**) has been removed from current macOS. This page is the whole procedure:
verify the download, install it, and then allow it once in **System Settings →
Privacy & Security**.

Read [Why there is no signature](#why-there-is-no-signature) before you do any of
it. If you do not want to run software Apple has not checked, do not install this
build — build it yourself from source instead
([Build and run](../../README.md#build-and-run)).

---

## 1. Download

From the [latest release](https://github.com/spdrman/plaudon/releases/latest)
download:

- `Plaudon-<version>-macos-arm64.zip` — the application (Apple Silicon only), and
- `checksums.txt` — the SHA-256 of every published artifact.

## 2. Verify the download

```sh
cd ~/Downloads
shasum -a 256 -c checksums.txt --ignore-missing
```

Expected output, with your version in place of `<version>`:

```
./Plaudon-<version>-macos-arm64.zip: OK
```

`checksums.txt` is produced by the release workflow with `sha256sum ./*.zip
./*.msi`, so its entries are prefixed with `./`; run the check from the directory
that holds the downloaded files. Without `--ignore-missing`, `shasum` reports the
Windows `.msi` you did not download as a failure.

If the line does not read `OK`, stop. Delete the file and download it again; a
mismatch means you do not have the bytes that were published.

A checksum proves the download was not altered in transit. It does **not** prove
the build is safe — the checksum comes from the same place as the file.

## 3. Install

```sh
unzip Plaudon-<version>-macos-arm64.zip
mv Plaudon.app /Applications/
```

Unzipping with Finder works just as well.

## 4. First launch is blocked

Open `/Applications/Plaudon.app`. macOS shows:

![The macOS Gatekeeper dialog that appears on the first launch: a padlock icon
with a yellow warning badge, the title “Plaudon.app” Not Opened, the text “Apple
could not verify “Plaudon.app” is free of malware that may harm your Mac or
compromise your privacy.”, a blue Move to Trash button and a Done
button.](../assets/install/macos-gatekeeper-blocked.png)

Click **Done**. Do **not** click *Move to Trash* — that deletes the application.

There is no *Open* item hiding in the right-click menu on current macOS. This
first, blocked attempt is required: it is what puts Plaudon into the Security
section of System Settings in the next step.

## 5. Allow it once in System Settings

Open **System Settings → Privacy & Security** and scroll down to **Security**.
Within about an hour of the blocked launch there is a row for Plaudon:

![The Security section of System Settings → Privacy & Security, showing “Allow
applications from: App Store & Known Developers”, a row reading “Plaudon.app”
was blocked to protect your Mac. with an Open Anyway button, and the explanation
“Apple could not verify “Plaudon.app” is free of malware that may harm your Mac
or compromise your privacy.”](../assets/install/macos-privacy-security-open-anyway.png)

1. Click **Open Anyway**.
2. Authenticate with your login password or Touch ID when macOS asks.
3. Confirm once more if macOS shows a final "are you sure" dialog; its button is
   **Open Anyway** too.

Plaudon starts and stays allowed; you will not be asked again for this copy of
the application. Replacing it with a newer release means doing this once more,
because the allowance is tied to the exact bundle you approved.

Leave **Allow applications from** on **App Store & Known Developers**. The
per-application exception above is all that is needed; lowering the global
setting weakens Gatekeeper for everything on the machine.

## 6. Terminal alternative

The quarantine flag Safari (or any browser) attaches to the download is what
triggers the block. Removing it has the same effect as *Open Anyway*:

```sh
xattr -dr com.apple.quarantine /Applications/Plaudon.app
open /Applications/Plaudon.app
```

Run it only on a copy you have checksum-verified and decided to trust. Pointing
that command at a directory you did not vet strips macOS's provenance marker from
everything inside it.

To see the flag before and after:

```sh
xattr -l /Applications/Plaudon.app     # com.apple.quarantine: 0081;...;Safari;
```

## 7. Privacy & Security permissions

First-run setup has a **macOS Privacy & Security** step, and the same panel lives
under **Settings → Privacy & Security permissions** so you can come back to it.
Every permission is optional, and **Skip — do later** never blocks setup.

| Permission | What needs it | Without it | Can Plaudon ask? |
| --- | --- | --- | --- |
| Bluetooth | Direct import from a PLAUD device | The device can't be found over Bluetooth | Yes, **Ask macOS** shows the system prompt |
| Notifications | Banners when an import or transcription finishes | No banners; everything still shows in the app | Yes, **Ask macOS** shows the system prompt |
| Removable Volumes | USB import from a PLAUD Note that mounts as a disk | USB import can't read the device | No, macOS asks the first time Plaudon reads it |
| Files and Folders | A watched folder inside Desktop, Documents, Downloads or a network drive | That folder can't be read | No, macOS asks the first time Plaudon reads it |

Each row has a button that opens the exact System Settings pane, numbered steps
for doing it by hand, and a live status (granted, denied, not asked yet,
restricted, or "not reported by macOS" for the two that macOS won't let apps
read). The status refreshes when you press **Check again** and whenever you come
back to the Plaudon window.

Plaudon never asks for **Full Disk Access**: nothing it does reads a location only
Full Disk Access unlocks. It doesn't ask for **Speech Recognition** either, because
Apple's on-device transcription (`SpeechAnalyzer`) needs no grant to analyse a file.

**Grants and the ad-hoc signature.** macOS ties each permission to the code
signature of the build that received it. This build is ad-hoc signed (see below),
so every release has a different signature and an update can look like a new
app. After updating, a permission may show as not asked yet, or stop working
while System Settings still shows it switched on. Switch Plaudon off and on again
in that list (or remove it with the minus button and add it back), then press
**Check again**.

## Why there is no signature

Distributing a macOS application that opens without this dance requires an Apple
Developer Program membership (a paid Developer ID certificate) and a notarization
submission to Apple. **This project holds neither**, so the release workflow
ad-hoc signs the bundle (`codesign --sign -`), which gives it a code signature
that satisfies Apple Silicon's "must be signed to run at all" requirement but
carries no identity Apple can vouch for. `spctl --assess` on the published bundle
returns `rejected`, and the release page states `signature: adhoc_verified,
notarization: not_verified` for exactly this reason.

So: nobody has checked this build except the people who wrote it, and the
instructions above are instructions for overriding a security control. Do it only
if you are prepared to extend that trust — after verifying the checksum, and
preferably after reading the source. Building from source avoids the question
entirely.

Plaudon is provided **as is, with no warranty of any kind**, and you use it at
your own risk (see the [MIT licence](../../LICENSE)). That applies with full
force here: overriding Gatekeeper is your decision, and its consequences are
yours.

## If something else goes wrong

| Symptom | Cause | Fix |
| --- | --- | --- |
| "Plaudon.app is damaged and can't be opened" | The download was truncated or modified, or the `.app` was re-zipped by a tool that dropped the signature | Re-download, verify the checksum, unzip again |
| The Security row never appears | More than an hour passed since the blocked launch, or the application was never launched | Launch Plaudon again, then reopen Privacy & Security |
| "You do not have permission to open the application" | The `.app` is still inside the quarantined `.zip` or in a sandboxed location such as `~/Downloads` under some MDM policies | Move `Plaudon.app` to `/Applications` first |
| It opens but the window is blank | Unrelated to Gatekeeper | See [Troubleshooting](./troubleshooting.md) |

Intel Macs are not covered: the release ships an Apple Silicon (`arm64`) build
only.

---

**How these screenshots were made.** Both are real captures, not mock-ups. The
Gatekeeper dialog is the published `Plaudon-0.1.0-macos-arm64.zip` (SHA-256
checked against `checksums.txt`), unzipped into a scratch directory, marked with
`xattr -w com.apple.quarantine "0081;…;Safari;"` and launched. The Privacy &
Security pane is the row that same kind of blocked launch produces, captured from
an identically unsigned local build of the application. Both were taken on a Mac
reporting `ProductVersion 27.0.1`, `BuildVersion 26A434`; wording and layout
differ slightly between macOS versions, and macOS 14 and earlier still offer the
right-click → **Open** route that current macOS does not.
