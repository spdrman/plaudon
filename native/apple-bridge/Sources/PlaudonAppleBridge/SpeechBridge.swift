// SpeechAnalyzer / SpeechTranscriber lifecycle (spec 11.3, 11.6).
//
// Spec 11.6 names `SpeechAnalyzer` for speech tasks. This file drives it
// directly and never falls back to the older `SFSpeechRecognizer`, which
// requires a speech-recognition authorization grant that a non-bundled
// process can never obtain and whose unauthorized task stalls instead of
// failing (recorded in docs/spikes/A1.3-apple-availability.md).
//
// Three product rules are enforced here rather than upstream:
//
//   * A volatile result is never a transcript. Only finalized results are
//     collected, and a run that produced nothing final is `output_not_final`.
//   * Assets are never downloaded implicitly. Missing assets are reported as
//     `model_asset_missing` unless the caller states that consent was
//     obtained (spec 12).
//   * Offsets are reported only when the engine reported them, and the
//     alignment origin says which case happened (spec 8.6A).

import Foundation
import AVFoundation
import CoreMedia

#if canImport(Speech)
import Speech
#endif

#if canImport(Speech)

/// Attribute set we always request: times and confidence are what the
/// transcript representation in spec 8.5 needs.
@available(macOS 26.0, *)
private func makeTranscriber(locale: Locale) -> SpeechTranscriber {
    SpeechTranscriber(
        locale: locale,
        transcriptionOptions: [],
        reportingOptions: [],
        attributeOptions: [.audioTimeRange, .transcriptionConfidence]
    )
}

@available(macOS 26.0, *)
private func assetStatusName(_ status: AssetInventory.Status) -> String {
    switch status {
    case .unsupported: return "unsupported"
    case .downloading: return "downloading"
    case .supported: return "supported"
    case .installed: return "installed"
    @unknown default: return "unsupported"
    }
}

/// Whether `locale` appears in the platform's installed set.
///
/// Compared on the identifier so `en_US` matches regardless of how the
/// `Locale` was constructed.
@available(macOS 26.0, *)
private func isInstalled(_ locale: Locale, in installed: [Locale]) -> Bool {
    installed.contains { $0.identifier == locale.identifier }
}

/// Resolve a requested tag to a locale the platform actually supports.
@available(macOS 26.0, *)
private func resolveLocale(_ requested: String?) async throws -> Locale {
    let candidate = requested.map(Locale.init(identifier:)) ?? Locale.current
    guard let supported = await SpeechTranscriber.supportedLocale(equivalentTo: candidate) else {
        throw BridgeFailure(.localeUnsupported, candidate.identifier)
    }
    return supported
}

// MARK: - probe_speech

