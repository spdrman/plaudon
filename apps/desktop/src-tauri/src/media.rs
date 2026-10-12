//! The `plaudon-media` URI scheme (E2.1).
//!
//! `plaudon-media://localhost/<recording_id>` serves the archived blob of a
//! recording, read-only, with `Range` support so the webview can seek. The URL
//! never carries a path: the only accepted form is a canonical recording UUID,
//! which is resolved through [`LibraryPort`] to a blob inside the archive. A
//! traversal attempt, a percent-encoded separator or any non-UUID is a 404.

use std::io::SeekFrom;
use std::sync::Arc;

use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::RecordingId;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

use crate::ports::{LibraryPort, ResolvedMedia};

/// Scheme registered on the Tauri builder.
pub const SCHEME: &str = "plaudon-media";

/// Largest slice served for one request, so a seek cannot ask us to buffer a
/// whole recording in memory.
pub const MAX_CHUNK_BYTES: u64 = 4 * 1024 * 1024;

/// A resolved byte range, inclusive of both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// First byte served.
    pub start: u64,
    /// Last byte served.
    pub end: u64,
}

impl ByteRange {
    /// Number of bytes in the range.
    #[must_use]
    pub const fn length(self) -> u64 {
        self.end - self.start + 1
    }
}

/// Why a `Range` header could not be honoured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    /// The header is not a byte range this server implements.
    Malformed,
    /// The range lies outside the resource.
    Unsatisfiable,
}

/// Parse a single-range `Range` header against a known resource length.
///
/// Multi-range requests are deliberately not implemented: the webview never
/// sends one for media playback, and answering one requires multipart bodies.
pub fn parse_range(header: &str, length: u64) -> Result<ByteRange, RangeError> {
    let spec = header
        .trim()
        .strip_prefix("bytes=")
        .ok_or(RangeError::Malformed)?;
    if spec.contains(',') {
        return Err(RangeError::Malformed);
    }
    let (raw_start, raw_end) = spec.split_once('-').ok_or(RangeError::Malformed)?;
    let raw_start = raw_start.trim();
    let raw_end = raw_end.trim();

    if length == 0 {
        return Err(RangeError::Unsatisfiable);
    }

    let range = if raw_start.is_empty() {
        // `bytes=-N`: the final N bytes.
        let suffix: u64 = raw_end.parse().map_err(|_| RangeError::Malformed)?;
        if suffix == 0 {
            return Err(RangeError::Unsatisfiable);
        }
        let start = length.saturating_sub(suffix);
        ByteRange {
            start,
            end: length - 1,
        }
    } else {
        let start: u64 = raw_start.parse().map_err(|_| RangeError::Malformed)?;
        if start >= length {
            return Err(RangeError::Unsatisfiable);
        }
        let end = if raw_end.is_empty() {
            length - 1
        } else {
            let end: u64 = raw_end.parse().map_err(|_| RangeError::Malformed)?;
            if end < start {
                return Err(RangeError::Malformed);
            }
            end.min(length - 1)
        };
        ByteRange { start, end }
    };

    Ok(ByteRange {
        start: range.start,
        end: range.end.min(range.start + MAX_CHUNK_BYTES - 1),
    })
}

/// Parse the path component of a `plaudon-media` URL.
///
/// Only `/<uuid>` is accepted. Anything else — a nested path, a traversal
/// attempt, a percent-encoded separator — is rejected without touching disk.
pub fn parse_media_path(path: &str) -> Result<RecordingId, AppError> {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() || trimmed.contains('/') || trimmed.contains('%') {
        return Err(AppError::new(
            AppErrorCode::RecordNotFound,
            "the media URL is not a bare recording id",
        ));
    }
    trimmed
        .parse()
        .map_err(|_| AppError::new(AppErrorCode::RecordNotFound, "unknown media identifier"))
}

/// MIME type for an archived blob, derived from the extension we stored.
#[must_use]
pub fn content_type_for(relative_path: &str) -> &'static str {
    let extension = std::path::Path::new(relative_path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        "opus" => "audio/opus",
        _ => "application/octet-stream",
    }
}

/// What the protocol handler should answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaResponse {
    /// HTTP status.
    pub status: u16,
    /// `Content-Type` of the body.
    pub content_type: String,
    /// `Content-Range`, present on a 206.
    pub content_range: Option<String>,
    /// Response body.
    pub body: Vec<u8>,
}

impl MediaResponse {
    fn error(status: u16) -> Self {
        Self {
            status,
            content_type: "text/plain".to_owned(),
            content_range: None,
            body: Vec::new(),
        }
    }
}

/// Resolve a URL path and serve the requested bytes.
pub async fn serve(
    library: &Arc<dyn LibraryPort>,
    path: &str,
    range_header: Option<&str>,
) -> MediaResponse {
    let Ok(recording_id) = parse_media_path(path) else {
        return MediaResponse::error(404);
    };
    let media = match library.resolve_media(recording_id).await {
        Ok(media) => media,
        Err(err) => {
            tracing::debug!(code = err.code().as_str(), "media request was refused");
            return MediaResponse::error(404);
        }
    };
    serve_resolved(&media, range_header).await
}

