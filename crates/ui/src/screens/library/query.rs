//! The library query: filters, sort, the keyset page chain and the rules for
//! merging pages.
//!
//! Everything here is plain data and plain functions. It is the Rust form of
//! `features/library/libraryQuery.ts`, including the parts that are easy to
//! get wrong and that its tests pinned down: an unknown value sorts last in
//! both directions rather than as zero, a filter or sort change invalidates
//! the cursor chain, and a recording identity never renders twice when two
//! pages overlap.

use std::collections::BTreeSet;
use std::time::Duration;

use plaudon_app::dto::{
    ImportStatus, ListRecordingsRequest, RecordingFilter, RecordingSort, RecordingSortField,
    RecordingSummary, SortDirection, TranscriptionStatus,
};

/// Rows requested per page (spec §5.2). Pages are appended until the backend
/// stops returning a cursor.
pub const PAGE_SIZE: u32 = 100;

/// How long the search field waits after the last keystroke before the query
/// is issued, so typing does not send one command per character.
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);

/// The sortable columns, in the order the header renders them.
pub const SORT_FIELDS: [RecordingSortField; 4] = [
    RecordingSortField::RecordedAt,
    RecordingSortField::ImportedAt,
    RecordingSortField::Title,
    RecordingSortField::DurationMs,
];

/// The source values the filter offers, with the label each one carries.
///
/// The values are the wire strings `validate::source_kinds` accepts; the
/// labels are the ones the screen being replaced showed.
pub const SOURCE_OPTIONS: [(&str, &str); 4] = [
    ("manual_import", "Manual import"),
    ("watched_folder", "Watched folder"),
    ("plaud_cloud", "PLAUD Cloud"),
    ("plaud_device", "PLAUD device"),
];

/// Import statuses the filter offers.
pub const IMPORT_STATUS_OPTIONS: [ImportStatus; 4] = [
    ImportStatus::Pending,
    ImportStatus::Importing,
    ImportStatus::Imported,
    ImportStatus::Failed,
];

/// Transcription statuses the filter offers.
pub const TRANSCRIPTION_STATUS_OPTIONS: [TranscriptionStatus; 6] = [
    TranscriptionStatus::None,
    TranscriptionStatus::Queued,
    TranscriptionStatus::Running,
    TranscriptionStatus::Completed,
    TranscriptionStatus::Failed,
    TranscriptionStatus::Stale,
];

/// Everything the user can change about the library listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryQuery {
    /// Free text, matched against the title by the backend.
    pub query: String,
    /// Import statuses to include; empty means all.
    pub import_status: Vec<ImportStatus>,
    /// Transcription statuses to include; empty means all.
    pub transcription_status: Vec<TranscriptionStatus>,
    /// Source kinds to include; empty means all.
    pub sources: Vec<String>,
    /// Tags to include; empty means all.
    pub tags: Vec<String>,
    /// Calendar day (`YYYY-MM-DD`) the user typed, not an instant.
    pub date_from: Option<String>,
    /// Calendar day (`YYYY-MM-DD`) the user typed, not an instant.
    pub date_to: Option<String>,
    /// Column the list is ordered by.
    pub sort_field: RecordingSortField,
    /// Direction of that order.
    pub sort_direction: SortDirection,
}

impl Default for LibraryQuery {
    fn default() -> Self {
        Self {
            query: String::new(),
            import_status: Vec::new(),
            transcription_status: Vec::new(),
            sources: Vec::new(),
            tags: Vec::new(),
            date_from: None,
            date_to: None,
            sort_field: RecordingSortField::RecordedAt,
            sort_direction: SortDirection::Desc,
        }
    }
}

/// Identity of everything that invalidates pagination.
///
/// Two states with the same signature may continue the same cursor chain;
/// anything else has to restart from the first page, because the backend's
/// cursor is only meaningful within one ordering of one filtered set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuerySignature {
    filter: RecordingFilter,
    sort: RecordingSort,
}

/// One change to the query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryAction {
    /// Replace the free-text query.
    Query(String),
    /// Keep only this import status, or clear the filter.
    ImportStatus(Option<ImportStatus>),
    /// Keep only this transcription status, or clear the filter.
    TranscriptionStatus(Option<TranscriptionStatus>),
    /// Keep only this source kind, or clear the filter.
    Source(Option<String>),
    /// Keep only this tag, or clear the filter.
    Tag(Option<String>),
    /// Inclusive lower calendar-day bound, or none.
    DateFrom(Option<String>),
    /// Inclusive upper calendar-day bound, or none.
    DateTo(Option<String>),
    /// Sort by this column, toggling the direction when it is already active.
    Sort(RecordingSortField),
    /// Clear every filter, keeping the sort.
    ClearFilters,
}

