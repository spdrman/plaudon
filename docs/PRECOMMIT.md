# The pre-commit gate

GitHub Actions is **disabled** for this repository (#257). The three workflows were switched off
with `gh workflow disable`; their files are still in `.github/workflows/` and nothing in them was
edited, so they can be switched back on in one command — see §5.

Until then the gate is local: [`scripts/pre-commit.sh`](../scripts/pre-commit.sh), run by
[`.githooks/pre-commit`](../.githooks/pre-commit). It reproduces every CI job step that a macOS
arm64 workstation (plus the Linux runner it already uses for heavy work) can honestly run, and it
is explicit about the ones it cannot. **Four jobs are not reproducible anywhere locally** — they
need a native Windows x86-64 runner — and no check below pretends otherwise (§3).

---

## 1. Install

```sh
scripts/install-hooks.sh          # git config core.hooksPath .githooks — idempotent
scripts/install-hooks.sh --uninstall
```

The hook is thin: it execs `scripts/pre-commit.sh --hook`. Everything else — the registry, the
ordering, the skip handling — lives in the script, so it can be run, listed and argued with by
hand.

```sh
scripts/pre-commit.sh                      # every check, fail-fast
scripts/pre-commit.sh --list               # the registry and the CI job each check stands in for
scripts/pre-commit.sh --only fmt,clippy    # one or two of them (repeatable, comma-separated)
scripts/pre-commit.sh --skip coverage      # everything except these
scripts/pre-commit.sh --skip-slow          # the seconds-long native gates only
scripts/pre-commit.sh --keep-going         # do not stop at the first failure; report at the end
```

`git commit` passes no flags, so the hook is configured with the environment:

| Variable | Effect |
| --- | --- |
| `PLAUDON_PRECOMMIT_ONLY=fmt,clippy` | same as `--only` |
| `PLAUDON_PRECOMMIT_SKIP=coverage,package-macos` | same as `--skip` |
| `PLAUDON_PRECOMMIT_SKIP_SLOW=1` | same as `--skip-slow` |
| `PLAUDON_PRECOMMIT_KEEP_GOING=1` | same as `--keep-going` |

**A partial run says so, loudly.** Any `--only`, `--skip` or `--skip-slow` prints a banner before
*and* after the run naming every check that did not run and the CI job it stands in for, and the
final "everything passed" line is only printed when nothing was skipped. A partial run must never
be mistaken for a full one in a terminal scrollback or a pasted log.

**`git commit --no-verify` still works and is still yours to take.** It is not disabled, not
detected and not recorded. The gate's job is to tell you what it checked, not to trap you.

---

## 2. What runs, and for which CI job

Order is fail-fast: the seconds-long native gates first, then the workspace builds, then the
packaging and coverage runs that cost the most.

| # | Check | Tier | Runs on | Stands in for (workflow › job › step) | Command |
| --- | --- | --- | --- | --- | --- |
| 1 | `fmt` | fast | this host | CI › `rust` (macos-15) › Formatting | `cargo fmt --all --check` |
| 2 | `compat-matrix` | fast | this host | CI › `release-gates` › Compatibility matrix is generated, not written | `plaudon-release compat-matrix --check` |
| 3 | `doc-lint` | fast | this host | CI › `release-gates` › Public documentation lint | `plaudon-release doc-lint` |
| 4 | `scenarios` | fast | this host | CI › `release-gates` and `acceptance` › Spec 18.7 scenario registry | `plaudon-release scenarios` |
| 5 | `deny` | fast | this host | CI › `supply-chain` › Advisories, licences, bans and sources | `cargo deny --all-features check licenses bans sources advisories` |
| 6 | `licence-register` | fast | this host | CI › `supply-chain` › Model and third-party licence register | `scripts/license-audit.sh --register-only` |
| 7 | `sbom` | fast | this host | CI › `supply-chain` › Software bill of materials | `scripts/sbom.sh --out target/precommit/sbom/sbom.json` |
| 8 | `build-metadata` | fast | this host | CI › `rust` (macos-15) › Reproducible build metadata | `plaudon-release build-metadata --out target/precommit/build-metadata/<host triple>.json` |
| 9 | `clippy` | slow | runner **+** this host | CI › `rust` (macos-15) › Clippy | runner: `cargo clippy --workspace --all-targets --all-features` minus the four Apple-tainted crates; this host: the same lints for `plaudon-ai-apple`, `plaudon-app`, `plaudon-egui`, `plaudon-hardware-tests`. Both deny warnings. |
| 10 | `tests` | slow | runner **+** this host | CI › `rust` (macos-15) › Unit, integration and contract tests | runner: `cargo test --workspace --all-features` minus the four; this host: `cargo test -p plaudon-app -p plaudon-hardware-tests --all-features` and `cargo test -p plaudon-egui --all-features --no-run` |
| 11 | `persistence` | slow | runner | CI › `rust` (macos-15) › Database migrations apply to a fresh database and reject drift | `cargo test -p plaudon-persistence --all-features` |
| 12 | `chaos` | slow | runner | CI › `rust` (macos-15) › Crash-recovery suite | `cargo test -p plaudon-chaos-tests --all-features` |
| 13 | `egui-suites` | slow | runner | CI › `egui-desktop` (macos-15) › UI, shell and packaging suites | `cargo test -p plaudon-egui -p plaudon-ui -p plaudon-ui-testkit -p plaudon-platform -p plaudon-craft-ui` |
| 14 | `acceptance` | slow | runner | CI › `acceptance` (macos-15) › Spec 18.7 registry + Acceptance scenarios | `plaudon-release scenarios` then `cargo test -p plaudon-e2e --all-features -- --nocapture` |
| 15 | `apple-hermetic` | slow | runner (Linux is the point) | Apple bridge › `hermetic`, all six steps | `ai-apple`, `app-core`, `h_apple_bridge`, `apple_bridge_stress`, clippy, and the check that `--features apple-bridge` *refuses* a non-macOS target |
| 16 | `apple-no-symbols` | slow | runner (Linux is the point) | Apple bridge › `no-apple-symbols` | `cargo build -p plaudon-ai-apple --release` then `scripts/check-no-apple-symbols.sh` |
| 17 | `apple-macos` | slow | this host (macOS only) | Apple bridge › `macos`, all five steps | `scripts/build-apple-bridge.sh build`, the `minos 13.0` / weak-framework link probe, `cargo test -p plaudon-ai-apple --features apple-bridge`, `scripts/apple-bridge-sanitizers.sh`, the opt-in hardware gate |
| 18 | `extraction-eval` | slow | runner | Extraction Evaluation › `evaluate` › Run extraction evaluation | `bash scripts/run-extraction-eval.sh` |
| 19 | `package-macos` | slow | this host (macOS only) | CI › `egui-desktop` (macos-15) › Package the macOS bundle | `scripts/package-macos-egui.sh --evidence …`; exit 3 or 0, and the report must say `publishable: false` and `notarization_status: not_verified` |
| 20 | `onnx-embedder` | slow | this host (macOS only) | no CI job — the ADR-0081 §3 inference runtime is off in every shipped configuration | `cargo clippy --offline`/`cargo test --offline -p plaudon-ai-speaker --features onnx`, and **only** when `PLAUDON_ONNX_EMBEDDER=1`. Without the variable the check prints what it skipped and why. The build is `--offline` on purpose: since #283 the ONNX Runtime is vendored by digest (`scripts/fetch-onnxruntime.sh`, `ORT_LIB_LOCATION`) instead of downloaded by `ort`'s build script, and `--offline` is what proves it stays that way. The crate is still excluded from the workspace `--all-features` runs because the vendored archive is 83 MB and is not in the repository, so a machine that has not fetched it cannot link the crate. |
| 21 | `ci-selftest` | slow | this host | CI › `pipeline-selftest` › Deliberately failing fixtures must fail their gates | `scripts/ci-selftest.sh` |
| 22 | `coverage` | slow | this host (macOS only) | CI › `coverage` (macos-15) › Branch coverage + Enforce the critical-module policy | `scripts/coverage.sh --json target/precommit/coverage/llvm-cov.json` then `plaudon-release coverage --llvm-cov …`. Needs `cargo install cargo-llvm-cov --locked --version ^0.6`. |

"runner" means [`scripts/e2e-remote.sh`](../scripts/e2e-remote.sh): the MARS Linux box in a
disposable Docker container, falling back to a local arm64 container when MARS is busy or
unreachable. The workstation budget is 5 cores / 20 GB across everything, so the script pins
native cargo work to `CARGO_BUILD_JOBS=2` and sends every workspace-wide build and every renderer
suite that Linux can compile to the runner — the same rule as `docs/EGUI_BUILD.md` §1 and
`docs/EGUI_TESTING.md`. What stays here is what only a Mac can do: the Swift bridge, the `.app`
bundle, the four crates whose `--all-features` pulls in `apple-bridge`, and the coverage run that
measures them.

CI's environment is reproduced too: `PLAUDON_LOCAL_ONLY=1` (an accidental outbound call from
application code fails loudly, spec 22(15)), `RUST_BACKTRACE=1`, and `RUSTFLAGS=-D warnings` for
the Apple-bridge checks, which is what `apple-bridge.yml` sets workflow-wide.

