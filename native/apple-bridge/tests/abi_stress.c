/*
 * abi_stress.c - the sanitizer-visible stress and fuzz driver for the C ABI
 * (spec 6.6 "we will fuzz and stress the bridge before shipping", 18.4).
 *
 * This driver exists in C rather than in Rust because the interesting
 * failures are allocator and threading failures across the language boundary,
 * and clang's AddressSanitizer / UndefinedBehaviorSanitizer / ThreadSanitizer
 * instrument both sides of a C-to-Swift call without needing a nightly Rust
 * toolchain.
 *
 * Build and run through `scripts/apple-bridge-sanitizers.sh`.
 *
 * Every check is an assertion about the documented contract, not about the
 * platform's answer: the suite must pass on a Mac with Apple Intelligence
 * disabled and on a Mac that supports nothing at all.
 */

#include <assert.h>
#include <dirent.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "plaudon_apple_bridge.h"

static int failures = 0;
static int checks = 0;

#define CHECK(cond, ...)                                                       \
    do {                                                                       \
        checks++;                                                              \
        if (!(cond)) {                                                         \
            failures++;                                                        \
            fprintf(stderr, "FAIL %s:%d: ", __FILE__, __LINE__);               \
            fprintf(stderr, __VA_ARGS__);                                      \
            fputc('\n', stderr);                                               \
        }                                                                      \
    } while (0)

/* ------------------------------------------------------------------ */
/* Tiny JSON probes. The driver only needs to find a few literal keys, */
/* so a substring search is both sufficient and dependency free.       */
/* ------------------------------------------------------------------ */

static bool has(const char *haystack, const char *needle) {
    return haystack != NULL && strstr(haystack, needle) != NULL;
}

static char *take(char *document) {
    /* Every returned document is released exactly once, through the library's
     * own free function (header rule 1.2). Returning the pointer after the
     * free would be a use-after-free the sanitizer catches, so callers copy
     * what they need before calling this. */
    plaudon_apple_string_free(document);
    return NULL;
}

static void expect_error(const char *what, char *document, const char *code) {
    CHECK(document != NULL, "%s returned NULL, which the contract forbids", what);
    if (document == NULL) {
        return;
    }
    CHECK(has(document, "\"ok\":false"), "%s: expected failure, got %s", what, document);
    CHECK(has(document, code), "%s: expected code %s, got %s", what, code, document);
    CHECK(has(document, "\"abi_version\":2"), "%s: missing abi_version in %s", what, document);
    CHECK(has(document, "\"envelope_version\":1"), "%s: missing envelope_version in %s", what,
          document);
    take(document);
}

/* ------------------------------------------------------------------ */
/* Progress relay observation                                          */
/* ------------------------------------------------------------------ */

typedef struct {
    atomic_int deliveries;
    atomic_int after_return;
    atomic_int poisoned;
} relay_observer;

static void on_progress(void *context, const char *progress_json) {
    relay_observer *observer = (relay_observer *)context;
    if (observer == NULL) {
        return;
    }
    /* Header rule 3.3/3.4: a delivery that happens after the owning call has
     * returned, or after cancel returned, is a contract violation. */
    if (atomic_load(&observer->poisoned)) {
        atomic_fetch_add(&observer->after_return, 1);
    }
    if (progress_json == NULL || strstr(progress_json, "\"phase\"") == NULL) {
        atomic_fetch_add(&observer->after_return, 1);
    }
    atomic_fetch_add(&observer->deliveries, 1);
}

/* ------------------------------------------------------------------ */
/* 1. Envelope conformance                                             */
/* ------------------------------------------------------------------ */

static void test_abi_version(void) {
    CHECK(plaudon_apple_bridge_abi_version() == PLAUDON_APPLE_BRIDGE_ABI_VERSION,
          "ABI version mismatch: library reports %d, header declares %d",
          plaudon_apple_bridge_abi_version(), PLAUDON_APPLE_BRIDGE_ABI_VERSION);
}

