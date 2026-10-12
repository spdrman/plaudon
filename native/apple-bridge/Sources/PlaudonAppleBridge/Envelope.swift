// Serialized request/response/progress envelopes (spec 6.6, 11.2).
//
// The C ABI carries exactly three document shapes, all described by
// `schemas/apple-bridge-envelope.v1.schema.json` and by the ownership rules in
// `include/plaudon_apple_bridge.h`. Nothing else crosses the boundary: no
// object pointers, no out-parameters, no errno conventions.

import Foundation

// MARK: - Versions

/// ABI version of this library. Must equal `PLAUDON_APPLE_BRIDGE_ABI_VERSION`.
let bridgeABIVersion: Int32 = 2

/// Envelope contract version this library implements.
let bridgeEnvelopeVersion: Int = 1

// MARK: - Error vocabulary

/// The closed error vocabulary. Adding a case is an ABI change.
///
/// The Rust side decodes any value it does not recognize as `engineFailure`,
/// so a newer library can never make an older caller believe in availability
/// it cannot verify.
enum BridgeErrorCode: String {
    case invalidRequest = "invalid_request"
    case unsupportedEnvelopeVersion = "unsupported_envelope_version"
    case invalidArgument = "invalid_argument"
    case platformUnsupported = "platform_unsupported"
    case hardwareUnsupported = "hardware_unsupported"
    case disabledByUser = "disabled_by_user"
    case permissionDenied = "permission_denied"
    case localeUnsupported = "locale_unsupported"
    case modelAssetMissing = "model_asset_missing"
    case modelAssetNotReady = "model_asset_not_ready"
    case assetDownloadFailed = "asset_download_failed"
    case concurrencyLimit = "concurrency_limit"
    case inputUnreadable = "input_unreadable"
    case unsupportedMedia = "unsupported_media"
    case cancelled = "cancelled"
    case engineTimeout = "engine_timeout"
    case guardrailViolation = "guardrail_violation"
    case refusal = "refusal"
    case contextSizeExceeded = "context_size_exceeded"
    case rateLimited = "rate_limited"
    case unsupportedLanguage = "unsupported_language"
    case schemaUnsupported = "schema_unsupported"
    case outputNotFinal = "output_not_final"
    case engineFailure = "engine_failure"

    /// Whether retrying the identical request could plausibly succeed.
    ///
    /// This is advice for the job engine, not a promise. A hardware or
    /// platform verdict never becomes retriable by waiting.
    var isRetriable: Bool {
        switch self {
        case .modelAssetNotReady, .assetDownloadFailed, .concurrencyLimit,
             .engineTimeout, .rateLimited, .engineFailure:
            return true
        default:
            return false
        }
    }
}

/// A failure carried inside a response envelope.
struct BridgeFailure: Error {
    let code: BridgeErrorCode
    let detail: String?

    init(_ code: BridgeErrorCode, _ detail: String? = nil) {
        self.code = code
        self.detail = detail
    }
}

// MARK: - Requests

/// A decoded request envelope.
///
/// Decoding is permissive about unknown fields and strict about the ones it
/// uses: an unusable value is `invalid_argument`, never a silent default.
struct BridgeRequest {
    let op: String
    private let body: [String: Any]

    /// Parse and validate the envelope frame.
    static func decode(_ json: String?, expecting op: String) throws -> BridgeRequest {
        guard let json else {
            throw BridgeFailure(.invalidRequest, "null_request")
        }
        guard let data = json.data(using: .utf8) else {
            throw BridgeFailure(.invalidRequest, "request_not_utf8")
        }
        let parsed: Any
        do {
            parsed = try JSONSerialization.jsonObject(with: data, options: [])
        } catch {
            throw BridgeFailure(.invalidRequest, "request_not_json")
        }
        guard let body = parsed as? [String: Any] else {
            throw BridgeFailure(.invalidRequest, "request_not_object")
        }
        guard let version = body["envelope_version"] as? Int else {
            throw BridgeFailure(.invalidRequest, "missing_envelope_version")
        }
        guard version == bridgeEnvelopeVersion else {
            throw BridgeFailure(.unsupportedEnvelopeVersion, "\(version)")
        }
        guard let declaredOp = body["op"] as? String else {
            throw BridgeFailure(.invalidRequest, "missing_op")
        }
        guard declaredOp == op else {
            throw BridgeFailure(.invalidRequest, "op_mismatch")
        }
        return BridgeRequest(op: op, body: body)
    }

    /// A required non-empty string.
    func requiredString(_ key: String) throws -> String {
        guard let value = body[key] as? String, !value.isEmpty else {
            throw BridgeFailure(.invalidArgument, key)
        }
        return value
    }

    /// An optional non-empty string. A present-but-empty value is an error,
    /// because it almost always means the caller lost data.
    func optionalString(_ key: String) throws -> String? {
        guard let raw = body[key] else { return nil }
        if raw is NSNull { return nil }
        guard let value = raw as? String else {
            throw BridgeFailure(.invalidArgument, key)
        }
        return value.isEmpty ? nil : value
    }

    func optionalBool(_ key: String, default fallback: Bool) throws -> Bool {
        guard let raw = body[key] else { return fallback }
        if raw is NSNull { return fallback }
        guard let value = raw as? Bool else {
            throw BridgeFailure(.invalidArgument, key)
        }
        return value
    }

