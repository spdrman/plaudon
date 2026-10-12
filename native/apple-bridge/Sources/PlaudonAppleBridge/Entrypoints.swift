// The C ABI entry points (spec 11.1, 11.2).
//
// `include/plaudon_apple_bridge.h` is the normative contract; this file is its
// only implementation. Scope is the four operations spec 11.1 names, plus the
// asset installation that spec 11.3's "let the user authorize required
// downloads, show progress" requires, plus cancellation, progress and the
// diagnostics the stress suite needs. No application logic lives here: every
// function parses an envelope, calls one platform API and encodes the answer.
//
// macOS older than the Speech/FoundationModels availability floor, and any
// build where the frameworks are absent, reaches the `platform_unsupported`
// tail of each function. The frameworks are weak-linked (see build.rs), so
// such a host loads the binary and gets a typed refusal rather than a dyld
// failure.

import Foundation

// MARK: - ABI and memory

@_cdecl("plaudon_apple_bridge_abi_version")
public func plaudon_apple_bridge_abi_version() -> Int32 { bridgeABIVersion }

@_cdecl("plaudon_apple_string_free")
public func plaudon_apple_string_free(_ value: UnsafeMutablePointer<CChar>?) {
    guard let value else { return }
    free(value)
}

/// Copy a Swift string into a C buffer the caller releases with
/// `plaudon_apple_string_free`.
///
/// `malloc` rather than `UnsafeMutablePointer.allocate` so the buffer is
/// freed by `free`, matching the documented release path and keeping the
/// allocation visible to leak checkers.
private func exportString(_ value: String) -> UnsafeMutablePointer<CChar> {
    let utf8 = Array(value.utf8CString)
    guard let buffer = malloc(utf8.count)?.assumingMemoryBound(to: CChar.self) else {
        // Header rule 1.5: returning NULL would be indistinguishable from a
        // lost error, and there is nothing useful left to do.
        fatalError("plaudon apple bridge: out of memory exporting a \(utf8.count) byte document")
    }
    buffer.update(from: utf8, count: utf8.count)
    return buffer
}

// MARK: - Cancellation

@_cdecl("plaudon_apple_new_cancel_token")
public func plaudon_apple_new_cancel_token() -> UInt64 {
    CancellationRegistry.shared.allocate()
}

@_cdecl("plaudon_apple_cancel")
public func plaudon_apple_cancel(_ token: UInt64) {
    CancellationRegistry.shared.cancel(token)
}

@_cdecl("plaudon_apple_release_cancel_token")
public func plaudon_apple_release_cancel_token(_ token: UInt64) {
    CancellationRegistry.shared.release(token)
}

@_cdecl("plaudon_apple_cancel_token_is_cancelled")
public func plaudon_apple_cancel_token_is_cancelled(_ token: UInt64) -> Int32 {
    CancellationRegistry.shared.isCancelled(token) ? 1 : 0
}

@_cdecl("plaudon_apple_live_cancel_token_count")
public func plaudon_apple_live_cancel_token_count() -> UInt64 {
    CancellationRegistry.shared.liveCount()
}

// MARK: - Probes

/// Budget for an availability probe.
///
/// Spec 11.3 treats availability as something the caller asks before every
/// decision, so it has to be cheap. A probe that cannot answer inside this
/// window is reported as a timeout, which the capability mapping reads as
/// "not usable", and the application proceeds with whisper.cpp.
private let probeBudgetMilliseconds: UInt64 = 2_000

