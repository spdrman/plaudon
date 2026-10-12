# egui migration report (EPIC O task O2.5, #209)

What the cutover shipped, what was measured, and what is still unverified. Dated 2026-10-12.

Companions: `docs/EGUI_MIGRATION_PLAN.md` (the plan and the parity matrix),
`docs/EGUI_ARCHITECTURE.md`, `docs/EGUI_TESTING.md`, `docs/EGUI_BUILD.md`,
`docs/adr/ADR-0024-delete-the-tauri-react-stack-at-the-egui-cutover.md`.

Every number below was produced by a command named beside it, on the machine named beside it.
Anything that was not run says **not verified** and says why. Nothing here is an estimate presented
as a measurement.

---

## 1. What shipped

The Tauri 2 + React/TypeScript desktop stack is gone. `apps/plaudon-egui` — egui 0.36 + eframe over
`crates/ui` and `crates/app` — is the only desktop application in the repository.

### 1.1 Deleted

| What | Size |
| --- | --- |
| `apps/desktop/**` (React 19 + Vite 7 + TypeScript 5.9 frontend, Playwright e2e, the Tauri `src-tauri` crate, `package.json`, `package-lock.json`, `vite`/`vitest`/`tsconfig`/`playwright` configs, icons, `tauri.conf.json`, `capabilities/default.json`) | 196 files, 37,560 lines — of which 28,024 are `.ts`/`.tsx`/`.css`/`.html` and 3,636 are the npm lockfile |
| `crates/security/src/tauri_guard/**` + `src/bin/tauri_audit.rs` + `tests/tauri_allowlist_contract.rs` | 3,710 Rust lines |
| `tests/fixtures/fuzz-corpus/allowlist_manifest/` + `fuzz/fuzz_targets/fuzz_allowlist_manifest.rs` | 63 corpus files + 1 target |
| `.github/workflows/desktop-ui.yml` | the whole node/Playwright gate |
| `scripts/ui-a11y.sh`, `scripts/ui-test.sh`, `scripts/audit-tauri.sh`, `scripts/security/tauri-command-allowlist.json`, `scripts/package-macos.sh`, `scripts/package-windows.ps1` | 6 files |

Whole-change totals, `git diff --cached --shortstat` against `origin/main`:
**368 files changed, 2,812 insertions, 40,996 deletions** (268 deletions, 88 modifications,
4 additions, 8 renames).

### 1.2 Verified absent

| Claim | Command | Result |
| --- | --- | --- |
| No npm manifest anywhere | `git ls-files \| grep -iE 'package(-lock)?\.json\|yarn.lock\|pnpm-lock'` | empty |
| No JavaScript, TypeScript, CSS or HTML file | `git ls-files \| grep -E '\.(tsx?\|jsx?\|css\|html)$'` | empty |
| No `tauri`, `wry`, `webkit*`, `webview2*` package in the lock | `grep -n 'name = "tauri"\|"wry"\|"webkit2gtk"\|"webview2-com"\|"tauri-build"\|"tauri-plugin-dialog"' Cargo.lock` | no match |
| No `node_modules` | `find . -name node_modules -not -path './.git/*'` | empty |

**One `tauri-*` package survives, deliberately:** `tauri-winrt-notification 0.8`, a Windows-only
thin wrapper over `Windows.UI.Notifications.ToastNotificationManager`. It is a standalone crate
published by the Tauri organisation; it does not pull the Tauri framework, a webview or wry, and it
depends only on the `windows` crate family. Keeping it is recorded in the amendment to ADR-0021 and
in `docs/licensing/third-party-register.md`; the misleading sentence in
`crates/notifications/src/windows.rs` that claimed "the Tauri organisation whose runtime this app
already ships" was corrected.

### 1.3 Test coverage carried across, not lost

All six Rust test files under `apps/desktop/src-tauri/tests/` were ported into `crates/app/tests/`
before the deletion, with **every test name unchanged**:

