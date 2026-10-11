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

export function InboxScreen({
  onOpenRecording,
}: {
  onOpenRecording: (recordingId: string) => void;
}) {
  const imports = useRecordingList(INBOX_IMPORT_QUERY);
  const work = useRecordingList(INBOX_WORK_QUERY);
  const { navigate } = useAppState();
  const { pickFiles } = useImport();

  const rows = sortRows(mergeInboxRows(imports.rows, work.rows), imports.state);
  const error = imports.error ?? work.error;
  const loading = (imports.loading || work.loading) && rows.length === 0;
  const hasMore = imports.hasMore || work.hasMore;

  if (error) {
    return (
      <div className="library">
        <ErrorNotice
          envelope={error.envelope}
          onRetry={() => {
            imports.reload();
            work.reload();
          }}
          context="The inbox could not be loaded."
        />
      </div>
    );
  }

  if (loading) {
    return (
      <div className="library">
        <p className="library__status" role="status">
          Loading the inbox…
        </p>
      </div>
    );
  }

  if (rows.length === 0) {
    return (
      <div className="library">
        <EmptyState
          title="Nothing waiting"
          body="Recordings appear here while they are importing or waiting to be transcribed. Finished recordings live in Library."
          actions={
            <>
              <Button variant="primary" onClick={() => void pickFiles()}>
                Import an audio file
              </Button>
              <Button onClick={() => navigate({ name: "library" })}>Go to Library</Button>
            </>
          }
        />
      </div>
    );
  }

  return (
    <div className="library">
      <RecordingTable
        label="Inbox recordings"
        rows={rows}
        rowCount={rows.length}
        sort={imports.state}
        onSort={(field) => {
          imports.dispatch({ type: "sort", field });
          work.dispatch({ type: "sort", field });
        }}
        onOpenRecording={onOpenRecording}
        hasMore={hasMore}
        loadingMore={imports.loadingMore || work.loadingMore}
        onLoadMore={() => {
          if (imports.hasMore) imports.loadMore();
          if (work.hasMore) work.loadMore();
        }}
      />
    </div>
  );
}
