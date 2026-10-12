# ADR-0023: An egui/eframe shell, and the evidence that answers ADR-0001's accessibility objection

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §1, §2.4, §5.3, §6.1, §6.3(1), §15.1, §15.3, §18.6A
- **Supersedes:** the GUI-toolkit decision of [ADR-0001](ADR-0001-rust-core-with-a-tauri-2-shell.md), effective at the EPIC O cutover (#209)

## Context

ADR-0001 chose a Tauri 2 shell and explicitly rejected "Rust with a pure-Rust GUI toolkit" on one
ground: *"accessibility support is materially weaker than a webview's, and spec §5.3 targets WCAG 2.2
AA with VoiceOver and Narrator testing."* EPIC O (#197) reverses the toolkit choice, so that
objection has to be answered with evidence rather than with a preference.

What the audit established (`docs/EGUI_MIGRATION_PLAN.md`, `docs/EGUI_BASELINE.md`):

- The application is already Rust. All 26 crates under `crates/` have no Tauri dependency, and of the
  shell's 26,366 lines only `lib.rs`, `main.rs`, `events.rs::TauriEventSink`, `media.rs`,
  `commands/mod.rs`, `build.rs` and the two Tauri configuration files touch Tauri at all. Task O1.2
  (#200) moved the rest into `crates/app` (`plaudon-app`) unchanged.
- The accessibility story has a concrete mechanism now that it did not have when ADR-0001 was
  written: egui 0.36 drives [AccessKit](https://accesskit.dev), and `eframe`'s `accesskit` feature
  exposes it to the platform APIs — `NSAccessibility`/VoiceOver on macOS and UI Automation/Narrator
  on Windows — through `accesskit_winit`.
- The webview's accessibility was never *tested* automatically either: the React suite's axe
  assertions check markup rules, and the release gate that actually protects §5.3 is the manual
  script in `docs/accessibility/manual-screen-reader-script.md`. That gate is toolkit-independent.
- The counterweight is real and was measured, not assumed: the three reference egui applications
  (photocraft, filmcraft, wordcraft) ship no AccessKit regression tests and commit no snapshot
  baselines, so Plaudon must build its own evidence rather than inherit one.

## Decision

The desktop shell becomes `apps/plaudon-egui`: an `eframe` 0.36 window with the `wgpu` renderer and
the `accesskit` feature, over the Tauri-free `plaudon-app`. The user interface is Rust only; there is
no webview, no JavaScript and no IPC boundary. `plaudon-app` must never depend on `egui` or `eframe`,
so the application layer stays testable headlessly and the dependency-boundary test keeps its meaning.

**ADR-0001's objection is answered by an evidence plan, not by assertion.** Every item below is a
gate, and each one is either already in force or owned by a named EPIC O task:

1. **AccessKit is on from the first commit.** `eframe` is built with `default_fonts`, `wgpu` and
   `accesskit`; the spike binary and every later screen run with it enabled. *(In force: O1.2.)*
2. **Every interactive widget carries a role and a label.** Asserted mechanically with
   `egui_kittest` through `plaudon-ui-testkit`'s `a11y::nodes`, which reads the AccessKit tree a
   frame produced. The first such assertion already exists: the vendored divider reports
   `Role::Splitter` with the label "Resize the library pane"
   (`apps/plaudon-egui/tests/spike_kittest.rs`). *(In force: O1.2; extended per screen in O1.3–O2.4.)*
3. **Keyboard reachability is a test, not a review note.** `a11y::focus_order` walks the focus order
   by pressing Tab and reports the labels reached; a screen fails if an action is not on that list.
   This replaces the Playwright check "no interactive element is unreachable by keyboard", and it is
   the mechanical half of the release rule that a step which cannot be completed with the keyboard
   alone is a blocker. *(In force: O1.2; applied to the shell in O1.3.)*
4. **Contrast is computed, not eyeballed.** The 20 colour roles × 4 themes port from `tokens.ts`
   together with their contrast tests, as plain Rust unit tests. *(Owned by O1.3, #201.)*
5. **The manual screen-reader script becomes a per-phase gate, not a cutover gate.** It is re-run
   with VoiceOver on macOS and Narrator on Windows at the end of each EPIC O phase, and its results
   are recorded. A phase whose script fails does not close. *(Owned by O1.3 onward; the script is
   updated for egui semantics in O2.5, #209.)*
6. **axe has no egui equivalent, and we say so.** Items 2–4 plus the manual script replace it. No
   document may claim an automated WCAG conformance check that does not exist.
7. **Where AccessKit is weaker than the DOM, the gap is recorded rather than hidden.** The migration
   report (O2.5) must state any role Plaudon could not express, with the screen it affects.

Vendored reuse is allowed where it strengthens this plan and the licence permits it:
`third_party/craft-ui` carries craft-libs' `ResizeHandle` — the best accessibility asset found in the
reference repositories — with its MIT/Apache-2.0 notices, a provenance header per file and a register
row. Git dependencies stay denied (`deny.toml`), so reuse means vendoring a pinned snapshot.

## Rejected alternatives

- **Keep the Tauri 2 shell.** It satisfies §5.3 today, but EPIC O's premise is a Rust-only product:
  two frontends mean two sets of behaviour to keep honest, and the React suite is already the larger
  half of the test surface. Rejected as the goal of the EPIC, not on technical grounds.
- **A pure-Rust toolkit without AccessKit (`iced`-style immediate accessibility, or none).** This is
  exactly what ADR-0001 rejected, and rightly: with no platform accessibility tree, §5.3 could not be
  met at all. Rejected.
- **Ship egui now and defer accessibility to the cutover.** Accessibility retrofitted at the end is
  accessibility not done; the roles, labels and focus order have to exist in the same commit as the
  widget. Rejected — hence items 1–3 being gates from O1.2.
- **Keep a webview for the screens judged "hard" (transcript editor, review panel).** A hybrid keeps
  the IPC boundary, the CSP, the capability manifest and the Node toolchain alive forever, and splits
  the accessibility story in two. Rejected.
- **Adopt `craft-ui` as a git dependency instead of vendoring it.** `deny.toml` denies unknown git
  sources, and the upstream crate has no CI, no changelog and no release. Rejected in favour of a
  vendored snapshot (`docs/EGUI_REFERENCE_COMPONENTS.md` §6).

## Consequences

- The IPC trust boundary becomes an in-process one. Validation does not move: `plaudon-app`'s
  `validate.rs` still runs for every command, because the view layer is untrusted input in exactly
  the sense ADR-0001 meant. `docs/threat-model/trust-boundaries.md` is updated at the cutover.
- The capability manifest, the CSP and the asset-protocol allowlist cease to exist, and with them the
  tests that pinned them. The properties they protected move to the packaging review (O2.5).
- Accessibility becomes our own responsibility rather than the webview's: every widget we write has
  to set a role and a label, and the kittest assertions above are what stops that from slipping.
- Rendering moves to `wgpu`, so the shell now has a GPU dependency. Headless and CI runs use a
  software adapter (MARS Docker, mesa/lavapipe), which is also where screenshot baselines are made.
- The licence surface grows by the egui/eframe/winit/wgpu/AccessKit/`rfd` trees; each is recorded in
  `docs/licensing/third-party-register.md` and gated by `cargo deny`.
- Two frontends coexist until the O2.5 cutover. `plaudon-desktop` keeps working against the same
  `plaudon-app` crate, so there is one implementation of the commands, not two.
