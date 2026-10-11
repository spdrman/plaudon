import { describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import type { TranscriptRevisionRef } from "../../ipc/contract";
import { RevisionSwitcher } from "./RevisionSwitcher";

const MACHINE: TranscriptRevisionRef = {
  revision_id: "rev-1",
  kind: "machine",
  created_at: "2026-02-01T09:00:00Z",
  engine_id: "whisper.cpp",
  model_id: "ggml-base.en",
  alignment_origin: "native",
  segment_count: 12,
  superseded: true,
};

const HUMAN: TranscriptRevisionRef = {
  ...MACHINE,
  revision_id: "rev-2",
  kind: "human",
  created_at: "2026-02-02T10:30:00Z",
  superseded: false,
};

describe("RevisionSwitcher", () => {
  it("names the machine revision as the original and marks superseded entries", async () => {
    renderWithApp(
      <RevisionSwitcher
        revisions={[MACHINE, HUMAN]}
        selectedRevisionId="rev-2"
        headRevisionId="rev-2"
        onSelect={vi.fn()}
      />,
    );
    const select = screen.getByLabelText("Transcript revision");
    const options = Array.from(select.querySelectorAll("option")).map((o) => o.textContent);
    expect(options[0]).toBe("Original (machine) · superseded");
    expect(options[1]).toContain("Edited by you");
    expect(options[1]).toContain("· current");
    await expectNoAxeViolations();
  });

  it("reports a non-head revision as read-only and switches on selection", async () => {
    const onSelect = vi.fn();
    const { user } = renderWithApp(
      <RevisionSwitcher
        revisions={[MACHINE, HUMAN]}
        selectedRevisionId="rev-1"
        headRevisionId="rev-2"
        onSelect={onSelect}
      />,
    );
    expect(screen.getByText("Read-only revision")).toBeInTheDocument();
    await user.selectOptions(screen.getByLabelText("Transcript revision"), "rev-2");
    expect(onSelect).toHaveBeenCalledWith("rev-2");
  });

  it("renders nothing when a recording has no revisions", () => {
    const { container } = renderWithApp(
      <RevisionSwitcher
        revisions={[]}
        selectedRevisionId={null}
        headRevisionId={null}
        onSelect={vi.fn()}
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });
});
