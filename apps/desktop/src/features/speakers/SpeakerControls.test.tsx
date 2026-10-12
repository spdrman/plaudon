import { describe, expect, it, vi } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { SpeakerControls } from "./SpeakerControls";
import type { Speaker } from "../../ipc/contract";

describe("SpeakerControls", () => {
  const inferredSpeaker: Speaker = {
    speaker_id: "spk-1",
    recording_id: "rec-1",
    label: "Speaker 1",
    name: null,
    confirmed: false,
    original_label: "Speaker 1",
    renamed_at: null,
    confirmed_at: null,
  };

  const renamedSpeaker: Speaker = {
    ...inferredSpeaker,
    label: "Alice",
    name: "Alice",
    renamed_at: "2026-10-11T10:00:00Z",
  };

  const confirmedSpeaker: Speaker = {
    ...renamedSpeaker,
    confirmed: true,
    confirmed_at: "2026-10-11T10:05:00Z",
  };

  it("renders inferred speaker distinctly from confirmed", () => {
    const { container, rerender } = renderWithApp(
      <SpeakerControls
        speaker={inferredSpeaker}
        onRename={vi.fn()}
        onConfirm={vi.fn()}
        onReset={vi.fn()}
      />,
    );

    expect(screen.getByText(/inferred cluster/)).toBeInTheDocument();
    expect(container.querySelector(".speaker-inferred")).toBeInTheDocument();

    rerender(
      <SpeakerControls
        speaker={confirmedSpeaker}
        onRename={vi.fn()}
        onConfirm={vi.fn()}
        onReset={vi.fn()}
      />,
    );

    expect(screen.queryByText(/inferred cluster/)).not.toBeInTheDocument();
    expect(container.querySelector(".speaker-confirmed")).toBeInTheDocument();
  });

  it("allows renaming and calls onRename", async () => {
    const onRename = vi.fn().mockResolvedValue(undefined);
    const { user } = renderWithApp(
      <SpeakerControls
        speaker={inferredSpeaker}
        onRename={onRename}
        onConfirm={vi.fn()}
        onReset={vi.fn()}
      />,
    );

    await user.click(screen.getByLabelText("Rename Speaker 1"));
    const input = screen.getByLabelText("New speaker name");
    await user.clear(input);
    await user.type(input, "Bob");
    await user.click(screen.getByLabelText("Save name"));

    await waitFor(() => expect(onRename).toHaveBeenCalledWith("spk-1", "Bob"));
  });

  it("shows confirm and reset only after rename", () => {
    renderWithApp(
      <SpeakerControls
        speaker={inferredSpeaker}
        onRename={vi.fn()}
        onConfirm={vi.fn()}
        onReset={vi.fn()}
      />,
    );

    expect(screen.queryByText("Confirm")).not.toBeInTheDocument();
    expect(screen.queryByText("Reset")).not.toBeInTheDocument();
  });

  it("allows confirming a renamed speaker", async () => {
    const onConfirm = vi.fn().mockResolvedValue(undefined);
    const { user } = renderWithApp(
      <SpeakerControls
        speaker={renamedSpeaker}
        onRename={vi.fn()}
        onConfirm={onConfirm}
        onReset={vi.fn()}
      />,
    );

    await user.click(screen.getByLabelText("Confirm Alice as identity"));
    await waitFor(() => expect(onConfirm).toHaveBeenCalledWith("spk-1"));
  });

  it("allows resetting to original label", async () => {
    const onReset = vi.fn().mockResolvedValue(undefined);
    const { user } = renderWithApp(
      <SpeakerControls
        speaker={renamedSpeaker}
        onRename={vi.fn()}
        onConfirm={vi.fn()}
        onReset={onReset}
      />,
    );

    await user.click(screen.getByLabelText("Reset to Speaker 1"));
    await waitFor(() => expect(onReset).toHaveBeenCalledWith("spk-1"));
  });

  it("has full keyboard navigation", async () => {
    const { user } = renderWithApp(
      <SpeakerControls
        speaker={renamedSpeaker}
        onRename={vi.fn()}
        onConfirm={vi.fn()}
        onReset={vi.fn()}
      />,
    );

    await user.tab();
    expect(screen.getByLabelText("Rename Alice")).toHaveFocus();
    await user.tab();
    expect(screen.getByLabelText("Confirm Alice as identity")).toHaveFocus();
    await user.tab();
    expect(screen.getByLabelText("Reset to Speaker 1")).toHaveFocus();
  });
});
