/*
 * plaudon_apple_bridge.h - the C ABI between Rust and Swift (spec 11.1, 11.2,
 * 11.6).
 *
 * This header is the normative contract. `docs/protocol/apple-bridge.md`
 * explains it; `schemas/apple-bridge-envelope.v1.schema.json` constrains the
 * documents; `crates/ai-apple/tests/abi_snapshot.rs` fails the build if the
 * three ever disagree.
 *
 * ===========================================================================
 * 1. Ownership
 * ===========================================================================
 *
 *  1.1 Every `char *` returned by this library is UTF-8, NUL terminated and
 *      allocated by the library. The caller MUST release it with
 *      `plaudon_apple_string_free` and MUST NOT call `free` on it. The two
 *      sides do not share an allocator.
 *  1.2 `plaudon_apple_string_free` is the only release function, and every
 *      returned buffer is released exactly once through it. A second release
 *      of the same pointer is a caller defect, not a supported call.
 *  1.3 Every `const char *` passed in is borrowed for the duration of the
 *      call only. The library copies anything it keeps.
 *  1.4 No Swift or Objective-C object pointer ever crosses this boundary.
 *      The only handles are opaque `uint64_t` cancellation tokens, which are
 *      indices into a library-private registry and carry no address.
 *  1.5 Returned documents are never NULL. An allocation failure inside the
 *      library aborts the process rather than returning NULL, because a
 *      caller cannot distinguish NULL from a lost error.
 *
 * ===========================================================================
 * 2. Envelopes
 * ===========================================================================
 *
 *  2.1 Every entry point takes one JSON request document and returns one JSON
 *      response document. There are no out-parameters and no errno-style
 *      conventions.
 *  2.2 Both documents carry `envelope_version`. A request whose
 *      `envelope_version` this library does not implement is refused with
 *      `unsupported_envelope_version`; the library never guesses.
 *  2.3 Responses additionally carry `abi_version`, equal to
 *      `PLAUDON_APPLE_BRIDGE_ABI_VERSION`, so a response can be validated even
 *      when it was captured from a log or a fixture.
 *  2.4 Unknown request fields are ignored. Unknown response fields and unknown
 *      enumeration values decode as unknown on the Rust side and never as an
 *      available capability (spec 6.6).
 *  2.5 Success and failure share one shape: `"ok": true` with `result`, or
 *      `"ok": false` with `error { code, detail?, retriable }`. The `code`
 *      vocabulary is closed and listed in section 5; an unrecognized code is
 *      treated as `engine_failure`.
 *
 * ===========================================================================
 * 3. Threads and callbacks
 * ===========================================================================
 *
 *  3.1 Entry points are blocking and may be called from any thread. They are
 *      safe to call concurrently; the library serializes what the platform
 *      requires to be serialized.
 *  3.2 A progress callback receives a JSON progress document. It is invoked on
 *      a library-owned thread, never on the caller's thread and never on the
 *      main queue. Progress deliveries for one call are serialized with
 *      respect to each other.
 *  3.3 No progress callback is delivered after the call that registered it has
 *      returned. The library drains and disarms the relay before returning.
 *  3.4 No progress callback is delivered after `plaudon_apple_cancel` has
 *      returned for that call's token. Cancellation disarms the relay
 *      synchronously, so a cancelled call cannot surprise a caller that has
 *      already freed its callback context.
 *  3.5 A progress callback MUST NOT call back into this library and MUST NOT
 *      block. Deliveries are made while holding the relay lock.
 *
 * ===========================================================================
 * 4. Cancellation and time budgets
 * ===========================================================================
 *
 *  4.1 Cancellation is cooperative and idempotent. `plaudon_apple_cancel` is
 *      safe before the operation starts, while it runs, after it completes and
 *      after the token has been released. Token 0 means "not cancellable" and
 *      every cancellation call on it is a no-op.
 *  4.2 A cancelled call returns an error document with code `cancelled`. It
 *      never returns a partial transcript or partial generated text
 *      (spec 11.3, 11.4).
 *  4.3 Releasing a token is idempotent. A released token never aliases a live
 *      operation: handles are drawn from a monotonically increasing counter
 *      and are never reused.
 *  4.4 Every long-running entry point takes `timeout_ms` in its request. 0
 *      selects the library default. The call returns `engine_timeout` when the
 *      budget elapses, so a platform component that never finalizes cannot
 *      hang the caller.
 *
 * ===========================================================================
 * 5. Error codes (closed vocabulary, stable across ABI 2)
 * ===========================================================================
 *
 *   invalid_request              malformed envelope, or an operation this
 *                                library does not implement
 *   unsupported_envelope_version envelope_version outside the supported range
 *   invalid_argument             a field is present but unusable
 *   platform_unsupported         framework absent, or OS older than the
 *                                deployment target of the API
 *   hardware_unsupported         this Mac is not eligible for the feature
 *   disabled_by_user             Apple Intelligence is switched off
 *   permission_denied            the OS withheld an authorization
 *   locale_unsupported           the locale is outside the supported set
 *   model_asset_missing          assets are absent and download was not
 *                                authorized by the caller
 *   model_asset_not_ready        assets are downloading or being verified
 *   asset_download_failed        an authorized installation failed
 *   concurrency_limit            the platform's concurrent-use limit is
 *                                reached (e.g. reserved locales)
 *   input_unreadable             the path does not exist or cannot be opened
 *   unsupported_media            the container or codec could not be decoded
 *   cancelled                    cooperative cancellation
 *   engine_timeout               the wall-clock budget elapsed
 *   guardrail_violation          an Apple safety guardrail blocked the request
 *   refusal                      the model declined to answer
 *   context_size_exceeded        the prompt exceeded the model's context
 *   rate_limited                 the platform throttled the request
 *   unsupported_language         the model does not support the language
 *   schema_unsupported           the guided-output schema cannot be expressed
 *   output_not_final             the platform produced only volatile output
 *   engine_failure               anything else, including unrecognized codes
 */

