//! Cross-recording library search (spec §3.1, §9.8, §17.5).

use plaudon_domain::error::AppError;
use plaudon_persistence::search::SearchPage;

use crate::dto::{SearchLibraryRequest, SearchLibraryResponse, SearchResultDto};
use crate::validate::{limit, PageCursor};
use crate::AppPorts;

/// Search the transcript corpus.
pub async fn search_library(
    ports: &AppPorts,
    request: SearchLibraryRequest,
) -> Result<SearchLibraryResponse, AppError> {
    let cursor = match request.cursor.as_deref() {
        None => PageCursor::START,
        Some(raw) => PageCursor::decode(raw)?,
    };
    let page_limit = limit(request.limit)?;
    let page = SearchPage {
        limit: page_limit,
        offset: cursor.offset(),
    };

    let results = ports.search.search(&request.query, page).await?;
    let results_dto: Vec<SearchResultDto> = results
        .into_iter()
        .map(|r| SearchResultDto {
            recording_id: r.recording_id,
            recording_title: r.recording_title,
            transcript_revision_id: r.transcript_revision_id,
            seq_index: r.seq_index,
            excerpt: r.excerpt,
            seek_ms: r.seek_ms,
            alignment_origin: r.alignment_origin,
        })
        .collect();

    let next_cursor = if results_dto.len() == page_limit as usize {
        Some(PageCursor::at(cursor.offset() + page_limit as u64).encode())
    } else {
        None
    };

    Ok(SearchLibraryResponse {
        results: results_dto,
        next_cursor,
    })
}
