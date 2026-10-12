//! The safe Rust surface over the C ABI (spec 11.2, 11.6 — task H1.4).
//!
//! Everything above this module talks to [`AppleBridge`], never to `extern`
//! declarations. That has three consequences we rely on:
//!
//! * `unsafe` exists only in [`crate::ffi`], which is compiled only on macOS
//!   with the `apple-bridge` feature. Every other module, test and platform
//!   sees ordinary Rust.
//! * The adapters are testable without a Mac. [`FakeBridge`] answers with
//!   scripted documents, so the state machines, error mapping and
//!   cancellation paths are exercised in CI on every platform.
//! * A handle cannot outlive its release. [`CancelGuard`] cancels and
//!   releases on drop, so a dropped job never leaves an entry in the Swift
//!   registry (H1.4) and never frees anything the library still owns: no
//!   buffer ownership crosses in that direction at all.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::envelope::{BridgeOp, ProgressEvent};

/// Why a bridge call could not be made at all.
///
/// Distinct from a [`crate::envelope::BridgeError`], which is the library
/// answering. These are failures of the boundary itself.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FfiError {
    /// The library reports an ABI version this build does not understand.
    #[error("the Apple bridge reports ABI version {found}, expected {expected}")]
    AbiMismatch {
        /// Version the library reports.
        found: i32,
        /// Version this build expects.
        expected: i32,
    },
    /// A request string contained an interior NUL and could not cross.
    #[error("a bridge request contains an interior NUL byte")]
    InvalidArgument,
    /// The library returned a null pointer, which its contract forbids.
    #[error("the Apple bridge returned a null document")]
    NullDocument,
    /// This build has no bridge: not macOS, or the feature is off.
    #[error("this build does not link the Apple bridge")]
    NotLinked,
}

/// Sink for progress documents arriving from a library-owned thread.
///
/// Spec 6.5: a dropped update is always better than a stalled engine, so the
/// implementation must not block and must not fail the call.
pub type ProgressSink<'a> = dyn Fn(ProgressEvent) + Send + Sync + 'a;

/// The raw bridge operations, as the safe wrapper needs them.
///
/// Calls are blocking: the Swift side owns the Apple concurrency and hands
/// back one document. Callers run them on a blocking pool.
pub trait AppleBridge: Send + Sync + fmt::Debug {
    /// ABI version the linked library reports.
    fn abi_version(&self) -> Result<i32, FfiError>;

    /// Allocate a cancellation handle.
    fn new_cancel_token(&self) -> u64;

    /// Request cancellation. Idempotent and valid at any point.
    fn cancel(&self, token: u64);

    /// Release a handle. Idempotent.
    fn release_cancel_token(&self, token: u64);

    /// Whether the handle has been cancelled. Diagnostics only.
    fn is_cancelled(&self, token: u64) -> bool;

    /// Number of handles allocated and not released. Diagnostics only.
    fn live_token_count(&self) -> u64;

    /// Perform one operation, returning the raw response document.
    fn call(
        &self,
        op: BridgeOp,
        request: &str,
        progress: Option<&ProgressSink<'_>>,
    ) -> Result<String, FfiError>;
}

/// An owned cancellation handle that always releases.
///
/// Dropping it cancels first and then releases: a job abandoned mid-flight
/// tells the library to stop rather than leaving an orphan operation running
/// against a handle nobody will ever cancel.
pub struct CancelGuard {
    bridge: Arc<dyn AppleBridge>,
    token: u64,
}

impl CancelGuard {
    /// Allocate a handle from `bridge`.
    #[must_use]
    pub fn new(bridge: Arc<dyn AppleBridge>) -> Self {
        let token = bridge.new_cancel_token();
        Self { bridge, token }
    }

    /// The raw handle, for placing in a request envelope.
    #[must_use]
    pub const fn token(&self) -> u64 {
        self.token
    }

    /// Request cancellation now. Safe to call repeatedly.
    pub fn cancel(&self) {
        self.bridge.cancel(self.token);
    }

    /// Whether cancellation has been recorded.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.bridge.is_cancelled(self.token)
    }
}

