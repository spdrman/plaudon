import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * App. B.9 quality gates and §4.2A.6: BLE output is compressed `.ASR`/Opus and
 * is never described as an original recording, high-quality audio or a
 * protected connection. The label for what a source yields comes from the
 * register-derived DTO, never from copy in this feature.
 */
const DIR = join(process.cwd(), "src", "features", "devices");
const sources = readdirSync(DIR)
  .filter((name) => /\.(tsx?|css)$/.test(name) && !/\.test\./.test(name))
  .map((name) => ({ name, text: readFileSync(join(DIR, name), "utf8") }));

const BANNED = [/original\s+wav/i, /high[- ]quality/i, /full[- ]resolution/i, /secure\s+connection/i];

describe("Devices UI copy", () => {
  it("scans the feature's own sources", () => {
    expect(sources.map((s) => s.name)).toEqual(
      expect.arrayContaining(["DeviceDetail.tsx", "DevicesScreen.tsx", "SetupWizard.tsx", "TransferList.tsx"]),
    );
  });

  it.each(BANNED.map((pattern) => [String(pattern), pattern] as const))(
    "no source matches %s",
    (_label, pattern) => {
      const offenders = sources.filter((s) => pattern.test(s.text)).map((s) => s.name);
      expect(offenders).toEqual([]);
    },
  );

  it("shows Cloud: not used only where the DTO says no cloud route was involved", () => {
    for (const { name, text } of sources) {
      let index = text.indexOf("Cloud: not used");
      while (index >= 0) {
        const context = text.slice(Math.max(0, index - 160), index);
        expect(context, `${name} prints "Cloud: not used" unconditionally`).toMatch(
          /cloud_route === "not_used"/,
        );
        index = text.indexOf("Cloud: not used", index + 1);
      }
    }
  });

  it("takes the media label from the DTO instead of hand-writing it", () => {
    for (const { name, text } of sources) {
      expect(text, name).not.toMatch(/compressed ASR/);
    }
  });
});
