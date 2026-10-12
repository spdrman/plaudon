// SystemLanguageModel lifecycle and guided generation (spec 11.4, 11.6).
//
// Two rules dominate this file:
//
//   * Spec 2.3: this is a text model. It never transcribes, and the probe
//     says so in a field the Rust side asserts on.
//   * Spec 11.4: a guardrail refusal is an error or a content-policy outcome,
//     never a successful summary. Every platform refusal path below lands on
//     `guardrail_violation` or `refusal`, and the success path is reachable
//     only with real generated content.

import Foundation

#if canImport(FoundationModels)
import FoundationModels
#endif

#if canImport(FoundationModels)

// MARK: - probe_foundation_model

@available(macOS 26.0, *)
func probeFoundationModelImpl(requestedLocale: String?) async -> Result<[String: Any], BridgeFailure> {
    let model = SystemLanguageModel.default
    switch model.availability {
    case .available:
        break
    case .unavailable(let reason):
        switch reason {
        case .deviceNotEligible:
            return .failure(BridgeFailure(.hardwareUnsupported, "device_not_eligible"))
        case .appleIntelligenceNotEnabled:
            return .failure(BridgeFailure(.disabledByUser, "apple_intelligence_not_enabled"))
        case .modelNotReady:
            return .failure(BridgeFailure(.modelAssetNotReady, "system_language_model"))
        @unknown default:
            return .failure(BridgeFailure(.engineFailure, "unknown_unavailability"))
        }
    @unknown default:
        return .failure(BridgeFailure(.engineFailure, "unknown_availability"))
    }

    let languages = model.supportedLanguages.map(\.maximalIdentifier).sorted()
    let supportsRequested: Any = requestedLocale
        .map { model.supportsLocale(Locale(identifier: $0)) } ?? NSNull()

    return .success([
        "available": true,
        // Spec 2.3, asserted by the Rust mapping.
        "is_transcriber": false,
        "supported_languages": languages,
        "supports_requested_locale": supportsRequested,
        "supports_structured_output": true,
        // The framework exposes no context window. It is revealed only inside
        // a context-overflow error, so the honest answer here is null and the
        // chunker uses a labelled conservative bound instead (spec 9.4).
        "max_context_tokens": NSNull(),
        "model_version": NSNull(),
        "os_build": HostFacts.osBuild,
        "os_version": HostFacts.osVersion,
        "hardware": HostFacts.hardware,
    ])
}

// MARK: - generate_text

@available(macOS 26.0, *)
func generateTextImpl(
    prompt: String,
    instructions: String?,
    schema: [String: Any]?,
    maxOutputTokens: Int?,
    temperature: Double?,
    relay: ProgressRelay
) async -> Result<[String: Any], BridgeFailure> {
    let model = SystemLanguageModel.default
    guard case .available = model.availability else {
        if case .unavailable(let reason) = model.availability {
            switch reason {
            case .deviceNotEligible:
                return .failure(BridgeFailure(.hardwareUnsupported, "device_not_eligible"))
            case .appleIntelligenceNotEnabled:
                return .failure(BridgeFailure(.disabledByUser, "apple_intelligence_not_enabled"))
            case .modelNotReady:
                return .failure(BridgeFailure(.modelAssetNotReady, "system_language_model"))
            @unknown default:
                break
            }
        }
        return .failure(BridgeFailure(.engineFailure, "model_unavailable"))
    }

    var generationSchema: GenerationSchema?
    if let schema {
        do {
            let root = try DynamicSchemaBuilder().build(schema, path: "Root")
            generationSchema = try GenerationSchema(root: root, dependencies: [])
        } catch let failure as BridgeFailure {
            return .failure(failure)
        } catch {
            return .failure(BridgeFailure(.schemaUnsupported, shortTypeName(error)))
        }
    }

    let options = GenerationOptions(
        temperature: temperature,
        maximumResponseTokens: maxOutputTokens
    )
    let session = LanguageModelSession(model: model, instructions: instructions)

    relay.report(progressDocument("generate_text", phase: "generate", fraction: 0.0))

    do {
        if let generationSchema {
            let response = try await session.respond(
                to: prompt,
                schema: generationSchema,
                includeSchemaInPrompt: true,
                options: options
            )
            relay.report(progressDocument("generate_text", phase: "generate", fraction: 1.0))
            return .success(generationResult(
                text: response.rawContent.jsonString,
                structured: true,
                usage: response.usageIfAvailable
            ))
        }
        let response = try await session.respond(to: prompt, options: options)
        relay.report(progressDocument("generate_text", phase: "generate", fraction: 1.0))
        return .success(generationResult(
            text: response.content,
            structured: false,
            usage: response.usageIfAvailable
        ))
    } catch {
        if Task.isCancelled {
            return .failure(BridgeFailure(.cancelled, "generation"))
        }
        return .failure(classify(error))
    }
}

@available(macOS 26.0, *)
private func generationResult(
    text: String,
    structured: Bool,
    usage: (input: Int, output: Int)?
) -> [String: Any] {
    [
        "text": text,
        "structured": structured,
        "model_version": NSNull(),
        "context_tokens": NSNull(),
        "input_tokens": usage.map { $0.input } ?? NSNull(),
        "output_tokens": usage.map { $0.output } ?? NSNull(),
    ]
}

@available(macOS 26.0, *)
extension LanguageModelSession.Response {
    /// Token usage, which the platform only reports from macOS 27.
    var usageIfAvailable: (input: Int, output: Int)? {
        if #available(macOS 27.0, *) {
            return (usage.input.totalTokenCount, usage.output.totalTokenCount)
        }
        return nil
    }
}

