// The macOS-only Apple speech and language bridge (spec 11.1-11.4).
//
// Scope is fixed at four operations plus cancellation and progress:
// probeSpeech, transcribeFile, probeFoundationModel, generateText. Nothing
// else crosses the boundary, and no application logic is duplicated here.
//
// Every entry point returns a JSON document so that success, unavailability
// and guardrail refusal all have one representation on the Rust side. A
// refusal is an error document, never generated text (spec 11.4), and a
// cancelled transcription returns an error rather than a partial result
// (spec 11.3).

import Foundation

#if canImport(Speech)
import Speech
#endif

#if canImport(FoundationModels)
import FoundationModels
#endif

// MARK: - ABI

@_cdecl("plaudon_apple_bridge_abi_version")
public func plaudon_apple_bridge_abi_version() -> Int32 { 1 }

@_cdecl("plaudon_apple_string_free")
public func plaudon_apple_string_free(_ value: UnsafeMutablePointer<CChar>?) {
    guard let value else { return }
    free(value)
}

/// Copies a Swift string into a C string the caller must free with
/// `plaudon_apple_string_free`.
private func exportString(_ value: String) -> UnsafeMutablePointer<CChar> {
    let utf8 = Array(value.utf8CString)
    let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: utf8.count)
    buffer.update(from: utf8, count: utf8.count)
    return buffer
}

private func exportJSON(_ object: [String: Any]) -> UnsafeMutablePointer<CChar> {
    guard
        let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]),
        let text = String(data: data, encoding: .utf8)
    else {
        return exportString(#"{"error":"serialization_failed"}"#)
    }
    return exportString(text)
}

/// Wall-clock budget applied when the caller passes 0.
///
/// A platform recognizer can stall without ever delivering a final result,
/// so every long-running call is bounded. Observed during the A1.3 spike:
/// `SFSpeechRecognizer` never finalizes on audio containing no speech.
private let defaultTimeoutMilliseconds: UInt64 = 10 * 60 * 1000

/// Wait for `semaphore`, polling for cancellation and a wall-clock deadline.
///
/// Returns `nil` on completion, or an error document describing why the wait
/// ended.
private func waitBounded(
    _ semaphore: DispatchSemaphore,
    cancelToken: UInt64,
    timeoutMilliseconds: UInt64,
    onCancel: () -> Void
) -> [String: Any]? {
    let budget = timeoutMilliseconds == 0 ? defaultTimeoutMilliseconds : timeoutMilliseconds
    let deadline = DispatchTime.now() + .milliseconds(Int(min(budget, UInt64(Int.max))))
    while semaphore.wait(timeout: .now() + 0.1) == .timedOut {
        if CancellationRegistry.shared.isCancelled(cancelToken) {
            onCancel()
            return errorDocument("cancelled")
        }
        if DispatchTime.now() >= deadline {
            onCancel()
            return errorDocument("engine_timeout", detail: "\(budget)ms")
        }
    }
    return nil
}

private func errorDocument(_ reason: String, detail: String? = nil) -> [String: Any] {
    var document: [String: Any] = ["ok": false, "error": reason]
    if let detail { document["detail"] = detail }
    return document
}

// MARK: - Cancellation

private final class CancellationRegistry: @unchecked Sendable {
    static let shared = CancellationRegistry()
    private let lock = NSLock()
    private var next: UInt64 = 1
    private var cancelled: Set<UInt64> = []
    private var live: Set<UInt64> = []

    func allocate() -> UInt64 {
        lock.lock(); defer { lock.unlock() }
        let token = next
        next &+= 1
        live.insert(token)
        return token
    }

    func cancel(_ token: UInt64) {
        lock.lock(); defer { lock.unlock() }
        if live.contains(token) { cancelled.insert(token) }
    }

    func isCancelled(_ token: UInt64) -> Bool {
        guard token != 0 else { return false }
        lock.lock(); defer { lock.unlock() }
        return cancelled.contains(token)
    }

    func release(_ token: UInt64) {
        lock.lock(); defer { lock.unlock() }
        live.remove(token)
        cancelled.remove(token)
    }
}

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

// MARK: - Speech availability

