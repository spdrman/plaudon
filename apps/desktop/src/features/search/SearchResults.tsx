/**
 * Search results grouped by recording (spec §3.1, §8.6A).
 */
import type { SearchResult } from "../../ipc/contract";
import { Button } from "../../components/Button";
import { EmptyState } from "../../components/EmptyState";

export interface SearchResultsProps {
  results: SearchResult[];
  onNavigate: (recordingId: string, revisionId: string, seqIndex: number) => void;
  onSeek?: (recordingId: string, seekMs: number) => void;
}

export function SearchResults({ results, onNavigate, onSeek }: SearchResultsProps) {
  if (results.length === 0) {
    return (
      <EmptyState
        title="No results found"
        body="No transcript segment in your library matches that search."
      />
    );
  }

  // Group results by recording
  const grouped = results.reduce(
    (acc, result) => {
      if (!acc[result.recording_id]) {
        acc[result.recording_id] = [];
      }
      acc[result.recording_id]!.push(result);
      return acc;
    },
    {} as Record<string, SearchResult[]>,
  );

  return (
    <div role="region" aria-label="Search results">
      {Object.entries(grouped).map(([recordingId, recordingResults]) => (
        <section key={recordingId}>
          <h3>{recordingResults[0]?.recording_title}</h3>
          <ul aria-label={`Results from ${recordingResults[0]?.recording_title}`}>
            {recordingResults.map((result, index) => (
              <li key={index}>
                <div>
                  <p>{result.excerpt}</p>
                  <div>
                    <Button
                      onClick={() =>
                        onNavigate(
                          result.recording_id,
                          result.transcript_revision_id,
                          result.seq_index,
                        )
                      }
                      aria-label={`Jump to segment ${result.seq_index}`}
                    >
                      Jump to transcript
                    </Button>
                    {/* §8.6A: Only offer seek when seek_ms is present */}
                    {result.seek_ms !== null && onSeek && (
                      <Button
                        onClick={() => onSeek(result.recording_id, result.seek_ms!)}
                        aria-label={`Seek to ${Math.floor(result.seek_ms / 1000)}s${result.alignment_origin === "derived" ? " (approximate)" : ""}`}
                      >
                        {result.alignment_origin === "derived" ? "Seek (approx.)" : "Seek to audio"}
                      </Button>
                    )}
                  </div>
                </div>
              </li>
            ))}
          </ul>
        </section>
      ))}
    </div>
  );
}