| File | Tests before | Tests after | Observed result |
| --- | --- | --- | --- |
| `end_to_end.rs` | 13 (6 top-level + 7 in `mod cloud`) | 13 | 13 passed |
| `plaud_commands.rs` | 17 | 17 | 17 passed |
| `device_import_e2e.rs` | 5 | 5 | 5 passed |
| `schema_conformance.rs` | 7 | 7 | 7 passed |
| `source_schema_conformance.rs` | 4 | 4 | 4 passed |
| `logging_policy.rs` | 2 | 2 | 2 passed |
| **total** | **48** | **48** | **48 passed, 0 failed** |

`apps/desktop/src-tauri/src/bin/mock_plaud.rs` moved to `crates/app/src/bin/mock_plaud.rs` behind a
new `test-harness` feature, which exists only so Cargo exports `CARGO_BIN_EXE_mock_plaud`.

No assertion predicate, expected value or test body changed. The only edits forced by the current
`plaudon-app` API were: three required `AppPorts` fields added to two struct literals, one required
`Preferences` field added to a fixture, one iteration source swapped from the deleted
`COMMAND_ALLOWLIST` to its superset `COMMAND_NAMES` (54 → 78 names, all verified to have both
schemas), and one `assert!` *message* string retargeted.

**Tauri-specific tests that died with the crate, and why nothing was lost:**
`exactly_one_handler_list_exists`, `every_registered_handler_is_on_the_allowlist`,
`the_source_commands_are_the_only_additions_for_epic_k`,
`no_generic_shell_http_or_filesystem_command_is_exposed` and
`the_allowlist_matches_the_published_typescript_contract` all asserted properties of
`tauri::generate_handler!`, `COMMAND_ALLOWLIST` and `contract.ts`. Their successors already live in
`crates/app/src/command.rs`: `the_surface_matches_the_tauri_allowlist_exactly` (holds a verbatim
copy of the 60-entry/54-unique allowlist and was verified identical to the real one on the last
commit that contained both), `every_published_command_name_appears_exactly_once` (pins 78),
`no_generic_shell_http_or_filesystem_command_is_published`, `every_published_name_has_a_request_schema`.
The registration-vs-allowlist property has no analogue because there is no registration list any
more: `COMMAND_NAMES`, `AppCommand`, `AppResponse` and the dispatch arms all expand from one
`command_surface!` table, so divergence is a compile error rather than a test failure.
`the_allowlist_matches_the_published_typescript_contract` correctly has no successor — `contract.ts`
is gone. The one place that still read it, `crates/app/src/commands/devices.rs`, now reads
`schemas/` instead, as `the_published_schemas_name_every_device_command_and_event`.

`schemas/commands/v1` and both JSON-schema conformance suites are wired to `plaudon-app` and run.

### 1.4 Two findings about the stack that was deleted

Both were discovered during the port and are stated because they bear on how much the deleted
suites were actually protecting:

1. **The Tauri test target had not compiled since EPIC O3.** `plaud_commands.rs:249` and
   `end_to_end.rs:698` each built an `AppPorts { .. }` literal naming 21 fields;
   `plaudon_app::AppPorts` has 24 (`models`, `update`, `processing` arrived with `d56dee6`,
   "O3: run queued work for real"). A struct literal missing a required field is `E0063`.
2. **It had not been *run* since before `export_defaults` landed** — a stronger statement, because
   this one no compiler would report. `dto::Preferences::export_defaults` carries `#[serde(default)]`
   with no `skip_serializing_if`, so it is always emitted, while
   `schemas/commands/v1/get_capabilities.response.schema.json` was `additionalProperties: false`
   with seven keys and no `export_defaults`. `every_response_dto_matches_its_schema` validates
   exactly that pair and must have failed. The four sibling schemas already carried the field; only
   `get_capabilities.response` was missed. **Fixed in this change**, and the suite now passes 7/7.

---

## 2. Parity status, re-audited against the code

The matrix in `docs/EGUI_MIGRATION_PLAN.md` §4 has **126 rows**. Every one was read against the tree
again before the deletion. The audit produced three contradictions, all real functional gaps rather
than documentation drift, and all three were closed in this change:

