/** §5.2 list page size; pages are appended until `next_cursor` is null. */
export const PAGE_SIZE = 100;
/** Search is debounced so typing does not issue a command per keystroke. */
export const SEARCH_DEBOUNCE_MS = 200;
export const SORT_FIELDS = [
    "recorded_at",
    "imported_at",
    "title",
    "duration_ms",
];
export const INITIAL_LIBRARY_QUERY = {
    query: "",
    importStatus: [],
    transcriptionStatus: [],
    sources: [],
    tags: [],
    dateFrom: null,
    dateTo: null,
    sortField: "recorded_at",
    sortDirection: "desc",
};
export function createLibraryQuery(overrides = {}) {
    return { ...INITIAL_LIBRARY_QUERY, ...overrides };
}
/* ------------------------------------------------------------------ inbox */
/**
 * The Inbox (§5.1) is "recently imported or pending processing". That is an OR
 * across two filter fields, which `RecordingFilter` cannot express, so it is
 * assembled from two requests and merged client-side by `mergeInboxRows`.
 */
export const INBOX_IMPORT_QUERY = createLibraryQuery({
    importStatus: ["pending", "importing", "failed"],
    sortField: "imported_at",
    sortDirection: "desc",
});
export const INBOX_WORK_QUERY = createLibraryQuery({
    transcriptionStatus: ["none", "queued", "running"],
    sortField: "imported_at",
    sortDirection: "desc",
});
/** Union of the two Inbox branches, de-duplicated by recording id. */
export function mergeInboxRows(...branches) {
    const byId = new Map();
    for (const branch of branches) {
        for (const row of branch) {
            if (!byId.has(row.recording_id))
                byId.set(row.recording_id, row);
        }
    }
    return [...byId.values()];
}
/**
 * Orders a client-side union the same way the backend orders a single query,
 * so the header's `aria-sort` always describes what is actually displayed.
 * Unknown values (`null`) sort last in both directions — they are not zero.
 */
export function sortRows(rows, sort) {
    const factor = sort.sortDirection === "asc" ? 1 : -1;
    return [...rows].sort((a, b) => {
        const left = sortKey(a, sort.sortField);
        const right = sortKey(b, sort.sortField);
        if (left === null && right === null)
            return a.recording_id < b.recording_id ? -1 : 1;
        if (left === null)
            return 1;
        if (right === null)
            return -1;
        if (left === right)
            return a.recording_id < b.recording_id ? -1 : 1;
        return left < right ? -factor : factor;
    });
}
function sortKey(row, field) {
    switch (field) {
        case "title":
            return row.title.toLowerCase();
        case "duration_ms":
            return row.duration_ms;
        case "recorded_at":
            return row.recorded_at;
        case "imported_at":
            return row.imported_at;
        default:
            return null;
    }
}
/** Default direction for a freshly chosen column: newest/longest first, A→Z for text. */
export function defaultDirectionFor(field) {
    return field === "title" ? "asc" : "desc";
}
export function libraryQueryReducer(state, action) {
    switch (action.type) {
        case "query":
            return { ...state, query: action.value };
        case "import-status":
            return { ...state, importStatus: action.value ? [action.value] : [] };
        case "transcription-status":
            return { ...state, transcriptionStatus: action.value ? [action.value] : [] };
        case "source":
            return { ...state, sources: action.value ? [action.value] : [] };
        case "tag":
            return { ...state, tags: action.value ? [action.value] : [] };
        case "date-from":
            return { ...state, dateFrom: action.value || null };
        case "date-to":
            return { ...state, dateTo: action.value || null };
        case "sort":
            return state.sortField === action.field
                ? { ...state, sortDirection: state.sortDirection === "asc" ? "desc" : "asc" }
                : { ...state, sortField: action.field, sortDirection: defaultDirectionFor(action.field) };
        case "clear-filters":
            return {
                ...state,
                query: "",
                importStatus: [],
                transcriptionStatus: [],
                sources: [],
                tags: [],
                dateFrom: null,
                dateTo: null,
            };
        default: {
            const never = action;
            throw new Error(`unknown library query action: ${JSON.stringify(never)}`);
        }
    }
}
export function hasActiveFilters(state) {
    return (state.query.trim().length > 0 ||
        state.importStatus.length > 0 ||
        state.transcriptionStatus.length > 0 ||
        state.sources.length > 0 ||
        state.tags.length > 0 ||
        state.dateFrom !== null ||
        state.dateTo !== null);
}
/* ---------------------------------------------------------------- requests */
/** Inclusive RFC3339 lower bound for a calendar day. */
export function dateBoundFrom(day) {
    return isCalendarDay(day) ? `${day}T00:00:00Z` : null;
}
/** Inclusive RFC3339 upper bound for a calendar day. */
export function dateBoundTo(day) {
    return isCalendarDay(day) ? `${day}T23:59:59Z` : null;
}
function isCalendarDay(day) {
    return typeof day === "string" && /^\d{4}-\d{2}-\d{2}$/.test(day);
}
export function buildFilter(state) {
    const query = state.query.trim();
    return {
        query: query.length > 0 ? query : null,
        import_status: [...state.importStatus],
        transcription_status: [...state.transcriptionStatus],
        sources: [...state.sources],
        tags: [...state.tags],
        date_from: dateBoundFrom(state.dateFrom),
        date_to: dateBoundTo(state.dateTo),
    };
}
export function buildListRequest(state, cursor, limit = PAGE_SIZE) {
    return {
        cursor,
        limit,
        filter: buildFilter(state),
        sort: { field: state.sortField, direction: state.sortDirection },
    };
}
/**
 * Identity of everything that invalidates pagination. Two states with the same
 * signature may continue the same cursor chain; anything else must restart.
 */