    func optionalUInt64(_ key: String, default fallback: UInt64) throws -> UInt64 {
        guard let raw = body[key] else { return fallback }
        if raw is NSNull { return fallback }
        guard let number = raw as? NSNumber else {
            throw BridgeFailure(.invalidArgument, key)
        }
        // A negative or fractional handle is a caller defect, not a value to
        // round into something plausible.
        let value = number.doubleValue
        guard value >= 0, value <= Double(UInt64.max), value == value.rounded() else {
            throw BridgeFailure(.invalidArgument, key)
        }
        return UInt64(value)
    }

    func optionalInt(_ key: String, minimum: Int) throws -> Int? {
        guard let raw = body[key] else { return nil }
        if raw is NSNull { return nil }
        guard let number = raw as? NSNumber else {
            throw BridgeFailure(.invalidArgument, key)
        }
        let value = number.doubleValue
        guard value >= Double(minimum), value <= Double(Int.max), value == value.rounded() else {
            throw BridgeFailure(.invalidArgument, key)
        }
        return Int(value)
    }

    func optionalDouble(_ key: String, minimum: Double, maximum: Double) throws -> Double? {
        guard let raw = body[key] else { return nil }
        if raw is NSNull { return nil }
        guard let number = raw as? NSNumber else {
            throw BridgeFailure(.invalidArgument, key)
        }
        let value = number.doubleValue
        guard value.isFinite, value >= minimum, value <= maximum else {
            throw BridgeFailure(.invalidArgument, key)
        }
        return value
    }

    func optionalObject(_ key: String) throws -> [String: Any]? {
        guard let raw = body[key] else { return nil }
        if raw is NSNull { return nil }
        guard let value = raw as? [String: Any] else {
            throw BridgeFailure(.invalidArgument, key)
        }
        return value
    }
}

// MARK: - Responses

/// Serialize a success response.
func successDocument(_ op: String, _ result: [String: Any]) -> String {
    encode([
        "envelope_version": bridgeEnvelopeVersion,
        "abi_version": Int(bridgeABIVersion),
        "op": op,
        "ok": true,
        "result": result,
    ])
}

/// Serialize a failure response.
func failureDocument(_ op: String, _ failure: BridgeFailure) -> String {
    var error: [String: Any] = [
        "code": failure.code.rawValue,
        "retriable": failure.code.isRetriable,
    ]
    error["detail"] = failure.detail ?? NSNull()
    return encode([
        "envelope_version": bridgeEnvelopeVersion,
        "abi_version": Int(bridgeABIVersion),
        "op": op,
        "ok": false,
        "error": error,
    ])
}

/// Serialize a progress notification.
func progressDocument(
    _ op: String,
    phase: String,
    fraction: Double? = nil,
    processedMilliseconds: Int? = nil,
    completedUnits: Int? = nil,
    totalUnits: Int? = nil
) -> String {
    var document: [String: Any] = [
        "envelope_version": bridgeEnvelopeVersion,
        "op": op,
        "phase": phase,
    ]
    // A fraction outside 0...1 would be a lie about progress; clamp rather
    // than publish it.
    document["fraction"] = fraction.map { min(max($0, 0.0), 1.0) } ?? NSNull()
    document["processed_ms"] = processedMilliseconds ?? NSNull()
    document["completed_units"] = completedUnits ?? NSNull()
    document["total_units"] = totalUnits ?? NSNull()
    return encode(document)
}

/// Serialize a dictionary, or abort.
///
/// A response that cannot be serialized has no honest representation: the
/// caller would see a success-shaped document describing nothing. The one
/// value that is always serializable is produced here as a last resort, and
/// anything beyond that is a programming error in this file.
private func encode(_ object: [String: Any]) -> String {
    guard
        let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]),
        let text = String(data: data, encoding: .utf8)
    else {
        let op = (object["op"] as? String) ?? "invalid_request"
        return """
            {"abi_version":\(bridgeABIVersion),"envelope_version":\(bridgeEnvelopeVersion),\
            "error":{"code":"engine_failure","detail":"response_serialization_failed","retriable":false},\
            "ok":false,"op":"\(op)"}
            """
    }
    return text
}

// MARK: - Host description

/// Runtime host facts recorded with every probe (spec 11.5, 11.6).
///
/// These exist so a compatibility cell is written from what the machine
/// reported, never from a marketing label.
enum HostFacts {
    static var osVersion: String {
        let v = ProcessInfo.processInfo.operatingSystemVersion
        return "\(v.majorVersion).\(v.minorVersion).\(v.patchVersion)"
    }

    static var osBuild: String {
        sysctlString("kern.osversion") ?? "unknown"
    }

    static var hardware: String {
        // `machdep.cpu.brand_string` names the silicon, which is what decides
        // Apple Intelligence eligibility.
        sysctlString("machdep.cpu.brand_string") ?? "unknown"
    }

    private static func sysctlString(_ name: String) -> String? {
        var size = 0
        guard sysctlbyname(name, nil, &size, nil, 0) == 0, size > 0 else { return nil }
        var buffer = [CChar](repeating: 0, count: size)
        guard sysctlbyname(name, &buffer, &size, nil, 0) == 0 else { return nil }
        return String(cString: buffer)
    }
}
