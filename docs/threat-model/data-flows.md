# Data flows

Source: SPEC v1.3 §4.1–§4.6, §7, §8, §10, §11, §13.4, §16.2A, §16.6 and
Appendix B.4/B.7. For each route the question answered here is the one §16.6
requires us to answer per ingestion route and per processing destination:
**which bytes leave this machine, and which never do.**

"Leaves the machine" means a byte that our process (or a subprocess we launch)
hands to a network stack. Radio transmission on a BLE link is called out
separately where it applies, because it is not internet egress but it is
still off-host.

Conventions used below:

* **Gate** — `plaudon_security::outbound::OutboundPolicy`. Nothing reaches a
  network transport or a network-capable subprocess without a ticket from it.
* **Local-only Mode** — the default. The gate refuses every network purpose and
  refuses to launch the PLAUD Cloud CLI at all.

---

## Ingestion routes

### I1. Manual file import

**Trigger:** the user picks files through the OS file dialog.

**Flow:** OS dialog → `plaudon_security::fsguard` canonicalizes the path and
confines it to permitted roots → copy-only staging into a private temp
directory → SHA-256 over the staged bytes → fsync → import-journal intent →
same-volume rename into the content-addressed media archive → SQLite commit.

**Leaves the machine:** nothing. No request of any kind is made, in Local-only
Mode or out of it.

**Never leaves the machine:** the audio bytes, the file name, the containing
directory path, the hash.

**Source file treatment:** read-only. The original is never moved, modified or
deleted (§6.3 invariant 11).

### I2. Watched folder

**Trigger:** a file appears in a folder the user authorized.

**Flow:** as I1, with a readiness probe (size/mtime stability, exclusive-open
check) before staging so a partially written file is never imported, and
reconciliation against existing recordings by content hash.

**Leaves the machine:** nothing *from this application*.

**Honest limit — the one case where bytes can leave without our involvement:**
if the watched folder is backed by a cloud-sync client (iCloud Drive, OneDrive,
Dropbox…), that client — not this app — may upload the file, and a
placeholder/dataless file may also need to be hydrated by it. Such a folder is
disabled or explicitly labelled while Local-only Mode is on. This is item 3 of
the not-guaranteed list in [`README.md`](README.md).

### I3. PLAUD Cloud CLI (official account route)

**Trigger:** the user connects a PLAUD account and runs an enumeration or
download. Requires network permission; **impossible in Local-only Mode**.

**Flow:** the gate authorizes a `Subprocess` destination pinned to the official
CLI program → the launcher builds a fixed list of typed `CliArg` values (no
shell, no user text spliced into a command line) and a scrubbed environment →
the CLI performs its own HTTPS session against PLAUD → its stdout is parsed
defensively → downloaded media is staged, hashed and committed exactly like I1.

**Leaves the machine:** whatever the official CLI sends to PLAUD — at minimum
the account credential it holds in its own storage, the enumeration query and
the download requests for the selected recordings. We do not add payloads of
our own on this route.

**Never leaves the machine:** our transcripts, our summaries, our annotations,
the OpenRouter key, any local file the user did not ask to sync.

**Specific handling:** signed download URLs appearing in CLI output stay in
process memory, are never persisted and never logged; log labels keep
`scheme://host:port` only (TM-010).

**Local-only Mode:** the subprocess is not launched at all — not launched and
then blocked. That distinction is the §16.2A requirement, because the CLI has
its own network stack we could not otherwise constrain.

### I4. Direct PLAUD device over BLE

**Trigger:** the user authorizes a specific nearby device.

**Flow:** scan → profile/capability probe → authenticate with the device-scoped
credential read from the OS keychain → delta file listing → read-only chunked
transfer of the compressed `.ASR` sidecar → bounded buffers streamed to a
staging file → hash and completeness verification → commit with provenance
recording the real transport and codec (never labelled "original stereo WAV").

**Leaves the machine over the internet:** nothing. This route makes no HTTP
request and launches no CLI; it works with Local-only Mode on.

**Leaves the machine over the radio link:** the authorization exchange and the
transferred audio bytes travel over BLE. On the examined legacy profile that
link has no application- or link-layer encryption and the credential is a
static per-device token, so an in-range listener may observe or replay it
(App. B.5). Local-only Mode does not change this; see not-guaranteed item 4.