### Differences from the CI step, and why

* **The runner has no `.git`.** `scripts/e2e-remote.sh` excludes `.git/` from its rsync, so suites
  that inspect the committed tree (`tests/integration/tests/workspace_layout.rs`,
  `tests/scale/src/results.rs`, the `ai-diarization` quality-metrics schema test) would fail for
  the wrong reason. The `tests`, `acceptance`, `apple-hermetic`, `extraction-eval` and `coverage`
  checks therefore make a throwaway snapshot commit inside the container before running. That is a
  repository, but it is not *this* repository's history: a test that reads real git history would
  see one commit.
* **`licence-register` repeats `deny`.** `scripts/license-audit.sh` ignores its arguments, so
  `--register-only` — the flag the CI step passes — runs the same `cargo deny … check licenses bans
  sources advisories` as check 5. Both are kept so the mapping to CI stays one-to-one; fixing the
  script is a separate change.
* **`--all-features` cannot be built on Linux, so checks 9, 10 and 21 are split or native.**
  `crates/ai-apple/build.rs` refuses any non-macOS target on purpose, and `apple-bridge` is
  re-exported by `plaudon-app`, `plaudon-egui` and `plaudon-hardware-tests` — so
  `--workspace --all-features` turns it on even with `--exclude plaudon-ai-apple`, and the runner
  cannot compile it. CI ran these on macos-15. Locally, everything that builds on Linux goes to
  the runner and the four Apple-tainted crates are built on this Mac, crate-scoped at
  `CARGO_BUILD_JOBS=2`; `coverage`, which measures the whole workspace in one process, runs
  natively in full. Nothing is dropped, but the split means the runner half and the native half
  do not share one feature-unified build the way the CI job did.