static void test_malformed_requests(void) {
    expect_error("null request", plaudon_apple_probe_speech(NULL), "invalid_request");
    expect_error("empty request", plaudon_apple_probe_speech(""), "invalid_request");
    expect_error("not json", plaudon_apple_probe_speech("not json at all"), "invalid_request");
    expect_error("array", plaudon_apple_probe_speech("[]"), "invalid_request");
    expect_error("no version", plaudon_apple_probe_speech("{\"op\":\"probe_speech\"}"),
                 "invalid_request");
    expect_error("future version",
                 plaudon_apple_probe_speech("{\"envelope_version\":99,\"op\":\"probe_speech\"}"),
                 "unsupported_envelope_version");
    expect_error("wrong op",
                 plaudon_apple_probe_speech("{\"envelope_version\":1,\"op\":\"generate_text\"}"),
                 "invalid_request");
    expect_error("locale wrong type",
                 plaudon_apple_probe_speech(
                     "{\"envelope_version\":1,\"op\":\"probe_speech\",\"locale\":42}"),
                 "invalid_argument");
    expect_error("missing path",
                 plaudon_apple_transcribe_file(
                     "{\"envelope_version\":1,\"op\":\"transcribe_file\"}", NULL, NULL),
                 "invalid_argument");
    expect_error("negative token",
                 plaudon_apple_transcribe_file("{\"envelope_version\":1,\"op\":\"transcribe_file\","
                                               "\"path\":\"/x\",\"cancel_token\":-1}",
                                               NULL, NULL),
                 "invalid_argument");
    expect_error("fractional timeout",
                 plaudon_apple_generate_text("{\"envelope_version\":1,\"op\":\"generate_text\","
                                             "\"prompt\":\"hi\",\"timeout_ms\":1.5}",
                                             NULL, NULL),
                 "invalid_argument");
    expect_error("temperature out of range",
                 plaudon_apple_generate_text("{\"envelope_version\":1,\"op\":\"generate_text\","
                                             "\"prompt\":\"hi\",\"temperature\":9}",
                                             NULL, NULL),
                 "invalid_argument");
    expect_error("empty prompt",
                 plaudon_apple_generate_text(
                     "{\"envelope_version\":1,\"op\":\"generate_text\",\"prompt\":\"\"}", NULL,
                     NULL),
                 "invalid_argument");
    expect_error("missing file",
                 plaudon_apple_transcribe_file(
                     "{\"envelope_version\":1,\"op\":\"transcribe_file\","
                     "\"path\":\"/nonexistent/plaudon/fixture.wav\"}",
                     NULL, NULL),
                 "input_unreadable");

    /* Unknown request fields are ignored, never fatal (header rule 2.4). */
    char *document = plaudon_apple_probe_speech(
        "{\"envelope_version\":1,\"op\":\"probe_speech\",\"from_the_future\":{\"a\":[1,2]}}");
    CHECK(document != NULL, "unknown fields produced NULL");
    CHECK(has(document, "\"abi_version\":2"), "unknown fields lost the envelope frame");
    take(document);
}

/* Feed every file in a corpus directory to every entry point. */
static void test_corpus(const char *directory) {
    DIR *dir = opendir(directory);
    if (dir == NULL) {
        fprintf(stderr, "note: no corpus at %s, skipping\n", directory);
        return;
    }
    struct dirent *entry;
    int seeds = 0;
    while ((entry = readdir(dir)) != NULL) {
        if (entry->d_name[0] == '.') {
            continue;
        }
        char path[2048];
        snprintf(path, sizeof(path), "%s/%s", directory, entry->d_name);
        FILE *file = fopen(path, "rb");
        if (file == NULL) {
            continue;
        }
        fseek(file, 0, SEEK_END);
        long size = ftell(file);
        fseek(file, 0, SEEK_SET);
        if (size < 0 || size > (1 << 22)) {
            fclose(file);
            continue;
        }
        char *buffer = (char *)malloc((size_t)size + 1);
        if (buffer == NULL) {
            fclose(file);
            continue;
        }
        size_t read = fread(buffer, 1, (size_t)size, file);
        buffer[read] = '\0';
        fclose(file);

        /* Every entry point must answer with a document, never crash and
         * never hang, for arbitrary input. */
        take(plaudon_apple_probe_speech(buffer));
        take(plaudon_apple_probe_foundation_model(buffer));
        take(plaudon_apple_transcribe_file(buffer, NULL, NULL));
        take(plaudon_apple_generate_text(buffer, NULL, NULL));
        take(plaudon_apple_install_speech_assets(buffer, NULL, NULL));
        free(buffer);
        seeds++;
    }
    closedir(dir);
    CHECK(seeds > 0, "corpus directory %s contained no seeds", directory);
    printf("  corpus seeds exercised: %d\n", seeds);
}

