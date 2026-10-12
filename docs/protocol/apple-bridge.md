# Apple bridge protocol

**Status:** normative for ABI version 2, envelope version 1
**Spec:** PLAUD BYOK Desktop v1.3 — §6.6, §11.1, §11.2, §11.6
**Normative files:**

| File | Role |
|---|---|
| `native/apple-bridge/include/plaudon_apple_bridge.h` | The contract. Ownership, threading, cancellation, error vocabulary. |
| `schemas/apple-bridge-envelope.v1.schema.json` | Document shapes in both directions. |
| `native/apple-bridge/Sources/PlaudonAppleBridge/` | The only implementation. |
| `crates/ai-apple/src/envelope.rs` | The only Rust decoder. |
| `crates/ai-apple/tests/abi_snapshot.rs` | Fails the build when the four disagree. |

This document explains the contract. Where it and the header differ, the
header wins.

## Why a C ABI and not Swift interop

§11.2 prefers "a stable C ABI over directly binding Rust to evolving
Swift-native actor types". `SpeechAnalyzer` is an `actor`, `SpeechTranscriber`
is a class with an `AsyncSequence` of results, and `LanguageModelSession` is
built on Swift concurrency. None of those have a stable calling convention a
non-Swift caller can rely on across compiler releases, and all three change
shape between OS versions.

So the division is: **Swift owns the Apple objects, Swift concurrency and the
analyzer lifecycle. Rust owns job orchestration, persistence and policy.**
Between them pass five operations, three document shapes and one integer
handle type. No object pointer crosses, in either direction.

## Shape of every call

```text
Rust                                   Swift
----                                   -----
encode_request(op, body)  ──request──▶ BridgeRequest.decode(json, expecting: op)
                                       │
                          ◀─progress──┤  relay.report(progressDocument(...))   (0..n)
                                       │
BridgeReply::parse(op, …) ◀─response──┘  successDocument / failureDocument
plaudon_apple_string_free(document)
```

Five operations: `probe_speech`, `probe_foundation_model`,
`install_speech_assets`, `transcribe_file`, `generate_text`.

§11.1 scopes the bridge to four operations "plus cancel/progress".
`install_speech_assets` is the fifth and is not an expansion of scope: §11.3
requires that we "let the user authorize required downloads, show progress",
and a download that may take minutes cannot share an entry point with a probe
that must answer in milliseconds. It carries no application logic; it asks
`AssetInventory` for an installation request and reports its `Progress`.

## Versioning

Two independent numbers, both carried in the documents:

- **`abi_version`** (2) describes the *entry points and their ownership
  rules*. A change here means a caller compiled against the old header can be
  wrong about memory. Rust refuses to call a library reporting any other
  value, before any operation runs.
- **`envelope_version`** (1) describes the *document shapes*. A request whose
  version Swift does not implement is refused with
  `unsupported_envelope_version`; a response whose version Rust does not
  implement is refused the same way. Neither side guesses.

Unknown *fields* are ignored. Unknown *enumeration values* decode to the
closed default: an unrecognized error code behaves as `engine_failure`, an
unrecognized asset status as `unsupported`, an unrecognized progress phase as
`unknown`. §6.6's rule — unknown is never support — is enforced by the types,
and `crates/ai-apple/tests/envelope_fuzz.rs` asserts it against arbitrary
input.

## Ownership

1. Every `char *` the library returns is UTF-8, NUL terminated, allocated with
   `malloc` inside the library, and released by the caller with
   `plaudon_apple_string_free` — never with `free`, and never by a Rust
   allocator. There is exactly one release function and exactly one release
   per returned buffer.
2. Every `const char *` the caller passes is borrowed for the duration of the
   call. The library copies anything it keeps. Rust holds the `CString`
   temporaries in named locals for precisely this reason.
3. The library never returns `NULL`. An allocation failure aborts, because a
   caller cannot distinguish `NULL` from a lost error.
