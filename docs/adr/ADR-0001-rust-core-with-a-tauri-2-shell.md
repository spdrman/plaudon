# ADR-0001: Rust core with a Tauri 2 shell

- **Status:** Superseded by [ADR-0023](ADR-0023-egui-shell-and-the-accessibility-evidence-plan.md) (the toolkit) and [ADR-0024](ADR-0024-delete-the-tauri-react-stack-at-the-egui-cutover.md) (the deletion of the shell), 2026-10-12. The Rust-core half of this record is still in force and was never in question.
- **Date:** 2026-10-11
- **Spec:** §1, §6.1, §6.3(1), §2.4

## Context

We need one desktop product on macOS and Windows with a small attack surface, predictable memory use for long audio files, and a user interface that two people can maintain. The core must stay testable without a window server and must not depend on the shell.

## Decision

We build the domain, persistence, job and AI logic as Rust crates with no mandatory dependency on Tauri, PLAUD, OpenRouter or Apple, and we present them through a Tauri 2 shell with an explicit command allowlist and a strict content security policy. A dependency-boundary test fails the build if `crates/domain` or `crates/app-core` acquires a shell, vendor or HTTP dependency.

## Rejected alternatives

- **Electron.** ships a browser runtime per application, costs hundreds of megabytes, and gives the renderer far more ambient authority than a Tauri capability manifest does.
- **A native SwiftUI application plus a separate Windows application.** duplicates every piece of domain logic in two languages, which is precisely how the two implementations drift.
- **A local web service with a browser front end.** spec §6.3(12) forbids a mandatory service runtime, and a listening socket is an attack surface we do not need.
- **Rust with a pure-Rust GUI toolkit.** accessibility support is materially weaker than a webview's, and spec §5.3 targets WCAG 2.2 AA with VoiceOver and Narrator testing. *(Revisited: ADR-0023 reverses this with egui + AccessKit and an explicit evidence plan; the Rust-core half of this record stands unchanged.)*

## Consequences

- The core is unit-testable with no window server, which is why the test suite runs in CI unchanged.
- The user interface is untrusted input to the Rust side; every command authorizes in Rust regardless of frontend validation.
- We carry a webview's rendering differences between platforms and must test both.

## Amendment, 2026-10-12: the shell this record chose no longer exists

The EPIC O cutover (#209) deleted `apps/desktop` — the Tauri 2 host, the React
frontend, the capability manifest and the content security policy this record
describes. The decision above is not rewritten, because it was the right
decision on the evidence available on 2026-10-11 and the reasoning is worth
reading: the one objection it raised against a pure-Rust toolkit,
accessibility, is exactly the objection ADR-0023 had to answer with an
evidence plan rather than a preference.

What survives unchanged is the half this record is actually about: the domain,
persistence, job and AI logic are Rust crates with no mandatory dependency on a
shell, a vendor or an HTTP client, and the dependency-boundary test still fails
the build if `crates/domain` acquires one. That is why replacing the shell was
a shell-sized change: the Tauri binary's library moved to `crates/app`
(`plaudon-app`) intact, and the new `apps/plaudon-egui` consumes the same
commands.

Three statements above are now historical rather than current:

- the presentation layer is `apps/plaudon-egui` (eframe 0.36, `wgpu`,
  AccessKit), not a Tauri 2 shell, and there is no command allowlist manifest
  or content security policy because there is nothing scriptable to
  constrain — the published command surface is a generated Rust table
  (`plaudon_app::command::COMMAND_NAMES`);
- "the user interface is untrusted input to the Rust side" is still true and
  still enforced, but by `plaudon_app::validate` on every dispatch rather than
  by a per-command IPC authorization check;
- we no longer carry a webview's rendering differences between platforms. We
  carry our own: one immediate-mode renderer, which is why the visual baselines
  are produced on exactly one renderer (`docs/EGUI_TESTING.md` §4).