@available(macOS 26.0, *)
func probeSpeechImpl(requestedLocale: String?) async -> Result<[String: Any], BridgeFailure> {
    guard SpeechTranscriber.isAvailable else {
        return .failure(BridgeFailure(.platformUnsupported, "speech_transcriber_unavailable"))
    }

    let supported = await SpeechTranscriber.supportedLocales
    let installed = await SpeechTranscriber.installedLocales
    let reserved = await AssetInventory.reservedLocales

    var resolved: Locale?
    var assetStatus = "unsupported"
    var localeInstalled = false
    var formats: [[String: Any]] = []

    if let candidate = try? await resolveLocale(requestedLocale) {
        resolved = candidate
        let transcriber = makeTranscriber(locale: candidate)
        assetStatus = assetStatusName(await AssetInventory.status(forModules: [transcriber]))
        localeInstalled = isInstalled(candidate, in: installed)
        formats = await transcriber.availableCompatibleAudioFormats.map {
            ["sample_rate_hz": $0.sampleRate, "channels": Int($0.channelCount)]
        }
    }

    return .success([
        // Availability is the framework answer, not an OS-version inference
        // (spec 11.5). Locale readiness is reported separately so the caller
        // can tell "no Apple speech here" from "this language is not
        // installed yet".
        "available": true,
        "supported_locales": supported.map(\.identifier).sorted(),
        "installed_locales": installed.map(\.identifier).sorted(),
        "resolved_locale": resolved?.identifier ?? NSNull(),
        "asset_status": assetStatus,
        // Readiness is taken from `installedLocales`, not from
        // `AssetInventory.status`. Measured on macOS 27.0.1 (26A434): with
        // `en_US` present in `installedLocales` and transcription working,
        // `AssetInventory.status(forModules:)` returned `.supported` rather
        // than `.installed`, for every combination of attribute options and
        // for both the raw and the resolved locale. Gating on it would
        // refuse a locale the machine can actually transcribe.
        "locale_installed": localeInstalled,
        // SpeechTranscriber analyses on device; reported as an observation of
        // the API contract we use, with no network path in this file.
        "supports_on_device": true,
        "is_transcriber": true,
        "max_concurrent_locales": AssetInventory.maximumReservedLocales,
        "reserved_locales": reserved.map(\.identifier).sorted(),
        "accepted_formats": formats,
        // Word timings come from the audioTimeRange attribute, which we
        // always request; confirmed on a real response during H2.1.
        "supports_word_timestamps": true,
        "os_build": HostFacts.osBuild,
        "os_version": HostFacts.osVersion,
        "hardware": HostFacts.hardware,
    ])
}

// MARK: - install_speech_assets

@available(macOS 26.0, *)
func installSpeechAssetsImpl(
    requestedLocale: String?,
    relay: ProgressRelay
) async -> Result<[String: Any], BridgeFailure> {
    guard SpeechTranscriber.isAvailable else {
        return .failure(BridgeFailure(.platformUnsupported, "speech_transcriber_unavailable"))
    }
    let locale: Locale
    do {
        locale = try await resolveLocale(requestedLocale)
    } catch let failure as BridgeFailure {
        return .failure(failure)
    } catch {
        return .failure(BridgeFailure(.engineFailure, "locale_resolution_failed"))
    }

    let transcriber = makeTranscriber(locale: locale)
    if isInstalled(locale, in: await SpeechTranscriber.installedLocales) {
        return .success([
            "asset_status": "installed",
            "locale": locale.identifier,
            "installed_bytes": NSNull(),
        ])
    }
    let before = await AssetInventory.status(forModules: [transcriber])
    if before == .installed {
        return .success([
            "asset_status": "installed",
            "locale": locale.identifier,
            "installed_bytes": NSNull(),
        ])
    }
    if before == .unsupported {
        return .failure(BridgeFailure(.localeUnsupported, locale.identifier))
    }

    // Reserving keeps the installed assets from being purged under disk
    // pressure. Over the platform limit this throws, which is a concurrency
    // limit rather than a download failure.
    do {
        _ = try await AssetInventory.reserve(locale: locale)
    } catch {
        return .failure(BridgeFailure(.concurrencyLimit, "reserved_locale_limit"))
    }

    let request: AssetInstallationRequest?
    do {
        request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber])
    } catch {
        return .failure(BridgeFailure(.assetDownloadFailed, "installation_request_failed"))
    }
    guard let request else {
        // No request means nothing to install; re-read the status rather than
        // claiming success we did not observe.
        let after = await AssetInventory.status(forModules: [transcriber])
        guard after == .installed else {
            return .failure(BridgeFailure(.modelAssetMissing, locale.identifier))
        }
        return .success([
            "asset_status": "installed",
            "locale": locale.identifier,
            "installed_bytes": NSNull(),
        ])
    }

    let progress = request.progress
    relay.report(progressDocument(
        "install_speech_assets",
        phase: "asset_download",
        fraction: progress.fractionCompleted,
        completedUnits: Int(progress.completedUnitCount),
        totalUnits: Int(progress.totalUnitCount)
    ))

    let watcher = Task {
        while !Task.isCancelled {
            try? await Task.sleep(nanoseconds: 250_000_000)
            if Task.isCancelled { return }
            relay.report(progressDocument(
                "install_speech_assets",
                phase: "asset_download",
                fraction: progress.fractionCompleted,
                completedUnits: Int(progress.completedUnitCount),
                totalUnits: Int(progress.totalUnitCount)
            ))
        }
    }
    defer { watcher.cancel() }

    do {
        try await request.downloadAndInstall()
    } catch {
        if Task.isCancelled {
            return .failure(BridgeFailure(.cancelled, "asset_download"))
        }
        return .failure(BridgeFailure(.assetDownloadFailed, shortTypeName(error)))
    }

    let after = await AssetInventory.status(forModules: [transcriber])
    guard after == .installed else {
        return .failure(BridgeFailure(.assetDownloadFailed, assetStatusName(after)))
    }
    relay.report(progressDocument(
        "install_speech_assets",
        phase: "asset_download",
        fraction: 1.0,
        completedUnits: Int(progress.totalUnitCount),
        totalUnits: Int(progress.totalUnitCount)
    ))
    return .success([
        "asset_status": "installed",
        "locale": locale.identifier,
        "installed_bytes": NSNull(),
    ])
}

