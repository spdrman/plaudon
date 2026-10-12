# ADR-0024: The Tauri/React stack is deleted, not kept beside the egui shell

- **Status:** Accepted
- **Date:** 2026-10-12
- **Spec:** §1, §2.4, §5.3, §6.1, §6.3(1), §15.1, §15.3, §18.4, §18.6A
- **Supersedes:** [ADR-0001](ADR-0001-rust-core-with-a-tauri-2-shell.md) in full; completes the toolkit supersession [ADR-0023](ADR-0023-egui-shell-and-the-accessibility-evidence-plan.md) began

## Context

ADR-0001 chose a Rust core behind a Tauri 2 shell and rejected a pure-Rust
toolkit on one explicit ground — accessibility. ADR-0023 reversed the toolkit
choice and, instead of asserting that egui is good enough, set an evidence bar:
AccessKit on from the first commit, a role and a label on every interactive
widget asserted from the AccessKit tree, a keyboard reachability walk, computed
contrast, and the manual VoiceOver/Narrator script as a per-phase gate. It also
said, in as many words, that the two frontends coexist until this cutover.

EPIC O then built the shell. `docs/EGUI_MIGRATION_PLAN.md` §4 is the parity
matrix, read out of the tree rather than inferred from closed tasks: 126 rows,
124 of them `done` (two of those `done as designed`, meaning the row will not be
built and says why), one `partial` and one `blocked`. Both exceptions are named
below, because a cutover justified by a parity matrix is only as honest as its
two worst rows.

This record is about what happens to the old tree, which is a separate decision
from which toolkit wins. Keeping a superseded shell costs nothing on the day it
is superseded and everything for every month after it.

## Decision

`apps/desktop` — the Tauri 2 host, the React/TypeScript frontend, the Playwright
and vitest suites, the npm workspace and the Tauri packaging scripts — is
deleted at this cutover. `apps/plaudon-egui` is the desktop application.

Three consequences of "deleted" are deliberate and are the point of the record:

- **No shim.** `plaudon-desktop`'s library was already extracted to
  `crates/app` (`plaudon-app`) unchanged, so nothing in the deleted tree is the
  only copy of anything except Tauri glue: `run()`, `TauriEventSink`, the
  `#[tauri::command]` wrappers, `main.rs` and `build.rs`. There is no
  compatibility module, no re-export and no `plaudon_desktop` alias; call sites
  move to `plaudon_app` by rename.
- **No dual-shell build.** There is no feature flag, no second binary and no CI
  job that builds a webview. After this change `cargo tree` resolves no `tauri`,
  `wry`, `webkit` or `webview2` package, and the repository contains no
  `package.json`, no TypeScript, JavaScript, CSS or HTML source, and no step
  anywhere that needs a node toolchain.
- **No bundler kept for packaging.** `scripts/package-macos-egui.sh` and
  `scripts/package-windows-egui.ps1` are the packaging path, documented in
  `docs/EGUI_BUILD.md`; the Tauri bundler and its configuration go with the
  tree.

The command and event surface that the IPC boundary used to carry is the
in-process typed one: `plaudon_app::command::AppCommand` /
`AppResponse`, dispatched by `plaudon_app::command::Dispatcher`, with outcomes
and §15.3 events arriving on the `plaudon_app::event` channel. The decisions in
ADR-0020 and ADR-0021 stand; only their transport changed, and both records
carry a dated amendment saying so.

## Rejected alternatives

- **Keep both shells for a release or two.** This is the option that sounds
  free. It is not: every command, every event, every copy string and every
  accessibility claim would have to stay true in two implementations, and the
  React suite is the larger half of the test surface, so the dual period would
  cost more maintenance than the whole migration did. It also keeps the
  webview's CSP, capability manifest and node toolchain alive, which is exactly
  the attack and build surface the cutover removes. Rejected.
