//! Validated logic behind `resolve_media`.
//!
//! The Tauri shell handed the webview a `plaudon-media://localhost/<uuid>`
//! URL and let [`crate::media::serve`] answer range requests over it. A native
//! player has no URL loader: it opens a file, and a peak builder reads the
//! same file off the render thread. Neither may be given the archive layout,
//! so this is the one command that turns a recording identity into a location
//! the player and the peak worker can use.
//!
//! It resolves through [`crate::ports::LibraryPort::resolve_media`], which is
//! the same path the media scheme used: an unknown, deleted or
//! blob-less recording is an error, never a path guess.

use plaudon_domain::error::AppError;
use plaudon_domain::ids::RecordingId;

use crate::dto::{MediaLocation, ResolveMediaRequest};
use crate::validate::parse_id;
use crate::AppPorts;

/// Where one recording's archived bytes are.
///
/// # Errors
///
/// Fails when the identifier is not a recording id, when no such recording
/// exists, when it has been deleted, or when its archived blob is missing.
pub async fn resolve_media(
    ports: &AppPorts,
    request: ResolveMediaRequest,
) -> Result<MediaLocation, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    let resolved = crate::media::resolve_media(&ports.library, recording_id).await?;
    Ok(MediaLocation {
        recording_id,
        // Lossy only for a path that is not valid Unicode; the archive names
        // its own files, so this is a UUID-derived name in practice, and a
        // name that could not survive the round trip would be a library this
        // process did not write.
        path: resolved.path.to_string_lossy().into_owned(),
        content_type: resolved.content_type,
        byte_length: resolved.byte_length,
    })
}
