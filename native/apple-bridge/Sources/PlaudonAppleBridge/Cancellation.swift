// Cancellation tokens, progress relays and the bounded runner (spec 11.2).
//
// Three guarantees are implemented here, and they are the reason the bridge
// can be called from a job engine without supervision:
//
//   1. Cancellation is idempotent and valid at any point in a call's life,
//      including before it starts and after it has finished.
//   2. No progress callback is delivered after the owning call returns, and
//      none is delivered after `plaudon_apple_cancel` returns for its token.
//      Both are enforced by a lock, not by hope.
//   3. Every long-running call has a wall-clock budget. A platform component
//      that never finalizes cannot hang the caller.

import Foundation

// MARK: - Progress relay

/// Delivers progress documents to a C callback, and can be disarmed.
///
/// Deliveries happen under `lock`, so they are serialized with respect to each
/// other and with respect to `disarm()`. Once `disarm()` has returned, no
/// delivery is in flight and none will start: the callback context is free to
/// be destroyed.
public typealias PlaudonProgressCallback =
    @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?) -> Void

final class ProgressRelay: @unchecked Sendable {
    typealias Callback = PlaudonProgressCallback

    private let lock = NSLock()
    private var callback: Callback?
    private var context: UnsafeMutableRawPointer?

    init(callback: Callback?, context: UnsafeMutableRawPointer?) {
        self.callback = callback
        self.context = context
    }

    /// A relay that discards everything, for calls with no callback.
    static var disarmed: ProgressRelay { ProgressRelay(callback: nil, context: nil) }

    func report(_ document: String) {
        lock.lock()
        defer { lock.unlock() }
        guard let callback else { return }
        // `document` is borrowed for the duration of the call; the callee must
        // not retain or free it (header rule 3.5).
        document.withCString { callback(context, $0) }
    }

    /// Stop delivering. Returns only once any in-flight delivery has finished.
    func disarm() {
        lock.lock()
        callback = nil
        context = nil
        lock.unlock()
    }
}

// MARK: - Cancellation registry

/// Library-private registry of cancellation handles.
///
/// Handles are drawn from a monotonically increasing counter and never reused,
/// so a stale handle can never address a live operation. A handle carries no
/// address, which is why no Swift object pointer crosses the ABI.
final class CancellationRegistry: @unchecked Sendable {
    static let shared = CancellationRegistry()

    private let lock = NSLock()
    private var next: UInt64 = 1
    private var live: Set<UInt64> = []
    private var cancelled: Set<UInt64> = []
    private var relays: [UInt64: [ObjectIdentifier: ProgressRelay]] = [:]

    func allocate() -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        let token = next
        next &+= 1
        live.insert(token)
        return token
    }

    /// Mark cancelled and disarm every relay bound to the token.
    ///
    /// Disarming happens outside the registry lock so a slow in-flight
    /// delivery cannot block an unrelated `allocate()`, and after the token is
    /// recorded as cancelled so a concurrent `report` sees the final state.
    func cancel(_ token: UInt64) {
        guard token != 0 else { return }
        lock.lock()
        let known = live.contains(token)
        if known { cancelled.insert(token) }
        let bound = known ? Array(relays[token]?.values ?? [:].values) : []
        lock.unlock()
        for relay in bound { relay.disarm() }
    }

    func isCancelled(_ token: UInt64) -> Bool {
        guard token != 0 else { return false }
        lock.lock()
        defer { lock.unlock() }
        return cancelled.contains(token)
    }

    /// Release a handle. Idempotent; a released handle is never reused.
    func release(_ token: UInt64) {
        guard token != 0 else { return }
        lock.lock()
        live.remove(token)
        cancelled.remove(token)
        relays.removeValue(forKey: token)
        lock.unlock()
    }

    func liveCount() -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return UInt64(live.count)
    }

    /// Bind a relay so `cancel` can disarm it.
    ///
    /// Returns false when the token is already cancelled, in which case the
    /// caller must not start work: binding after cancellation would create a
    /// relay no one will ever disarm on the cancel path.
    func bind(_ relay: ProgressRelay, to token: UInt64) -> Bool {
        guard token != 0 else { return true }
        lock.lock()
        defer { lock.unlock() }
        guard live.contains(token), !cancelled.contains(token) else { return false }
        relays[token, default: [:]][ObjectIdentifier(relay)] = relay
        return true
    }

    func unbind(_ relay: ProgressRelay, from token: UInt64) {
        guard token != 0 else { return }
        lock.lock()
        relays[token]?.removeValue(forKey: ObjectIdentifier(relay))
        if relays[token]?.isEmpty == true { relays.removeValue(forKey: token) }
        lock.unlock()
    }
}

