//! The `plaudon-media` URI scheme (E2.1).
//!
//! `plaudon-media://localhost/<recording_id>` is the webview's only route to
//! archived audio. Resolution, range parsing and the byte slicing live in
//! `plaudon_app::media`; this module is the Tauri scheme binding and is
//! deleted with the Tauri shell.

use std::sync::Arc;

pub use plaudon_app::media::{
    content_type_for, parse_media_path, parse_range, serve, ByteRange, MediaResponse, RangeError,
    MAX_CHUNK_BYTES, SCHEME,
};

/// Register the scheme on the Tauri builder.
pub fn register<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.register_asynchronous_uri_scheme_protocol(SCHEME, move |context, request, responder| {
        use tauri::Manager as _;
        let Some(state) = context.app_handle().try_state::<crate::AppState>() else {
            responder.respond(
                tauri::http::Response::builder()
                    .status(503)
                    .body(Vec::new())
                    .unwrap_or_default(),
            );
            return;
        };
        let library = Arc::clone(&state.ports.library);
        let path = request.uri().path().to_owned();
        let range = request
            .headers()
            .get(tauri::http::header::RANGE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);

        tauri::async_runtime::spawn(async move {
            let media = serve(&library, &path, range.as_deref()).await;
            let mut builder = tauri::http::Response::builder()
                .status(media.status)
                .header(tauri::http::header::CONTENT_TYPE, &media.content_type)
                .header(tauri::http::header::ACCEPT_RANGES, "bytes")
                .header(tauri::http::header::CACHE_CONTROL, "no-store")
                .header(tauri::http::header::CONTENT_LENGTH, media.body.len());
            if let Some(content_range) = &media.content_range {
                builder = builder.header(tauri::http::header::CONTENT_RANGE, content_range);
            }
            responder.respond(builder.body(media.body).unwrap_or_default());
        });
    })
}
