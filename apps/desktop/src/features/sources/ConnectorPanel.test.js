import { jsx as _jsx } from "react/jsx-runtime";
import { describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { fakePlaudSnapshot } from "../../testing/fakeBackend";
import { renderWithApp } from "../../testing/renderWithApp";
import { expectNoAxeViolations } from "../../testing/axe";
import { ConnectorPanel } from "./ConnectorPanel";
import { PLAUD_CLI_DOCS_URL } from "./plaudPresentation";
const noop = () => undefined;
function panel(action, overrides = {}, handlers = {}) {
    return renderWithApp(_jsx(ConnectorPanel, { snapshot: fakePlaudSnapshot({ next_action: action, ...overrides }), busy: false, onRun: handlers.onRun ?? noop, onDisconnect: handlers.onDisconnect ?? noop }));
}
describe("ConnectorPanel: the next_action becomes one concrete control", () => {
    it("install_cli links to PLAUD's official documentation", () => {
        panel({ kind: "install_cli" }, { state: "unconfigured", headline: "H", detail: "D", compatibility: "unknown", detected_version: null });
        const link = screen.getByRole("link", { name: "Open PLAUD's installation guide" });
        expect(link).toHaveAttribute("href", PLAUD_CLI_DOCS_URL);
        expect(link).toHaveAttribute("rel", expect.stringContaining("noopener"));
    });
    it("install_tested_version links to the docs and lists the tested versions", () => {
        panel({ kind: "install_tested_version", usable_versions: ["1.4.2", "1.5.0"] }, { state: "incompatible_version", compatibility: "untested", detected_version: "9.9.9" });
        expect(screen.getByRole("link", { name: "Open PLAUD's installation guide" })).toBeInTheDocument();
        expect(screen.getByText("1.4.2, 1.5.0")).toBeInTheDocument();
        expect(screen.getByText("Untested", { selector: "dd" })).toBeInTheDocument();
        expect(screen.getByText("9.9.9")).toBeInTheDocument();
    });
    it.each([
        [{ kind: "sign_in" }, "Sign in to PLAUD", "connect"],
        [{ kind: "check_account_and_region" }, "Sign in again", "connect"],
        [{ kind: "retry" }, "Try again", "retry"],
        [{ kind: "wait_and_retry" }, "Try again", "retry"],
        [{ kind: "review_local_only_mode" }, "Review Local-only Mode", "review_local_only_mode"],
    ])("%j renders a button that runs the right operation", async (action, label, run) => {
        const onRun = vi.fn();
        const { user } = panel(action, {}, { onRun });
        await user.click(screen.getByRole("button", { name: label }));
        expect(onRun).toHaveBeenCalledWith(run);
    });
    it("import_manually offers the persistent import control", () => {
        panel({ kind: "import_manually" });
        expect(screen.getByRole("button", { name: "Import audio" })).toBeInTheDocument();
    });
    it("none renders no corrective control", () => {
        panel({ kind: "none" });
        expect(screen.queryByRole("link")).toBeNull();
        expect(screen.queryByRole("button", { name: /Sign in|Try again|Review/ })).toBeNull();
    });
    it("shows the state, version, verdict, origin and last error code", () => {
        panel({ kind: "retry" }, { state: "error", compatibility: "tested", detected_version: "1.4.2", last_error: "MALFORMED_OUTPUT" });
        expect(screen.getByText("Stopped")).toBeInTheDocument();
        expect(screen.getByText("1.4.2")).toBeInTheDocument();
        expect(screen.getByText("Tested", { selector: "dd" })).toBeInTheDocument();
        expect(screen.getByText("standard installation directory")).toBeInTheDocument();
        expect(screen.getByText("MALFORMED_OUTPUT")).toBeInTheDocument();
    });
});
describe("ConnectorPanel: copy from Rust, verbatim", () => {
    it("renders headline, detail and every warning exactly as received", () => {
        panel({ kind: "none" }, {
            headline: "Headline from Rust",
            detail: "Detail from Rust, with <b>markup</b> & quotes “kept”.",
            warnings: ["Scraping warning from Rust.", "Possibly-incomplete warning from Rust."],
        });
        expect(screen.getByRole("heading", { name: "Headline from Rust" })).toBeInTheDocument();
        expect(screen.getByText("Detail from Rust, with <b>markup</b> & quotes “kept”.")).toBeInTheDocument();
        const warnings = screen.getByRole("list", { name: "Warnings" });
        expect(warnings).toHaveTextContent("Scraping warning from Rust.");
        expect(warnings).toHaveTextContent("Possibly-incomplete warning from Rust.");
    });
    it("composes no user-facing string of its own", () => {
        const rust = {
            headline: "RUST-HEADLINE",
            detail: "RUST-DETAIL",
            warnings: ["RUST-WARNING"],
        };
        const snapshot = fakePlaudSnapshot({ ...rust, next_action: { kind: "sign_in" } });
        snapshot.disconnect = { variant: "association_only", label: "RUST-LABEL", body: "RUST-BODY" };
        const { container } = renderWithApp(_jsx(ConnectorPanel, { snapshot: snapshot, busy: false, onRun: noop, onDisconnect: noop }));
        const rustStrings = new Set(["RUST-HEADLINE", "RUST-DETAIL", "RUST-WARNING", "RUST-LABEL", "RUST-BODY"]);
        const own = [];
        const walker = document.createTreeWalker(container, NodeFilter.SHOW_TEXT);
        for (let node = walker.nextNode(); node; node = walker.nextNode()) {
            const text = (node.textContent ?? "").trim();
            if (text && !rustStrings.has(text))
                own.push(text);
        }
        // Everything the frontend wrote: labels, a state word, a verdict word and
        // the facts Rust reported (version, origin). No sentence, no error text.
        expect(own).toMatchInlineSnapshot(`
      [
        "State",
        "✓",
        "Ready",
        "Tool version",
        "1.4.2",
        "Compatibility",
        "Tested",
        "Found in",
        "standard installation directory",
        "!",
        "Sign in to PLAUD",
      ]
    `);
    });
});
describe("ConnectorPanel: disconnect wording follows the DisconnectCopy variant", () => {
    it("association_only says it does not sign the user out", async () => {
        const onDisconnect = vi.fn();
        const { user } = renderWithApp(_jsx(ConnectorPanel, { snapshot: fakePlaudSnapshot({}, "association_only"), busy: false, onRun: noop, onDisconnect: onDisconnect }));
        expect(screen.getByText(/does not sign you out of PLAUD/)).toBeInTheDocument();
        await user.click(screen.getByRole("button", { name: "Disconnect from this app" }));
        expect(onDisconnect).toHaveBeenCalledTimes(1);
        expect(screen.queryByRole("button", { name: "Disconnect and sign out" })).toBeNull();
    });
    it("with_vendor_logout may claim the sign-out", () => {
        renderWithApp(_jsx(ConnectorPanel, { snapshot: fakePlaudSnapshot({}, "with_vendor_logout"), busy: false, onRun: noop, onDisconnect: noop }));
        expect(screen.getByRole("button", { name: "Disconnect and sign out" })).toBeInTheDocument();
        expect(screen.getByText(/runs the tool's sign-out command/)).toBeInTheDocument();
        expect(screen.queryByText(/does not sign you out/)).toBeNull();
    });
});
describe("ConnectorPanel: accessibility", () => {
    it("has no WCAG 2.2 AA violations and is fully keyboard operable", async () => {
        const onRun = vi.fn();
        const onDisconnect = vi.fn();
        const { user } = panel({ kind: "sign_in" }, { warnings: ["A warning."] }, { onRun, onDisconnect });
        await expectNoAxeViolations();
        await user.tab();
        expect(screen.getByRole("button", { name: "Sign in to PLAUD" })).toHaveFocus();
        await user.keyboard("{Enter}");
        expect(onRun).toHaveBeenCalledWith("connect");
        await user.tab();
        expect(screen.getByRole("button", { name: "Disconnect from this app" })).toHaveFocus();
        await user.keyboard(" ");
        expect(onDisconnect).toHaveBeenCalled();
    });
});