**Never sent to the device:** delete, clear, settings-write, OTA or
cloud-sync-initiation opcodes. The import path's opcode allowlist does not
contain them (§6.3 invariant 14, App. B.6), and an acknowledgement that would
let the device drop its copy is never sent before verification — and not at all
during routine read-only import.

**Completeness honesty:** the device's delta list is not a history query. The
phone app may already have removed files; the result is labelled
`on_device_only`/incomplete rather than treated as the full recording history
(TM-008).

### I5. Legacy USB recording volume

**Trigger:** the user authorizes a mounted volume (only the original Note's
legacy configuration exposes one; Note Pro and NotePin S do not).

**Flow:** OS mount discovery / user folder authorization → structure validation
that does not trust the volume label → read-only enumeration →
`plaudon_security::fsguard` path confinement → staging, hashing and commit as
in I1.

**Leaves the machine:** nothing.

**Never done:** opening raw disk devices, installing drivers, writing to the
volume, or deleting source recordings after transfer.

---

## Processing destinations

### P1. Local STT (whisper.cpp family)

**Flow:** archive bytes → sandboxed decoder helper → chunked PCM → local
inference in the same sandboxed helper process → transcript revision written to
SQLite.

**Leaves the machine:** nothing. The helper is launched without an
`EgressTicket` and without credentials, so it has no path to our network
transport.

**Caveat about model acquisition:** the *model artifact* may have been
downloaded earlier over the network. That download is a separate, explicitly
approved egress purpose which Local-only Mode refuses; once the artifact is on
disk, transcription itself is offline. Using an already-installed model never
triggers a request.

### P2. Apple native (SpeechAnalyzer / Foundation Models)

**Flow:** archive bytes → `native/apple-bridge` over a narrow C ABI → Apple
on-device APIs → transcript or summary back across the same ABI.

**Leaves the machine from our process:** nothing. We open no socket on this
route and the bridge is not a network client.

**Honest limit:** the behaviour of Apple's frameworks is Apple's, not ours. We
call the on-device APIs and we gate availability on a runtime probe; we do not
claim to have proven what the OS does internally. This sits under
not-guaranteed item 1 (OS-level activity is outside our egress control).

### P3. OpenRouter (cloud AI, BYOK)

**Trigger:** an explicit user action with network permission and — once §16.3
consent lands — a scoped grant covering this purpose, content type and
provider. **Impossible in Local-only Mode.**

**Flow:** the dispatcher builds a typed request → the gate authorizes
(`Subsystem::OpenRouter`, purpose, payload class, allowlisted https host) →
the HTTP transport attaches the key from the keychain as a header → response
is validated against the typed contract and JSON Schema → output stored as a
derived revision with its model/policy snapshot.

**Leaves the machine (exactly):**

* for transcription: the selected audio bytes (or the selected chunks of them);
* for summarization, extraction and ask-about-recording: the transcript text or
  the selected excerpt, plus the prompt template and the output schema;
* the OpenRouter API key, as an `Authorization` header;
* the model identifier and routing/provider preferences.

**Never leaves the machine on this route:** the OS keychain contents beyond
that one key, device credentials, other recordings, file-system paths, the
local archive layout, and telemetry (telemetry is a separate purpose with its
own opt-in).

**Never in a URL:** the key, and any credential. Destination labels used in
logs and error envelopes are `scheme://host:port` with no path or query.

**Retention honesty:** what the provider does with the payload is governed by
the provider's policy. A zero-data-retention setting is a provider claim about
their side; it does not make the workflow local and is not presented as
satisfying any compliance or residency requirement (§16.6, §23).

---

## Cross-cutting: traffic we initiate that carries no recording content

| Purpose | Payload | Local-only Mode | Extra consent |
|---|---|---|---|
| Model catalog / pricing lookup | model ids, no user content | refused | network permission |
| Model artifact download | artifact request, no user content | refused | explicit model-download approval, on top of network permission |
| Update check | app version, platform | refused | network permission |
| Telemetry | aggregate counters, no content | refused | explicit opt-in, on top of network permission |

Each of these is a distinct `EgressPurpose` at the gate, so "the user allowed
networking for OpenRouter" never silently authorizes a model download or a
telemetry upload (TM-024, TM-026).