- **Keep a thin Tauri shell purely as a bundler.** Tauri's bundler is attractive
  because it is written and works, but using it means keeping `tauri`, `wry` and
  a platform webview in the dependency graph of a product that renders no HTML —
  a linked browser engine, with its CVE stream, for a `.app` layout and an NSIS
  script we already have. `tests/packaging.rs` asserts what the two scripts
  produce (bundle identifier against `plaudon_app::runtime::APPLICATION_IDENTIFIER`,
  the five reviewed entitlements, the NSIS registry claim against
  `plaudon_platform::associations`, icon geometry, the Windows manifest), so the
  bundler is replaced by something testable rather than by nothing. Rejected.
- **Keep the TypeScript contract tests as a second opinion on the command
  surface.** They tested `apps/desktop/src/ipc/contract.ts` against the Tauri
  allowlist. With no IPC there is no second party to agree with: the surface is
  now generated from one macro table in `crates/app/src/command.rs`, which emits
  the enums, `COMMAND_NAMES` and the dispatch arms together, so a command cannot
  exist in one and be missing from another. Keeping the TypeScript would mean
  keeping node to run a check that compares the Rust to a hand-maintained copy of
  itself. The versioned JSON schemas under `schemas/commands/v1/` remain, and
  they are the artifact that outlives the transport. Rejected.
- **Keep the React screens as a fallback for anything egui renders badly.** A
  fallback nobody runs is not a fallback; a fallback people do run is a second
  product. The parity matrix exists so that the gaps are named instead of
  hedged. Rejected.

## Consequences

We accept these, including the ones that are not comfortable:

- **Parity row D12 is `partial`.** The real Bluetooth radio is behind the
  `hardware-scan` feature, which `apps/plaudon-egui` forwards to `plaudon-app`
  and which is off by default and never enabled in CI. With it off the Devices
  screen reports `RadioState::UnavailableInBuild` — "no radio in this build",
  which is deliberately not the same message as "no devices found". That is the
  honest state, and it is the same state the Tauri shell shipped with (its
  `build_ports_with_radio` had no caller at all), so the cutover does not lose a
  capability here. It does not gain one either.
- **Parity row D13 is `blocked`.** There is no `device_*` command for a mounted
  USB recording volume — none existed in the Tauri shell either, because the
  gap is in the command surface, not in the UI. The egui screen offers ordinary
  import of the files on the mounted volume and says why that is what it is
  offering. Closing D13 means adding a command from the §15.2 table, which is a
  decision for whoever owns that work, not something the cutover may quietly
  imply has happened.
- **The manual VoiceOver and Narrator run is still owed.** `docs/accessibility/manual-screen-reader-script.md`
  states at the top that it has not been executed, and parity row S17 says
  "done for what automation can cover; the manual run is still owed". What
  exists is mechanical: the AccessKit role/label/value assertions per screen,
  `a11y::assert_every_control_is_named` over an explicit list of interactive
  roles with an empty exemption list, the Tab-order walk, and the token contrast
  tests. ADR-0023 made the manual script a per-phase gate and that gate is open.
  No document may read the parity matrix as a WCAG 2.2 AA conformance claim
  until the script has been run on both platforms and its results recorded.
- **Notarization is unverified.** `scripts/package-macos-egui.sh` exits 3 —
  "built and verifiable, NOT publishable" — in every configuration except
  `PLAUDON_MACOS_IDENTITY` plus `PLAUDON_NOTARY_PROFILE`, and we hold no Apple
  credentials, so no notarized build of the egui bundle exists. The same is true
  of Authenticode signing on Windows. The scripts fail closed and report
  `not_verified` rather than printing a status they did not earn; that is a
  working honesty mechanism, not a satisfied gate.
- **The Windows leg is type-checked, not run, outside CI.** §18.4 forbids
  calling it green from anywhere but Windows hardware or a Windows runner; the
  `egui-desktop` job on `windows-2025` is where that happens.
- **The webview trust boundary is gone, and with it the tests that pinned it.**
  The CSP, the capability allowlist and the asset-protocol rules protected
  properties that now have different owners: argument validation stays in
  `plaudon-app`'s `validate.rs` (the view layer is untrusted input in exactly
  the sense ADR-0001 meant), path handling stays in `plaudon_security::fsguard`,
  and the "no debugger, no devtools in a production artifact" property moves to
  the macOS entitlement audit in `scripts/package-macos-egui.sh` and the
  privilege audit in `scripts/package-windows-egui.ps1`.
  `docs/threat-model/trust-boundaries.md` and `traceability.json` are updated in
  the same change.
