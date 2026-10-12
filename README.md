# Plaudon

A local-first desktop application for importing, transcribing, searching and
exporting voice recordings, on macOS and Windows. It is an independent product:
not produced by, affiliated with or approved by PLAUD, OpenRouter or Apple.

It is Rust, end to end. The user interface is [egui](https://github.com/emilk/egui)
0.36 drawn by `eframe` with the `wgpu` renderer and AccessKit; there is no
embedded browser engine, no JavaScript, no IPC boundary, and **no node, npm,
vite or Playwright anywhere in the build or the test suite**. Everything below
runs through `cargo` and two platform packaging scripts.

That was not always true: the first shell was Tauri 2 with a React frontend.
`docs/adr/ADR-0023-egui-shell-and-the-accessibility-evidence-plan.md` records
why the toolkit changed and the accessibility evidence that justified it, and
`docs/adr/ADR-0024-delete-the-tauri-react-stack-at-the-egui-cutover.md` records
why the old stack was deleted outright rather than kept beside the new one.

## Layout

| Path | What it is |
| --- | --- |
| `apps/plaudon-egui` | the desktop application: binary `plaudon-egui`, product name "Plaudon", bundle identifier `app.plaudon.desktop` |
| `crates/ui`, `crates/ui-testkit` | the screens and widgets, and the `egui_kittest` harness they are tested through |
| `crates/app` (`plaudon-app`) | the application layer: the published command surface, the event channel, DTOs, ports, validation. It must never depend on `egui` or `eframe`, so it stays testable headlessly |
| `crates/domain`, `crates/persistence`, `crates/jobs`, `crates/security`, … | the core: no shell, vendor or HTTP dependency, enforced by a dependency-boundary test |
| `crates/automation` | the `plaudon` command-line tool, installed beside the application |
| `third_party/craft-ui` | one vendored widget (the accessible resize handle), with its provenance and licence |

## Build and run

```sh
cargo run -j 2 -p plaudon-egui                 # debug
cargo build -j 2 --release -p plaudon-egui     # what the packages contain
```

On macOS, `whisper-rs` needs the SDK's C++ headers — export these separately,
in this order:

```sh
export SDKROOT=$(xcrun --show-sdk-path)
export CXXFLAGS="-isystem $SDKROOT/usr/include/c++/v1"
```

On Linux the shell additionally needs `libdbus-1-dev` (AccessKit's AT-SPI
adapter and the secret-service keyring backend) and `libasound2-dev` (cpal).

**Resource policy.** One cargo process at `-j 2`, scoped with `-p`. Anything
workspace-wide, any clippy `--workspace`, anything touching `whisper-rs`, and
every snapshot or performance run goes through `scripts/e2e-remote.sh`.

### Feature flags

All off by default; the defaults produce a working application that reports
what it cannot do instead of pretending.

| Feature | Crate it reaches | Effect |
| --- | --- | --- |
| `apple-bridge` | `plaudon-ai-apple`, `plaudon-app` | links the Swift bridge so Apple Speech and Foundation Models are reachable. macOS only; needs `scripts/build-apple-bridge.sh` and Xcode. The dependency is named directly by `apps/plaudon-egui` because Cargo only hands `DEP_<LINKS>_LINK_ARGS` to a direct dependent's build script, and without it the bundle loses the Swift rpath and fails to launch |
| `hardware-scan` | `plaudon-app` | links the real Bluetooth radio (`btleplug`). With it off the Devices screen reports `UnavailableInBuild` — "no radio in this build", which is deliberately not the same message as "no devices found". CI never enables it |
| `local-whisper` | `plaudon-app` | links the local whisper.cpp speech engine. Off by default because it compiles whisper.cpp; with it off the transcribe executor reports the engine as unavailable in this build. **A release bundle is expected to turn it on** |
| `cpal-output` | `plaudon-audio-playback` | the cpal device backend. The binary enables it; `crates/ui` deliberately does not, so the widgets and the kittest suite build in a container with no audio stack |

```sh
cargo build -j 2 --release -p plaudon-egui --features "apple-bridge,local-whisper"
```

### Running against another library

`PLAUDON_DATA_DIR` replaces the data directory for one run — database, media
archive, `logs/`, `peak-cache/` and `shell-state.json` all follow it, so the
installed library is untouched. It must be an absolute path; an empty or
relative value is refused with a typed error rather than guessed at.

```sh
PLAUDON_DATA_DIR=/tmp/plaudon-smoke cargo run -j 2 -p plaudon-egui
```

## Test

Every test in the repository is a Rust test. UI, end-to-end and visual tests
are `egui_kittest` through `cargo test`; pixel comparison is opt-in
(`PLAUDON_VISUAL=1`) and valid only on the one renderer that defines the
baselines. `docs/EGUI_TESTING.md` is the full strategy, including which suite
runs where.

```sh
cargo test -j 2 -p plaudon-egui -p plaudon-ui -p plaudon-ui-testkit -p plaudon-platform -p plaudon-craft-ui
```

## Package

```sh
scripts/package-macos-egui.sh          # dist/macos-egui/Plaudon.app, signing report
pwsh scripts/package-windows-egui.ps1  # dist/windows-egui/, NSIS installer, signing report
```

There is no bundler in the build: these scripts *are* the bundler, and
`apps/plaudon-egui/tests/packaging.rs` asserts their inputs on both platforms.
Both fail closed without a signing credential and exit 3 — "built and
verifiable, NOT publishable" — rather than printing a status they did not earn.
No notarized macOS build and no Authenticode-signed Windows installer exists
today, because the project holds no credentials. `docs/EGUI_BUILD.md` has the
exact prerequisites, the environment variables, the report contents and the
release gate.

## Documentation

| Document | Contents |
| --- | --- |
| `docs/adr/README.md` | every architecture decision record, with the range each work stream owns |
| `docs/EGUI_BUILD.md` | building, testing and packaging the application |
| `docs/EGUI_TESTING.md` | the test strategy, the accessibility assertions and the snapshot policy |
| `docs/EGUI_ARCHITECTURE.md`, `docs/EGUI_DESIGN_SYSTEM.md` | the layers and how to add a screen; tokens and components |
| `docs/threat-model/` | the machine-checked threat, control and verification matrix |
| `docs/public/` | the user-facing claim documents, gated by `plaudon-release doc-lint` |
| `docs/licensing/third-party-register.md`, `NOTICE` | every third-party component, its licence and the attribution it requires |

## Licence

Apache-2.0, as every workspace crate declares. Third-party material bundled or vendored here is
recorded in `NOTICE` and in `docs/licensing/third-party-register.md`.
