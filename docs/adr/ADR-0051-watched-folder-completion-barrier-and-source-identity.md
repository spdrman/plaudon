# ADR-0051: A filesystem event authorises nothing; a completion barrier and a content digest do

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** SPEC v1.3 §4.3, §7.5, §7.10, §13.3A, §17.4, App. A.2, App. A.4

## Status

Accepted — 2026-10-11. Implemented in `crates/source-folder`
(`barrier.rs`, `placeholder.rs`, `queue.rs`, `inventory.rs`, `service.rs`),
`crates/app-core/src/reconciliation.rs` and migration
`crates/persistence/migrations/0400_source_reconciliation.sql`, with fixtures
in `tests/integration/tests/k2_watched_folder.rs` and
`tests/integration/tests/k2_reconciliation.rs`.

## Context

Two separate subsystems — the watched folder and the cloud source connector —
face the same question from opposite directions: *on what evidence may this
item be imported, and what makes it the same item as one we already hold?*

App. A.4 answers the first for folders and is blunt about it: a creation event
is not a complete transfer. A cloud sync client can leave a `.icloud` brick
that stats like a file and contains nothing. A copy in progress has a valid
RIFF header after the first four kilobytes. An editor's atomic save produces
create, rename and remove events for bytes that were never in flux. A kernel
event buffer overflows silently.

§7.5 answers the second for connectors, and is equally specific: a vendor's
`remote_version` is a *hint*, and a new media revision is created only if the
bytes differ. §7.10 adds that a filename is never sufficient identity, that a
remote deletion grants no local deletion authority, and that a local tombstone
must survive and block the next sync tick.

The tempting design — treat the event as the trigger, the path as the key and
the version string as the change detector — fails all three cases in ways the
user cannot see: a half-imported recording, a duplicated meeting after a
rename, and a resurrected recording after a deletion.

## Decision

**1. Events are hints; a completion barrier authorises imports.**
`notify` and the periodic inventory sweep both do exactly one thing: push a
canonical path into a coalescing queue. An import happens only after the
barrier passes, in this order: size and mtime unchanged across the quiescence
window for N consecutive probes; a cloud-placeholder probe says the bytes are
local; the file opens and its first *and last* block are read; a media sniff
recognises the container; the volume has room; a full SHA-256 is taken and a
second `stat` confirms nothing moved while we hashed. Anything else defers and
is retried. The placeholder probe runs *before* the read, because reading a
dataless file is what triggers an unwanted hydration download.

**2. Identity is the filesystem's, then the content's — never the path.**
A watched-folder item's external identifier is `file:<dev>:<ino>` (Unix) or
`file:<volume>:<index>` (Windows), falling back to `sha256:<digest>` where the
platform has neither. A rename therefore does not create a second recording,
and replacement-by-atomic-rename at the same path correctly does. Path, mtime
and size are recorded in `watched_folder_items` as provenance only.

**3. Three columns, three different facts.** `source_items` now carries
`remote_version` (vendor hint), `observed_content_sha256` (what we hashed) and
`acquisition_complete` (whether those bytes landed). A changed version with an
unchanged digest reconciles metadata and appends nothing; an unchanged version
with `acquisition_complete = 0` re-downloads. Conflating any two of these
produces either a stalled sync or a redundant one.

**4. Nothing destructive is ever a remedy.** A remote trash marks
`remote_state` and stops. A vanished source file sets `unavailable_since` and
stops. A failed import leaves the source file exactly as found, and the
service re-verifies byte identity after every import and records the result as
a §17.4 metric. There is no code path in `crates/source-folder` that renames,
moves, truncates or deletes a file below the watched root.

**5. The watcher is an optimisation; the bounded sweep is the guarantee.**
Every lost-event condition — a `notify` error, a rescan request, a full queue —
increments a missed-change counter, and the next depth-, entry- and
time-bounded sweep recovers the work. The sweep only enqueues, so it never
blocks an import in flight.

## Consequences

- Import latency is bounded below by the quiescence window. That is the price
  of never importing a partial file, and §17.4 measures the latency *from true
  completion* so the window is not mistaken for slowness.
- A file written, paused for longer than the window, and resumed can still be
  caught mid-write. The post-hash re-`stat` turns that into a deferral rather
  than a bad import, but the window is evidence, not proof, and the ADR says so
  rather than claiming a guarantee we cannot make.
- Placeholder detection is a heuristic over what each platform exposes
  (`.icloud` names and zero allocated blocks on macOS,
  `FILE_ATTRIBUTE_RECALL_ON_*`/`OFFLINE` on Windows). A sync client that hides
  eviction entirely is caught only by the read probe failing. Detection is
  behind a trait so both the real behaviour and the simulated one are testable.
- Two files with identical bytes under two sources share one `media_assets`
  row and keep two `source_items` rows and two recordings. Deduplication is a
  storage decision, never a merge of meetings.
- Reconciliation reaches a fixpoint after one pass rather than being literally
  idempotent on the first: moving state is the first pass's job. The property
  test asserts the fixpoint and that no pass adds rows.

## Rejected alternatives

- **Debounce alone, with no readability or digest check.** The common shape in
  folder-watcher libraries, and insufficient: a cloud placeholder's size and
  mtime are perfectly stable, so quiescence alone imports an empty file as a
  finished recording — precisely what §7.10 forbids.
- **Path as the item key.** Simple, and wrong in both directions: a rename
  duplicates a recording, and a replacement at the same path silently
  overwrites a different meeting's association. §7.10 states outright that a
  filename is never sufficient identity.
- **Trusting `remote_version` to mean the audio changed.** This is what the
  vendor field looks like it means. §7.5 forbids it, and the concrete harm is
  an appended media revision for a metadata-only edit, which makes every
  derived output look stale.
- **Deleting or moving the source file after a successful import.** The
  Applaud pattern (App. A.6), explicitly rejected by App. A.4. It also makes
  every failure mode unrecoverable, because the input no longer exists.
- **Polling only, with no platform watcher.** Correct but slow: §4.3 asks for
  both, and a sweep frequent enough to feel responsive is a sweep expensive
  enough to be noticed on a large folder.
- **A global, always-on vendor transcript import.** Deferred to a verified
  official export path (§7.5, §18.6); the capability gate defaults to refusing
  and states why.

## Spec references

- §4.3 — watched-folder import and source reconciliation journey.
- §7.5 — source changes, version hints, vendor artifacts.
- §7.10 — source reconciliation and deletion safety.
- §13.3A — canonical identities and the durable import order.
- §17.4 — watcher and source quality metrics.
- App. A.2 — the reconciliation event/action table.
- App. A.4 — file watcher acceptance and hostile-input rules.
