/*
 * plaudon_apple_bridge.h - the C ABI between Rust and Swift (spec 11.2).
 *
 * Ownership rules, which the Rust side relies on and the Swift side
 * implements:
 *
 *  1. Every `char *` returned by this library is UTF-8, NUL terminated, and
 *     allocated by the library. The caller MUST release it with
 *     `plaudon_apple_string_free` and MUST NOT call `free` on it.
 *  2. Every `const char *` passed in is borrowed for the duration of the call
 *     only. The library copies anything it needs to keep.
 *  3. Every function returns a JSON document. Success and failure are both
 *     expressed inside that document, so there is no out-parameter and no
 *     errno-style convention to get wrong.
 *  4. Callbacks are invoked on an arbitrary library-owned thread. The caller
 *     must not assume the main thread and must not block inside a callback.
 *  5. Cancellation is cooperative: `plaudon_apple_cancel` marks a token, and
 *     the in-flight call returns a document whose "error" is "cancelled".
 *     A cancelled call never returns a partial transcript.
 *  6. Every long-running call takes a wall-clock budget in milliseconds and
 *     returns an "engine_timeout" document when it elapses. A platform
 *     recognizer that never finalizes must not be able to hang the caller.
 */

#ifndef PLAUDON_APPLE_BRIDGE_H
#define PLAUDON_APPLE_BRIDGE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ABI version. The Rust side refuses to load a different major value. */
#define PLAUDON_APPLE_BRIDGE_ABI_VERSION 1

/* Returns the ABI version this library was built with. */
int32_t plaudon_apple_bridge_abi_version(void);

/* Releases a string returned by any function in this library. */
void plaudon_apple_string_free(char *value);

/*
 * Runtime availability probe for SpeechAnalyzer/SpeechTranscriber.
 * `locale` is a BCP 47 tag or NULL to probe without a locale requirement.
 * Returns a JSON document; never NULL.
 */
char *plaudon_apple_probe_speech(const char *locale);

/*
 * Runtime availability probe for the on-device SystemLanguageModel.
 * Returns a JSON document; never NULL.
 */
char *plaudon_apple_probe_foundation_model(void);

/*
 * Transcribes a file. `cancel_token` is a value previously returned by
 * `plaudon_apple_new_cancel_token`, or 0 for an uncancellable call.
 * `timeout_ms` is a wall-clock budget; 0 means the library's default.
 * `progress` may be NULL; when present it is called with a 0..1 fraction on a
 * library-owned thread.
 * Returns a JSON document; never NULL.
 */
char *plaudon_apple_transcribe_file(const char *path,
                                    const char *locale,
                                    uint64_t cancel_token,
                                    uint64_t timeout_ms,
                                    void (*progress)(void *context, double fraction),
                                    void *context);

/*
 * Generates text with the on-device language model.
 * Returns a JSON document; never NULL. A guardrail refusal is reported as an
 * error document, never as generated text.
 */
char *plaudon_apple_generate_text(const char *prompt,
                                  uint64_t cancel_token,
                                  uint64_t timeout_ms);

/* Allocates a cancellation token. Returns 0 on failure. */
uint64_t plaudon_apple_new_cancel_token(void);

/* Marks a token cancelled. Safe to call from any thread, and idempotent. */
void plaudon_apple_cancel(uint64_t cancel_token);

/* Releases a cancellation token. */
void plaudon_apple_release_cancel_token(uint64_t cancel_token);

#ifdef __cplusplus
}
#endif

#endif /* PLAUDON_APPLE_BRIDGE_H */