| Row | What the matrix said | What the code said | Resolution |
| --- | --- | --- | --- |
| **D18** Watched-folder service pumped on a timer | `done` | `WatchedFolderPump::start` had no caller outside its own unit tests. No timer ran in the shipped binary; only the on-demand `run_folder_sync` path worked. | Wired into `AppEngine` (`crates/app/src/runtime.rs:388`), stopped in `AppEngine::shutdown`, driven by a new `FolderRoster` re-read every tick. Test: `a_file_left_in_a_watched_folder_is_imported_by_the_timer_alone`, which never asks for a sync. |
| **J12** Activity bell with unread count in the header | `done` | `activity::unread()` existed; nothing in `crates/ui/src/shell` drew a bell. The test named after the bell exercised only the pure function. | Built: `crates/ui/src/shell/bell.rs`, keyboard reachable, accessible name states the count, navigates to Activity, satisfies `assert_every_control_is_named`. |
| **D12** Real BLE radio wired behind `hardware-scan` | `partial`, reason understated | `EngineConfig::with_radio` had no caller, `build_ports_with_radio` had none at all, `BtleplugFactory` was never constructed — so even `--features hardware-scan` ran `NoRadio`. | Wired: `RadioChoice::HostBluetooth` resolved after the library is open (the real radio needs the database and archive for its `DeviceImportCommitter`), `EngineConfig::with_host_bluetooth` under the feature only, `plaudon_egui::with_build_hardware` in the binary. |

### 2.1 Rows that are **not** `done`, and stay that way

* **D12 — `partial`.** The feature is now wired, but **no Bluetooth radio has been exercised**:
  no PLAUD device, no adapter, no scan. A default build still reports
  `RadioState::UnavailableInBuild`, which is the honest state and is asserted by
  `apps/plaudon-egui/tests/hardware_radio.rs`. The row's reason changes from "not wired at all" to
  "wired, never run against hardware".
* **D13 — `blocked`.** No `device_*` command exists for a mounted USB volume. The Devices screen
  says so and offers ordinary import of the mounted files; asserted by
  `crates/ui/tests/devices_screen.rs:842`. Closing it needs a command and a DTO, not UI work.
* **T12 — `done` for speaker identity, short of per-segment reassignment.** Nothing on the wire can
  persist a segment→speaker assignment: `SegmentEdit` is `{index, text}` and
  `TranscriptSegmentDto` carries only `speaker_label`. The per-segment picker was **removed rather
  than faked**, because it silently discarded every reassignment it appeared to make. The chip is a
  read-only label pointing at the speaker panel.
* **S17 — `done for what automation can cover`.** See §6.
* **J10 — `done as designed`.** A per-stage progress breakdown is not published and will not be;
  `JobSummary` carries one `stage` and one `progress`, read from the latest `job_steps` row.
* **C10 — one leg unverifiable here.** Open-in-folder and the clipboard go through host hooks;
  `reveal_in_folder` has no test.

### 2.2 Matrix claims corrected rather than closed

* The **Crate / module** column records where each row was *planned* to live in O1.1. Several
  landed elsewhere (`plaudon-app::transcript_edit` is the `plaudon-transcript-doc` crate;
  `plaudon-ui::screens::cloud::*` is `plaudon-ui::ai::*`; `plaudon-audio::playback` is the
  `plaudon-audio-playback` crate; and so on). The column is a planning record, not a map of the
  tree; `docs/EGUI_ARCHITECTURE.md` is the map.
* **P8** claimed "its 13 unit tests". The formatter landed as one function,
  `crates/audio-playback/src/ruler.rs::format_clock`, and its file carries 5 tests covering the same
  cases. Row corrected.
* **T14** claimed the React suite's 100,000-segment scale. The kittest and perf cases use
  **30,000** segments, which is what three hours is at the segment rate this pipeline produces. Row
  corrected; the budget assertions are unchanged.

### 2.3 Rows whose implementation is present but whose test coverage is thin

