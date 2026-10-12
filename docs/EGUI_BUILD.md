# Building, testing and packaging the egui desktop app

Exact commands for `apps/plaudon-egui`, the desktop application (EPIC O, #197). Since the cutover
(#209, `docs/adr/ADR-0024-delete-the-tauri-react-stack-at-the-egui-cutover.md`) it is the only
application in the repository: there is no second shell, no `apps/desktop`, and no node, npm or
JavaScript toolchain anywhere in the build. Everything below is cargo and two platform scripts.

Companion documents: `docs/EGUI_MIGRATION_PLAN.md` (what is being built and why),
`docs/EGUI_ARCHITECTURE.md` (the layers, the command/event flow and how to add a screen),
`docs/EGUI_TESTING.md` (the test strategy and the MARS renderer rule),
`docs/EGUI_DESIGN_SYSTEM.md` (tokens and components).

---

## 1. Prerequisites

| Platform | Needed for | How |
| --- | --- | --- |
| all | the pinned toolchain | `rustup show` reads `rust-toolchain.toml` |
| macOS | C++ headers for `whisper-rs` | `export SDKROOT=$(xcrun --show-sdk-path)` then, **separately**, `export CXXFLAGS="-isystem $SDKROOT/usr/include/c++/v1"` |
| macOS | bundling, signing, the icon | Xcode Command Line Tools: `codesign`, `iconutil`, `sips`, `plutil`, `PlistBuddy` |
| macOS | notarization (optional) | `xcrun notarytool store-credentials` once, then `PLAUDON_NOTARY_PROFILE` |
| Windows | the executable's icon, version block and manifest | the Windows SDK resource compiler (`rc.exe`) or LLVM's `llvm-rc` on `PATH`, or `RC=<path>` |
| Windows | the installer | the WiX toolset, pinned: `dotnet tool install --global wix --version 5.0.2` — or `PLAUDON_WIX=<path to wix.exe>` |
| Windows | signing (optional) | `signtool.exe` from the Windows SDK plus a PFX in `PLAUDON_WINDOWS_CERT_PATH` |
| Linux | building and running the shell | `libdbus-1-dev` (AccessKit's AT-SPI adapter and the secret-service keyring backend), `libasound2-dev` (cpal) |

**Resource policy.** A workstation runs **one** cargo process at `-j 2`, scoped with `-p`. Anything
workspace-wide, any clippy `--workspace`, anything touching `whisper-rs`, and every snapshot or
performance run goes through `scripts/e2e-remote.sh` (MARS Docker, falling back to a local arm64
container when MARS is busy). The packaging scripts default to `-j 2` for the same reason;
`CARGO_BUILD_JOBS` overrides it on a CI runner.

**Who the container runs as.** Both modes install packages as root and then run the command itself
under an ordinary user. In local mode (`PLAUDON_E2E_HOST=local`, rootful Docker Desktop) that is the
host's own uid/gid, which owns the bind-mounted working copy and the `/cache` volume; root would
write straight through a read-only directory and turn a test that asserts a permission failure —
`tests/chaos`' unwritable archive — into a false pass. On MARS the command keeps running as the
container's root, because rootless Docker already maps that onto an unprivileged host uid and it is
the only id that can write the working copy.

---

## 2. Build

```sh
# debug, for development
cargo build -j 2 -p plaudon-egui
cargo run -j 2 -p plaudon-egui

# release, what the packages contain
export SDKROOT=$(xcrun --show-sdk-path)
export CXXFLAGS="-isystem $SDKROOT/usr/include/c++/v1"
cargo build -j 2 --release -p plaudon-egui
```

Optional features, all three off by default:

| Feature | Effect | Cost |
| --- | --- | --- |
| `apple-bridge` | links the Swift bridge so Apple Speech and Foundation Models are reachable; also enables the direct optional `plaudon-ai-apple` dependency, which is load-bearing (below) | needs `scripts/build-apple-bridge.sh` and Xcode; macOS only |
| `hardware-scan` | links the real Bluetooth radio (`btleplug`) | the build then touches a radio at run time; CI never enables it |
| `local-whisper` | links the local whisper.cpp speech engine (`plaudon-app/local-whisper`) | compiles whisper.cpp, which every `cargo test -p plaudon-egui` would otherwise pay for; with it off the transcribe executor reports the engine as unavailable in this build rather than pretending a model could run. **A release bundle is expected to turn it on.** |

Run or package with a feature the usual way:

```sh
cargo run -j 2 -p plaudon-egui --features local-whisper
cargo build -j 2 --release -p plaudon-egui --features "apple-bridge,local-whisper"
```

`apps/plaudon-egui/build.rs` does the two things Cargo will not do by itself:

* **It restates the Apple bridge's link arguments.** `crates/ai-apple` publishes them as `links`
  metadata (`DEP_*_LINK_ARGS`), and Cargo applies `rustc-link-arg` only to targets of the package
  that emitted it — so the binary that actually links the Swift static library has to repeat them.
  Without the `/usr/lib/swift` rpath the bundle cannot resolve `libswift_Concurrency.dylib`; without
  the weak framework references dyld refuses to start the app on a Mac older than macOS 26. This is
  also why `apple-bridge` names `plaudon-ai-apple` as a *direct* optional dependency rather than
  reaching it through `plaudon-app`: Cargo publishes `DEP_<LINKS>_LINK_ARGS` only to the build
  script of a package that depends on the `links` package directly. Without that edge `build.rs`
  finds no `DEP_PLAUDONAPPLEBRIDGE_LINK_ARGS`, drops the rpath and the weak references silently,
  and the bundle fails to launch.
* **It compiles the Windows resources** (`packaging/windows/plaudon-egui.rc`: icon, `VERSIONINFO`,
  side-by-side manifest) with `llvm-rc` or `rc.exe` and links the `.res` into the binary. With no
  resource compiler it prints a warning and continues, so a Linux or macOS cross-check still builds;
  `scripts/package-windows-egui.ps1` then refuses the resulting executable, because an installer
  must not ship a binary that lost its manifest.

### 2.1 Running against a library that is not the installed one

`PLAUDON_DATA_DIR` (`plaudon_app::runtime::DATA_DIR_ENV`) replaces the data directory for one run
of the process. `EngineConfig::for_this_platform()` honours it, and everything derived from the
data directory follows — the database, the media archive, `logs/`, `peak-cache/` and
`shell-state.json` — because each of them is built by joining onto `EngineConfig::data_dir()`:

```sh
PLAUDON_DATA_DIR=/tmp/plaudon-smoke cargo run -j 2 -p plaudon-egui
```

It must be an **absolute** path. An empty or relative value is refused with a typed error rather
than resolved against the working directory, which for a double-clicked bundle is `/`. This is what
a release smoke test uses, so launching the artifact never touches the installed library;
`default_data_dir()` deliberately ignores the override, so code that means "the real library" can
still say so.

---

## 3. Test

All UI, end-to-end and visual tests are `egui_kittest` through `cargo test`. No vitest, no
Playwright (`docs/EGUI_TESTING.md` §1).

```sh
# the shell, its screens, the harness, the platform adapters and the vendored widgets
cargo test -j 2 -p plaudon-egui -p plaudon-ui -p plaudon-ui-testkit -p plaudon-platform -p plaudon-craft-ui

# only the packaging inputs (identifier, entitlements, registry claim, icons, manifest)
cargo test -j 2 -p plaudon-egui --test packaging

# pixel comparison — MARS only; a baseline produced anywhere else is not valid
PLAUDON_E2E_APT_PACKAGES=libdbus-1-dev scripts/e2e-remote.sh shell \
  'PLAUDON_VISUAL=1 cargo test -p plaudon-egui --test spike_kittest'
```

`tests/packaging.rs` is the one suite that runs everywhere and asserts what neither packaging script
can assert on the other platform: the bundle identifier equals
`plaudon_app::runtime::APPLICATION_IDENTIFIER` (migration-plan risk R7), the entitlement set is
exactly the five reviewed privileges, the WiX `RegistryValue` rows are byte-equal to what
`plaudon_platform::associations::windows_registry_script` generates, the uninstall removes every
key the installer writes, the `.ico` carries the sizes Explorer asks for, and both scripts fail
closed without a credential.

---

## 4. The application icon

`apps/plaudon-egui/packaging/icon.svg` is the only hand-edited icon asset: original artwork, drawn
from primitives, in the design system's light-theme accent. Two derived artifacts are committed
beside it because the platforms that consume them have no SVG rasteriser:

```sh
scripts/make-app-icon.sh     # rewrites packaging/icon-1024.png and packaging/windows/plaudon.ico
```

Needs `rsvg-convert` (or `magick`, `convert`, `inkscape`) and, for the `.ico`, `magick`/`convert`.
The macOS `.icns` is not committed: `scripts/package-macos-egui.sh` builds it from the master raster
with `sips` and `iconutil` on every run, so the bundle cannot carry a stale icon.

---

## 5. Package — macOS (arm64 `.app`)

```sh
export SDKROOT=$(xcrun --show-sdk-path)
export CXXFLAGS="-isystem $SDKROOT/usr/include/c++/v1"
scripts/package-macos-egui.sh                       # dist/macos-egui/
scripts/package-macos-egui.sh --evidence ci-artifacts/evidence
scripts/package-macos-egui.sh --features apple-bridge
scripts/package-macos-egui.sh --skip-build          # reuse target/release/plaudon-egui
```

There is no bundler in the egui build, so the script *is* the bundler. It lays out

```
dist/macos-egui/Plaudon.app/Contents/
  Info.plist          template + the generated associations, merged with PlistBuddy, plutil-linted
  MacOS/plaudon-egui  the release binary
  Resources/plaudon.icns
  PkgInfo
  _CodeSignature/
```

and then signs, audits, zips with `ditto`, hashes and reports.

**Info.plist.** `apps/plaudon-egui/packaging/macos/Info.plist` holds everything fixed, with three
`@TOKEN@` placeholders (short version, bundle version, `LSMinimumSystemVersion`, default 13.0).
`CFBundleDocumentTypes`, `UTExportedTypeDeclarations` and `CFBundleURLTypes` — the `.plaudon`
archive type and the `plaudon://` scheme — are **not** in the template: they are merged in from
`crates/platform/resources/Info.associations.plist`, which `plaudon_platform::associations`
generates and a golden test pins. The usage descriptions are
`NSBluetoothAlwaysUsageDescription` (the BLE prompt only appears for a bundle that declares it),
`NSSpeechRecognitionUsageDescription` (Apple's recogniser refuses an unbundled process) and
`NSRemovableVolumesUsageDescription` (the legacy USB import reads a mounted device volume; macOS has
no separate raw-USB usage key and Plaudon never opens a USB device directly). There is deliberately
no microphone or camera key.

**Entitlements.** `apps/plaudon-egui/packaging/macos/entitlements.plist`, five keys: sandbox off,
user-selected files read-write, outbound network client, Bluetooth, USB. The script refuses to
package if the file requests a hardened-runtime escape hatch (JIT, unsigned executable memory,
library-validation opt-out, dyld environment variables, debugger, `get-task-allow`, camera,
audio input, network server), and refuses again after signing if the *signature* granted one —
`codesign -d --entitlements -` is what it audits, not the file it passed in. A debugger-attachable
production bundle is the native equivalent of shipping devtools, and that is where it would appear.

**Signing and notarization.** The honesty rules are absolute: a status is printed only when it was
earned.

| Environment | Signature | Notarization | Exit |
| --- | --- | --- | --- |
| `PLAUDON_MACOS_IDENTITY` + `PLAUDON_NOTARY_PROFILE` | `verified` | `verified`, stapled, `spctl` assessed | 0 — the only publishable configuration |
| `PLAUDON_MACOS_IDENTITY` only | `verified` | `not_verified` | **3** |
| neither | `adhoc_verified` | `not_verified` | **3** |

Exit 3 means "built and verifiable, NOT publishable". The script never prints "signed" for an
unsigned artifact and never prints "notarized" for one that was not submitted. `codesign --verify
--deep --strict` runs in every configuration; a failure there is exit 1, not a warning.

**Output.** `dist/macos-egui/Plaudon.app`, `Plaudon.app.zip`, `macos-signing-report.json`
(artifact and executable SHA-256, bundle identifier, resolved library directory, signature and
notarization status with their detail strings, entitlement audit, host build, commit,
`publishable`). With `--evidence <dir>` the report is copied to
`<dir>/signing-verification/macos-egui.json`.

**Verify a built bundle by hand:**

```sh
codesign --verify --deep --strict --verbose=2 dist/macos-egui/Plaudon.app
codesign -d --entitlements - dist/macos-egui/Plaudon.app
plutil -p dist/macos-egui/Plaudon.app/Contents/Info.plist | head -40
dist/macos-egui/Plaudon.app/Contents/MacOS/plaudon-egui      # it should stay up
ls "$HOME/Library/Application Support/app.plaudon.desktop"   # the library it opened
```

**The library directory.** The bundle identifier is `app.plaudon.desktop` — the identifier the
deleted Tauri shell shipped with, kept deliberately. `plaudon_app::runtime::default_data_dir`
resolves `dirs::data_dir().join(APPLICATION_IDENTIFIER)`, which on macOS is
`~/Library/Application Support/app.plaudon.desktop`: exactly where the old shell's
`app_data_dir()` put it. That is what makes
an existing library open in the new shell (migration-plan risk R7). The packaging script fails with
`reason=identifier_mismatch` if the plist and the application layer ever disagree, the packaging
test asserts the same thing on every platform, and `scripts/release-gate.sh` proves it on a real
launch.

---

## 6. Package — Windows (x64 `.msi`)

```powershell
pwsh scripts/package-windows-egui.ps1                                 # dist/windows-egui/
pwsh scripts/package-windows-egui.ps1 -Evidence ci-artifacts/evidence
pwsh scripts/package-windows-egui.ps1 -SkipBuild
```

WiX v5, pinned: `dotnet tool install --global wix --version 5.0.2`, or `PLAUDON_WIX=<path to
wix.exe>`. The script refuses a different version rather than building with whatever is installed.

**WiX, not NSIS — and why that reverses the earlier decision.** Until O2.10 this was an NSIS
`setup.exe`, chosen because NSIS's zlib/libpng licence is permissive and the WiX Toolset is MS-RL.
That reasoning was sound for the artifact NSIS produces and wrong about the artifact WiX produces:
the NSIS stub is *inside* the installer we hand to a user, so its licence is a redistribution
question, whereas an `.msi` is a database interpreted by the operating system's own installer
service and contains no WiX code at all. MS-RL applies to the toolset we build with, which we do
not redistribute, exactly like the Windows SDK's `signtool`. What the `.msi` buys is the thing the
release workflow needs: Windows' own install, upgrade, repair and uninstall path, a Programs and
Features entry written by the system rather than by us, and `MajorUpgrade` instead of a hand-rolled
"uninstall the old one first". `cargo-packager` is still rejected — it would put a generator
between the repository and the installer definition, and the registry claim has to stay comparable
with `plaudon_platform::associations` (`docs/licensing/third-party-register.md`, "Packaging and
installer tooling"; the 2026-10-12 amendment to ADR-0024).

**What the installer does**, and nothing more (`apps/plaudon-egui/packaging/windows/plaudon-egui.wxs`):

* installs per user into `%LOCALAPPDATA%\Programs\Plaudon` with `Scope="perUser"`; no elevation, no
  service, no scheduled task, no `PATH` edit, no autostart, nothing under `HKLM`, and no custom
  action that runs code — the only two `CustomAction` rows are type 51 property assignments;
* installs `plaudon-egui.exe`, the `plaudon` automation CLI and `plaudon.ico`, and writes a Start
  Menu shortcut under `HKCU`; the Programs-and-Features entry is written by Windows Installer
  itself, from the `ARP*` properties;
* claims `.plaudon` through the `Plaudon.Archive` ProgID using `OpenWithProgids` (so Windows' own
  "choose a default app" flow stays in charge) and registers the `plaudon://` protocol handler,
  with the keys, values and data generated by
  `plaudon_platform::associations::windows_registry_script` and `%EXEPATH%` resolved to the
  formatted reference `[#PlaudonEgui]`, which is the installed executable's full path;
* carries a fixed `UpgradeCode` and `MajorUpgrade`, so installing 0.2.0 over 0.1.0 replaces it;
* on uninstall removes every key it wrote and **leaves `%APPDATA%\app.plaudon.desktop` alone** — the
  library outlives the application.

Two NSIS behaviours did not survive the change, and are not papered over: the installer no longer
broadcasts `SHCNE_ASSOCCHANGED` (that needs a custom action, which is executable code inside an
installer), so Explorer picks the association up on its own refresh rather than immediately; and
there is no directory page — `msiexec /i Plaudon-<version>-x64.msi INSTALLFOLDER="…"` is the
override.

**Order of operations.** The payload executables are Authenticode-signed *before* `wix build` packs
them, then the `.msi` itself is signed. There is no generated uninstaller of ours to sign: Windows
Installer performs the uninstall with its own code.

| Environment | Signature | Exit |
| --- | --- | --- |
| `PLAUDON_WINDOWS_CERT_PATH` (+ `PLAUDON_WINDOWS_CERT_PASSWORD`, optional `PLAUDON_WINDOWS_TIMESTAMP_URL`) | `verified` on the installer and both executables | 0 |
| unset | `not_verified` | **3** |
| set but the file does not exist | — | 1, `reason=missing_credential` |

Before packing, the script asserts that the built executable carries its version resource and its
embedded manifest (`reason=missing_binary` if the resource compiler was missing), that the manifest
requests `asInvoker` with `uiAccess="false"`, and that the `.wxs` writes nothing to `HKLM`. After
packing it opens the built database through Windows Installer's own automation interface and
asserts the same things about what was produced rather than about what was written: every `Registry`
row is root 1 (`HKCU`), `ALLUSERS` is empty, and the summary information declares that elevation is
not required.

**Output.** `dist/windows-egui/Plaudon-<version>-x64.msi` and
`windows-signing-report.json` (installer, executable and CLI SHA-256, installer tool, install scope,
library directory, associations, embedded version, upgrade code, signature status and detail,
`notarization_status: not_applicable`, privilege audit, host build, commit, `publishable`).

**What a non-Windows machine can and cannot check.** `cargo check --target x86_64-pc-windows-msvc`
(cargo-xwin, or the MARS container) type-checks the Windows build, and `wix build` compiles and
links the installer definition anywhere .NET runs, so the `.wxs` can be checked without Windows —
but only up to the point where WiX writes the database, because that is done through Windows' own
`msi.dll`. Compiling to the linked intermediate is what a Linux container can prove:

```sh
docker run --rm -v "$PWD:/work" -w /work mcr.microsoft.com/dotnet/sdk:8.0 bash -c '
  export PATH="$PATH:/root/.dotnet/tools"
  dotnet tool install --global wix --version 5.0.2 >/dev/null
  stage=$(mktemp -d)
  : > "$stage/plaudon-egui.exe"; : > "$stage/plaudon.exe"
  cp apps/plaudon-egui/packaging/windows/plaudon.ico "$stage/"
  wix build -arch x64 -d Version=0.1.0 -b "$stage" -o /tmp/plaudon.wixipl \
    apps/plaudon-egui/packaging/windows/plaudon-egui.wxs'
```

The `.wixipl` is a zip holding `wix-ir.json`, whose symbols are the rows of the `Registry`,
`Shortcut`, `Property`, `Upgrade`, `Directory` and `CustomAction` tables; reading them is how the
per-user claim is inspected off Windows. (WiX canonicalises every `Directory/@Name` through a
Windows-only path routine, which is why the install location is a conditional `SetDirectory` rather
than a named directory tree — the file would otherwise be uncheckable anywhere but Windows.)

Neither of those is a Windows run. Producing the `.msi`, installing it, launching it, the
association click-through and the `plaudon://` handler can only be verified on Windows hardware or
a Windows CI runner — spec 18.4 forbids calling a Windows leg green from anywhere else, and the
`egui-desktop` job on `windows-2025` and the `windows` job in `.github/workflows/release.yml` are
where that happens.

---

## 7. The release gate

```sh
scripts/release-gate.sh --artifact dist/macos-egui/Plaudon.app.zip \
  --evidence ci-artifacts/evidence --allow-unsigned
```

Steps, each recorded as `pass` / `fail` / `not_verified` / `not_applicable` in
`<evidence>/release-gate/steps.jsonl`: clean-machine declaration, artifact presence, extraction,
signature (Gatekeeper), tamper detection (a modified binary must fail `codesign`), launch,
**library directory** (the launched bundle created `~/Library/Application Support/app.plaudon.desktop`
and the plist identifier matches `APPLICATION_IDENTIFIER`), offline smoke, the **egui UI suite**
(kittest), signed update and rollback,
portable archive round trip, repeatability.

`--allow-unsigned` only downgrades the signature verdict to `development_only`; the gate still exits
non-zero. There is no flag that turns a red gate green.

---

## 8. CI

| Job (`.github/workflows/ci.yml`) | Runner | What it does |
| --- | --- | --- |
| `rust` | macos-15, windows-2025 | fmt, clippy, `cargo test --workspace --all-features` |
| `egui-desktop` | macos-15, windows-2025 | the kittest suites through cargo, then `package-macos-egui.sh` / `package-windows-egui.ps1`; exit 3 is the expected unsigned outcome, and the job asserts the report says `publishable: false` |
| — | — | the `desktop-ui` job that ran the Tauri/React suites was deleted with `apps/desktop` at the cutover; no job in the pipeline needs node |
| `acceptance`, `supply-chain`, `coverage`, `release-gates`, `pipeline-selftest` | as before | unchanged |

The Windows leg of `egui-desktop` locates the SDK's `rc.exe` and exports `RC` before building, and
installs the pinned WiX toolset as a .NET global tool.

## 9. Releases

A release is cut by moving `main` onto the `release` branch:

```sh
git push origin main:release     # the first one creates the branch
```

That push is the only manual step. `.github/workflows/release.yml` does the rest — build, tag,
release, merge back — with no external script beyond the two packaging scripts above.

| Job | Runner | What it does |
| --- | --- | --- |
| `version` | ubuntu-latest | reads the workspace version, derives `v<version>`, and **fails if that tag already exists** — a second release of the same version is a mistake, not a retry |
| `macos` | macos-15 | optional Developer ID keychain and notarytool API key from secrets, `package-macos-egui.sh --features local-whisper`, a ten-second launch of the unzipped bundle against a throwaway `PLAUDON_DATA_DIR`, SHA-256, artifact upload, keychain deleted even on failure |
| `windows` | windows-2025 | Vulkan SDK (ggml's Windows GPU backend) and the pinned WiX toolset, optional PFX from a secret, `package-windows-egui.ps1 -Features local-whisper`, `plaudon.exe --version` against the staged payload (the GUI binary has no `--version`; installing the `.msi` needs an interactive session and is not attempted), SHA-256, artifact upload |
| `publish` | ubuntu-latest, `release` branch only, `contents: write` | creates the tag at the released commit, assembles `checksums.txt`, reads both signing reports and **drafts** the release |
| `merge-back` | ubuntu-latest, `release` branch only, `contents`/`pull-requests`/`issues: write` | opens a tracking issue and a `release → main` pull request and merges it with `--merge` |

`workflow_dispatch` runs the two build jobs and stops: artifacts, no tag, no release, no merge.

**The macOS bundle is built without `apple-bridge`, and that is a gap rather than a preference.**
`native/apple-bridge` uses `SpeechTranscriber`, `AssetInventory` and `LanguageModelError`, so it
compiles only against the macOS 27 SDK; no hosted runner image has one (macos-15 carries Xcode 16.4,
macos-26 tops out at Xcode 26.6, and both were tried). Until an image ships Xcode 27 the released
bundle reports the Apple engines as unavailable in this build, and `cargo_features` in the signing
report says what was actually built.

Signing is optional and fails open into honesty, never into a claim: with no credentials the
packaging scripts exit 3, which this workflow accepts as "built, not publishable". The `publish`
job then marks the release a prerelease, labels both assets `UNSIGNED / NOT NOTARIZED — development
build`, and says so at the top of the notes. A release is only described as signed when
`macos-signing-report.json` says `signature_status: verified` *and* `notarization_status: verified`
and `windows-signing-report.json` says `signature_status: verified`; a missing or malformed report
counts as unsigned. Every release is created as a draft — publishing is a human decision. The tag,
however, is created by the workflow and not by publishing the draft, so the released commit is
identifiable even while the draft sits unpublished.

Secrets, all optional, none of them echoed: `PLAUDON_MACOS_CERTIFICATE_BASE64`,
`PLAUDON_MACOS_CERTIFICATE_PASSWORD`, `PLAUDON_MACOS_IDENTITY`, `PLAUDON_NOTARY_KEY_BASE64`,
`PLAUDON_NOTARY_KEY_ID`, `PLAUDON_NOTARY_ISSUER_ID`, `PLAUDON_WINDOWS_CERT_BASE64`,
`PLAUDON_WINDOWS_CERT_PASSWORD`. The macOS certificate is imported into a temporary keychain that
the job deletes in an `always()` step.

**Two GitHub behaviours the workflow is built around.** Anything `GITHUB_TOKEN` does — the tag, the
release, the merge commit on `main` — raises no workflow event, so nothing downstream is triggered
and CI does **not** re-run on the merge commit; the tree that was built and released is the tree
that lands on `main`. And `gh pr create` from a workflow needs *Settings → Actions → General →
Workflow permissions → "Allow GitHub Actions to create and approve pull requests"*. At the time of
writing that setting is **off** in this repository
(`gh api repos/spdrman/plaudon/actions/permissions/workflow` reports
`can_approve_pull_request_reviews: false`), so `merge-back` will fail until it is switched on; it
detects exactly that failure and prints the setting to enable rather than GitHub's own message. The
release itself is unaffected when that happens: the tag exists, the draft exists, and the merge can
be done by hand or by re-running the job.

If the merge-back pull request conflicts, the job fails loudly and leaves the pull request open
rather than merging anything blind. Re-running it after `main` already contains the release is a
no-op.
