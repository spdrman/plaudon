import { describe, expect, it, vi } from "vitest";
import { CommandError, invokeCommand, isEnvelope, setTransport, toEnvelope } from "./client";
import type { Transport } from "./client";

const okTransport = (impl: (cmd: string, args: unknown) => unknown): Transport => ({
  invoke: async (cmd, args) => impl(cmd, args),
  listen: async () => () => undefined,
});

describe("invokeCommand", () => {
  it("passes arguments under the `request` key for the Tauri bridge", async () => {
    const invoke = vi.fn(async () => ({ ok: true }));
    setTransport({ invoke, listen: async () => () => undefined });
    await invokeCommand("remove_openrouter_key", {});
    expect(invoke).toHaveBeenCalledWith("remove_openrouter_key", {});
  });

  it("wraps backend envelopes in a typed CommandError", async () => {
    setTransport(
      okTransport(() => {
        throw {
          code: "CONFLICT",
          message: "The transcript changed since you opened it.",
          retryable: false,
          action: "OPEN_DIAGNOSTICS",
          diagnostic_id: "abc123",
        };
      }),
    );
    await expect(
      invokeCommand("save_transcript_revision", {
        recording_id: "r1",
        base_revision_id: "rev1",
        edits: [],
      }),
    ).rejects.toMatchObject({ code: "CONFLICT", retryable: false });
  });

  it("normalizes non-envelope failures without inventing retryability", () => {
    const envelope = toEnvelope(new Error("boom"));
    expect(envelope.code).toBe("INTERNAL_ERROR");
    expect(envelope.retryable).toBe(false);
    expect(envelope.action).toBeNull();
  });

  it("recognises only well-formed envelopes with known actions", () => {
    expect(
      isEnvelope({
        code: "X",
        message: "m",
        retryable: true,
        action: "WAIT_AND_RETRY",
        diagnostic_id: "d",
      }),
    ).toBe(true);
    expect(
      isEnvelope({ code: "X", message: "m", retryable: true, action: "HACK", diagnostic_id: "d" }),
    ).toBe(false);
    expect(isEnvelope({ code: "X" })).toBe(false);
  });

  it("CommandError exposes the envelope verbatim", () => {
    const envelope = {
      code: "STORAGE_FULL",
      message: "Not enough space.",
      retryable: true,
      action: "FREE_DISK_SPACE" as const,
      diagnostic_id: "d1",
    };
    const err = new CommandError(envelope);
    expect(err.envelope).toEqual(envelope);
    expect(err.message).toBe("Not enough space.");
  });

  it("throws a clear error when no transport is configured", async () => {
    setTransport(null);
    await expect(invokeCommand("get_capabilities", {})).rejects.toThrow(/transport is not configured/);
  });
});
