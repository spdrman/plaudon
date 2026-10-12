# Privacy disclosure

Plaudon is an independent, local-first desktop application for importing,
transcribing and reviewing your own recordings. It is not produced by, affiliated
with or approved by PLAUD, OpenRouter or Apple.

This page states exactly what leaves your machine, when, where it goes and under
what permission.

## What the application does with your data by default

On a first launch, Local-only Mode is on. In that state the application performs
no app-initiated network request at all: no update check, no model download, no
telemetry, no account login, no cloud inference. Imports, transcription with a
local engine, transcript editing and exports all run on your machine.

The scope of that statement is deliberately narrow. It covers traffic this
application initiates. It says nothing about your operating system, your
browser, an OS update agent, a cloud-sync client acting on a folder you chose,
or any other process on the machine. A folder backed by iCloud Drive, OneDrive
or a similar client is identified as potentially network-dependent, and such
folders are not watched while Local-only Mode is on.

## What leaves the machine, and when

| Destination | What is sent | When | Permission |
|---|---|---|---|
| OpenRouter (`openrouter.ai`) | The audio bytes or transcript text of the specific recording you chose, plus the model id | Only when you ask for a cloud transcription or cloud summary | A scoped consent grant you give per purpose and content type; an API key alone is never read as consent |
| PLAUD Cloud, via PLAUD's own command-line tool | Your PLAUD account session, handled entirely by that tool | Only when you connect the optional PLAUD account source, and never while Local-only Mode is on | Explicit connection of the source |
| Update endpoint (`updates.plaudon.app`) | The current version, to fetch a signed release manifest | Only when Local-only Mode is off | Leaving Local-only Mode |
| Model distribution | A request for the offline model artifact you selected | Only when you choose to download a local model and Local-only Mode is off | Your explicit download |

Nothing else is sent anywhere. There is no analytics endpoint, no crash-report
upload, no account system of ours and no server component. The application runs
no listening socket and no background service.

## Consent is scoped, not a global switch

A consent grant covers a purpose (for example "transcribe this recording"), a
content type (audio bytes, or transcript text), a provider or permitted model
set, retention and routing constraints, and the version of this disclosure you
agreed to. Revoking or expiring a grant fails the next dispatch check. When the
destination, purpose or privacy posture changes, you are asked again.

## About "zero data retention"

Some OpenRouter routes advertise zero data retention. That is a property of the
provider and the specific route, not of this application, and it must be
validated per endpoint. Choosing such a route does not make a workflow local,
and it is not a statement about any jurisdiction's data residency rules. The
application shows cloud processing as visibly different from local processing so
you always know which one you used.

## About "free local inference"

Running a model on your own machine costs no per-call fee. It still costs your
CPU, GPU, memory, disk and battery. "Free" here means "no metered charge", and
nothing more.

## Bluetooth and the direct-device path

Where a direct connection to a compatible PLAUD recorder is available, the legacy
protocol may use a static per-device credential over a link that is not
encrypted at the radio layer. Disabling every internet connection does not make
that link private: a nearby radio could observe or replay it. Identity is
labelled `profile-authenticated`, which means the device profile matched — not
that its identity was proved cryptographically.

Audio copied over Bluetooth is a compressed `.ASR` derivative. It is not the
original recording the device stored, and the application never claims the two
are of equal fidelity.

## Credentials

Your OpenRouter key and any device authorization material are held in the macOS
Keychain or the Windows Credential Manager. They are never written to the
database, a `.env` file, a log, a crash report, a command line or a subprocess
environment. If the platform secret store is unavailable, the affected feature
reports a locked state rather than silently falling back to plaintext.

Transient buffers are zeroized where that is feasible. In-process memory cannot
be fully protected from software that is already running with privilege on your
machine, and no application can honestly claim otherwise.

## Local data

Your library — original media, transcripts, revisions, outputs and metadata —
is stored unencrypted in the application's data directory with restrictive
permissions. Use your operating system's full-disk encryption if the contents
are sensitive. A portable backup you export is also unencrypted unless you
choose an encrypted archive, and the application warns you before writing a
plaintext backup.

## Deletion

Deleting a recording removes the managed media and derived outputs it owns and
leaves no dangling records behind. It cannot reach data already sent to a cloud
provider under a consent you gave: a provider's retention is governed by that
provider's policy, which is why the consent prompt states retention before
anything is sent.

## Related reading

- [Vendor dependency matrix](./vendor-matrix.md)
- [Compatibility matrix](./compatibility.md)
- [Troubleshooting](./troubleshooting.md)
- [Backup and restore](./backup-and-restore.md)
