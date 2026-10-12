# ADR-0040: Apple bridge envelopes, and a load floor far below the feature floor

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §6.6, §11.1, §11.2, §11.3, §11.6, §12
- **Supersedes the ABI described in:** ADR-0005 (whose decision — a minimal
  macOS-only Swift surface behind a C ABI, with availability always probed at
  runtime — stands unchanged)

## Context

ADR-0005 fixed *what* crosses the Apple boundary. Building it raised four
questions it did not answer, each of which has a wrong answer that is easy to
reach by accident.

**How do arguments and results cross?** ADR-0005 left the ABI as positional C
parameters. Implementing the §11.3 speech lifecycle added a consent flag, a
word-timestamp flag, a time budget and a cancellation handle to one call, and
§11.4 added a schema, a token ceiling and a temperature to another. Growing
the parameter lists means every addition is an ABI break and every optional
value needs a sentinel.

**What macOS must the binary load on?** `Speech` and `FoundationModels` exist
only from macOS 26. The product supports older Macs through whisper.cpp
(§8.4), so building against a macOS 26 deployment target would make dyld
refuse to launch the whole application on exactly the machines whose fallback
path we promised.

**Who decides a locale is ready?** §11.3 lists "required downloadable assets"
among the things to check, and the Speech framework exposes two answers:
`SpeechTranscriber.installedLocales` and
`AssetInventory.status(forModules:)`. They disagree.

**How is a guardrail refusal distinguished from a failure?** The platform
throws `LanguageModelSession.GenerationError` on macOS 26 and
`LanguageModelError` on macOS 27 for the same conditions.

## Decision

**One JSON request envelope in, one JSON response envelope out, per
operation.** Five named entry points keep the operations discoverable;
everything variable travels inside the document. Two independent version
numbers are carried: `abi_version` (2) governs the entry points and their
ownership rules and is refused on mismatch before any call runs;
`envelope_version` (1) governs the document shapes and is refused by name.
Unknown fields are ignored and unknown enumeration values decode to the
closed default, so §6.6's "unknown is never support" holds against arbitrary
input.

**Deployment target macOS 13.0, with `Speech` and `FoundationModels`
weak-linked.** Every call into them sits behind `guard #available(macOS 26.0,
*)`. One binary therefore loads on macOS 13 through 27+: on an older system
the frameworks resolve to nothing, the probe answers `platform_unsupported`
with a typed reason, and the application transcribes with whisper.cpp.

**`installedLocales` is the readiness gate; `AssetInventory.status` is
corroboration.** Measured on macOS 27.0.1 build 26A434: with `en_US` present
in `installedLocales` and transcription demonstrably working,
`AssetInventory.status(forModules:)` returned `.supported` rather than
`.installed`, for every combination of attribute options and for both the raw
and the resolved locale. The status is consulted only when the locale is
absent from the installed set, where it distinguishes "downloading" from
"installable" from "unsupported".

**Both platform error families are classified.** A refusal maps to
`guardrail_violation` or `refusal` whichever type was thrown, so a refusal is
an error on the whole supported OS range rather than on half of it.

## Rejected alternatives

- **Growing the positional C parameter lists.** Each new option becomes an
  ABI break, optional values need sentinels, and a caller compiled against an
  older header passes garbage into a longer signature. The envelope moves
  that cost to a version check that fails closed.
- **A single `plaudon_apple_call(op, request)` entry point.** Smaller surface
  and a natural fuzz target, but it erases §11.1's named operations from the
  header, so the contract no longer says what the bridge is allowed to do.
  Five named entry points taking envelopes keep both properties.
- **Deployment target macOS 26, matching the feature floor.** Simpler build,
  no weak linking, no availability guards — and a Mac on macOS 13 cannot
  launch the application at all, which costs the whisper.cpp path on every
  machine that needs it most.
- **`@available` checks alone, with hard framework links.** The guards make
  the *calls* safe but not the *load*: dyld resolves a hard framework
  reference before any Swift code runs.
- **Trusting `AssetInventory.status` as the readiness gate.** It is the
  API whose name suggests it is the answer, and it refused a locale this
  machine transcribes successfully. Gating on it would report Apple speech as
  unavailable on a fully capable Mac.
- **Catching only `LanguageModelError`.** It does not exist before macOS 27,
  so the code would not compile against the deployment target; catching only
  the older `GenerationError` compiles and then silently reclassifies every
  macOS 27 refusal as a generic engine failure, which §11.4 forbids.
- **An `install_speech_assets` call folded into `transcribe_file`.** It fits
  §11.1's "four operations" more literally, but a download that takes minutes
  cannot share an entry point with a probe budgeted at milliseconds, and the
  consent step (§12) needs a result before any audio is involved.

## Consequences

- Adding a request field is not an ABI change; changing ownership rules is,
  and is caught by the version check rather than by a crash.
- `crates/ai-apple/tests/abi_snapshot.rs` fails the build when the header,
  the schema, the Swift source and the Rust decoder disagree on the entry
  points, the versions or the 24-code error vocabulary.
- The Apple capability can report `Supported` on a machine whose
  `AssetInventory` disagrees, which is correct and is covered by a named
  regression test.
- A Mac that cannot run Apple AI still runs the product, and the probe says
  why in words the interface shows (§5.4).
- The weak-linking decision is verified by inspecting the binary in CI
  (`LC_BUILD_VERSION minos 13.0`, two `LC_LOAD_WEAK_DYLIB` entries), not by
  trusting the build flags.