/* Structured mutation of a valid envelope: truncation, byte flips and
 * oversized payloads, all of which must produce a document. */
static void test_mutations(void) {
    const char *seed = "{\"envelope_version\":1,\"op\":\"transcribe_file\",\"path\":\"/tmp/a.wav\","
                       "\"locale\":\"en-US\",\"cancel_token\":0,\"timeout_ms\":1000,"
                       "\"allow_asset_download\":false,\"want_word_timestamps\":true}";
    size_t length = strlen(seed);

    for (size_t cut = 0; cut <= length; cut++) {
        char *truncated = (char *)malloc(cut + 1);
        memcpy(truncated, seed, cut);
        truncated[cut] = '\0';
        take(plaudon_apple_transcribe_file(truncated, NULL, NULL));
        free(truncated);
    }

    unsigned int state = 0x5eed1234u;
    for (int round = 0; round < 4000; round++) {
        char *mutated = strdup(seed);
        int flips = 1 + (int)(state % 4u);
        for (int i = 0; i < flips; i++) {
            state = state * 1103515245u + 12345u;
            size_t at = state % length;
            state = state * 1103515245u + 12345u;
            /* Includes bytes that are not valid UTF-8 on their own. */
            mutated[at] = (char)(state & 0xffu);
        }
        take(plaudon_apple_transcribe_file(mutated, NULL, NULL));
        take(plaudon_apple_generate_text(mutated, NULL, NULL));
        free(mutated);
    }

    /* An oversized but well-formed envelope must be refused by argument
     * validation, not by the allocator. */
    size_t big = 1u << 20;
    char *oversized = (char *)malloc(big + 128);
    int head = snprintf(oversized, 128,
                        "{\"envelope_version\":1,\"op\":\"generate_text\",\"prompt\":\"");
    memset(oversized + head, 'a', big);
    memcpy(oversized + head + big, "\"}", 3);
    char *document = plaudon_apple_generate_text(oversized, NULL, NULL);
    CHECK(document != NULL, "oversized prompt produced NULL");
    take(document);
    free(oversized);
    printf("  mutations exercised: %zu truncations, 8000 flipped calls\n", length + 1);
}

/* ------------------------------------------------------------------ */
/* 2. Cancellation lifecycle                                           */
/* ------------------------------------------------------------------ */

static void test_cancellation_lifecycle(void) {
    uint64_t baseline = plaudon_apple_live_cancel_token_count();

    uint64_t token = plaudon_apple_new_cancel_token();
    CHECK(token != 0, "token allocation returned 0");
    CHECK(plaudon_apple_live_cancel_token_count() == baseline + 1, "live count did not rise");
    CHECK(plaudon_apple_cancel_token_is_cancelled(token) == 0, "fresh token reads cancelled");

    /* Idempotent at every point. */
    plaudon_apple_cancel(token);
    plaudon_apple_cancel(token);
    plaudon_apple_cancel(token);
    CHECK(plaudon_apple_cancel_token_is_cancelled(token) == 1, "cancel did not stick");

    plaudon_apple_release_cancel_token(token);
    plaudon_apple_release_cancel_token(token);
    CHECK(plaudon_apple_cancel_token_is_cancelled(token) == 0,
          "released token still reports cancelled");
    /* Cancelling and releasing after release must be harmless. */
    plaudon_apple_cancel(token);
    plaudon_apple_release_cancel_token(token);

    /* Never-allocated and reserved handles. */
    plaudon_apple_cancel(0);
    plaudon_apple_release_cancel_token(0);
    CHECK(plaudon_apple_cancel_token_is_cancelled(0) == 0, "token 0 reports cancelled");
    plaudon_apple_cancel(UINT64_MAX);
    CHECK(plaudon_apple_cancel_token_is_cancelled(UINT64_MAX) == 0,
          "unallocated handle reports cancelled");

    CHECK(plaudon_apple_live_cancel_token_count() == baseline,
          "live token count did not return to baseline");

    /* Handles are never reused, so a stale handle cannot address a live
     * operation. */
    uint64_t first = plaudon_apple_new_cancel_token();
    plaudon_apple_release_cancel_token(first);
    uint64_t second = plaudon_apple_new_cancel_token();
    CHECK(second != first, "a released handle was reused");
    plaudon_apple_release_cancel_token(second);
}