/// Direction a freshly chosen column starts in: newest or longest first for
/// the quantitative columns, A→Z for the text one.
#[must_use]
pub const fn default_direction_for(field: RecordingSortField) -> SortDirection {
    match field {
        RecordingSortField::Title => SortDirection::Asc,
        _ => SortDirection::Desc,
    }
}

impl LibraryQuery {
    /// Apply one change.
    pub fn apply(&mut self, action: QueryAction) {
        match action {
            QueryAction::Query(value) => self.query = value,
            QueryAction::ImportStatus(value) => {
                self.import_status = value.into_iter().collect();
            }
            QueryAction::TranscriptionStatus(value) => {
                self.transcription_status = value.into_iter().collect();
            }
            QueryAction::Source(value) => self.sources = value.into_iter().collect(),
            QueryAction::Tag(value) => self.tags = value.into_iter().collect(),
            QueryAction::DateFrom(value) => self.date_from = value.filter(|v| !v.is_empty()),
            QueryAction::DateTo(value) => self.date_to = value.filter(|v| !v.is_empty()),
            QueryAction::Sort(field) => {
                if self.sort_field == field {
                    self.sort_direction = match self.sort_direction {
                        SortDirection::Asc => SortDirection::Desc,
                        SortDirection::Desc => SortDirection::Asc,
                    };
                } else {
                    self.sort_field = field;
                    self.sort_direction = default_direction_for(field);
                }
            }
            QueryAction::ClearFilters => {
                self.query.clear();
                self.import_status.clear();
                self.transcription_status.clear();
                self.sources.clear();
                self.tags.clear();
                self.date_from = None;
                self.date_to = None;
            }
        }
    }

    /// Whether anything is filtering the list.
    #[must_use]
    pub fn has_active_filters(&self) -> bool {
        !self.query.trim().is_empty()
            || !self.import_status.is_empty()
            || !self.transcription_status.is_empty()
            || !self.sources.is_empty()
            || !self.tags.is_empty()
            || self.date_from.is_some()
            || self.date_to.is_some()
    }

    /// The filter this query sends.
    #[must_use]
    pub fn filter(&self) -> RecordingFilter {
        let trimmed = self.query.trim();
        RecordingFilter {
            query: (!trimmed.is_empty()).then(|| trimmed.to_owned()),
            import_status: self.import_status.clone(),
            transcription_status: self.transcription_status.clone(),
            sources: self.sources.clone(),
            tags: self.tags.clone(),
            date_from: date_bound_from(self.date_from.as_deref()),
            date_to: date_bound_to(self.date_to.as_deref()),
        }
    }

    /// The sort this query sends.
    #[must_use]
    pub const fn sort(&self) -> RecordingSort {
        RecordingSort {
            field: self.sort_field,
            direction: self.sort_direction,
        }
    }

    /// The pagination identity of this query.
    #[must_use]
    pub fn signature(&self) -> QuerySignature {
        QuerySignature {
            filter: self.filter(),
            sort: self.sort(),
        }
    }

    /// The request for one page, continuing `cursor` when there is one.
    #[must_use]
    pub fn list_request(&self, cursor: Option<String>, limit: u32) -> ListRecordingsRequest {
        ListRecordingsRequest {
            cursor,
            limit,
            filter: self.filter(),
            sort: self.sort(),
        }
    }
}

/// The Inbox's import branch: everything that has not finished importing.
#[must_use]
pub fn inbox_import_query() -> LibraryQuery {
    LibraryQuery {
        import_status: vec![
            ImportStatus::Pending,
            ImportStatus::Importing,
            ImportStatus::Failed,
        ],
        sort_field: RecordingSortField::ImportedAt,
        sort_direction: SortDirection::Desc,
        ..LibraryQuery::default()
    }
}

/// The Inbox's work branch: everything whose transcription has not finished.
#[must_use]
pub fn inbox_work_query() -> LibraryQuery {
    LibraryQuery {
        transcription_status: vec![
            TranscriptionStatus::None,
            TranscriptionStatus::Queued,
            TranscriptionStatus::Running,
        ],
        sort_field: RecordingSortField::ImportedAt,
        sort_direction: SortDirection::Desc,
        ..LibraryQuery::default()
    }
}

/// Union of the Inbox's two branches, de-duplicated by recording identity.
///
/// `RecordingFilter` cannot express "import unfinished **or** transcription
/// unfinished", so the screen issues two queries and unions them here.
#[must_use]
pub fn merge_inbox_rows(branches: &[&[RecordingSummary]]) -> Vec<RecordingSummary> {
    let mut seen = BTreeSet::new();
    let mut merged = Vec::new();
    for branch in branches {
        for row in *branch {
            if seen.insert(row.recording_id) {
                merged.push(row.clone());
            }
        }
    }
    merged
}