// MARK: - transcribe_file

/// One finalized transcriber result, flattened for encoding.
@available(macOS 26.0, *)
private struct FinalizedResult {
    let start: Double
    let end: Double
    let text: String
    let confidence: Double?
    let words: [[String: Any]]
}

@available(macOS 26.0, *)
func transcribeFileImpl(
    path: String,
    requestedLocale: String?,
    allowAssetDownload: Bool,
    wantWordTimestamps: Bool,
    relay: ProgressRelay
) async -> Result<[String: Any], BridgeFailure> {
    guard FileManager.default.isReadableFile(atPath: path) else {
        return .failure(BridgeFailure(.inputUnreadable, "not_readable"))
    }
    guard SpeechTranscriber.isAvailable else {
        return .failure(BridgeFailure(.platformUnsupported, "speech_transcriber_unavailable"))
    }

    let locale: Locale
    do {
        locale = try await resolveLocale(requestedLocale)
    } catch let failure as BridgeFailure {
        return .failure(failure)
    } catch {
        return .failure(BridgeFailure(.engineFailure, "locale_resolution_failed"))
    }

    let transcriber = makeTranscriber(locale: locale)
    // `installedLocales` is the authority on readiness; `AssetInventory`
    // status is consulted only when the locale is absent from it. See the
    // measurement note on `locale_installed` in `probeSpeechImpl`.
    if !isInstalled(locale, in: await SpeechTranscriber.installedLocales) {
        switch await AssetInventory.status(forModules: [transcriber]) {
        case .installed:
            break
        case .downloading:
            return .failure(BridgeFailure(.modelAssetNotReady, locale.identifier))
        case .unsupported:
            return .failure(BridgeFailure(.localeUnsupported, locale.identifier))
        case .supported:
            // Spec 12: nothing large is fetched without an explicit decision.
            guard allowAssetDownload else {
                return .failure(BridgeFailure(.modelAssetMissing, locale.identifier))
            }
            if case .failure(let failure) = await installSpeechAssetsImpl(
                requestedLocale: locale.identifier,
                relay: relay
            ) {
                return .failure(failure)
            }
        @unknown default:
            return .failure(BridgeFailure(.modelAssetMissing, locale.identifier))
        }
    }

    let file: AVAudioFile
    do {
        file = try AVAudioFile(forReading: URL(fileURLWithPath: path))
    } catch {
        return .failure(BridgeFailure(.unsupportedMedia, shortTypeName(error)))
    }
    let sampleRate = file.processingFormat.sampleRate
    let durationSeconds = sampleRate > 0 ? Double(file.length) / sampleRate : 0
    guard durationSeconds > 0 else {
        return .failure(BridgeFailure(.unsupportedMedia, "zero_length"))
    }

    let analyzer = SpeechAnalyzer(modules: [transcriber])

    // Collect only finalized results (spec 11.3). Volatile updates drive
    // progress and are then discarded.
    let collector = Task { () -> [FinalizedResult] in
        var finalized: [FinalizedResult] = []
        for try await result in transcriber.results {
            let end = result.range.end.seconds
            if end.isFinite, durationSeconds > 0 {
                relay.report(progressDocument(
                    "transcribe_file",
                    phase: "transcribe",
                    fraction: end / durationSeconds,
                    processedMilliseconds: Int((end * 1000).rounded())
                ))
            }
            guard result.isFinal else { continue }
            finalized.append(flatten(result, wantWords: wantWordTimestamps))
        }
        return finalized
    }

    do {
        _ = try await analyzer.analyzeSequence(from: file)
        try await analyzer.finalizeAndFinishThroughEndOfInput()
    } catch {
        collector.cancel()
        await analyzer.cancelAndFinishNow()
        if Task.isCancelled { return .failure(BridgeFailure(.cancelled, "analysis")) }
        return .failure(BridgeFailure(.engineFailure, shortTypeName(error)))
    }

    let finalized: [FinalizedResult]
    do {
        finalized = try await collector.value
    } catch {
        return .failure(BridgeFailure(.engineFailure, shortTypeName(error)))
    }

    guard !finalized.isEmpty else {
        // The analyzer completed without finalizing anything. Reporting an
        // empty transcript as a success would silently destroy the recording's
        // only chance at a real one.
        return .failure(BridgeFailure(.outputNotFinal, "no_finalized_results"))
    }

    var segments: [[String: Any]] = []
    var sawTimes = false
    for (index, result) in finalized.enumerated() {
        var segment: [String: Any] = ["seq_index": index, "text": result.text]
        if result.end > result.start {
            sawTimes = true
            segment["start_ms"] = Int((result.start * 1000).rounded())
            segment["end_ms"] = Int((result.end * 1000).rounded())
        } else {
            segment["start_ms"] = NSNull()
            segment["end_ms"] = NSNull()
        }
        segment["confidence"] = result.confidence ?? NSNull()
        if wantWordTimestamps && !result.words.isEmpty {
            segment["words"] = result.words
        }
        segments.append(segment)
    }

    let text = finalized.map(\.text).joined()
    relay.report(progressDocument(
        "transcribe_file",
        phase: "transcribe",
        fraction: 1.0,
        processedMilliseconds: Int((durationSeconds * 1000).rounded())
    ))

    return .success([
        "text": text,
        "segments": segments,
        // Spec 8.6A: a text-only result never gains a timeline.
        "alignment_origin": sawTimes ? "native" : "none",
        "locale": locale.identifier,
        "duration_ms": Int((durationSeconds * 1000).rounded()),
        "final": true,
    ])
}