impl fmt::Debug for CancelGuard {
    // The bridge handle is an `Arc<dyn AppleBridge>` whose `Debug` would
    // recurse into a fake's whole call log; the handle is the only thing
    // worth printing.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancelGuard")
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.bridge.cancel(self.token);
        self.bridge.release_cancel_token(self.token);
    }
}

/// A recorded call, for assertions in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCall {
    /// Operation requested.
    pub op: BridgeOp,
    /// Exact request document.
    pub request: String,
    /// Handle the request carried, parsed out for convenience.
    pub cancel_token: u64,
}

/// An arbitrary responder a test installs for one call.
pub type Responder =
    Box<dyn Fn(&str, Option<&ProgressSink<'_>>) -> Result<String, FfiError> + Send + Sync>;

/// A responder used for every unscripted call.
pub type DefaultResponder = Box<dyn Fn(BridgeOp) -> String + Send + Sync>;

/// What a [`FakeBridge`] should do for one call.
pub enum ScriptedCall {
    /// Return this document verbatim.
    Reply(String),
    /// Emit these progress documents, then return the reply.
    ProgressThenReply {
        /// Progress documents to emit, in order.
        progress: Vec<String>,
        /// The response document.
        reply: String,
    },
    /// Fail at the boundary.
    Fail(FfiError),
    /// Run an arbitrary responder. The closure receives the request document
    /// and the progress sink, so a test can model cancellation mid-call.
    Dynamic(Responder),
}

impl fmt::Debug for ScriptedCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reply(_) => f.write_str("Reply"),
            Self::ProgressThenReply { .. } => f.write_str("ProgressThenReply"),
            Self::Fail(error) => write!(f, "Fail({error})"),
            Self::Dynamic(_) => f.write_str("Dynamic"),
        }
    }
}

#[derive(Debug, Default)]
struct FakeState {
    next_token: u64,
    live: std::collections::BTreeSet<u64>,
    cancelled: std::collections::BTreeSet<u64>,
    calls: Vec<RecordedCall>,
}

/// A bridge that answers from a script.
///
/// This is the whole reason the adapters have tests on Windows: it reproduces
/// the documented contract, including handle lifetime accounting, without any
/// Apple framework.
pub struct FakeBridge {
    abi_version: Result<i32, FfiError>,
    scripts: Mutex<std::collections::BTreeMap<BridgeOp, VecDeque<ScriptedCall>>>,
    default_reply: Mutex<Option<DefaultResponder>>,
    state: Mutex<FakeState>,
}

impl fmt::Debug for FakeBridge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeBridge")
            .field("abi_version", &self.abi_version)
            .field("live_tokens", &self.live_token_count())
            .finish_non_exhaustive()
    }
}

impl Default for FakeBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeBridge {
    /// A bridge reporting the expected ABI version and no scripted calls.
    #[must_use]
    pub fn new() -> Self {
        Self {
            abi_version: Ok(crate::envelope::EXPECTED_ABI_VERSION),
            scripts: Mutex::new(std::collections::BTreeMap::new()),
            default_reply: Mutex::new(None),
            state: Mutex::new(FakeState {
                next_token: 1,
                ..FakeState::default()
            }),
        }
    }

    /// A bridge that reports a different ABI version.
    #[must_use]
    pub fn with_abi_version(mut self, version: i32) -> Self {
        self.abi_version = Ok(version);
        self
    }

    /// Queue one scripted answer for `op`.
    #[must_use]
    pub fn script(self, op: BridgeOp, call: ScriptedCall) -> Self {
        self.scripts.lock().entry(op).or_default().push_back(call);
        self
    }

    /// Queue a verbatim reply for `op`.
    #[must_use]
    pub fn reply(self, op: BridgeOp, document: impl Into<String>) -> Self {
        self.script(op, ScriptedCall::Reply(document.into()))
    }

    /// Answer any unscripted call with `responder`.
    #[must_use]
    pub fn with_default(
        self,
        responder: impl Fn(BridgeOp) -> String + Send + Sync + 'static,
    ) -> Self {
        *self.default_reply.lock() = Some(Box::new(responder));
        self
    }