/// Reports why speech recognition is not authorized for this process, or
/// `nil` when it is.
///
/// `SFSpeechRecognizer` requires an authorization grant that is only
/// obtainable from an application bundle carrying
/// `NSSpeechRecognitionUsageDescription`. A plain executable is therefore
/// permanently `notDetermined`, and an unauthorized recognition task stalls
/// instead of failing, which is why this check exists.
@available(macOS 26.0, *)
private func speechAuthorizationDenial() -> [String: Any]? {
    #if canImport(Speech)
    switch SFSpeechRecognizer.authorizationStatus() {
    case .authorized:
        return nil
    case .denied:
        return errorDocument("permission_denied", detail: "speech_recognition_denied")
    case .restricted:
        return errorDocument("permission_denied", detail: "speech_recognition_restricted")
    case .notDetermined:
        return errorDocument(
            "permission_denied",
            detail: "speech_recognition_not_authorized_for_this_process"
        )
    @unknown default:
        return errorDocument("probe_failed", detail: "unknown_authorization_status")
    }
    #else
    return errorDocument("platform_unsupported")
    #endif
}

@_cdecl("plaudon_apple_probe_speech")
public func plaudon_apple_probe_speech(
    _ locale: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar> {
    let requested = locale.map { String(cString: $0) }

    #if canImport(Speech)
    guard #available(macOS 26.0, *) else {
        return exportJSON(errorDocument(
            "platform_unsupported",
            detail: "SpeechTranscriber requires a newer macOS than this host runs"
        ))
    }

    // Availability is a runtime question, never an OS-version inference
    // (spec 11.5): ask for the installed locale set and answer from that.
    let supported = SFSpeechRecognizer.supportedLocales().map(\.identifier).sorted()
    guard let recognizerLocale = requested.map(Locale.init(identifier:)) ?? Locale.current as Locale? else {
        return exportJSON(errorDocument("locale_unsupported"))
    }
    guard let recognizer = SFSpeechRecognizer(locale: recognizerLocale) else {
        return exportJSON(errorDocument("locale_unsupported", detail: recognizerLocale.identifier))
    }
    guard recognizer.isAvailable else {
        return exportJSON(errorDocument("model_asset_not_ready", detail: recognizerLocale.identifier))
    }
    // Authorization is a separate gate from availability. An unauthorized
    // process does not get an error from the recognizer; its task simply
    // never finalizes, so we must refuse up front (spec 11.5).
    if let denial = speechAuthorizationDenial() {
        return exportJSON(denial)
    }

    return exportJSON([
        "ok": true,
        "available": true,
        "locales": supported,
        "supports_on_device": recognizer.supportsOnDeviceRecognition,
        // Timestamp availability is reported as observed on a real result,
        // never asserted from documentation (spec 8.6A).
        "timestamps": "not_observed",
        "requested_locale": recognizerLocale.identifier,
    ])
    #else
    return exportJSON(errorDocument(
        "platform_unsupported",
        detail: "the Speech framework is not available in this build"
    ))
    #endif
}

// MARK: - Transcription

@_cdecl("plaudon_apple_transcribe_file")
public func plaudon_apple_transcribe_file(
    _ path: UnsafePointer<CChar>?,
    _ locale: UnsafePointer<CChar>?,
    _ cancelToken: UInt64,
    _ timeoutMilliseconds: UInt64,
    _ progress: (@convention(c) (UnsafeMutableRawPointer?, Double) -> Void)?,
    _ context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<CChar> {
    guard let path else {
        return exportJSON(errorDocument("invalid_argument", detail: "path is null"))
    }
    let filePath = String(cString: path)
    guard FileManager.default.fileExists(atPath: filePath) else {
        return exportJSON(errorDocument("input_unreadable"))
    }
    if CancellationRegistry.shared.isCancelled(cancelToken) {
        return exportJSON(errorDocument("cancelled"))
    }

    #if canImport(Speech)
    guard #available(macOS 26.0, *) else {
        return exportJSON(errorDocument("platform_unsupported"))
    }
    let requested = locale.map { Locale(identifier: String(cString: $0)) } ?? Locale.current
    guard let recognizer = SFSpeechRecognizer(locale: requested), recognizer.isAvailable else {
        return exportJSON(errorDocument("model_asset_not_ready", detail: requested.identifier))
    }
    if let denial = speechAuthorizationDenial() {
        return exportJSON(denial)
    }

    let request = SFSpeechURLRecognitionRequest(url: URL(fileURLWithPath: filePath))
    request.requiresOnDeviceRecognition = true
    request.shouldReportPartialResults = false

    let semaphore = DispatchSemaphore(value: 0)
    var document: [String: Any] = errorDocument("engine_failure")

    let task = recognizer.recognitionTask(with: request) { result, error in
        if let error {
            document = errorDocument("engine_failure", detail: error.localizedDescription)
            semaphore.signal()
            return
        }
        guard let result else { return }
        // Spec 11.3: a volatile, non-final update is never a transcript.
        guard result.isFinal else {
            progress?(context, 0.5)
            return
        }

        var segments: [[String: Any]] = []
        var sawTimes = false
        for (index, segment) in result.bestTranscription.segments.enumerated() {
            var encoded: [String: Any] = [
                "seq_index": index,
                "text": segment.substring,
                "confidence": segment.confidence,
            ]
            if segment.duration > 0 {
                sawTimes = true
                encoded["start_ms"] = Int(segment.timestamp * 1000.0)
                encoded["end_ms"] = Int((segment.timestamp + segment.duration) * 1000.0)
            }
            segments.append(encoded)
        }
        document = [
            "ok": true,
            "text": result.bestTranscription.formattedString,
            "segments": segments,
            // Declared from what was observed in this response, so a
            // text-only result can never gain a timeline (spec 8.6A).
            "alignment_origin": sawTimes ? "native" : "none",
            "locale": requested.identifier,
        ]
        semaphore.signal()
    }

    if let interrupted = waitBounded(
        semaphore,
        cancelToken: cancelToken,
        timeoutMilliseconds: timeoutMilliseconds,
        onCancel: { task.cancel() }
    ) {
        return exportJSON(interrupted)
    }
    return exportJSON(document)
    #else
    return exportJSON(errorDocument("platform_unsupported"))
    #endif
}

