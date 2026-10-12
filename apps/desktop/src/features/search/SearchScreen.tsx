/**
 * Cross-recording search screen (spec §3.1, §9.8, §17.5, §5.4).
 */
import { useCallback, useState } from "react";
import { useCommandQuery } from "../../ipc/useCommand";
import { SearchField } from "./SearchField";
import { SearchResults } from "./SearchResults";
import { EmptyState } from "../../components/EmptyState";
import { Button } from "../../components/Button";
import { CommandErrorNotice } from "../recording/errorNotice";

export interface SearchScreenProps {
  onNavigate: (recordingId: string, revisionId: string, seqIndex: number) => void;
}

export function SearchScreen({ onNavigate }: SearchScreenProps) {
  const [query, setQuery] = useState("");
  const [submitted, setSubmitted] = useState(false);

  const { data, error, loading, refetch } = useCommandQuery(
    "search_library",
    { query, cursor: null, limit: 50 },
    { enabled: submitted && query.length > 0, deps: [query, submitted] },
  );

  const handleSearch = useCallback((newQuery: string) => {
    setQuery(newQuery);
    setSubmitted(true);
  }, []);

  const handleSeek = useCallback(
    (recordingId: string, seekMs: number) => {
      // Navigate to recording and signal seek position via URL state
      // This is a simplified version - full implementation would coordinate with player
      console.log(`Seek to ${recordingId} at ${seekMs}ms`);
    },
    [],
  );

  return (
    <div className="search-screen">
      <SearchField onSearch={handleSearch} disabled={loading} />

      {error && <CommandErrorNotice error={error} onRetry={refetch} />}

      {loading && <p role="status" aria-live="polite">Searching...</p>}

      {!submitted && !loading && (
        <EmptyState title="Search library" body="Enter a search term to find content across all recordings" />
      )}

      {submitted && !loading && !error && data && data.results.length === 0 && (
        <EmptyState
          title="No results"
          body={`No results found for \u201c${query}\u201d`}
          actions={<Button onClick={() => setSubmitted(false)}>Clear search</Button>}
        />
      )}
      {submitted && !loading && !error && data && data.results.length > 0 && (
        <SearchResults results={data.results} onNavigate={onNavigate} onSeek={handleSeek} />
      )}
    </div>
  );
}