export function querySignature(state) {
    const filter = buildFilter(state);
    return JSON.stringify([
        filter.query,
        filter.import_status,
        filter.transcription_status,
        filter.sources,
        filter.tags,
        filter.date_from,
        filter.date_to,
        state.sortField,
        state.sortDirection,
    ]);
}
/** A filter or sort change resets the cursor; nothing else does. */
export function shouldResetCursor(previous, next) {
    return querySignature(previous) !== querySignature(next);
}
/**
 * Appends a page, keeping arrival order and dropping ids we already hold. Rows
 * can repeat when the library changes between page fetches; an id must never
 * render twice and must never be lost.
 */
export function appendPage(rows, items) {
    const seen = new Set(rows.map((row) => row.recording_id));
    const merged = [...rows];
    for (const item of items) {
        if (seen.has(item.recording_id))
            continue;
        seen.add(item.recording_id);
        merged.push(item);
    }
    return merged;
}
export function ariaSortFor(state, field) {
    if (state.sortField !== field)
        return "none";
    return state.sortDirection === "asc" ? "ascending" : "descending";
}
/** §8.6A: an unknown duration is stated, never rendered as 0:00. */
export function formatDuration(durationMs) {
    if (durationMs === null || !Number.isFinite(durationMs) || durationMs < 0)
        return "Unknown";
    const totalSeconds = Math.floor(durationMs / 1000);
    const seconds = totalSeconds % 60;
    const minutes = Math.floor(totalSeconds / 60) % 60;
    const hours = Math.floor(totalSeconds / 3600);
    const mm = hours > 0 ? String(minutes).padStart(2, "0") : String(minutes);
    return `${hours > 0 ? `${hours}:` : ""}${mm}:${String(seconds).padStart(2, "0")}`;
}
const DATE_TIME_FORMAT = new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
});
/** A missing timestamp is "Unknown"; we never substitute "now" or the epoch. */
export function formatDateTime(iso) {
    if (!iso)
        return "Unknown";
    const date = new Date(iso);
    if (Number.isNaN(date.getTime()))
        return "Unknown";
    return DATE_TIME_FORMAT.format(date);
}
export const IMPORT_STATUS_BADGE = {
    pending: { tone: "waiting", glyph: "◷", label: "Pending", description: "Waiting to import" },
    importing: { tone: "running", glyph: "⟳", label: "Importing", description: "Copying into the library" },
    imported: { tone: "success", glyph: "✓", label: "Imported", description: "In the library" },
    failed: { tone: "danger", glyph: "✕", label: "Import failed", description: "Import did not finish" },
};
export const TRANSCRIPTION_STATUS_BADGE = {
    none: { tone: "neutral", glyph: "–", label: "Not transcribed", description: "No transcript yet" },
    queued: { tone: "waiting", glyph: "◷", label: "Queued", description: "Waiting for a transcription slot" },
    running: { tone: "running", glyph: "⟳", label: "Transcribing", description: "Transcription in progress" },
    completed: { tone: "success", glyph: "✓", label: "Transcribed", description: "Transcript available" },
    failed: { tone: "danger", glyph: "✕", label: "Transcription failed", description: "Transcription did not finish" },
    stale: { tone: "warning", glyph: "!", label: "Stale", description: "Audio changed after this transcript" },
};
export const SOURCE_OPTIONS = [
    { value: "manual_import", label: "Manual import" },
    { value: "watched_folder", label: "Watched folder" },
    { value: "plaud_cloud", label: "PLAUD Cloud" },
    { value: "plaud_device", label: "PLAUD device" },
];
export const IMPORT_STATUS_OPTIONS = [
    "pending",
    "importing",
    "imported",
    "failed",
];
export const TRANSCRIPTION_STATUS_OPTIONS = [
    "none",
    "queued",
    "running",
    "completed",
    "failed",
    "stale",
];
/** Narrows a `<select>` value back to the contract union; "" means no filter. */
export function parseImportStatus(value) {
    return IMPORT_STATUS_OPTIONS.find((status) => status === value) ?? null;
}
export function parseTranscriptionStatus(value) {
    return TRANSCRIPTION_STATUS_OPTIONS.find((status) => status === value) ?? null;
}
/** Tag choices offered by the toolbar: whatever the loaded rows actually carry. */
export function tagOptions(rows, selected) {
    const tags = new Set(selected);
    for (const row of rows)
        for (const tag of row.tags)
            tags.add(tag);
    return [...tags].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
}
/** Ids covered by a Shift range selection over the loaded rows. */
export function rangeIds(rows, anchor, focus) {
    const [start, end] = anchor <= focus ? [anchor, focus] : [focus, anchor];
    const ids = [];
    for (let i = Math.max(0, start); i <= Math.min(rows.length - 1, end); i += 1) {
        const row = rows[i];
        if (row)
            ids.push(row.recording_id);
    }
    return ids;
}
export function describeSelection(count, loaded) {
    if (count === 0)
        return `No recordings selected. ${loaded} loaded.`;
    return `${count} of ${loaded} loaded recordings selected.`;
}
