import { jsx as _jsx, Fragment as _Fragment, jsxs as _jsxs } from "react/jsx-runtime";
/**
 * Inbox (§5.1): recordings that just arrived or still have work pending.
 *
 * The condition is "import not finished OR transcription not started/running",
 * which `RecordingFilter` cannot express as one request, so two cursor-paged
 * queries run side by side and `mergeInboxRows` unions them by recording id.
 */
import { EmptyState } from "../../components/EmptyState";
import { Button } from "../../components/Button";
import { useAppState } from "../../app/AppState";
import { useImport } from "../import/useImport";
import { ErrorNotice } from "./ErrorNotice";
import { RecordingTable } from "./RecordingTable";
import { useRecordingList } from "./useRecordingList";
import { INBOX_IMPORT_QUERY, INBOX_WORK_QUERY, mergeInboxRows, sortRows } from "./libraryQuery";
import "./library.css";
export function InboxScreen({ onOpenRecording, }) {
    const imports = useRecordingList(INBOX_IMPORT_QUERY);
    const work = useRecordingList(INBOX_WORK_QUERY);
    const { navigate } = useAppState();
    const { pickFiles } = useImport();
    const rows = sortRows(mergeInboxRows(imports.rows, work.rows), imports.state);
    const error = imports.error ?? work.error;
    const loading = (imports.loading || work.loading) && rows.length === 0;
    const hasMore = imports.hasMore || work.hasMore;
    if (error) {
        return (_jsx("div", { className: "library", children: _jsx(ErrorNotice, { envelope: error.envelope, onRetry: () => {
                    imports.reload();
                    work.reload();
                }, context: "The inbox could not be loaded." }) }));
    }
    if (loading) {
        return (_jsx("div", { className: "library", children: _jsx("p", { className: "library__status", role: "status", children: "Loading the inbox\u2026" }) }));
    }
    if (rows.length === 0) {
        return (_jsx("div", { className: "library", children: _jsx(EmptyState, { title: "Nothing waiting", body: "Recordings appear here while they are importing or waiting to be transcribed. Finished recordings live in Library.", actions: _jsxs(_Fragment, { children: [_jsx(Button, { variant: "primary", onClick: () => void pickFiles(), children: "Import an audio file" }), _jsx(Button, { onClick: () => navigate({ name: "library" }), children: "Go to Library" })] }) }) }));
    }
    return (_jsx("div", { className: "library", children: _jsx(RecordingTable, { label: "Inbox recordings", rows: rows, rowCount: rows.length, sort: imports.state, onSort: (field) => {
                imports.dispatch({ type: "sort", field });
                work.dispatch({ type: "sort", field });
            }, onOpenRecording: onOpenRecording, hasMore: hasMore, loadingMore: imports.loadingMore || work.loadingMore, onLoadMore: () => {
                if (imports.hasMore)
                    imports.loadMore();
                if (work.hasMore)
                    work.loadMore();
            } }) }));
}