4. A progress document is borrowed for the duration of the callback. The
   callee must not retain or free it.

Double-free and use-after-free are therefore impossible *by contract* rather
than by discipline: the only pointer the caller ever owns is the response
buffer, it has exactly one documented release function, and the Rust wrapper
releases it inside `take_document` before the pointer escapes the module.

## Threads and callbacks

Entry points are blocking and may be called from any thread, concurrently.

A progress callback is invoked on a library-owned thread. Deliveries for one
call are serialized against each other by a lock inside `ProgressRelay`, and
the same lock provides the two guarantees a caller needs to free its context
safely:

- **No delivery after the call returns.** `runBounded` disarms the relay in a
  `defer`, under every exit path.
- **No delivery after `plaudon_apple_cancel` returns** for that call's token.
  Cancellation looks up the relays bound to the token and disarms them, so a
  caller that cancels and then tears down its context cannot be surprised.

Disarming takes the relay lock, so it returns only once any in-flight
delivery has finished. A callback must not re-enter the library and must not
block; deliveries happen while the lock is held.

`native/apple-bridge/tests/abi_stress.c` asserts both guarantees under a
cancel storm: six threads cancelling the same token while a call runs, with
the observer poisoned the instant the call returns.

## Cancellation

Handles come from a monotonically increasing counter and are never reused, so
a stale handle can never address a live operation. Token `0` means "not
cancellable" and every cancellation call on it is a no-op.

| Moment | Behaviour |
|---|---|
| Before the operation starts | Refused immediately with `cancelled` / `before_start`. No progress is delivered. |
| While it runs | The in-flight call stops within one 25 ms poll, asks the platform task to stop, waits up to 2 s for it to unwind, and returns `cancelled` / `in_flight`. |
| After it completes | No-op. |
| After the handle is released | No-op. |
| On an unallocated handle | No-op. |
| Twice, or from several threads | Idempotent. |

A cancelled call never returns a partial transcript or partial generated
text. On the Rust side, `CancelGuard` cancels and then releases on drop, so an
abandoned job tells the library to stop rather than leaving an orphan.

## Time budgets

Every long-running entry point takes `timeout_ms`; `0` selects the library
default of ten minutes. Probes have their own 2 s ceiling inside the library
and a 100 ms ceiling in Rust (`plaudon_ai_apple::probe::PROBE_BUDGET`): a
probe that overruns is reported as `engine_timeout`, the capability maps to
`Unknown`, and the application proceeds with whisper.cpp.

The bound is not theoretical. The A1.3 spike recorded `SFSpeechRecognizer`
never finalizing on an unauthorized process, with no error and no callback.
The current bridge does not use that API, but the lesson — a platform
component can simply never answer — is why no call here is unbounded.

## Error vocabulary

Closed, 24 codes, listed in section 5 of the header and in the schema's
`errorCode` enum. The three files are compared by `abi_snapshot.rs`.

`retriable` is advice for the job engine, not a promise. A hardware or
platform verdict never becomes retriable by waiting.

Error `detail` is a short machine-readable discriminator. It is never a
localized message and never a platform exception string: §6.6 forbids
surfacing third-party exception text, which can carry paths or user content.
`shortTypeName` is used instead, which gives the error's type and nothing
else.

## Operations

### `probe_speech`

```json
{"envelope_version":1,"op":"probe_speech","locale":"en-US"}
```

Answers everything §11.3 asks for: module availability, supported *and*
installed locales (they differ, and the difference decides whether a download
is needed), the asset state for the resolved locale, the accepted audio
formats, the concurrency limit, and the OS build and hardware the answer came
from.

Availability and readiness are reported separately on purpose. "Apple speech
exists on this Mac" and "this language can be transcribed right now" are
different answers, and conflating them is how a product claims support it
does not have.

### `probe_foundation_model`