* **`plaudon-egui`'s suites are compiled here and run there.** The renderer rule
  (`docs/EGUI_TESTING.md`) keeps kittest off workstations, so check 10 builds them with
  `--all-features --no-run` and check 13 executes them on the runner with default features. No
  local run executes the egui suites *with* `apple-bridge` on; CI's macOS leg did.
* **`chaos` and `tests` are pinned to the local container.** `tests/chaos`'
  `an_unwritable_archive_fails_the_commit_without_registering_an_asset` asserts that a read-only
  directory really cannot be written. Inside a MARS container the command runs as the container's
  root, which ignores the permission bit; in local mode `scripts/e2e-remote.sh` drops to this
  host's uid, which does not. Measured both ways: the test passes locally and fails on MARS. So
  those two checks default to `PLAUDON_E2E_HOST=local`, and an explicit `PLAUDON_E2E_HOST` is
  still obeyed.
* **`coverage` needs a tool on your machine.** `cargo install cargo-llvm-cov --locked --version
  ^0.6` once; CI installed it per run. Without it the check reports `blocked (missing tool)` and
  fails — it never passes quietly.
* **Artifact uploads are not reproduced.** CI's `upload-artifact` steps have no local equivalent;
  the files land in `target/precommit/` (build metadata, SBOM, evidence, hardware log) and
  `target/e2e-results/` instead.