@_cdecl("plaudon_apple_probe_speech")
public func plaudon_apple_probe_speech(
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar> {
    let op = "probe_speech"
    let locale: String?
    do {
        let request = try BridgeRequest.decode(requestJSON.map { String(cString: $0) }, expecting: op)
        locale = try request.optionalString("locale")
    } catch let failure as BridgeFailure {
        return exportString(failureDocument(op, failure))
    } catch {
        return exportString(failureDocument(op, BridgeFailure(.invalidRequest)))
    }

    #if canImport(Speech)
    guard #available(macOS 26.0, *) else {
        return exportString(failureDocument(
            op,
            BridgeFailure(.platformUnsupported, "speech_requires_macos_26")
        ))
    }
    let captured = locale
    return exportString(runProbe(op: op, timeoutMilliseconds: probeBudgetMilliseconds) {
        await probeSpeechImpl(requestedLocale: captured)
    })
    #else
    return exportString(failureDocument(
        op,
        BridgeFailure(.platformUnsupported, "speech_framework_absent")
    ))
    #endif
}

@_cdecl("plaudon_apple_probe_foundation_model")
public func plaudon_apple_probe_foundation_model(
    _ requestJSON: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar> {
    let op = "probe_foundation_model"
    let locale: String?
    do {
        let request = try BridgeRequest.decode(requestJSON.map { String(cString: $0) }, expecting: op)
        locale = try request.optionalString("locale")
    } catch let failure as BridgeFailure {
        return exportString(failureDocument(op, failure))
    } catch {
        return exportString(failureDocument(op, BridgeFailure(.invalidRequest)))
    }

    #if canImport(FoundationModels)
    guard #available(macOS 26.0, *) else {
        return exportString(failureDocument(
            op,
            BridgeFailure(.platformUnsupported, "foundation_models_requires_macos_26")
        ))
    }
    let captured = locale
    return exportString(runProbe(op: op, timeoutMilliseconds: probeBudgetMilliseconds) {
        await probeFoundationModelImpl(requestedLocale: captured)
    })
    #else
    return exportString(failureDocument(
        op,
        BridgeFailure(.platformUnsupported, "foundation_models_framework_absent")
    ))
    #endif
}

// MARK: - Speech assets

@_cdecl("plaudon_apple_install_speech_assets")
public func plaudon_apple_install_speech_assets(
    _ requestJSON: UnsafePointer<CChar>?,
    _ progress: PlaudonProgressCallback?,
    _ context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<CChar> {
    let op = "install_speech_assets"
    let locale: String?
    let cancelToken: UInt64
    let timeout: UInt64
    do {
        let request = try BridgeRequest.decode(requestJSON.map { String(cString: $0) }, expecting: op)
        locale = try request.optionalString("locale")
        cancelToken = try request.optionalUInt64("cancel_token", default: 0)
        timeout = try request.optionalUInt64("timeout_ms", default: 0)
    } catch let failure as BridgeFailure {
        return exportString(failureDocument(op, failure))
    } catch {
        return exportString(failureDocument(op, BridgeFailure(.invalidRequest)))
    }

    #if canImport(Speech)
    guard #available(macOS 26.0, *) else {
        return exportString(failureDocument(
            op,
            BridgeFailure(.platformUnsupported, "speech_requires_macos_26")
        ))
    }
    let captured = locale
    let relay = ProgressRelay(callback: progress, context: context)
    return exportString(runBounded(
        op: op,
        cancelToken: cancelToken,
        timeoutMilliseconds: timeout,
        relay: relay
    ) { relay in
        await installSpeechAssetsImpl(requestedLocale: captured, relay: relay)
    })
    #else
    return exportString(failureDocument(
        op,
        BridgeFailure(.platformUnsupported, "speech_framework_absent")
    ))
    #endif
}

// MARK: - Transcription