// MARK: - Bounded execution

/// Wall-clock budget applied when the caller passes 0.
///
/// Measured on a 7.6 s fixture the analyzer finishes in about one second, so
/// ten minutes is generous for a long meeting and still finite. The A1.3 spike
/// recorded a platform recognizer that never finalized at all, which is the
/// failure this bound exists to contain.
let defaultTimeoutMilliseconds: UInt64 = 10 * 60 * 1000

/// Grace period allowed for a cancelled or timed-out task to unwind.
///
/// We do not wait indefinitely: the point of the budget is that the caller
/// always gets an answer.
private let unwindGraceMilliseconds: UInt64 = 2_000

/// A box a background task can write once, readable under a lock.
private final class Box<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: T?

    func set(_ newValue: T) {
        lock.lock()
        value = newValue
        lock.unlock()
    }

    func get() -> T? {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}

/// Run `body` with a cancellation token, a wall-clock budget and a progress
/// relay, and return exactly one outcome.
///
/// The relay is disarmed before this function returns, under every exit path,
/// which is header rule 3.3.
func runBounded(
    op: String,
    cancelToken: UInt64,
    timeoutMilliseconds: UInt64,
    relay: ProgressRelay,
    body: @escaping @Sendable (ProgressRelay) async -> Result<[String: Any], BridgeFailure>
) -> String {
    let registry = CancellationRegistry.shared
    defer { relay.disarm() }

    if registry.isCancelled(cancelToken) {
        return failureDocument(op, BridgeFailure(.cancelled, "before_start"))
    }
    guard registry.bind(relay, to: cancelToken) else {
        return failureDocument(op, BridgeFailure(.cancelled, "before_start"))
    }
    defer { registry.unbind(relay, from: cancelToken) }

    let budget = timeoutMilliseconds == 0 ? defaultTimeoutMilliseconds : timeoutMilliseconds
    let deadline = DispatchTime.now() + .milliseconds(Int(min(budget, UInt64(Int.max))))
    let outcome = Box<Result<[String: Any], BridgeFailure>>()
    let semaphore = DispatchSemaphore(value: 0)

    let task = Task.detached(priority: .userInitiated) {
        let result = await body(relay)
        outcome.set(result)
        semaphore.signal()
    }

    var interruption: BridgeFailure?
    while semaphore.wait(timeout: .now() + .milliseconds(25)) == .timedOut {
        if registry.isCancelled(cancelToken) {
            interruption = BridgeFailure(.cancelled, "in_flight")
            break
        }
        if DispatchTime.now() >= deadline {
            interruption = BridgeFailure(.engineTimeout, "\(budget)ms")
            break
        }
    }

    guard let interruption else {
        switch outcome.get() {
        case .some(.success(let result)):
            return successDocument(op, result)
        case .some(.failure(let failure)):
            return failureDocument(op, failure)
        case .none:
            // The semaphore signalled, so the box is written; reaching here
            // would mean a torn write, which the lock prevents.
            return failureDocument(op, BridgeFailure(.engineFailure, "missing_outcome"))
        }
    }

    // Ask the platform work to stop and give it a bounded chance to unwind so
    // its resources are released before we return.
    task.cancel()
    _ = semaphore.wait(timeout: .now() + .milliseconds(Int(unwindGraceMilliseconds)))
    return failureDocument(op, interruption)
}

/// Run a short, non-cancellable operation with a hard budget.
///
/// Probes use this: they must answer in milliseconds, and an availability
/// query that blocks is itself an answer of "not usable" (spec 11.3).
func runProbe(
    op: String,
    timeoutMilliseconds: UInt64,
    body: @escaping @Sendable () async -> Result<[String: Any], BridgeFailure>
) -> String {
    let outcome = Box<Result<[String: Any], BridgeFailure>>()
    let semaphore = DispatchSemaphore(value: 0)
    let task = Task.detached(priority: .userInitiated) {
        let result = await body()
        outcome.set(result)
        semaphore.signal()
    }
    if semaphore.wait(timeout: .now() + .milliseconds(Int(timeoutMilliseconds))) == .timedOut {
        task.cancel()
        return failureDocument(op, BridgeFailure(.engineTimeout, "\(timeoutMilliseconds)ms"))
    }
    switch outcome.get() {
    case .some(.success(let result)):
        return successDocument(op, result)
    case .some(.failure(let failure)):
        return failureDocument(op, failure)
    case .none:
        return failureDocument(op, BridgeFailure(.engineFailure, "missing_outcome"))
    }
}