* **`cargo-deny` and `rustup` toolchain installs are not reproduced.** CI installs them per run;
  locally they are prerequisites. A missing tool makes its check report `blocked (missing tool)`
  and fail — it never passes quietly.
* **CI's "Verify the host target matches the matrix leg" has no local meaning**; `build-metadata`
  records the real host triple instead.

---

## 3. CI jobs nothing local can reproduce

These are **not checked** by the hook, by the repo, or by anything else while Actions is disabled.
They are listed in `scripts/pre-commit.sh --list` as well, so the gap is visible from the terminal.

| Workflow › job | Why not |
| --- | --- |
| CI › `rust` (**windows-2025**) | fmt, clippy, the workspace tests, the migration and chaos suites and the build metadata on a native Windows x86-64 runner. Spec 18.4 forbids calling a Windows leg green from a non-native environment, so this is not emulated under Wine or a VM and is not approximated by the macOS leg. |
| CI › `egui-desktop` (**windows-2025**) | the kittest suites on Windows, the Windows SDK resource compiler (`rc.exe`) linking the icon/version/manifest through `apps/plaudon-egui/build.rs`, NSIS, and `scripts/package-windows-egui.ps1` with its Authenticode signing report. |
| CI › `acceptance` (**windows-2025**) | the end-to-end acceptance scenarios on Windows. |
| Apple bridge › `macos` *on a non-macOS workstation* | needs a real Mac with Swift. Check 17 covers it on this host; a Linux or Windows contributor cannot run it at all. |

Everything Windows-shaped in this repository is therefore **unverified** until Actions is re-enabled
or someone runs the suites on a Windows machine. Do not describe a locally green commit as "CI
green".

---

## 4. The self-test

A gate nobody has seen fail is a gate nobody knows works, so the gate has its own self-test, in the
spirit of `scripts/ci-selftest.sh`:

```sh
scripts/pre-commit-selftest.sh
```

It clones the repository into `target/`, copies in the working-tree hook, runner and installer,
installs the hook in that clone, and proves five things: an unformatted file **blocks** `git commit`
and leaves `HEAD` where it was; the same commit succeeds once formatted; `--no-verify` goes through
regardless; a partial run prints the "did NOT run" banner naming the skipped checks; and
`install-hooks.sh` is idempotent and reversible. Your working copy is never touched.

---

## 5. Re-enabling GitHub CI

The workflow files were never edited — only the workflows were disabled — so:

```sh
gh workflow enable "CI"
gh workflow enable "Apple bridge"
gh workflow enable "Extraction Evaluation"
gh workflow list --all          # confirm: all three should read "active"
```

(By file: `gh workflow enable .github/workflows/ci.yml`, `apple-bridge.yml`,
`extraction-eval.yml`.) Re-enabling does not make the hook redundant — the hook is still the only
thing that runs before a commit exists — but it is what restores the Windows legs in §3 and the
artifact uploads.

---

## 6. How long it takes, measured

Measured on the workstation this was written on — Apple Silicon, 10 cores but a 5-core / 20 GB
budget shared with everything else, so native cargo runs at `-j 2` — with the MARS runner (28
cores) taking the remote half. Every number below is a real run of `scripts/pre-commit.sh`, not an
estimate.