@available(macOS 26.0, *)
private func flatten(_ result: SpeechTranscriber.Result, wantWords: Bool) -> FinalizedResult {
    var confidence: Double?
    var words: [[String: Any]] = []
    for run in result.text.runs {
        if confidence == nil, let value = run.transcriptionConfidence {
            confidence = value
        }
        guard wantWords, let range = run.audioTimeRange else { continue }
        let piece = String(result.text[run.range].characters)
        let trimmed = piece.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { continue }
        let start = range.start.seconds
        let end = range.end.seconds
        guard start.isFinite, end.isFinite, end >= start else { continue }
        words.append([
            "text": trimmed,
            "start_ms": Int((start * 1000).rounded()),
            "end_ms": Int((end * 1000).rounded()),
        ])
    }
    let start = result.range.start.seconds
    let end = result.range.end.seconds
    return FinalizedResult(
        start: start.isFinite ? start : 0,
        end: end.isFinite ? end : 0,
        text: String(result.text.characters),
        confidence: confidence,
        words: words
    )
}

#endif

/// A stable, non-localized discriminator for an unexpected platform error.
///
/// Spec 6.6 forbids leaking third-party exception text, which can contain
/// paths or user content; the type name is enough to triage.
func shortTypeName(_ error: Error) -> String {
    String(describing: type(of: error))
}