// MARK: - Foundation Models

@_cdecl("plaudon_apple_probe_foundation_model")
public func plaudon_apple_probe_foundation_model() -> UnsafeMutablePointer<CChar> {
    #if canImport(FoundationModels)
    guard #available(macOS 26.0, *) else {
        return exportJSON(errorDocument("platform_unsupported"))
    }
    let model = SystemLanguageModel.default
    switch model.availability {
    case .available:
        return exportJSON([
            "ok": true,
            "available": true,
            // Spec 2.3: this is a text model, not a transcriber.
            "is_transcriber": false,
        ])
    case .unavailable(let reason):
        switch reason {
        case .deviceNotEligible:
            return exportJSON(errorDocument("hardware_unsupported"))
        case .appleIntelligenceNotEnabled:
            return exportJSON(errorDocument("disabled_by_user"))
        case .modelNotReady:
            return exportJSON(errorDocument("model_asset_not_ready"))
        @unknown default:
            return exportJSON(errorDocument("probe_failed", detail: "unknown_unavailability"))
        }
    @unknown default:
        return exportJSON(errorDocument("probe_failed", detail: "unknown_availability"))
    }
    #else
    return exportJSON(errorDocument(
        "platform_unsupported",
        detail: "FoundationModels is not available in this build"
    ))
    #endif
}

@_cdecl("plaudon_apple_generate_text")
public func plaudon_apple_generate_text(
    _ prompt: UnsafePointer<CChar>?,
    _ cancelToken: UInt64,
    _ timeoutMilliseconds: UInt64
) -> UnsafeMutablePointer<CChar> {
    guard let prompt else {
        return exportJSON(errorDocument("invalid_argument", detail: "prompt is null"))
    }
    let text = String(cString: prompt)
    if CancellationRegistry.shared.isCancelled(cancelToken) {
        return exportJSON(errorDocument("cancelled"))
    }

    #if canImport(FoundationModels)
    guard #available(macOS 26.0, *) else {
        return exportJSON(errorDocument("platform_unsupported"))
    }
    let model = SystemLanguageModel.default
    guard case .available = model.availability else {
        return exportJSON(errorDocument("model_unavailable"))
    }

    let semaphore = DispatchSemaphore(value: 0)
    var document: [String: Any] = errorDocument("engine_failure")
    Task {
        do {
            let session = LanguageModelSession(model: model)
            let response = try await session.respond(to: text)
            document = ["ok": true, "text": response.content]
        } catch {
            // Spec 11.4: a guardrail refusal is an error, never a success
            // with empty or apologetic text.
            document = errorDocument("guardrail_refusal", detail: "\(type(of: error))")
        }
        semaphore.signal()
    }
    if let interrupted = waitBounded(
        semaphore,
        cancelToken: cancelToken,
        timeoutMilliseconds: timeoutMilliseconds,
        onCancel: {}
    ) {
        return exportJSON(interrupted)
    }
    return exportJSON(document)
    #else
    return exportJSON(errorDocument(
        "platform_unsupported",
        detail: "FoundationModels is not available in this build"
    ))
    #endif
}