static void test_cancel_before_start(void) {
    uint64_t token = plaudon_apple_new_cancel_token();
    plaudon_apple_cancel(token);

    char request[512];
    snprintf(request, sizeof(request),
             "{\"envelope_version\":1,\"op\":\"generate_text\",\"prompt\":\"hello\","
             "\"cancel_token\":%llu,\"timeout_ms\":30000}",
             (unsigned long long)token);

    relay_observer observer = {0};
    char *document = plaudon_apple_generate_text(request, on_progress, &observer);
    CHECK(document != NULL, "cancel-before-start returned NULL");
    CHECK(has(document, "cancelled"), "cancel-before-start did not report cancelled: %s", document);
    take(document);
    CHECK(atomic_load(&observer.deliveries) == 0,
          "a cancelled-before-start call delivered %d progress events",
          atomic_load(&observer.deliveries));
    plaudon_apple_release_cancel_token(token);
}

typedef struct {
    uint64_t token;
    atomic_int done;
} storm_arg;

static void *cancel_spinner(void *raw) {
    storm_arg *arg = (storm_arg *)raw;
    while (!atomic_load(&arg->done)) {
        plaudon_apple_cancel(arg->token);
    }
    return NULL;
}

/* A cancel storm against a live operation: many threads cancelling the same
 * token while the operation runs, then the relay must be silent. */
static void test_cancel_storm(const char *audio_path) {
    uint64_t baseline = plaudon_apple_live_cancel_token_count();
    const int rounds = 24;
    const int spinners = 6;

    for (int round = 0; round < rounds; round++) {
        uint64_t token = plaudon_apple_new_cancel_token();
        storm_arg arg = {.token = token};
        atomic_init(&arg.done, 0);
        pthread_t threads[8];
        for (int i = 0; i < spinners; i++) {
            pthread_create(&threads[i], NULL, cancel_spinner, &arg);
        }

        relay_observer observer = {0};
        char request[2048];
        if (audio_path != NULL) {
            snprintf(request, sizeof(request),
                     "{\"envelope_version\":1,\"op\":\"transcribe_file\",\"path\":\"%s\","
                     "\"locale\":\"en-US\",\"cancel_token\":%llu,\"timeout_ms\":20000}",
                     audio_path, (unsigned long long)token);
            take(plaudon_apple_transcribe_file(request, on_progress, &observer));
        } else {
            snprintf(request, sizeof(request),
                     "{\"envelope_version\":1,\"op\":\"generate_text\",\"prompt\":\"Summarize "
                     "this short meeting note.\",\"cancel_token\":%llu,\"timeout_ms\":20000}",
                     (unsigned long long)token);
            take(plaudon_apple_generate_text(request, on_progress, &observer));
        }

        /* Poison after the call returns: any further delivery is a bug. */
        atomic_store(&observer.poisoned, 1);
        atomic_store(&arg.done, 1);
        for (int i = 0; i < spinners; i++) {
            pthread_join(threads[i], NULL);
        }
        /* Keep cancelling after the call returned; the relay is disarmed. */
        plaudon_apple_cancel(token);
        usleep(20000);
        CHECK(atomic_load(&observer.after_return) == 0,
              "round %d: %d progress deliveries arrived after the call returned", round,
              atomic_load(&observer.after_return));
        plaudon_apple_release_cancel_token(token);
    }

    CHECK(plaudon_apple_live_cancel_token_count() == baseline,
          "cancel storm leaked tokens: %llu live, expected %llu",
          (unsigned long long)plaudon_apple_live_cancel_token_count(),
          (unsigned long long)baseline);
}