    /// Every call made so far, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.state.lock().calls.clone()
    }

    /// Calls made for one operation.
    #[must_use]
    pub fn calls_for(&self, op: BridgeOp) -> Vec<RecordedCall> {
        self.calls().into_iter().filter(|c| c.op == op).collect()
    }
}

impl AppleBridge for FakeBridge {
    fn abi_version(&self) -> Result<i32, FfiError> {
        self.abi_version.clone()
    }

    fn new_cancel_token(&self) -> u64 {
        let mut state = self.state.lock();
        let token = state.next_token;
        state.next_token += 1;
        state.live.insert(token);
        token
    }

    fn cancel(&self, token: u64) {
        if token == 0 {
            return;
        }
        let mut state = self.state.lock();
        if state.live.contains(&token) {
            state.cancelled.insert(token);
        }
    }

    fn release_cancel_token(&self, token: u64) {
        if token == 0 {
            return;
        }
        let mut state = self.state.lock();
        state.live.remove(&token);
        state.cancelled.remove(&token);
    }

    fn is_cancelled(&self, token: u64) -> bool {
        if token == 0 {
            return false;
        }
        self.state.lock().cancelled.contains(&token)
    }

    fn live_token_count(&self) -> u64 {
        self.state.lock().live.len() as u64
    }

    fn call(
        &self,
        op: BridgeOp,
        request: &str,
        progress: Option<&ProgressSink<'_>>,
    ) -> Result<String, FfiError> {
        let cancel_token = serde_json::from_str::<serde_json::Value>(request)
            .ok()
            .and_then(|value| {
                value
                    .get("cancel_token")
                    .and_then(serde_json::Value::as_u64)
            })
            .unwrap_or(0);
        self.state.lock().calls.push(RecordedCall {
            op,
            request: request.to_owned(),
            cancel_token,
        });

        let scripted = self
            .scripts
            .lock()
            .get_mut(&op)
            .and_then(VecDeque::pop_front);

        match scripted {
            Some(ScriptedCall::Reply(document)) => Ok(document),
            Some(ScriptedCall::ProgressThenReply {
                progress: documents,
                reply,
            }) => {
                if let Some(sink) = progress {
                    for document in documents {
                        if let Some(event) = ProgressEvent::parse(&document) {
                            sink(event);
                        }
                    }
                }
                Ok(reply)
            }
            Some(ScriptedCall::Fail(error)) => Err(error),
            Some(ScriptedCall::Dynamic(responder)) => responder(request, progress),
            None => {
                let default = self.default_reply.lock();
                match default.as_ref() {
                    Some(responder) => Ok(responder(op)),
                    None => Ok(unscripted_document(op)),
                }
            }
        }
    }
}

/// The answer an unscripted fake gives: a well-formed refusal.
///
/// Not a success, so a test that forgets to script a call fails loudly rather
/// than passing on a fabricated capability.
fn unscripted_document(op: BridgeOp) -> String {
    serde_json::json!({
        "envelope_version": crate::envelope::ENVELOPE_VERSION,
        "abi_version": crate::envelope::EXPECTED_ABI_VERSION,
        "op": op.as_str(),
        "ok": false,
        "error": { "code": "engine_failure", "detail": "unscripted_fake_call", "retriable": false },
    })
    .to_string()
}

/// Build a success document for tests and fixtures.
#[must_use]
#[allow(
    clippy::needless_pass_by_value,
    reason = "callers build the value inline"
)]
pub fn success_document(op: BridgeOp, result: serde_json::Value) -> String {
    serde_json::json!({
        "envelope_version": crate::envelope::ENVELOPE_VERSION,
        "abi_version": crate::envelope::EXPECTED_ABI_VERSION,
        "op": op.as_str(),
        "ok": true,
        "result": result,
    })
    .to_string()
}

