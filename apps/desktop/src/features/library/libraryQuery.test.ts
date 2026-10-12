import { describe, expect, it } from "vitest";
import type { RecordingPage, RecordingSummary } from "../../ipc/contract";
import { invokeCommand, setTransport } from "../../ipc/client";
import { FakeBackend } from "../../testing/fakeBackend";
import {
  INBOX_IMPORT_QUERY,
  INBOX_WORK_QUERY,
  PAGE_SIZE,
  appendPage,
  ariaSortFor,
  buildListRequest,
  createLibraryQuery,
  dateBoundFrom,
  dateBoundTo,
  describeSelection,
  formatDateTime,
  formatDuration,
  hasActiveFilters,
  libraryQueryReducer,
  mergeInboxRows,
  parseImportStatus,
  parseTranscriptionStatus,
  rangeIds,
  shouldResetCursor,
  sortRows,
  tagOptions,
} from "./libraryQuery";

function row(overrides: Partial<RecordingSummary> & Pick<RecordingSummary, "recording_id">): RecordingSummary {
  return {
    title: overrides.recording_id,
    recorded_at: "2026-02-01T09:00:00Z",
    imported_at: "2026-02-01T10:00:00Z",
    duration_ms: 60_000,
    source: { source_kind: "manual_import", label: "Manual import" },
    import_status: "imported",
    transcription_status: "none",
    processing_engine: null,
    last_error: null,
    tags: [],
    byte_length: 1024,
    sha256: "a".repeat(64),
    ...overrides,
  };
}

describe("library request assembly", () => {
  it("builds a list request from the query state", () => {
    const state = createLibraryQuery({
      query: "  weekly  ",
      importStatus: ["failed"],
      transcriptionStatus: ["queued"],
      sources: ["manual_import"],
      tags: ["standup"],
      dateFrom: "2026-01-05",
      dateTo: "2026-01-09",
      sortField: "title",
      sortDirection: "asc",
    });
    expect(buildListRequest(state, null)).toEqual({
      cursor: null,
      limit: PAGE_SIZE,
      filter: {
        query: "weekly",
        import_status: ["failed"],
        transcription_status: ["queued"],
        sources: ["manual_import"],
        tags: ["standup"],
        date_from: "2026-01-05T00:00:00Z",
        date_to: "2026-01-09T23:59:59Z",
      },
      sort: { field: "title", direction: "asc" },
    });
  });

  it("uses a page size of 100 and passes the cursor through untouched", () => {
    expect(PAGE_SIZE).toBe(100);
    const request = buildListRequest(createLibraryQuery(), "rec-00042");
    expect(request.cursor).toBe("rec-00042");
    expect(request.limit).toBe(100);
  });

  it("omits an empty query and malformed dates instead of inventing bounds", () => {
    const filter = buildListRequest(createLibraryQuery({ query: "   ", dateFrom: "x" }), null).filter;
    expect(filter.query).toBeNull();
    expect(filter.date_from).toBeNull();
    expect(dateBoundFrom(null)).toBeNull();
    expect(dateBoundTo("2026-01-09")).toBe("2026-01-09T23:59:59Z");
  });

  it("resets the cursor when a filter or sort changes, and only then", () => {
    const base = createLibraryQuery();
    expect(shouldResetCursor(base, createLibraryQuery())).toBe(false);
    expect(shouldResetCursor(base, libraryQueryReducer(base, { type: "query", value: "a" }))).toBe(
      true,
    );
    expect(
      shouldResetCursor(base, libraryQueryReducer(base, { type: "sort", field: "title" })),
    ).toBe(true);
    expect(
      shouldResetCursor(base, libraryQueryReducer(base, { type: "import-status", value: null })),
    ).toBe(false);
  });

  it("toggles direction on the active sort column and defaults per column type", () => {
    const base = createLibraryQuery();
    const byTitle = libraryQueryReducer(base, { type: "sort", field: "title" });
    expect(byTitle).toMatchObject({ sortField: "title", sortDirection: "asc" });
    expect(libraryQueryReducer(byTitle, { type: "sort", field: "title" })).toMatchObject({
      sortDirection: "desc",
    });
    expect(libraryQueryReducer(byTitle, { type: "sort", field: "duration_ms" })).toMatchObject({
      sortField: "duration_ms",
      sortDirection: "desc",
    });
  });

  it("clears every filter but keeps the sort", () => {
    const dirty = createLibraryQuery({
      query: "a",
      tags: ["x"],
      dateTo: "2026-01-01",
      sortField: "title",
      sortDirection: "asc",
    });
    expect(hasActiveFilters(dirty)).toBe(true);
    const cleared = libraryQueryReducer(dirty, { type: "clear-filters" });
    expect(hasActiveFilters(cleared)).toBe(false);
    expect(cleared).toMatchObject({ sortField: "title", sortDirection: "asc" });
  });

  it("narrows select values back to the contract unions", () => {
    expect(parseImportStatus("failed")).toBe("failed");
    expect(parseImportStatus("")).toBeNull();
    expect(parseImportStatus("nonsense")).toBeNull();
    expect(parseTranscriptionStatus("stale")).toBe("stale");
    expect(parseTranscriptionStatus("")).toBeNull();
  });

  it("reports aria-sort only for the active column", () => {
    const state = createLibraryQuery({ sortField: "imported_at", sortDirection: "asc" });
    expect(ariaSortFor(state, "imported_at")).toBe("ascending");
    expect(ariaSortFor(state, "title")).toBe("none");
  });
});

