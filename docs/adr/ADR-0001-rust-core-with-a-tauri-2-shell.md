# ADR-0001: Rust core with a Tauri 2 shell

- **Status:** Accepted; the GUI-toolkit choice is superseded by [ADR-0023](ADR-0023-egui-shell-and-the-accessibility-evidence-plan.md), effective at the EPIC O cutover (#209)
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
