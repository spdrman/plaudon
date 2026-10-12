# ADR-0015: Untrusted media decoding and model inference run out of process

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** SPEC v1.3 (see "Spec references")

## Status

Accepted — 2026-10-11. Implemented in `crates/security/src/sandbox/**`
(out-of-process execution, resource caps, deny-by-default profile and an
honest isolation report). The corpus/sanitizer half of the control is open as
finding F-012 against the EPIC F audio pipeline. Binding on every caller that
decodes media or runs a local model.

## Context

SPEC §6.3A is explicit: "We will restrict model inference helpers and media
decoders with OS process sandboxing where possible; pure Rust memory safety
does not make untrusted codecs or native C/C++ libraries safe." §16.2 lists
"Malicious audio decoder input" with the control "untrusted input sandboxing
where feasible; patched codecs; resource caps".

The exposure is real and unavoidable. Audio arrives from five routes (manual
import, watched folder, PLAUD Cloud CLI download, BLE transfer, USB volume);
all five are untrusted input domains. Decoding them means running format
parsers and codecs that are, in practice, C or C++ (the whisper.cpp family,
ffmpeg-derived decoders, vendor `.ASR`/Opus handling). A crafted container can
aim for memory corruption, an unbounded allocation, or a decode that never
terminates. The surrounding Rust tells us nothing about those libraries'
safety, and a sanitizer run finds bugs rather than proving their absence.

Equally important is availability: a 180-minute recording, a corrupt file, or
a model that exhausts memory must produce a failed job, never a dead
application and never an import that is marked complete without verified bytes
(§6.3A, §14.7).

## Decision

1. Media decoding and local model inference run in a **separate OS process**
   supervised by `plaudon_security::sandbox`, not in the application process.
2. The helper is launched with the OS sandboxing available on the platform
   (seatbelt profile on macOS, restricted token / job object on Windows) and
   with explicit caps: wall-clock timeout, memory cap, output-size cap.
3. The helper receives only what it needs — a file descriptor or byte range
   and its parameters. It receives **no** credential, no keychain handle, no
   `EgressTicket`, and therefore no route to our network transport. Its
   environment is scrubbed.
4. A crash, a hang, an OOM or a cap violation is an **expected outcome**: the
   supervisor kills the process and the job records a failed state with a
   stable error code. It is never recorded as `Completed`, and partial output
   is discarded rather than committed.
5. The helper's output is untrusted too: lengths and frame counts are
   validated against the caps before any allocation in the parent.
6. Where a platform or a dependency makes out-of-process execution impossible
   for a given operation, the limitation is named in the type and in the
   user-facing status rather than being quietly ignored, and the operation
   keeps its resource caps.

## Consequences

* A per-job process launch costs milliseconds and some memory. For a workload
  measured in seconds to minutes of decode and inference, that is negligible.
* Streaming between the parent and the helper needs a defined IPC format with
  bounded frames; this is more work than calling a function, and it is the
  price of the isolation.
* Fuzzing gets easier, not harder: the helper is a standalone binary over a
  byte stream, which is the ideal libFuzzer/AFL target shape.
* The sandbox is a blast-radius reduction, not a proof. A sandbox escape is a
  real (if expensive) attack. We therefore keep patched codecs, resource caps
  and a fuzz corpus as parallel controls instead of claiming the problem is
  solved.
* Apple's frameworks (EPIC H) are invoked through the in-process C ABI bridge
  rather than our helper, because the processing happens inside Apple's own
  services. That is a different boundary with a different owner, and the
  threat model says so rather than implying our sandbox covers it.

## Rejected alternatives

- **Decode in-process behind `catch_unwind` and careful bounds checks.**
  Rejected: `catch_unwind` does not contain memory corruption in C code, and a
  segfault takes the whole application down mid-import.
- **Pure-Rust decoders only (e.g. symphonia everywhere).** Attractive, and
  preferred where format support allows, but it does not cover the formats and
  runtimes we actually need (vendor `.ASR`, whisper.cpp), so it cannot be the
  isolation strategy. Memory-safe code still needs the resource caps.
- **WebAssembly sandboxing of codecs.** Interesting for a future iteration;
  rejected for V1 because of the toolchain burden for the native inference
  runtimes and the performance cost on long recordings.
- **A single long-lived helper daemon for all jobs.** Rejected: it makes one
  hostile file's corruption persist across jobs and it drifts toward the
  always-running privileged component §6.2 rules out. One process per unit of
  work keeps failures isolated.

## Spec references

SPEC v1.3 §6.3A, §6.2, §8.1 (media validation), §14.7 (job state and resource
arbitration), §16.2 ("Malicious audio decoder input"), §18.5 and §18.6A
(adversarial and property-based clusters), §22.5.

Matrix row: TM-012 in
[`../threat-model/traceability.json`](../threat-model/traceability.json);
finding F-012 in
[`../threat-model/open-findings.md`](../threat-model/open-findings.md);
untrusted input domains 2, 3 and 6 in
[`../threat-model/trust-boundaries.md`](../threat-model/trust-boundaries.md).