| Run | Wall clock |
| --- | --- |
| Everything, `--keep-going`, warm caches (21 checks, nothing skipped) | **15 min 17 s** |
| Everything, cold MARS cache | **≈ 50 min** — the first remote workspace build alone took 36 min, after which the `/cache` volume is warm |
| `--skip-slow` (the eight fast checks) | **13 s** |
| `scripts/pre-commit-selftest.sh` | **16 s** |

Per check, from the warm full run:

| Check | Time | | Check | Time |
| --- | --- | --- | --- | --- |
| `fmt` | 1 s | | `acceptance` | 1 m 17 s |
| `compat-matrix` | <1 s | | `apple-hermetic` | 2 m 30 s |
| `doc-lint` | <1 s | | `apple-no-symbols` | 36 s |
| `scenarios` | <1 s | | `apple-macos` | 32 s |
| `deny` | 1 s | | `extraction-eval` | 1 m 22 s |
| `licence-register` | 1 s | | `package-macos` | 3 s warm, 3 m 54 s from a clean `target/` |
| `sbom` | <1 s | | `ci-selftest` | 2 s |
| `build-metadata` | 1 s | | `coverage` | 1 s (it fails immediately; see §7) |
| `clippy` | 1 m 18 s | | `persistence` | 1 m 12 s |
| `tests` | 3 m 17 s | | `chaos` | 31 s |

Two honest caveats about these numbers. They are **warm**: the MARS `/cache` volume already held a
built workspace and this clone already had a `target/`, which is the normal case for someone
committing several times a day and not the case on a fresh clone. And several checks **stop early
because the repository is currently red** (§7) — a green `clippy` and a green `tests` compile and
run more than a failing one does, so a fully-green full run will be somewhat longer than 15 min.

---

## 7. What the gate found, and what is still unverified

Running every check against `main` (commit `dea71c0`) found seven pre-existing failures. None of
them are caused by the hook; they are what has been accumulating while Actions has been off, and
they are *why* the gate exists. They are reported here rather than fixed, because fixing them is
not this change:

| Check | Failure |
| --- | --- |
| `compat-matrix` | `docs/public/compatibility.md` is stale; `plaudon-release compat-matrix` regenerates it |
| `clippy` | `plaudon-egress-monitor` has 5 lint errors (`single_match_else` and friends) under `-D warnings` |
| `tests` | `plaudon-diagnostics`' `committed_fixtures_pass_the_repository_gate`: committed updater/archive/eval fixtures trip the secret scanner on `device_token_hex`. Reproduced natively as well as on the runner, so it is not a runner artefact |
| `tests` (native half) | `cargo test -p plaudon-app --all-features` builds a test binary that cannot start: `dyld: Library not loaded: @rpath/libswift_Concurrency.dylib`. `crates/ai-apple/build.rs` emits `-Wl,-rpath,/usr/lib/swift` only for its own package's targets, so a dependent crate's test binary has no such rpath. CI's macos-15 `rust` job runs the same command |
| `egui-suites` | `plaudon-ui`'s `a_jobs_deep_link_is_announced_on_the_row_it_names` fails — **fixed in #275**: it was a race, not a broken assertion. The screen's first frame put two `list_jobs` answers in flight, and the later one overwrote the page the test had seeded |
| `apple-hermetic` | the job's workflow-wide `RUSTFLAGS: -D warnings` turns `plaudon-jobs`' `#[inline(always)]` into an error, so the clippy step cannot compile a dependency |
| `coverage` | `scripts/coverage.sh` passes `--branch`, which needs `-Z coverage-options=branch`, which needs nightly; `rust-toolchain.toml` pins **stable**. The CI job would fail identically |

Unverified, and deliberately so:

* **Everything Windows.** §3. No local check touches it.
* **A fully green full run.** It has not been observed, because the repository is not green. What
  has been observed is every check running, each producing the verdict above.
* **`package-macos` from a clean tree inside a full run.** Measured on its own (3 m 54 s); inside
  the warm full run it reused the bundle it had already built.
* **The MARS-versus-local split for suites other than `chaos` and `tests`.** The permission
  problem was found and fixed for those two; no other suite was audited for the same pattern.