/// Sort key of one row, or `None` when the fact was never measured.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum SortKey {
    Text(String),
    Number(u64),
}

fn sort_key(row: &RecordingSummary, field: RecordingSortField) -> Option<SortKey> {
    match field {
        RecordingSortField::Title => Some(SortKey::Text(row.title.to_lowercase())),
        RecordingSortField::DurationMs => row.duration_ms.map(SortKey::Number),
        RecordingSortField::RecordedAt => row.recorded_at.clone().map(SortKey::Text),
        RecordingSortField::ImportedAt => Some(SortKey::Text(row.imported_at.clone())),
    }
}

/// Order a client-side union the way the backend orders a single query, so the
/// header's reported sort always describes what is actually displayed.
///
/// An unknown value sorts last in **both** directions: a recording whose
/// duration was never measured is not a zero-length recording.
pub fn sort_rows(rows: &mut [RecordingSummary], sort: RecordingSort) {
    rows.sort_by(|left, right| {
        let a = sort_key(left, sort.field);
        let b = sort_key(right, sort.field);
        let by_id = left.recording_id.cmp(&right.recording_id);
        match (a, b) {
            (None, None) => by_id,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (Some(a), Some(b)) => match a.cmp(&b) {
                std::cmp::Ordering::Equal => by_id,
                ordering => match sort.direction {
                    SortDirection::Asc => ordering,
                    SortDirection::Desc => ordering.reverse(),
                },
            },
        }
    });
}

/// Append a page, keeping arrival order and dropping identities already held.
///
/// Rows can repeat when the library changes between two page fetches; a
/// recording must never render twice and must never be lost.
pub fn append_page(rows: &mut Vec<RecordingSummary>, items: Vec<RecordingSummary>) {
    let mut seen: BTreeSet<_> = rows.iter().map(|row| row.recording_id).collect();
    for item in items {
        if seen.insert(item.recording_id) {
            rows.push(item);
        }
    }
}

/// Tag choices the toolbar offers: whatever the loaded rows actually carry,
/// plus whatever is already selected so the active filter stays selectable.
#[must_use]
pub fn tag_options(rows: &[RecordingSummary], selected: &[String]) -> Vec<String> {
    let mut tags: BTreeSet<&str> = selected.iter().map(String::as_str).collect();
    for row in rows {
        for tag in &row.tags {
            tags.insert(tag.as_str());
        }
    }
    tags.into_iter().map(str::to_owned).collect()
}

/// Inclusive RFC 3339 lower bound for a calendar day.
#[must_use]
pub fn date_bound_from(day: Option<&str>) -> Option<String> {
    calendar_day(day).map(|day| format!("{day}T00:00:00Z"))
}

/// Inclusive RFC 3339 upper bound for a calendar day.
#[must_use]
pub fn date_bound_to(day: Option<&str>) -> Option<String> {
    calendar_day(day).map(|day| format!("{day}T23:59:59Z"))
}