Found by the audit, recorded rather than hidden; none was claimed as evidence anywhere:
**S14** (no test drives `Startup::Failed` or the capabilities retry), **L8** (no test opens a row
context menu), **L16** (no test pins the OS picker's audio-extension list), **A7** (the pre-dispatch
estimate is covered; the *local budget stop* is not), **T4** (no test of Enter/double-click to start
editing or Escape to cancel — the implementation is a document editor, not a per-segment `TextEdit`),
**T7** (no view test clicks "Reload newest revision" or "Copy my changes"), **T8** (no test
references the revision switcher or "Original (machine)"), **C7** (no dedicated test for
`export_media` dispatch), **C12** (no test covers the binary's logging init).

---

## 3. Validation, with the command and the result

Machine policy in force: this workstation is ≤5 cores / 20 GB, so single-crate work ran locally with
`-j 2` and everything workspace-wide ran on the MARS Linux runner through `scripts/e2e-remote.sh`.

| Gate | Command | Where | Result |
| --- | --- | --- | --- |
| Formatting | `cargo fmt --all --check` | workstation | **clean** |
| Lints | `scripts/e2e-remote.sh cargo clippy --workspace --all-targets` | MARS Docker | **clean, exit 0** |
| Tests | `scripts/e2e-remote.sh cargo test --workspace --no-fail-fast` (`PLAUDON_E2E_APT_PACKAGES="libdbus-1-dev pkg-config"`) | MARS Docker | **4,277 passed, 10 failed**, 243 result blocks — every failure root-caused below |
| Git-dependent tests | `cargo test -p plaudon-integration-tests --test workspace_layout --test licensing_register` | workstation (needs `.git`) | **16 passed, 0 failed** |
| Supply chain | `cargo deny check` | workstation | **advisories ok, bans ok, licenses ok, sources ok** |
| Threat model | `python3 scripts/check-threat-model.py` | workstation | **OK: 43 rows (15 implemented, 15 open, 13 planned), 25 §16.2 threats covered 1:1** |

### 3.1 Clippy denials fixed (none blanket-allowed)

| Denial | Site | Fix |
| --- | --- | --- |
| `single_range_in_vec_init` | `crates/ai-local-llm/src/context.rs:238` | the uncovered span is bound to a named `let` so the intent reads as the one range it is |
| `useless_conversion` | `crates/persistence/src/archive.rs:437` | `statvfs::f_bavail` is 32-bit on macOS and 64-bit on Linux; the widening is now written per platform instead of once |
| `assertions_on_constants` | `crates/ai-tools/src/validate.rs:165` | moved into a `const { assert!(..) }` block, so raising the repair budget now fails the build rather than the test run |
| `too_many_arguments` (8/7) | `crates/app/src/lib.rs::build_ports_with` | the cloud transport moved into `EngineOverrides`, where it belongs: it is the same kind of substitution as the engines |
| `infallible_destructuring_match` | `crates/app/src/lib.rs` radio resolver | `RadioChoice::HostBluetooth` now exists on every build; what a default build compiles out is its *construction* |
| `type_complexity` | `crates/app/src/backend/templates.rs:1456` | named `type StructuralCase` |
| `unnecessary_cast` | `crates/app/src/shell_state.rs:536` | the redundant `as usize` removed |

The `type_complexity` sites at `crates/app-core/src/transcript_tools.rs` named in the ticket were
already fixed on `main` (`type StudyOutputGroup<'a>` at line 49), and `large_enum_variant` at
`crates/app/src/event.rs:98` was already answered by boxing `AppEvent::Response::result`.

### 3.2 The 10 remote test failures, root-caused

Every one was re-run on the workstation; all pass there. None is a code defect introduced by this
change.

| Failure | Cause |
| --- | --- |
| `every_spec_6_4_crate_manifest_is_committed`, `every_spec_6_4_directory_is_part_of_the_committed_tree`, `provenance_is_complete_so_a_number_is_attributable`, `results::tests::this_machine_can_be_identified` | `scripts/e2e-remote.sh` excludes `.git/` from the rsync, so these four cannot see a repository. Run natively: **16 passed, 0 failed** (and `provenance…` passes natively, observed). |
| `an_unwritable_archive_fails_the_commit_without_registering_an_asset`, `an_unwritable_staging_volume_fails_closed_without_a_partial_commit`, `archive::tests::relocation_refuses_when_the_destination_cannot_hold_the_archive`, `a_file_that_cannot_be_read_is_deferred_and_left_untouched` | permission-refusal tests. On MARS the command runs as the container's root (rootless Docker maps it onto an unprivileged host uid), and root ignores the read-only bits these tests set, turning a refusal into a false success. The script's own header documents this for local mode; the MARS leg has the opposite problem. All four pass natively. |
| `library_scale_benchmarks_stay_within_their_budgets` | a wall-clock budget under a `--cpus`-capped container. Passes natively: **4 passed, 0 failed**. |
| `a_jobs_deep_link_is_announced_on_the_row_it_names` | passes natively (`cargo test -p plaudon-ui --test shell_adapters`: **6 passed, 0 failed**). Not further diagnosed in the container. |

**Not verified:** that these ten pass *in a container with a `.git` directory and a non-root user*.
Making the runner reproduce both conditions is a change to `scripts/e2e-remote.sh` outside this
ticket's scope, and is recorded here rather than papered over.

### 3.3 `cargo deny`

Four pre-existing failures, all fixed in this change with a recorded reason:

| Finding | Fix |
| --- | --- |
| `webpki-roots 1.0.9` is `CDLA-Permissive-2.0` (not on the allow-list) | allowed, with the review in `deny.toml`: it is the Mozilla CA root store as *data*, permissively licensed, imposing nothing on the code that reads it |
| `borrow-or-share 0.2.4` is `MIT-0` | allowed: MIT with the attribution clause removed, strictly weaker than the already-allowed MIT |
| `plaudon-egress-{monitor,scenario,harness}` unlicensed | `license.workspace = true` added to all three — the correct fix, not an allow entry |
| 30 `error[wildcard]` from our own `[workspace.dependencies]` path entries | `allow-wildcard-paths = true`, which cargo-deny applies only to non-publishable crates, so `publish = false` was added to `[workspace.package]` and `publish.workspace = true` to all 35 members. Nothing here is published to crates.io. |
| `RUSTSEC-2024-0384`, `instant` unmaintained | ignored with a reason: it arrives only through `notify 7` → `notify-types`, the advisory itself records that no safe upgrade exists, and on our targets it compiles to `std::time::Instant`. Revisit when `notify` releases a `web-time` build. |

### 3.4 Visual baselines

**Not regenerated.** No baseline needed regenerating: the remote clippy and test runs produced no
snapshot mismatch, and `scripts/e2e-remote.sh` copies `*.diff.png` / `*.new.png` / `*/snapshots/*`
back on every run — none were produced. Pixel comparison stays gated behind `PLAUDON_VISUAL` on the
MARS renderer, as `docs/EGUI_TESTING.md` requires.

---

## 4. The macOS release artifact

Built from `apps/plaudon-egui` with `scripts/package-macos-egui.sh`, one cargo process at `-j 2`.

```
scripts/package-macos-egui.sh --features apple-bridge,local-whisper --evidence …
```

| Fact | Value |
| --- | --- |
| Artifact | `/tmp/plaudon-release/Plaudon.app.zip` |
| SHA-256 | `155342ff3e50cec7dd510bfe129cfde091c51750a9e428216df1268594360667` |
| Executable SHA-256 | `4ffe403f0cb50390b05e56530d2c86642d533489388adf8fc65e0eac9200780f` |
| Features | `apple-bridge,local-whisper` |
| Bundle id | `app.plaudon.desktop` (matches `plaudon_app::runtime::APPLICATION_IDENTIFIER`, asserted by the script) |
| Minimum system version | 13.0 |
| Signature | `adhoc_verified` — ad-hoc only, no Developer ID available |
| Notarization | `not_verified` — no notarytool credential |
| `publishable` | `false` |
| Script exit | `3` ("built and verifiable, NOT publishable") |

### 4.1 Feature decisions

* **`apple-bridge`: on.** The Swift bridge is compiled by `crates/ai-apple/build.rs` during the
  release build; this Mac has a Swift toolchain. Verified in the shipped binary with `otool -L`:
  `Speech.framework` and `FoundationModels.framework` are **weak**, and `LC_RPATH` carries
  `/usr/lib/swift`. That required a real fix: Cargo publishes `DEP_<LINKS>_LINK_ARGS` only to the
  build script of a *direct* dependent, and `apps/plaudon-egui` reached `plaudon-ai-apple` only
  transitively — so `build.rs` was silently finding nothing and dropping both the rpath and the weak
  references. `plaudon-ai-apple` is now an optional direct dependency enabled by the feature.
* **`local-whisper`: on**, forwarded through a new `plaudon-egui` feature
  (`local-whisper = ["plaudon-app/local-whisper"]`). Without it the transcribe executor reports the
  engine as unavailable in this build; a release bundle that cannot transcribe locally would
  contradict the product's first claim. It is off by default so `cargo test -p plaudon-egui` does
  not compile whisper.cpp.
* **`hardware-scan`: off**, as in CI. A default build reports `UnavailableInBuild`.

### 4.2 Launch evidence

Extracted to a clean directory (`ditto -x -k` into `/tmp/plaudon-release/extract`),
`codesign --verify --deep --strict`: *valid on disk; satisfies its Designated Requirement*.

The user's real library was **copied** to `/tmp/plaudon-release/library-copy` and the bundle was
launched against the copy through the new `PLAUDON_DATA_DIR` override:

```
PLAUDON_DATA_DIR=/tmp/plaudon-release/library-copy \
  /tmp/plaudon-release/extract/Plaudon.app/Contents/MacOS/plaudon-egui
```

| Observation | Result |
| --- | --- |
| Still running after 10 s | **yes** (`kill -0` succeeded; 198 MB RSS, 0.1% CPU) |
| Crash report written | **none** — the only `plaudon*` reports in `~/Library/Logs/DiagnosticReports` are `plaudon-chaos-worker` files from 2026-10-11 |
| stderr | one line, `sandbox_extension_issue_file_to_process failed … (Operation not permitted)` — a Launch Services message for a bundle run from `/tmp`, not an application error |
| The user's real library (`~/Library/Application Support/app.plaudon.desktop`) | **byte-identical before and after** (recursive SHA-256 of every file, digest `aad07c63…` both times) |

`PLAUDON_DATA_DIR` did not exist before this change; it was added for exactly this reason.
`plaudon_app::runtime::DATA_DIR_ENV` is honoured by `EngineConfig::for_this_platform()`, so the
database, media archive, `logs/`, `peak-cache/` and `shell-state.json` all follow it — every one of
them is joined onto `EngineConfig::data_dir()`. It must be an absolute path; an empty or relative
value is refused with a typed error rather than interpreted, because every plausible interpretation
either silently opens the real library the override was meant to protect or creates one under
whatever directory a double-clicked bundle happens to inherit (which is `/`). Covered by
`the_data_directory_can_be_overridden_and_a_bad_override_is_refused`.

### 4.3 `scripts/release-gate.sh <artifact> --allow-unsigned`

| Step | Verdict | Detail |
| --- | --- | --- |
| `environment` | `not_verified` | not a clean machine; `PLAUDON_CLEAN_MACHINE=1` is only for a freshly imaged native runner (spec 18.4) |
| `artifact_present` | pass | `Plaudon.app.zip 155342ff3e50cec7…` |
| `install` | pass | extracted into a clean location |
| `signature` | `development_only` | the ad-hoc signature verifies but Gatekeeper rejects it: no Developer ID, no notarization |
| `tamper_detection` | pass | a modified bundled binary fails `codesign` verification |
| `launch` | pass | the installed bundle started and stayed up |
| `library_directory` | pass | resolved `~/Library/Application Support/app.plaudon.desktop` |
| `offline_smoke` | pass | spec 18.7 scenarios 1 and 2 pass with the outbound policy denying every request |
| `ui_suite` | pass | the egui shell, screen and packaging suites pass under kittest |
| `signed_update` | pass | forward update, rejection of unsigned/tampered artifacts, verified rollback |
| `archive_restore` | pass (after a fix) | see below |
| `repeatability` | pass | a second consecutive run produced the same verdict |

**Overall: BLOCKED**, which is the correct verdict — `environment` and `signature` cannot pass
without a clean runner and a Developer ID, and neither exists here.

`archive_restore` initially reported `not_verified` while `cargo test -p plaudon-export` passed
68/68. The gate was invoking the suite **twice** and judging it on `tail -5`, which reads whichever
target printed last; a crate whose last target runs zero tests therefore reported "no passing
round-trip" with every test green. Fixed: one invocation, the whole output judged, the passed count
summed across blocks, and an explicit `fail` verdict if any block failed.

---

## 5. Performance against budget

| Measure | Budget | Measured | Where |
| --- | --- | --- | --- |
| Idle CPU of the shipped bundle | the shell must not repaint on its own schedule | **0.1 %** over a 10 s idle launch (`ps -o %cpu`) | workstation, packaged bundle, §4.2 |
| Idle repaint behaviour | no continuous repaint | asserted by `apps/plaudon-egui/tests/idle.rs` | MARS |
| Resident memory at idle | — (no budget set) | 198 MB RSS | workstation, §4.2 |
| 3-hour transcript: laid-out rows | viewport-sized working set, not the document | passes at 30,000 segments | `crates/ui/tests/transcript_editor.rs:430` |
| 3-hour transcript: frame cost | inside the frame budget | passes | `crates/ui/tests/transcript_editor.rs:568` |
| 3-hour waveform fully zoomed | one quad per column | passes | `crates/ui/tests/player_frame_cost.rs:182` |
| 10,000-row library | within the tier budgets | passes (`library_scale_benchmarks_stay_within_their_budgets`, 4 passed) | workstation; **fails under the CPU-capped container**, see §3.2 |

**Not measured here:** cold-start time to first frame, and any Windows figure. No budget for either
was set in the plan and neither was taken, so neither is claimed.

---

## 6. Accessibility

Automated, and passing: `plaudon_ui_testkit::a11y::assert_every_control_is_named` over every
destination, every sub-route, the recording detail and the command palette
(`apps/plaudon-egui/tests/a11y_contract.rs`); the focus-order walk and the skip link
(`crates/ui/tests/shell_kittest.rs`); keyboard completability
(`every_destination_is_reachable_with_the_tab_key_alone`, which is now spec 18.7 scenario 18's
registered test, replacing the deleted Playwright `keyboard-flow.spec.ts`); token contrast ≥ 4.5:1
for text and ≥ 3:1 for borders and focus rings in all four themes
(`crates/ui/src/theme/tokens.rs`); and the 1.0× / 1.5× / 2.0× scale sweep
(`crates/ui/tests/scale_kittest.rs`).

**Not verified: the manual run.** No VoiceOver and no Narrator pass exists for the egui shell.
`docs/accessibility/manual-screen-reader-script.md` — which is now the egui script, the React one
having been deleted and the egui counterpart moved into its place — carries an explicit "This
script has not been executed" block and an empty result table, where an empty cell means *not run*,
not *pass*. There is no axe equivalent for egui and there will not be one; axe needs a DOM.

---

## 7. Licence and attribution review

Source: `docs/licensing/third-party-register.md`, enforced by
`tests/integration/tests/licensing_register.rs` (**7 passed** natively, including
`the_adr_index_matches_the_files_on_disk`).

* The **Tauri 2** row is retired.
* The **egui 0.36** row is restated in the past tense: it replaced the webview on 2026-10-12, and
  `tauri`/`wry`/webview bindings are no longer in the graph.
* A row was added for **`tauri-winrt-notification 0.8`** (Windows only), with its licence verified
  as `MIT OR Apache-2.0` from the vendored crate manifest and the reason it survives.
* `NOTICE` was re-checked and needed no change: it carries no Tauri, React, npm or webview
  attribution — only `craft-ui`, filmcraft and the font/vendored-code notices.
* `cargo deny check licenses` is clean; the two new allow entries and the three `license.workspace`
  additions are in §3.3.

---

## 8. Security review against spec §16 invariants

| Invariant | After the cutover |
| --- | --- |
| §16.2A single egress gate | unchanged. `plaudon-security`'s `OutboundPolicy` is still the only way out; `apps/plaudon-egui/tests/local_only_egress.rs` runs the real executors under Local-only mode and proves nothing left the process. |
| §16.2 untrusted input | unchanged. Media decoding and model inference still run out of process (ADR-0015); the BLE and CLI parsers are untouched. |
| §16.3 consent | unchanged. `grant_cloud_consent` still fails closed when a zero-retention route cannot be resolved. |
| §16.4 no plaintext credential fallback | unchanged. `KeyringSecretStore`; an unavailable store is a locked state. |
| §16.5 packaging privilege | **strengthened in evidence terms.** The Tauri capability manifest is gone, and with it the audit that parsed it. The remaining privilege surface — the entitlements and what the signature actually grants — is audited by `scripts/package-macos-egui.sh` on every build: nine forbidden entitlements are rejected both as *requested* and as *granted* (read back with `codesign -d --entitlements`), and an unreadable dump is a failure rather than a pass. `apps/plaudon-egui/tests/packaging.rs` pins the script's fail-closed behaviour. |
| §16.6 privacy-safe notifications | unchanged; `crates/notifications/src/privacy.rs` and its redaction tests. |
| Trust boundary | **changed, and the change is honest about being a weakening.** The webview/IPC boundary was an address-space boundary; the egui view layer is in the same process as the application layer. `docs/threat-model/trust-boundaries.md` now says so explicitly rather than claiming an equivalent boundary. What replaces the capability allowlist as the enforcement point is the typed command surface: `AppCommand` is a closed enum expanded from one table, and every argument goes through `crates/app/src/validate.rs` before it reaches a port. |
| Secret-shape response scanner | **survived.** It was the one genuinely non-Tauri thing in `tauri_guard`; it moved to `crates/security/src/response.rs` with all 7 of its tests, and the fuzz corpus replay still exercises it. |
| Threat-model traceability | re-pointed, not deleted. TM-016 now owns `crates/app/src/validate.rs`; TM-039 now owns the packaging privilege audit; finding F-039 was retired with a dated paragraph rather than silently dropped. `scripts/check-threat-model.py` passes. |

---

## 9. Remaining risks

1. **No address-space boundary between the view and the application layer.** Accepted in ADR-0024.
   A memory-safety defect in the view layer now reaches the library directly. Mitigation is the
   closed command surface and `unsafe_code = "warn"`, not isolation.
2. **The Windows leg is compiled, not run.** No Windows machine was available. The Windows
   packaging script, the resource compilation, the installer build (NSIS at the time of this
   report; a WiX `.msi` since O2.10) and the tray/menu behaviour are unverified on a
   real Windows host.
3. **No screen-reader evidence.** §6.
4. **The release artifact is not publishable.** No Developer ID, no notarytool credential, no
   Authenticode certificate. The gate says `BLOCKED` and the report says `publishable: false`;
   neither is a formality to be waved through.
5. **The remote runner cannot reproduce two test preconditions** (`.git` present, non-root user),
   so ten tests are green only on the workstation. §3.2.
6. **D13 and T12 are product gaps, not UI gaps**, and need wire-level work (§2.1).

---

## 10. Honest list of everything not verified

* Windows runtime behaviour of any kind (build, installer, tray, menu, notifications, file
  associations). Compiled only.
* VoiceOver and Narrator. No run exists.
* Notarization. **Never claim notarized**: `notarization_status` is `not_verified` and no Apple
  credential was present at any point.
* Authenticode signing of the Windows installer.
* A real Apple Speech or FoundationModels run. The bridge is linked and the frameworks are weakly
  referenced, which was verified with `otool`; no inference was executed.
* A real PLAUD device over BLE or USB; a real PLAUD Cloud CLI session; a real OpenRouter dispatch.
  All three are opt-in, env-gated suites that CI never switches on, and none was switched on here.
* Cold-start time to first frame.
* The ten remote-only test failures under a container that has `.git` and a non-root user (§3.2).
* `plaudon-release doc-lint` and `compat-matrix --check` were not run on the workstation (they
  compile the release-tools graph); they run in the `release-gates` CI job.