#ifndef PLAUDON_APPLE_BRIDGE_H
#define PLAUDON_APPLE_BRIDGE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ABI version. The Rust side refuses to call a library reporting any other
 * value: the ownership rules themselves could have changed. */
#define PLAUDON_APPLE_BRIDGE_ABI_VERSION 2

/* Envelope version this library implements. */
#define PLAUDON_APPLE_BRIDGE_ENVELOPE_VERSION 1

/* Returns the ABI version this library was built with. */
int32_t plaudon_apple_bridge_abi_version(void);

/* Releases a string returned by any function in this library. Passing NULL is
 * a no-op. */
void plaudon_apple_string_free(char *value);

/*
 * Progress callback. `progress_json` is owned by the library, valid only for
 * the duration of the call, and must not be freed by the callee.
 */
typedef void (*plaudon_apple_progress_fn)(void *context, const char *progress_json);

/*
 * Runtime availability probe for SpeechTranscriber (spec 11.3).
 * Request:  { "envelope_version": 1, "op": "probe_speech", "locale": "en-US" }
 * Returns a JSON response document; never NULL. Must not block on I/O long
 * enough to be perceptible: the Rust side budgets this call at 100 ms.
 */
char *plaudon_apple_probe_speech(const char *request_json);

/*
 * Runtime availability probe for the on-device SystemLanguageModel
 * (spec 11.4). Request: { "envelope_version": 1,
 *                         "op": "probe_foundation_model", "locale": "en-US" }
 * Returns a JSON response document; never NULL.
 */
char *plaudon_apple_probe_foundation_model(const char *request_json);

/*
 * Downloads and installs the speech assets for a locale (spec 11.3, 12).
 * Never called implicitly: the caller performs the consent step first.
 * Request:  { "envelope_version": 1, "op": "install_speech_assets",
 *             "locale": "en-US", "cancel_token": 0, "timeout_ms": 0 }
 * Progress: { "phase": "asset_download", "fraction": 0.42,
 *             "completed_units": 42, "total_units": 100 }
 * Returns a JSON response document; never NULL.
 */
char *plaudon_apple_install_speech_assets(const char *request_json,
                                          plaudon_apple_progress_fn progress,
                                          void *context);

/*
 * Transcribes a file with SpeechAnalyzer + SpeechTranscriber (spec 11.3).
 * Request:  { "envelope_version": 1, "op": "transcribe_file",
 *             "path": "/abs/path.wav", "locale": "en-US",
 *             "cancel_token": 0, "timeout_ms": 0,
 *             "allow_asset_download": false, "want_word_timestamps": false }
 * Progress: { "phase": "transcribe", "fraction": 0.5, "processed_ms": 3000 }
 * Returns a JSON response document; never NULL. Only finalized results appear
 * in the response: a volatile partial is never reported as a transcript.
 */
char *plaudon_apple_transcribe_file(const char *request_json,
                                    plaudon_apple_progress_fn progress,
                                    void *context);

/*
 * Generates text with the on-device language model (spec 11.4).
 * Request:  { "envelope_version": 1, "op": "generate_text",
 *             "prompt": "...", "cancel_token": 0, "timeout_ms": 0,
 *             "schema": { ... JSON Schema subset ... },
 *             "max_output_tokens": 1024, "temperature": 0.2 }
 * Returns a JSON response document; never NULL. A guardrail refusal is an
 * error document, never generated text.
 */
char *plaudon_apple_generate_text(const char *request_json,
                                  plaudon_apple_progress_fn progress,
                                  void *context);

/* Allocates a cancellation token. Returns 0 only if the registry is
 * exhausted, which cannot happen before 2^64 allocations. */
uint64_t plaudon_apple_new_cancel_token(void);

/* Marks a token cancelled and disarms any progress relay bound to it. Safe to
 * call from any thread, at any time, any number of times. */
void plaudon_apple_cancel(uint64_t cancel_token);

/* Releases a cancellation token. Idempotent. Handles are never reused. */
void plaudon_apple_release_cancel_token(uint64_t cancel_token);

/* Diagnostics: 1 if the token has been cancelled, 0 otherwise (including for
 * released and never-allocated tokens). Used by the bridge test suites. */
int32_t plaudon_apple_cancel_token_is_cancelled(uint64_t cancel_token);

/* Diagnostics: number of tokens allocated and not yet released. The stress
 * suite asserts this returns to its starting value. */
uint64_t plaudon_apple_live_cancel_token_count(void);

#ifdef __cplusplus
}
#endif

#endif /* PLAUDON_APPLE_BRIDGE_H */