/// Map a platform error onto the closed bridge vocabulary.
///
/// Both error families are handled: `LanguageModelSession.GenerationError`
/// exists from macOS 26, and macOS 27 throws the newer `LanguageModelError`
/// for the same conditions. Catching only one of them would turn a guardrail
/// refusal into a generic failure on half the supported range.
@available(macOS 26.0, *)
private func classify(_ error: Error) -> BridgeFailure {
    if #available(macOS 27.0, *), let modern = error as? LanguageModelError {
        switch modern {
        case .guardrailViolation:
            return BridgeFailure(.guardrailViolation, "language_model_guardrail")
        case .refusal:
            return BridgeFailure(.refusal, "model_refused")
        case .contextSizeExceeded(let context):
            return BridgeFailure(.contextSizeExceeded, "\(context.contextSize)")
        case .rateLimited:
            return BridgeFailure(.rateLimited, "language_model_rate_limited")
        case .unsupportedLanguageOrLocale:
            return BridgeFailure(.unsupportedLanguage, "language_model_locale")
        case .unsupportedGenerationGuide:
            return BridgeFailure(.schemaUnsupported, "unsupported_generation_guide")
        case .unsupportedCapability:
            return BridgeFailure(.schemaUnsupported, "unsupported_capability")
        case .unsupportedTranscriptContent:
            return BridgeFailure(.invalidArgument, "unsupported_transcript_content")
        case .timeout:
            return BridgeFailure(.engineTimeout, "language_model_timeout")
        @unknown default:
            return BridgeFailure(.engineFailure, "language_model_error")
        }
    }
    if let legacy = error as? LanguageModelSession.GenerationError {
        switch legacy {
        case .guardrailViolation:
            return BridgeFailure(.guardrailViolation, "generation_guardrail")
        case .refusal:
            return BridgeFailure(.refusal, "model_refused")
        case .exceededContextWindowSize:
            return BridgeFailure(.contextSizeExceeded, "generation_context_window")
        case .rateLimited:
            return BridgeFailure(.rateLimited, "generation_rate_limited")
        case .concurrentRequests:
            return BridgeFailure(.concurrencyLimit, "concurrent_requests")
        case .assetsUnavailable:
            return BridgeFailure(.modelAssetNotReady, "generation_assets")
        case .unsupportedLanguageOrLocale:
            return BridgeFailure(.unsupportedLanguage, "generation_locale")
        case .unsupportedGuide:
            return BridgeFailure(.schemaUnsupported, "unsupported_guide")
        case .decodingFailure:
            return BridgeFailure(.engineFailure, "decoding_failure")
        @unknown default:
            return BridgeFailure(.engineFailure, "generation_error")
        }
    }
    if error is CancellationError {
        return BridgeFailure(.cancelled, "generation")
    }
    return BridgeFailure(.engineFailure, shortTypeName(error))
}

// MARK: - JSON Schema to DynamicGenerationSchema

/// Translates the JSON Schema subset the application uses into the platform's
/// guided-generation schema.
///
/// Anything outside the subset is refused with `schema_unsupported` rather
/// than silently dropped: a guided generation against a schema the model did
/// not actually receive would produce output the Rust validator then rejects,
/// wasting a model run and confusing the failure.
@available(macOS 26.0, *)
private struct DynamicSchemaBuilder {
    /// Platform schema names must be unique within one generation; the JSON
    /// path supplies that uniqueness.
    func build(_ node: [String: Any], path: String) throws -> DynamicGenerationSchema {
        let description = node["description"] as? String

        if let choices = node["enum"] as? [Any] {
            let values = choices.compactMap { $0 as? String }
            guard values.count == choices.count, !values.isEmpty else {
                throw BridgeFailure(.schemaUnsupported, "non_string_enum")
            }
            return DynamicGenerationSchema(name: path, description: description, anyOf: values)
        }

        guard let type = node["type"] as? String else {
            throw BridgeFailure(.schemaUnsupported, "missing_type_at_\(path)")
        }

        switch type {
        case "object":
            let properties = node["properties"] as? [String: Any] ?? [:]
            guard !properties.isEmpty else {
                throw BridgeFailure(.schemaUnsupported, "object_without_properties_at_\(path)")
            }
            let required = Set((node["required"] as? [Any] ?? []).compactMap { $0 as? String })
            // Sorted so a schema always produces the same platform structure
            // and therefore reproducible generations.
            var members: [DynamicGenerationSchema.Property] = []
            for key in properties.keys.sorted() {
                guard let child = properties[key] as? [String: Any] else {
                    throw BridgeFailure(.schemaUnsupported, "property_not_object_at_\(path).\(key)")
                }
                members.append(DynamicGenerationSchema.Property(
                    name: key,
                    description: child["description"] as? String,
                    schema: try build(child, path: "\(path)_\(key)"),
                    isOptional: !required.contains(key)
                ))
            }
            return DynamicGenerationSchema(name: path, description: description, properties: members)

        case "array":
            guard let items = node["items"] as? [String: Any] else {
                throw BridgeFailure(.schemaUnsupported, "array_without_items_at_\(path)")
            }
            return DynamicGenerationSchema(
                arrayOf: try build(items, path: "\(path)_item"),
                minimumElements: node["minItems"] as? Int,
                maximumElements: node["maxItems"] as? Int
            )

        case "string":
            return DynamicGenerationSchema(type: String.self)
        case "integer":
            return DynamicGenerationSchema(type: Int.self)
        case "number":
            return DynamicGenerationSchema(type: Double.self)
        case "boolean":
            return DynamicGenerationSchema(type: Bool.self)
        default:
            throw BridgeFailure(.schemaUnsupported, "type_\(type)_at_\(path)")
        }
    }
}

#endif