describe("page accumulation", () => {
  it("appends pages in order and never repeats an id", () => {
    const first = [row({ recording_id: "a" }), row({ recording_id: "b" })];
    const second = [row({ recording_id: "b" }), row({ recording_id: "c" })];
    expect(appendPage(first, second).map((r) => r.recording_id)).toEqual(["a", "b", "c"]);
    expect(appendPage([], []).length).toBe(0);
  });

  it("keeps every id exactly once across a cursor walk, including an insert mid-paging", async () => {
    const backend = new FakeBackend({ recordings: 250 });
    setTransport(backend);
    const expected = backend.recordings.map((rec) => rec.recording_id);
    const state = createLibraryQuery();

    let cursor: string | null = null;
    let rows: RecordingSummary[] = [];
    let pages = 0;
    do {
      const page: RecordingPage = await invokeCommand(
        "list_recordings",
        buildListRequest(state, cursor, 40),
      );
      rows = appendPage(rows, page.items);
      cursor = page.next_cursor;
      pages += 1;
      // A fresh import lands while the user is still paging.
      if (pages === 2) backend.addRecording({ title: "Arrived mid-paging" });
    } while (cursor !== null && pages < 100);

    const counts = new Map<string, number>();
    for (const item of rows) {
      counts.set(item.recording_id, (counts.get(item.recording_id) ?? 0) + 1);
    }
    expect([...counts.values()].every((count) => count === 1)).toBe(true);
    for (const id of expected) expect(counts.get(id)).toBe(1);
    expect(pages).toBeGreaterThan(5);
  });

  it("keeps rows unique when a mid-paging insert lands inside a later page", async () => {
    const backend = new FakeBackend({ recordings: 120 });
    setTransport(backend);
    const expected = backend.recordings.map((rec) => rec.recording_id);
    const state = createLibraryQuery({ sortDirection: "asc" });

    let cursor: string | null = null;
    let rows: RecordingSummary[] = [];
    let pages = 0;
    do {
      const page: RecordingPage = await invokeCommand(
        "list_recordings",
        buildListRequest(state, cursor, 25),
      );
      rows = appendPage(rows, page.items);
      cursor = page.next_cursor;
      pages += 1;
      if (pages === 1) backend.addRecording({ title: "Newest while paging" });
    } while (cursor !== null && pages < 100);

    const ids = rows.map((item) => item.recording_id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const id of expected) expect(ids).toContain(id);
    // Ascending by recorded_at, so the new recording sorts last and is seen.
    expect(ids.length).toBe(expected.length + 1);
  });
});

describe("row presentation", () => {
  it("states an unknown duration instead of rendering 0:00", () => {
    expect(formatDuration(null)).toBe("Unknown");
    expect(formatDuration(0)).toBe("0:00");
    expect(formatDuration(61_000)).toBe("1:01");
    expect(formatDuration(3_723_000)).toBe("1:02:03");
  });

  it("states an unknown timestamp instead of inventing one", () => {
    expect(formatDateTime(null)).toBe("Unknown");
    expect(formatDateTime("not-a-date")).toBe("Unknown");
    expect(formatDateTime("2026-02-01T12:00:00Z")).toContain("2026");
  });

  it("offers the tags the loaded rows actually carry, plus the current selection", () => {
    const rows = [row({ recording_id: "a", tags: ["standup", "q1"] }), row({ recording_id: "b" })];
    expect(tagOptions(rows, ["archived"])).toEqual(["archived", "q1", "standup"]);
  });

  it("describes a range selection and announces the count", () => {
    const rows = ["a", "b", "c", "d"].map((id) => row({ recording_id: id }));
    expect(rangeIds(rows, 2, 0)).toEqual(["a", "b", "c"]);
    expect(rangeIds(rows, 0, 99)).toEqual(["a", "b", "c", "d"]);
    expect(describeSelection(0, 4)).toContain("No recordings selected");
    expect(describeSelection(2, 4)).toBe("2 of 4 loaded recordings selected.");
  });
});

describe("inbox union", () => {
  it("filters the two branches the §5.1 definition requires", () => {
    expect(buildListRequest(INBOX_IMPORT_QUERY, null).filter.import_status).toEqual([
      "pending",
      "importing",
      "failed",
    ]);
    expect(buildListRequest(INBOX_WORK_QUERY, null).filter.transcription_status).toEqual([
      "none",
      "queued",
      "running",
    ]);
    expect(buildListRequest(INBOX_IMPORT_QUERY, null).sort).toEqual({
      field: "imported_at",
      direction: "desc",
    });
  });

  it("unions the branches without duplicates, newest import first", () => {
    const a = [row({ recording_id: "a", imported_at: "2026-02-01T10:00:00Z" })];
    const b = [
      row({ recording_id: "a", imported_at: "2026-02-01T10:00:00Z" }),
      row({ recording_id: "b", imported_at: "2026-02-02T10:00:00Z" }),
    ];
    const merged = sortRows(mergeInboxRows(a, b), INBOX_IMPORT_QUERY);
    expect(merged.map((item) => item.recording_id)).toEqual(["b", "a"]);
  });

  it("sorts unknown durations last rather than treating them as zero", () => {
    const rows = [
      row({ recording_id: "a", duration_ms: null }),
      row({ recording_id: "b", duration_ms: 1000 }),
    ];
    const ascending = sortRows(rows, { sortField: "duration_ms", sortDirection: "asc" });
    const descending = sortRows(rows, { sortField: "duration_ms", sortDirection: "desc" });
    expect(ascending.map((item) => item.recording_id)).toEqual(["b", "a"]);
    expect(descending.map((item) => item.recording_id)).toEqual(["b", "a"]);
  });
});