Availability, supported languages and whether the requested locale is among
them. `max_context_tokens` is `null`: the framework publishes no context
window. The only place a real number appears is inside a context-overflow
error, which is why `generate_text` surfaces it in the error detail and
`plaudon_app_core::chunking` has a labelled conservative bound.

`is_transcriber` is always `false` and the Rust mapping asserts it. §2.3:
`SystemLanguageModel` is a text model and is never a substitute for
transcription.

### `install_speech_assets`

Called only after the user approved a plan obtained from the probe. Reserves
the locale so the installed assets are not purged, then drives
`AssetInventory.assetInstallationRequest(supporting:)` and reports its
`Progress` every 250 ms. `transcribe_file` will not install anything unless
its request carries `allow_asset_download: true`.

### `transcribe_file`

Drives `SpeechAnalyzer` with a `SpeechTranscriber` configured for
`audioTimeRange` and `transcriptionConfidence`. Only finalized results are
collected; volatile updates drive progress and are discarded. A run that
finalizes nothing is `output_not_final`, never an empty success — reporting
an empty transcript as a success would silently destroy the recording's only
chance at a real one.

`alignment_origin` is `native` when the engine reported offsets and `none`
when it did not. §8.6A: a text-only result never gains a timeline.

### `generate_text`

Drives `LanguageModelSession.respond`. With a `schema` it uses guided
generation and returns the generated JSON; the Rust side validates it again
regardless, because the platform constrains its decoder against its own
translation of the schema, not against ours.

`instructions` and `prompt` are separate fields and stay separate: §9.6
treats transcript text as untrusted input, so the task lives in the
instruction channel and the transcript lives in the prompt.

A guardrail or refusal is an error document. Both `LanguageModelError`
(macOS 27) and `LanguageModelSession.GenerationError` (macOS 26) are
classified, because the platform throws the newer type on newer systems and
catching only one would turn a refusal into a generic failure on half the
supported range.

## Platform floor and weak linking

The Swift library is built with a macOS 13.0 deployment target, and `Speech`
and `FoundationModels` are weak-linked. Every call into them sits behind
`guard #available(macOS 26.0, *)`.

The result is that one binary loads on macOS 13 through 27+. On an older
system the frameworks resolve to nothing, the availability guard fails, the
probe answers `platform_unsupported`, and the application transcribes with
whisper.cpp. Hard-linking would make dyld refuse to launch the whole
application on those machines.

Verified on the build in `scripts/build-apple-bridge.sh`:

```text
LC_BUILD_VERSION minos 13.0
LC_LOAD_WEAK_DYLIB  /System/Library/Frameworks/Speech.framework/…/Speech
LC_LOAD_WEAK_DYLIB  /System/Library/Frameworks/FoundationModels.framework/…/FoundationModels
```

The Swift runtime needs an rpath of `/usr/lib/swift`; without it the binary
cannot resolve `libswift_Concurrency.dylib` and fails to launch. `build.rs`
emits it, and a dependent binary reads the same flags from
`DEP_PLAUDON_AI_APPLE_LINK_ARGS`.

## Test surfaces

| Suite | Runs | Covers |
|---|---|---|
| `crates/ai-apple` unit tests | Every platform, default CI | Envelope decoding, capability mapping, probe cache, both adapters against `FakeBridge` |
| `crates/ai-apple/tests/abi_snapshot.rs` | Every platform, default CI | Header, schema, Swift and Rust agree |
| `crates/ai-apple/tests/envelope_fuzz.rs` | Every platform, default CI | Arbitrary input never produces a capability claim |
| `native/apple-bridge/tests/abi_stress.c` | macOS, `scripts/apple-bridge-sanitizers.sh` | Memory, threading, cancel storms, handle balance, under ASan/UBSan/TSan |
| `tests/hardware/apple_compatibility.rs` | macOS, opt-in | The three Mac classes of §11.5, against real hardware |

Nothing in the first three touches an Apple framework. The fourth needs a Mac
but no Apple Intelligence and no models. Only the fifth needs real hardware,
and it never runs by default.