- **Git history is the only record of the deleted code.** That is accepted: the
  tree was 26,366 lines of shell plus its React frontend, and a directory kept
  "just in case" is a directory that stops compiling and then lies.

## Amendment, 2026-10-12: the Windows deliverable is a WiX `.msi`, and the licence reasoning that chose NSIS was wrong about WiX

**What changes.** `apps/plaudon-egui/packaging/windows/plaudon-egui.nsi` is deleted and replaced by
`plaudon-egui.wxs`, a WiX v5 source producing a per-user `Plaudon-<version>-x64.msi` (task O2.10,
#250). `scripts/package-windows-egui.ps1` drives `wix build` instead of `makensis`, and
`apps/plaudon-egui/tests/packaging.rs` parses the `.wxs` against
`plaudon_platform::associations::windows_registry_script` with the same strictness it applied to the
`.nsi`, plus assertions the NSIS file had no equivalent of: every registry row names `HKCU`
explicitly, the package is `Scope="perUser"`, the `UpgradeCode` is the constant it must stay, every
component carries a written-out GUID, and the package contains no custom action, no `Binary` row and
no sequence override. The record above is unchanged in substance — "no bundler kept for packaging"
still holds, and the two scripts are still the packaging path — but one sentence in it, "an NSIS
script we already have", now names a file that no longer exists.

**The licence reasoning is corrected, not quietly dropped.** The `.nsi` header and
`docs/licensing/third-party-register.md` said NSIS was chosen over WiX because NSIS's zlib/libpng
licence is permissive and the WiX Toolset is MS-RL, a reciprocal licence "we do not want in the
artifact path". The comparison was not like for like. NSIS's stub really is in the artifact path:
a `setup.exe` is an executable containing NSIS's own code, which makes its licence a redistribution
question and earned it a `NOTICE` entry. An `.msi` is a database that Windows' own installer service
interprets; it contains our files and our table rows and no WiX code at all. MS-RL constrains what
is distributed, so it binds no one here: WiX belongs in the same row as `signtool`, `codesign` and
`notarytool` — tools that stay on the build machine. The register row and the `NOTICE` entry have
been rewritten to say exactly that, and the NSIS notice was removed rather than left to imply that
something of NSIS still ships.

**Why change at all, if the NSIS installer worked.** Because the release workflow added in the same
task publishes to users, and what Windows expects from a published application is an `.msi`: install,
upgrade (`MajorUpgrade` against a fixed `UpgradeCode`, instead of a hand-written "uninstall the old
one first"), repair, and an uninstall performed by the operating system, with the Programs and
Features entry written by the system rather than by eleven `WriteRegStr` lines of ours. It is also
less of our code in the privileged position: the `.msi` runs nothing of ours at install time, where
the NSIS script ran its own `System::Call` into `shell32`.

**What this costs, stated rather than glossed over.** The package no longer broadcasts
`SHCNE_ASSOCCHANGED`, because that needs a custom action and a custom action is executable code
inside an installer; Explorer therefore picks the `.plaudon` association up on its own refresh
instead of immediately. There is no directory page; `msiexec /i … INSTALLFOLDER="…"` is the
override. And the install location is expressed as a conditional `SetDirectory` rather than a named
directory tree, because `wix build` canonicalises every `Directory/@Name` through a Windows-only
path routine — written the WiX-idiomatic way, the file could not be compiled, and therefore could
not be checked, anywhere but Windows.

**What is still unverified.** Everything about the `.msi` that needs Windows: WiX binds the database
through `msi.dll`, so no `.msi` has been produced on this workstation, and installing, upgrading and
uninstalling one has not been done on real hardware. The Linux container proves only that the `.wxs`
compiles and links, and `.github/workflows/release.yml`'s `windows` job is where the package itself
is built. The consequence "the Windows leg is type-checked, not run, outside CI" above applies to
this change in full.