@_cdecl("plaudon_apple_transcribe_file")
public func plaudon_apple_transcribe_file(
    _ requestJSON: UnsafePointer<CChar>?,
    _ progress: PlaudonProgressCallback?,
    _ context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<CChar> {
    let op = "transcribe_file"
    let path: String
    let locale: String?
    let cancelToken: UInt64
    let timeout: UInt64
    let allowDownload: Bool
    let wantWords: Bool
    do {
        let request = try BridgeRequest.decode(requestJSON.map { String(cString: $0) }, expecting: op)
        path = try request.requiredString("path")
        locale = try request.optionalString("locale")
        cancelToken = try request.optionalUInt64("cancel_token", default: 0)
        timeout = try request.optionalUInt64("timeout_ms", default: 0)
        allowDownload = try request.optionalBool("allow_asset_download", default: false)
        wantWords = try request.optionalBool("want_word_timestamps", default: false)
    } catch let failure as BridgeFailure {
        return exportString(failureDocument(op, failure))
    } catch {
        return exportString(failureDocument(op, BridgeFailure(.invalidRequest)))
    }

    #if canImport(Speech)
    guard #available(macOS 26.0, *) else {
        return exportString(failureDocument(
            op,
            BridgeFailure(.platformUnsupported, "speech_requires_macos_26")
        ))
    }
    let capturedPath = path
    let capturedLocale = locale
    let relay = ProgressRelay(callback: progress, context: context)
    return exportString(runBounded(
        op: op,
        cancelToken: cancelToken,
        timeoutMilliseconds: timeout,
        relay: relay
    ) { relay in
        await transcribeFileImpl(
            path: capturedPath,
            requestedLocale: capturedLocale,
            allowAssetDownload: allowDownload,
            wantWordTimestamps: wantWords,
            relay: relay
        )
    })
    #else
    return exportString(failureDocument(
        op,
        BridgeFailure(.platformUnsupported, "speech_framework_absent")
    ))
    #endif
}

// MARK: - Generation

@_cdecl("plaudon_apple_generate_text")
public func plaudon_apple_generate_text(
    _ requestJSON: UnsafePointer<CChar>?,
    _ progress: PlaudonProgressCallback?,
    _ context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<CChar> {
    let op = "generate_text"
    let prompt: String
    let instructions: String?
    let schema: [String: Any]?
    let cancelToken: UInt64
    let timeout: UInt64
    let maxOutputTokens: Int?
    let temperature: Double?
    do {
        let request = try BridgeRequest.decode(requestJSON.map { String(cString: $0) }, expecting: op)
        prompt = try request.requiredString("prompt")
        instructions = try request.optionalString("instructions")
        schema = try request.optionalObject("schema")
        cancelToken = try request.optionalUInt64("cancel_token", default: 0)
        timeout = try request.optionalUInt64("timeout_ms", default: 0)
        maxOutputTokens = try request.optionalInt("max_output_tokens", minimum: 1)
        temperature = try request.optionalDouble("temperature", minimum: 0.0, maximum: 2.0)
    } catch let failure as BridgeFailure {
        return exportString(failureDocument(op, failure))
    } catch {
        return exportString(failureDocument(op, BridgeFailure(.invalidRequest)))
    }

    #if canImport(FoundationModels)
    guard #available(macOS 26.0, *) else {
        return exportString(failureDocument(
            op,
            BridgeFailure(.platformUnsupported, "foundation_models_requires_macos_26")
        ))
    }
    let capturedPrompt = prompt
    let capturedInstructions = instructions
    // `[String: Any]` is not `Sendable`; the schema is immutable for the
    // duration of the call and is only read on the generation task, so it is
    // forwarded through a Sendable box rather than captured directly.
    let capturedSchema = UncheckedBox(schema)
    let relay = ProgressRelay(callback: progress, context: context)
    return exportString(runBounded(
        op: op,
        cancelToken: cancelToken,
        timeoutMilliseconds: timeout,
        relay: relay
    ) { relay in
        await generateTextImpl(
            prompt: capturedPrompt,
            instructions: capturedInstructions,
            schema: capturedSchema.value,
            maxOutputTokens: maxOutputTokens,
            temperature: temperature,
            relay: relay
        )
    })
    #else
    return exportString(failureDocument(
        op,
        BridgeFailure(.platformUnsupported, "foundation_models_framework_absent")
    ))
    #endif
}

/// Carries an immutable, never-mutated value across a concurrency boundary.
private struct UncheckedBox<T>: @unchecked Sendable {
    let value: T
    init(_ value: T) { self.value = value }
}
