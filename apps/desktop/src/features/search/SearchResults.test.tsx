import { describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { SearchResults } from "./SearchResults";
import type { SearchResult } from "../../ipc/contract";

describe("SearchResults", () => {
  it("shows empty state when no results", () => {
    renderWithApp(<SearchResults results={[]} onNavigate={vi.fn()} />);
    expect(screen.getByText("No results found")).toBeInTheDocument();
  });

  it("groups results by recording", () => {
    const results: SearchResult[] = [
      {
        recording_id: "rec-1",
        recording_title: "Meeting A",
        transcript_revision_id: "rev-1",
        seq_index: 5,
        excerpt: "First result",
        seek_ms: 1000,
        alignment_origin: "native",
      },
      {
        recording_id: "rec-1",
        recording_title: "Meeting A",
        transcript_revision_id: "rev-1",
        seq_index: 10,
        excerpt: "Second result",
        seek_ms: 2000,
        alignment_origin: "native",
      },
    ];

    renderWithApp(<SearchResults results={results} onNavigate={vi.fn()} />);
    expect(screen.getByText("Meeting A")).toBeInTheDocument();
    expect(screen.getByText("First result")).toBeInTheDocument();
    expect(screen.getByText("Second result")).toBeInTheDocument();
  });

  it("shows seek affordance only when seek_ms is present (spec §8.6A)", () => {
    const withSeek: SearchResult = {
      recording_id: "rec-1",
      recording_title: "With timing",
      transcript_revision_id: "rev-1",
      seq_index: 1,
      excerpt: "Has timing",
      seek_ms: 5000,
      alignment_origin: "native",
    };

    const withoutSeek: SearchResult = {
      recording_id: "rec-2",
      recording_title: "Text only",
      transcript_revision_id: "rev-2",
      seq_index: 1,
      excerpt: "No timing",
      seek_ms: null,
      alignment_origin: "none",
    };

    renderWithApp(
      <SearchResults results={[withSeek, withoutSeek]} onNavigate={vi.fn()} onSeek={vi.fn()} />,
    );

    const seekButtons = screen.queryAllByText(/Seek to audio/);
    expect(seekButtons).toHaveLength(1);
  });

  it("labels derived alignment as approximate", () => {
    const derived: SearchResult = {
      recording_id: "rec-1",
      recording_title: "Derived",
      transcript_revision_id: "rev-1",
      seq_index: 1,
      excerpt: "Derived timing",
      seek_ms: 3000,
      alignment_origin: "derived",
    };

    renderWithApp(<SearchResults results={[derived]} onNavigate={vi.fn()} onSeek={vi.fn()} />);
    expect(screen.getByText(/Seek \(approx\.\)/)).toBeInTheDocument();
  });
});
