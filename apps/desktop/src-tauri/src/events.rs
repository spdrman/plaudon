//! Tauri delivery for the §15.3 UI event stream.
//!
//! The stream itself — [`EventDraft`], [`EventPublisher`] and the durable
//! sequence allocated after commit — lives in `plaudon_app::events`. Only the
//! webview sink is Tauri's, and it is deleted with the Tauri shell.

use async_trait::async_trait;

use plaudon_app::dto::UiEvent;
pub use plaudon_app::events::{
    EventDraft, EventPublisher, EventSink, NullEventSink, RecordingEventSink,
};

/// Sink that forwards to the Tauri webview.
#[derive(Debug, Clone)]
pub struct TauriEventSink<R: tauri::Runtime> {
    app: tauri::AppHandle<R>,
}

impl<R: tauri::Runtime> TauriEventSink<R> {
    /// Sink bound to a running application handle.
    #[must_use]
    pub fn new(app: tauri::AppHandle<R>) -> Self {
        Self { app }
    }
}

#[async_trait]
impl<R: tauri::Runtime> EventSink for TauriEventSink<R> {
    async fn emit(&self, event: UiEvent) {
        use tauri::Emitter as _;
        let name = event.kind.as_str();
        if let Err(err) = self.app.emit(name, &event) {
            tracing::warn!(kind = name, error = %err, "ui event delivery failed");
        }
    }
}
