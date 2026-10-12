/**
 * Library (§5.2): searchable, filterable, sortable, virtualized recordings.
 *
 * Recording detail is opened through `onOpenRecording`, never rendered here:
 * detail stays inside Library and is not a top-level destination (§5.1).
 */
import { useEffect, useState } from "react";
import { Button } from "../../components/Button";
import { EmptyState } from "../../components/EmptyState";
import { SelectField, TextField } from "../../components/Field";
import { useAppState } from "../../app/AppState";
import { useImport } from "../import/useImport";
import { ErrorNotice } from "./ErrorNotice";
import { RecordingTable } from "./RecordingTable";
import { useRecordingList } from "./useRecordingList";
import {
  IMPORT_STATUS_BADGE,
  IMPORT_STATUS_OPTIONS,
  INITIAL_LIBRARY_QUERY,
  SEARCH_DEBOUNCE_MS,
  SOURCE_OPTIONS,
  TRANSCRIPTION_STATUS_BADGE,
  TRANSCRIPTION_STATUS_OPTIONS,
  hasActiveFilters,
  parseImportStatus,
  parseTranscriptionStatus,
  tagOptions,
} from "./libraryQuery";
import "./library.css";

export function LibraryScreen({
  onOpenRecording,
}: {
  onOpenRecording: (recordingId: string) => void;
}) {
  const list = useRecordingList(INITIAL_LIBRARY_QUERY);
  const { navigate } = useAppState();
  const { pickFiles } = useImport();
  const { state, dispatch } = list;
  const [searchText, setSearchText] = useState(state.query);

  useEffect(() => {
    if (searchText === state.query) return;
    const timer = setTimeout(
      () => dispatch({ type: "query", value: searchText }),
      SEARCH_DEBOUNCE_MS,
    );
    return () => clearTimeout(timer);
  }, [searchText, state.query, dispatch]);

  const filtered = hasActiveFilters(state);

  return (
    <div className="library">
      <form
        className="library__toolbar"
        role="search"
        aria-label="Filter recordings"
        onSubmit={(event) => event.preventDefault()}
      >
        <div className="library__search">
          <TextField
            label="Search recordings"
            type="search"
            value={searchText}
            onChange={(event) => setSearchText(event.target.value)}
          />
        </div>
        <SelectField
          label="Import status"
          value={state.importStatus[0] ?? ""}
          onChange={(event) =>
            dispatch({ type: "import-status", value: parseImportStatus(event.target.value) })
          }
        >
          <option value="">All import states</option>
          {IMPORT_STATUS_OPTIONS.map((status) => (
            <option key={status} value={status}>
              {IMPORT_STATUS_BADGE[status].label}
            </option>
          ))}
        </SelectField>
        <SelectField
          label="Transcription status"
          value={state.transcriptionStatus[0] ?? ""}
          onChange={(event) =>
            dispatch({
              type: "transcription-status",
              value: parseTranscriptionStatus(event.target.value),
            })
          }
        >
          <option value="">All transcription states</option>
          {TRANSCRIPTION_STATUS_OPTIONS.map((status) => (
            <option key={status} value={status}>
              {TRANSCRIPTION_STATUS_BADGE[status].label}
            </option>
          ))}
        </SelectField>
        <SelectField
          label="Source"
          value={state.sources[0] ?? ""}
          onChange={(event) => dispatch({ type: "source", value: event.target.value || null })}
        >
          <option value="">All sources</option>
          {SOURCE_OPTIONS.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </SelectField>
        <SelectField
          label="Tag"
          value={state.tags[0] ?? ""}
          onChange={(event) => dispatch({ type: "tag", value: event.target.value || null })}
        >
          <option value="">All tags</option>
          {tagOptions(list.rows, state.tags).map((tag) => (
            <option key={tag} value={tag}>
              {tag}
            </option>
          ))}
        </SelectField>
        <TextField
          label="From date"
          type="date"
          value={state.dateFrom ?? ""}
          onChange={(event) => dispatch({ type: "date-from", value: event.target.value })}
        />
        <TextField
          label="To date"
          type="date"
          value={state.dateTo ?? ""}
          onChange={(event) => dispatch({ type: "date-to", value: event.target.value })}
        />
        {filtered ? (
          <Button
            className="library__clear"
            onClick={() => {
              setSearchText("");
              dispatch({ type: "clear-filters" });
            }}
          >
            Clear filters
          </Button>
        ) : null}
      </form>

      {list.error ? (
        <ErrorNotice
          envelope={list.error.envelope}
          onRetry={list.reload}
          context="Recordings could not be loaded."
        />
      ) : list.loading ? (
        <p className="library__status" role="status">
          Loading recordings…
        </p>
      ) : list.rows.length === 0 ? (
        filtered ? (
          <EmptyState
            title="No recordings match these filters"
            body="Nothing in the library matches the current search, status, source, tag or date range."
            actions={
              <Button
                variant="primary"
                onClick={() => {
                  setSearchText("");
                  dispatch({ type: "clear-filters" });
                }}
              >
                Clear filters
              </Button>
            }
          />
        ) : (
          <EmptyState
            title="No recordings"
            body="Import audio from this computer to get started. Direct PLAUD device import is not part of this build; Settings lists the sources that are supported."
            actions={
              <>
                <Button variant="primary" onClick={() => void pickFiles()}>
                  Import an audio file
                </Button>
                <Button onClick={() => navigate({ name: "settings" })}>Connect PLAUD</Button>
              </>
            }
          />
        )
      ) : (
        <RecordingTable
          label="Recordings"
          rows={list.rows}
          rowCount={list.total}
          sort={state}
          onSort={(field) => dispatch({ type: "sort", field })}
          onOpenRecording={onOpenRecording}
          hasMore={list.hasMore}
          loadingMore={list.loadingMore}
          onLoadMore={list.loadMore}
        />
      )}
    </div>
  );
}