static void *token_churn(void *unused) {
    (void)unused;
    for (int i = 0; i < 20000; i++) {
        uint64_t token = plaudon_apple_new_cancel_token();
        if ((i & 1) == 0) {
            plaudon_apple_cancel(token);
        }
        plaudon_apple_release_cancel_token(token);
    }
    return NULL;
}

/* Thousands of allocate/release cycles across threads: the registry must not
 * grow and must not race. */
static void test_token_churn(void) {
    uint64_t baseline = plaudon_apple_live_cancel_token_count();
    pthread_t threads[8];
    for (int i = 0; i < 8; i++) {
        pthread_create(&threads[i], NULL, token_churn, NULL);
    }
    for (int i = 0; i < 8; i++) {
        pthread_join(threads[i], NULL);
    }
    CHECK(plaudon_apple_live_cancel_token_count() == baseline,
          "160000 allocate/release cycles leaked %llu tokens",
          (unsigned long long)(plaudon_apple_live_cancel_token_count() - baseline));
}

/* ------------------------------------------------------------------ */
/* 3. Probe churn and concurrency                                      */
/* ------------------------------------------------------------------ */

static void *probe_spinner(void *unused) {
    (void)unused;
    for (int i = 0; i < 40; i++) {
        char *speech =
            plaudon_apple_probe_speech("{\"envelope_version\":1,\"op\":\"probe_speech\","
                                       "\"locale\":\"en-US\"}");
        if (speech == NULL) {
            abort();
        }
        plaudon_apple_string_free(speech);
        char *text = plaudon_apple_probe_foundation_model(
            "{\"envelope_version\":1,\"op\":\"probe_foundation_model\"}");
        if (text == NULL) {
            abort();
        }
        plaudon_apple_string_free(text);
    }
    return NULL;
}

static void test_probe_churn(void) {
    pthread_t threads[6];
    for (int i = 0; i < 6; i++) {
        pthread_create(&threads[i], NULL, probe_spinner, NULL);
    }
    for (int i = 0; i < 6; i++) {
        pthread_join(threads[i], NULL);
    }
    CHECK(1, "probe churn completed");
}

/* ------------------------------------------------------------------ */
/* 4. Timeout containment                                              */
/* ------------------------------------------------------------------ */

/* A one-millisecond budget must return promptly with engine_timeout or an
 * earlier typed refusal, never hang. */
static void test_timeout_containment(const char *audio_path) {
    if (audio_path == NULL) {
        return;
    }
    char request[2048];
    snprintf(request, sizeof(request),
             "{\"envelope_version\":1,\"op\":\"transcribe_file\",\"path\":\"%s\","
             "\"locale\":\"en-US\",\"cancel_token\":0,\"timeout_ms\":1}",
             audio_path);
    char *document = plaudon_apple_transcribe_file(request, NULL, NULL);
    CHECK(document != NULL, "timeout test returned NULL");
    CHECK(has(document, "\"ok\":false"), "a 1 ms budget produced a success: %s", document);
    take(document);
}

/* ------------------------------------------------------------------ */

int main(int argc, char **argv) {
    const char *corpus = (argc > 1 && argv[1][0] != '\0') ? argv[1] : NULL;
    const char *audio = (argc > 2 && argv[2][0] != '\0') ? argv[2] : NULL;

    printf("plaudon apple bridge ABI stress\n");
    printf("  abi version: %d\n", plaudon_apple_bridge_abi_version());
    printf("  audio fixture: %s\n", audio ? audio : "(none, transcription paths skipped)");

    test_abi_version();
    test_malformed_requests();
    if (corpus != NULL) {
        test_corpus(corpus);
    }
    test_mutations();
    test_cancellation_lifecycle();
    test_cancel_before_start();
    test_token_churn();
    test_probe_churn();
    test_cancel_storm(audio);
    test_timeout_containment(audio);

    printf("%d checks, %d failures\n", checks, failures);
    return failures == 0 ? 0 : 1;
}