fn calendar_day(day: Option<&str>) -> Option<&str> {
    let day = day?;
    let bytes = day.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit());
    shaped.then_some(day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::library::testing::{row, row_with};

    #[test]
    fn the_default_query_is_newest_recordings_first_and_unfiltered() {
        let query = LibraryQuery::default();
        assert_eq!(query.sort_field, RecordingSortField::RecordedAt);
        assert_eq!(query.sort_direction, SortDirection::Desc);
        assert!(!query.has_active_filters());
        assert_eq!(query.filter().query, None);
    }

    #[test]
    fn choosing_a_new_column_uses_its_default_direction_and_re_choosing_toggles() {
        let mut query = LibraryQuery::default();
        query.apply(QueryAction::Sort(RecordingSortField::Title));
        assert_eq!(query.sort_direction, SortDirection::Asc);
        query.apply(QueryAction::Sort(RecordingSortField::Title));
        assert_eq!(query.sort_direction, SortDirection::Desc);
        query.apply(QueryAction::Sort(RecordingSortField::DurationMs));
        assert_eq!(query.sort_direction, SortDirection::Desc);
    }

    #[test]
    fn a_whitespace_only_query_is_not_a_filter() {
        let mut query = LibraryQuery::default();
        query.apply(QueryAction::Query("   ".to_owned()));
        assert!(!query.has_active_filters());
        assert_eq!(query.filter().query, None);
    }

    #[test]
    fn clearing_filters_keeps_the_sort() {
        let mut query = LibraryQuery::default();
        query.apply(QueryAction::Sort(RecordingSortField::Title));
        query.apply(QueryAction::Query("board".to_owned()));
        query.apply(QueryAction::Tag(Some("work".to_owned())));
        query.apply(QueryAction::ClearFilters);
        assert!(!query.has_active_filters());
        assert_eq!(query.sort_field, RecordingSortField::Title);
        assert_eq!(query.sort_direction, SortDirection::Asc);
    }

    #[test]
    fn only_a_filter_or_sort_change_invalidates_the_cursor_chain() {
        let mut query = LibraryQuery::default();
        let before = query.signature();
        query.apply(QueryAction::Query("  ".to_owned()));
        assert_eq!(before, query.signature(), "whitespace is not a filter");
        query.apply(QueryAction::Query("board".to_owned()));
        assert_ne!(before, query.signature());
        let filtered = query.signature();
        query.apply(QueryAction::Sort(RecordingSortField::Title));
        assert_ne!(filtered, query.signature());
    }

    #[test]
    fn a_calendar_day_becomes_an_inclusive_instant_and_anything_else_becomes_nothing() {
        assert_eq!(
            date_bound_from(Some("2026-03-12")).as_deref(),
            Some("2026-03-12T00:00:00Z")
        );
        assert_eq!(
            date_bound_to(Some("2026-03-12")).as_deref(),
            Some("2026-03-12T23:59:59Z")
        );
        for bad in ["2026-3-12", "12/03/2026", "", "2026-03-12T00:00:00Z"] {
            assert_eq!(date_bound_from(Some(bad)), None, "{bad} was accepted");
        }
    }

    #[test]
    fn an_unmeasured_duration_sorts_last_in_both_directions() {
        let mut rows = vec![
            row_with("Known", |row| row.duration_ms = Some(1_000)),
            row_with("Unknown", |row| row.duration_ms = None),
            row_with("Longer", |row| row.duration_ms = Some(9_000)),
        ];
        sort_rows(
            &mut rows,
            RecordingSort {
                field: RecordingSortField::DurationMs,
                direction: SortDirection::Asc,
            },
        );
        assert_eq!(
            rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
            ["Known", "Longer", "Unknown"]
        );
        sort_rows(
            &mut rows,
            RecordingSort {
                field: RecordingSortField::DurationMs,
                direction: SortDirection::Desc,
            },
        );
        assert_eq!(
            rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
            ["Longer", "Known", "Unknown"]
        );
    }

    #[test]
    fn titles_sort_case_insensitively() {
        let mut rows = vec![row("beta"), row("Alpha"), row("gamma")];
        sort_rows(
            &mut rows,
            RecordingSort {
                field: RecordingSortField::Title,
                direction: SortDirection::Asc,
            },
        );
        assert_eq!(
            rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
            ["Alpha", "beta", "gamma"]
        );
    }

    #[test]
    fn appending_a_page_never_repeats_and_never_loses_a_recording() {
        let first = row("First");
        let second = row("Second");
        let mut rows = vec![first.clone()];
        append_page(&mut rows, vec![first.clone(), second.clone()]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].recording_id, first.recording_id);
        assert_eq!(rows[1].recording_id, second.recording_id);
    }

    #[test]
    fn the_inbox_union_keeps_the_first_sighting_of_each_recording() {
        let shared = row("Shared");
        let only_work = row("Work");
        let merged = merge_inbox_rows(&[
            std::slice::from_ref(&shared),
            &[shared.clone(), only_work.clone()][..],
        ]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].recording_id, shared.recording_id);
        assert_eq!(merged[1].recording_id, only_work.recording_id);
    }

    #[test]
    fn tag_options_are_sorted_unique_and_keep_the_active_filter() {
        let rows = vec![
            row_with("One", |row| row.tags = vec!["work".into(), "ops".into()]),
            row_with("Two", |row| row.tags = vec!["work".into()]),
        ];
        let options = tag_options(&rows, &["archive".to_owned()]);
        assert_eq!(options, ["archive", "ops", "work"]);
    }

    #[test]
    fn the_inbox_branches_ask_for_the_two_unfinished_conditions() {
        assert_eq!(
            inbox_import_query().import_status,
            [
                ImportStatus::Pending,
                ImportStatus::Importing,
                ImportStatus::Failed
            ]
        );
        assert_eq!(
            inbox_work_query().transcription_status,
            [
                TranscriptionStatus::None,
                TranscriptionStatus::Queued,
                TranscriptionStatus::Running
            ]
        );
    }

    #[test]
    fn a_page_request_carries_the_cursor_unchanged() {
        let query = LibraryQuery::default();
        let request = query.list_request(Some("0a1b".to_owned()), PAGE_SIZE);
        assert_eq!(request.cursor.as_deref(), Some("0a1b"));
        assert_eq!(request.limit, PAGE_SIZE);
    }
}
