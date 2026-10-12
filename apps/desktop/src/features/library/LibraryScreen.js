import { jsx as _jsx, jsxs as _jsxs, Fragment as _Fragment } from "react/jsx-runtime";
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
import { IMPORT_STATUS_BADGE, IMPORT_STATUS_OPTIONS, INITIAL_LIBRARY_QUERY, SEARCH_DEBOUNCE_MS, SOURCE_OPTIONS, TRANSCRIPTION_STATUS_BADGE, TRANSCRIPTION_STATUS_OPTIONS, hasActiveFilters, parseImportStatus, parseTranscriptionStatus, tagOptions, } from "./libraryQuery";
import "./library.css";
export function LibraryScreen({ onOpenRecording, }) {
    const list = useRecordingList(INITIAL_LIBRARY_QUERY);
    const { navigate } = useAppState();
    const { pickFiles } = useImport();
    const { state, dispatch } = list;
    const [searchText, setSearchText] = useState(state.query);
    useEffect(() => {
        if (searchText === state.query)
            return;
        const timer = setTimeout(() => dispatch({ type: "query", value: searchText }), SEARCH_DEBOUNCE_MS);
        return () => clearTimeout(timer);
    }, [searchText, state.query, dispatch]);
    const filtered = hasActiveFilters(state);
    return (_jsxs("div", { className: "library", children: [_jsxs("form", { className: "library__toolbar", role: "search", "aria-label": "Filter recordings", onSubmit: (event) => event.preventDefault(), children: [_jsx("div", { className: "library__search", children: _jsx(TextField, { label: "Search recordings", type: "search", value: searchText, onChange: (event) => setSearchText(event.target.value) }) }), _jsxs(SelectField, { label: "Import status", value: state.importStatus[0] ?? "", onChange: (event) => dispatch({ type: "import-status", value: parseImportStatus(event.target.value) }), children: [_jsx("option", { value: "", children: "All import states" }), IMPORT_STATUS_OPTIONS.map((status) => (_jsx("option", { value: status, children: IMPORT_STATUS_BADGE[status].label }, status)))] }), _jsxs(SelectField, { label: "Transcription status", value: state.transcriptionStatus[0] ?? "", onChange: (event) => dispatch({
                            type: "transcription-status",
                            value: parseTranscriptionStatus(event.target.value),
                        }), children: [_jsx("option", { value: "", children: "All transcription states" }), TRANSCRIPTION_STATUS_OPTIONS.map((status) => (_jsx("option", { value: status, children: TRANSCRIPTION_STATUS_BADGE[status].label }, status)))] }), _jsxs(SelectField, { label: "Source", value: state.sources[0] ?? "", onChange: (event) => dispatch({ type: "source", value: event.target.value || null }), children: [_jsx("option", { value: "", children: "All sources" }), SOURCE_OPTIONS.map((option) => (_jsx("option", { value: option.value, children: option.label }, option.value)))] }), _jsxs(SelectField, { label: "Tag", value: state.tags[0] ?? "", onChange: (event) => dispatch({ type: "tag", value: event.target.value || null }), children: [_jsx("option", { value: "", children: "All tags" }), tagOptions(list.rows, state.tags).map((tag) => (_jsx("option", { value: tag, children: tag }, tag)))] }), _jsx(TextField, { label: "From date", type: "date", value: state.dateFrom ?? "", onChange: (event) => dispatch({ type: "date-from", value: event.target.value }) }), _jsx(TextField, { label: "To date", type: "date", value: state.dateTo ?? "", onChange: (event) => dispatch({ type: "date-to", value: event.target.value }) }), filtered ? (_jsx(Button, { className: "library__clear", onClick: () => {
                            setSearchText("");
                            dispatch({ type: "clear-filters" });
                        }, children: "Clear filters" })) : null] }), list.error ? (_jsx(ErrorNotice, { envelope: list.error.envelope, onRetry: list.reload, context: "Recordings could not be loaded." })) : list.loading ? (_jsx("p", { className: "library__status", role: "status", children: "Loading recordings\u2026" })) : list.rows.length === 0 ? (filtered ? (_jsx(EmptyState, { title: "No recordings match these filters", body: "Nothing in the library matches the current search, status, source, tag or date range.", actions: _jsx(Button, { variant: "primary", onClick: () => {
                        setSearchText("");
                        dispatch({ type: "clear-filters" });
                    }, children: "Clear filters" }) })) : (_jsx(EmptyState, { title: "No recordings", body: "Import audio from this computer to get started. Direct PLAUD device import is not part of this build; Settings lists the sources that are supported.", actions: _jsxs(_Fragment, { children: [_jsx(Button, { variant: "primary", onClick: () => void pickFiles(), children: "Import an audio file" }), _jsx(Button, { onClick: () => navigate({ name: "settings" }), children: "Connect PLAUD" })] }) }))) : (_jsx(RecordingTable, { label: "Recordings", rows: list.rows, rowCount: list.total, sort: state, onSort: (field) => dispatch({ type: "sort", field }), onOpenRecording: onOpenRecording, hasMore: list.hasMore, loadingMore: list.loadingMore, onLoadMore: list.loadMore }))] }));
}
