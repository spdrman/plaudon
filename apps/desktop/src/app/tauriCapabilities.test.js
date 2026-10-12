import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
/**
 * E1.1 / §15.1 / D2.1: the shell fails closed. The capability manifest and the
 * Tauri configuration must never grant shell execution, a generic HTTP client
 * or filesystem access, and the content security policy must stay strict.
 */
const TAURI_DIR = join(process.cwd(), "src-tauri");
const config = JSON.parse(readFileSync(join(TAURI_DIR, "tauri.conf.json"), "utf8"));
const defaultCapability = JSON.parse(readFileSync(join(TAURI_DIR, "capabilities/default.json"), "utf8"));
const FORBIDDEN_PERMISSION = /^(shell:|http:|fs:|os:allow-exec|core:webview:allow-print)/;
describe("tauri capability manifest", () => {
    it("scopes the default capability to the main window", () => {
        expect(defaultCapability.windows).toEqual(["main"]);
        expect(config.app.security.capabilities).toEqual(["default"]);
    });
    it("grants no shell, HTTP or filesystem permission", () => {
        const permissions = defaultCapability.permissions;
        expect(permissions.length).toBeGreaterThan(0);
        for (const permission of permissions) {
            expect(permission, `${permission} is forbidden`).not.toMatch(FORBIDDEN_PERMISSION);
        }
    });
    it("grants only the dialog plugin permissions needed for file selection", () => {
        const pluginPermissions = defaultCapability.permissions.filter((p) => !p.startsWith("core:"));
        expect(pluginPermissions.sort()).toEqual(["dialog:allow-open", "dialog:allow-save"]);
    });
    it("keeps the asset protocol disabled and unscoped", () => {
        expect(config.app.security.assetProtocol).toEqual({ enable: false, scope: [] });
    });
    it("does not expose the Tauri API globally and freezes the prototype", () => {
        expect(config.app.withGlobalTauri).toBe(false);
        expect(config.app.security.freezePrototype).toBe(true);
        expect(config.app.security.dangerousDisableAssetCspModification).toBe(false);
        for (const key of Object.keys(config.app.security)) {
            if (key.startsWith("dangerous")) {
                expect(config.app.security[key], key).toBeFalsy();
            }
        }
    });
    it("ships a strict content security policy", () => {
        const csp = config.app.security.csp;
        const directives = Object.fromEntries(csp
            .split(";")
            .map((part) => part.trim())
            .filter(Boolean)
            .map((part) => {
            const [name, ...values] = part.split(/\s+/);
            return [name, values];
        }));
        expect(directives["default-src"]).toEqual(["'self'"]);
        expect(directives["script-src"]).toEqual(["'self'"]);
        expect(directives["object-src"]).toEqual(["'none'"]);
        expect(directives["frame-src"]).toEqual(["'none'"]);
        expect(directives["base-uri"]).toEqual(["'none'"]);
        expect(directives["form-action"]).toEqual(["'none'"]);
        // Playback uses the custom media scheme; no remote origin is reachable.
        expect(directives["media-src"]).toEqual([
            "'self'",
            "plaudon-media:",
            "http://plaudon-media.localhost",
        ]);
        expect(directives["connect-src"]).toEqual(["'self'", "ipc:", "http://ipc.localhost"]);
        expect(csp).not.toMatch(/unsafe-eval/);
        expect(directives["script-src"]).not.toContain("'unsafe-inline'");
        expect(directives["default-src"]).not.toContain("'unsafe-inline'");
    });
    it("loads only the bundled frontend, never a remote URL", () => {
        expect(config.build.frontendDist).toBe("../dist");
        for (const window of config.app.windows) {
            expect(window.url ?? "").not.toMatch(/^https?:/);
        }
        expect(config.build.devUrl).toMatch(/^http:\/\/127\.0\.0\.1:/);
    });
    it("keeps a desktop-class minimum window size", () => {
        const [main] = config.app.windows;
        expect(main.minWidth).toBeGreaterThanOrEqual(880);
        expect(main.minHeight).toBeGreaterThanOrEqual(560);
        expect(main.dragDropEnabled).toBe(true);
    });
});