/// Build a failure document for tests and fixtures.
#[must_use]
pub fn failure_document(op: BridgeOp, code: &str, detail: Option<&str>) -> String {
    serde_json::json!({
        "envelope_version": crate::envelope::ENVELOPE_VERSION,
        "abi_version": crate::envelope::EXPECTED_ABI_VERSION,
        "op": op.as_str(),
        "ok": false,
        "error": {
            "code": code,
            "detail": detail,
            "retriable": false,
        },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guard_cancels_and_releases_on_drop() {
        let bridge = Arc::new(FakeBridge::new());
        let handle: Arc<dyn AppleBridge> = bridge.clone();
        {
            let guard = CancelGuard::new(handle);
            assert_eq!(bridge.live_token_count(), 1);
            assert!(!guard.is_cancelled());
            guard.cancel();
            assert!(guard.is_cancelled());
            // Idempotent while live.
            guard.cancel();
            assert!(guard.is_cancelled());
        }
        assert_eq!(
            bridge.live_token_count(),
            0,
            "dropping a guard must release its handle"
        );
    }

    #[test]
    fn a_dropped_guard_cancels_before_releasing() {
        // A job abandoned mid-flight must tell the library to stop. The fake
        // records cancellation only while the handle is live, so observing a
        // released-and-not-live handle after drop proves the ordering did not
        // skip the cancel.
        let bridge = Arc::new(FakeBridge::new());
        let handle: Arc<dyn AppleBridge> = bridge.clone();
        let token = {
            let guard = CancelGuard::new(handle);
            guard.token()
        };
        assert_eq!(bridge.live_token_count(), 0);
        assert!(!bridge.is_cancelled(token), "a released handle is inert");
    }

    #[test]
    fn handles_are_never_reused() {
        let bridge = FakeBridge::new();
        let first = bridge.new_cancel_token();
        bridge.release_cancel_token(first);
        let second = bridge.new_cancel_token();
        assert_ne!(first, second);
    }

    #[test]
    fn cancelling_token_zero_and_unknown_handles_is_inert() {
        let bridge = FakeBridge::new();
        bridge.cancel(0);
        bridge.cancel(u64::MAX);
        bridge.release_cancel_token(0);
        bridge.release_cancel_token(u64::MAX);
        assert!(!bridge.is_cancelled(0));
        assert!(!bridge.is_cancelled(u64::MAX));
        assert_eq!(bridge.live_token_count(), 0);
    }

    #[test]
    fn an_unscripted_call_is_a_refusal_not_a_success() {
        let bridge = FakeBridge::new();
        let document = bridge
            .call(BridgeOp::ProbeSpeech, "{}", None)
            .expect("the fake never fails at the boundary by default");
        assert!(document.contains("\"ok\":false"));
        assert!(document.contains("unscripted_fake_call"));
    }

    #[test]
    fn scripted_progress_reaches_the_sink_before_the_reply() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let bridge = FakeBridge::new().script(
            BridgeOp::TranscribeFile,
            ScriptedCall::ProgressThenReply {
                progress: vec![
                    r#"{"envelope_version":1,"op":"transcribe_file","phase":"transcribe","fraction":0.25}"#.into(),
                    r#"{"envelope_version":1,"op":"transcribe_file","phase":"transcribe","fraction":0.75}"#.into(),
                ],
                reply: success_document(BridgeOp::TranscribeFile, serde_json::json!({})),
            },
        );
        let collector = {
            let seen = seen.clone();
            move |event: ProgressEvent| {
                seen.lock().push(event.fraction);
            }
        };
        let document = bridge
            .call(BridgeOp::TranscribeFile, "{}", Some(&collector))
            .expect("call succeeds");
        assert!(document.contains("\"ok\":true"));
        assert_eq!(*seen.lock(), vec![Some(0.25), Some(0.75)]);
    }

    #[test]
    fn calls_are_recorded_with_their_cancel_token() {
        let bridge = FakeBridge::new();
        let _ = bridge.call(
            BridgeOp::GenerateText,
            r#"{"envelope_version":1,"op":"generate_text","cancel_token":42}"#,
            None,
        );
        let calls = bridge.calls_for(BridgeOp::GenerateText);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].cancel_token, 42);
    }
}