async fn serve_resolved(media: &ResolvedMedia, range_header: Option<&str>) -> MediaResponse {
    let Some(header) = range_header else {
        return match read_slice(
            media,
            ByteRange {
                start: 0,
                end: media.byte_length.max(1) - 1,
            },
        )
        .await
        {
            Ok(body) => MediaResponse {
                status: 200,
                content_type: media.content_type.clone(),
                content_range: None,
                body,
            },
            Err(()) => MediaResponse::error(404),
        };
    };

    match parse_range(header, media.byte_length) {
        Ok(range) => match read_slice(media, range).await {
            Ok(body) => MediaResponse {
                status: 206,
                content_type: media.content_type.clone(),
                content_range: Some(format!(
                    "bytes {}-{}/{}",
                    range.start, range.end, media.byte_length
                )),
                body,
            },
            Err(()) => MediaResponse::error(404),
        },
        Err(RangeError::Unsatisfiable) => {
            let mut response = MediaResponse::error(416);
            response.content_range = Some(format!("bytes */{}", media.byte_length));
            response
        }
        Err(RangeError::Malformed) => MediaResponse::error(400),
    }
}

async fn read_slice(media: &ResolvedMedia, range: ByteRange) -> Result<Vec<u8>, ()> {
    let mut file = tokio::fs::File::open(&media.path).await.map_err(|err| {
        tracing::warn!(error = %err, "archived blob could not be opened");
    })?;
    file.seek(SeekFrom::Start(range.start))
        .await
        .map_err(|err| {
            tracing::warn!(error = %err, "archived blob could not be positioned");
        })?;
    let length = usize::try_from(range.length().min(MAX_CHUNK_BYTES)).unwrap_or(0);
    let mut body = vec![0u8; length];
    let read = file.read_exact(&mut body).await.unwrap_or(0);
    body.truncate(read);
    Ok(body)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    const LENGTH: u64 = 1000;

    #[test]
    fn range_parsing_covers_the_forms_a_media_element_sends() {
        assert_eq!(
            parse_range("bytes=0-", LENGTH).unwrap(),
            ByteRange { start: 0, end: 999 }
        );
        assert_eq!(
            parse_range("bytes=0-99", LENGTH).unwrap(),
            ByteRange { start: 0, end: 99 }
        );
        assert_eq!(
            parse_range("bytes=500-", LENGTH).unwrap(),
            ByteRange {
                start: 500,
                end: 999
            }
        );
        assert_eq!(
            parse_range("bytes=-100", LENGTH).unwrap(),
            ByteRange {
                start: 900,
                end: 999
            }
        );
        assert_eq!(
            parse_range("bytes=0-100000", LENGTH).unwrap(),
            ByteRange { start: 0, end: 999 },
            "the end is clamped to the resource"
        );
        assert_eq!(parse_range("bytes=0-0", LENGTH).unwrap().length(), 1);
    }

    #[test]
    fn range_parsing_rejects_what_it_cannot_honour() {
        assert_eq!(parse_range("items=0-1", LENGTH), Err(RangeError::Malformed));
        assert_eq!(
            parse_range("bytes=0-1,5-6", LENGTH),
            Err(RangeError::Malformed)
        );
        assert_eq!(
            parse_range("bytes=abc-", LENGTH),
            Err(RangeError::Malformed)
        );
        assert_eq!(
            parse_range("bytes=10-5", LENGTH),
            Err(RangeError::Malformed)
        );
        assert_eq!(
            parse_range("bytes=1000-", LENGTH),
            Err(RangeError::Unsatisfiable)
        );
        assert_eq!(
            parse_range("bytes=-0", LENGTH),
            Err(RangeError::Unsatisfiable)
        );
        assert_eq!(parse_range("bytes=0-", 0), Err(RangeError::Unsatisfiable));
    }

    #[test]
    fn a_huge_range_is_capped_to_one_chunk() {
        let range = parse_range("bytes=0-", MAX_CHUNK_BYTES * 4).unwrap();
        assert_eq!(range.length(), MAX_CHUNK_BYTES);
    }

    #[test]
    fn only_a_bare_recording_id_resolves() {
        let id = RecordingId::new();
        assert_eq!(parse_media_path(&format!("/{id}")).unwrap(), id);
        for hostile in [
            "/..%2F..%2Fetc%2Fpasswd",
            "/../../etc/passwd",
            "/media/0a/0b/deadbeef.wav",
            "/",
            "",
            "/not-a-uuid",
            "/%2e%2e",
            "/00000000-0000-0000-0000-000000000000/../x",
        ] {
            assert!(
                parse_media_path(hostile).is_err(),
                "{hostile} must not resolve"
            );
        }
    }

    #[test]
    fn content_types_come_from_the_stored_extension() {
        assert_eq!(content_type_for("media/ab/cd/x.wav"), "audio/wav");
        assert_eq!(content_type_for("media/ab/cd/x.MP3"), "audio/mpeg");
        assert_eq!(content_type_for("media/ab/cd/x.opus"), "audio/opus");
        assert_eq!(
            content_type_for("media/ab/cd/x"),
            "application/octet-stream"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_range_request_is_answered_with_206_and_the_exact_slice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.wav");
        let payload: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        std::fs::write(&path, &payload).unwrap();
        let media = ResolvedMedia {
            path,
            content_type: "audio/wav".to_owned(),
            byte_length: payload.len() as u64,
        };

        let response = serve_resolved(&media, Some("bytes=10-19")).await;
        assert_eq!(response.status, 206);
        assert_eq!(response.content_range.as_deref(), Some("bytes 10-19/1000"));
        assert_eq!(response.body, payload[10..=19]);

        let full = serve_resolved(&media, None).await;
        assert_eq!(full.status, 200);
        assert_eq!(full.body.len(), 1000);
        assert_eq!(full.content_range, None);

        let unsatisfiable = serve_resolved(&media, Some("bytes=5000-")).await;
        assert_eq!(unsatisfiable.status, 416);
        assert_eq!(unsatisfiable.content_range.as_deref(), Some("bytes */1000"));
    }
}
