import { useEffect } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen } from "@testing-library/react";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { mediaUrl } from "../../ipc/media";
import { AudioPlayer, usePlayerController, usePlayerShortcuts } from "./AudioPlayer";

function Harness({ durationMs }: { durationMs: number | null }) {
  const controller = usePlayerController(durationMs);
  const shortcuts = usePlayerShortcuts(controller);
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => shortcuts(event);
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [shortcuts]);
  return <AudioPlayer recordingId="rec-00001" controller={controller} title="Weekly sync 1" />;
}

function announcement(): string {
  return screen.getByRole("status").textContent ?? "";
}

beforeEach(() => {
  vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(() => Promise.resolve());
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("waveform", () => {
  it("renders a plain seek bar when the platform cannot decode audio", () => {
    renderWithApp(<Harness durationMs={725_000} />);
    expect(screen.queryByTestId("player-waveform")).toBeNull();
    expect(screen.getByRole("slider", { name: "Seek" })).toBeInTheDocument();
  });

  it("decodes peaks off the critical path and draws them on an aria-hidden canvas", async () => {
    const channel = Float32Array.from({ length: 10_000 }, (_, i) => (i % 3 === 0 ? 0.9 : 0.1));
    const decodeAudioData = vi.fn().mockResolvedValue({ getChannelData: () => channel });
    vi.stubGlobal(
      "AudioContext",
      class {
        decodeAudioData = decodeAudioData;
        close = vi.fn().mockResolvedValue(undefined);
      },
    );
    const fetchMock = vi.fn().mockResolvedValue({ arrayBuffer: async () => new ArrayBuffer(8) });
    vi.stubGlobal("fetch", fetchMock);

    renderWithApp(<Harness durationMs={725_000} />);
    const canvas = await screen.findByTestId("player-waveform");
    expect(canvas).toHaveAttribute("aria-hidden", "true");
    expect(fetchMock).toHaveBeenCalledWith(mediaUrl("rec-00001"));
    // The slider keeps the semantics; the canvas is decoration only.
    expect(screen.getByRole("slider", { name: "Seek" })).toBeInTheDocument();
  });
});

describe("AudioPlayer with a known duration", () => {
  it("plays the media from a media URL, never a path", () => {
    renderWithApp(<Harness durationMs={725_000} />);
    const audio = screen.getByTestId("player-media");
    expect(audio.getAttribute("src")).toBe(mediaUrl("rec-00001"));
    expect(audio.getAttribute("src")).not.toMatch(/[/\\](Users|home|Library)[/\\]/);
  });

  it("exposes position and duration as text", async () => {
    const { user } = renderWithApp(<Harness durationMs={725_000} />);
    expect(screen.getByText("0:00 of 12:05")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /forward 10 s/i }));
    expect(screen.getByText("0:10 of 12:05")).toBeInTheDocument();
  });

  it("toggles play and pause from the keyboard and announces the state", async () => {
    const { user } = renderWithApp(<Harness durationMs={725_000} />);
    const play = screen.getByRole("button", { name: /^play$/i });
    play.focus();
    await user.keyboard("{Enter}");
    expect(screen.getByRole("button", { name: /^pause$/i })).toBeInTheDocument();
    expect(announcement()).toBe("Playing");
    expect(HTMLMediaElement.prototype.play).toHaveBeenCalled();

    // Space works when focus is not on a control or in a text field.
    await user.click(document.body);
    await user.keyboard(" ");
    expect(screen.getByRole("button", { name: /^play$/i })).toBeInTheDocument();
    expect(announcement()).toBe("Paused");
  });

  it("seeks with J and L and pauses with K", async () => {
    const { user } = renderWithApp(<Harness durationMs={725_000} />);
    await user.keyboard("l");
    expect(screen.getByText("0:10 of 12:05")).toBeInTheDocument();
    expect(announcement()).toBe("Position 0:10");
    await user.keyboard("j");
    expect(screen.getByText("0:00 of 12:05")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /^play$/i }));
    await user.click(document.body);
    await user.keyboard("k");
    expect(screen.getByRole("button", { name: /^play$/i })).toBeInTheDocument();
    expect(announcement()).toBe("Paused");
  });

  it("operates the scrubber entirely from the keyboard", async () => {
    const { user } = renderWithApp(<Harness durationMs={725_000} />);
    const slider = screen.getByRole("slider", { name: "Seek" });
    expect(slider).toHaveAttribute("aria-valuemin", "0");
    expect(slider).toHaveAttribute("aria-valuemax", "725");
    slider.focus();
    expect(slider).toHaveFocus();

    await user.keyboard("{ArrowRight}");
    expect(slider).toHaveAttribute("aria-valuenow", "5");
    expect(slider).toHaveAttribute("aria-valuetext", "0:05 of 12:05");

    await user.keyboard("{PageUp}");
    expect(slider).toHaveAttribute("aria-valuenow", "35");

    await user.keyboard("{Home}");
    expect(slider).toHaveAttribute("aria-valuenow", "0");
    await user.keyboard("{ArrowLeft}");
    expect(slider).toHaveAttribute("aria-valuenow", "0");

    await user.keyboard("{End}");
    expect(slider).toHaveAttribute("aria-valuenow", "725");
    await user.keyboard("{ArrowRight}");
    expect(slider).toHaveAttribute("aria-valuenow", "725");
  });

  it("changes speed with the control and with , and .", async () => {
    const { user } = renderWithApp(<Harness durationMs={725_000} />);
    const speed = screen.getByLabelText("Playback speed");
    await user.selectOptions(speed, "1.5");
    expect(speed).toHaveValue("1.5");
    expect(announcement()).toBe("Speed 1.5×");

    await user.click(document.body);
    await user.keyboard(",");
    expect(speed).toHaveValue("1.25");
    await user.keyboard(".");
    expect(speed).toHaveValue("1.5");
  });

  it("invokes no command while playing and seeking", async () => {
    const { user, backend } = renderWithApp(<Harness durationMs={725_000} />);
    await user.click(screen.getByRole("button", { name: /^play$/i }));
    await user.click(screen.getByRole("button", { name: /forward 10 s/i }));
    await user.keyboard("j");
    const mutating = backend.calls.filter((call) => call.command !== "get_capabilities");
    expect(mutating).toEqual([]);
  });

  it("has no accessibility violations", async () => {
    renderWithApp(<Harness durationMs={725_000} />);
    await expectNoAxeViolations();
  });
});

describe("AudioPlayer without a measurable duration", () => {
  it("renders an honest indeterminate transport", async () => {
    renderWithApp(<Harness durationMs={null} />);
    expect(screen.queryByRole("slider")).toBeNull();
    expect(screen.getByText(/Duration unknown/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /forward 10 s/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /back 10 s/i })).toBeDisabled();
    expect(screen.getByText("0:00 of an unknown duration")).toBeInTheDocument();
    await expectNoAxeViolations();
  });

  it("downgrades to unknown when the media reports a non-finite duration", async () => {
    renderWithApp(<Harness durationMs={725_000} />);
    expect(screen.getByRole("slider")).toBeInTheDocument();
    const audio = screen.getByTestId("player-media");
    Object.defineProperty(audio, "duration", { value: Number.NaN, configurable: true });
    fireEvent.durationChange(audio);
    expect(screen.queryByRole("slider")).toBeNull();
    expect(screen.getByText(/Duration unknown/)).toBeInTheDocument();
  });
});
