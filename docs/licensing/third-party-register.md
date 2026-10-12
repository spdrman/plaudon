# Third-party provenance and licensing register

Spec §2.6 requires a recorded licence and distribution decision for every
component we intend to use, and spec §18.8 requires a licence audit in the
release evidence bundle. An entry whose licence is unknown is **excluded from
bundling**; it is never shipped on the assumption that absence of a licence
means permission.

`scripts/license-audit.sh` enforces the Rust half of this register against
`deny.toml`: a dependency carrying a licence outside the reviewed allowlist, an
unknown registry or an open advisory fails the build.

Reviewed on 2026-10-12 (O1.6: the Opus decoder row is now a shipped dependency).

## Redistribution vocabulary

| Decision | Meaning |
|---|---|
| **bundle** | Shipped inside our installer. |
| **user-installed** | The user obtains it; we never redistribute the bytes. |
| **excluded** | Not shipped and not linked. Designs only, if anything. |

## Reference implementations we read

| Component | Licence | Redistribution | Attribution | Security note | Decision |
|---|---|---|---|---|---|
| Riffado (`riffado/riffado`) | AGPL-3.0 | excluded | none required (no code used) | its PLAUD account integration uses non-public service interfaces | Designs only. Copying would make the whole application AGPL (ADR-0009). We adopt the source-version comparison, idempotent import and bulk-export *ideas*, not the implementation. |
| `landoncrabtree/applaud` | none observed on the repository page | excluded | none required (no code used) | its watcher shells out to command strings, deletes originals and reacts to add events without a completion barrier | Designs only, and mostly as failure patterns to avoid (ADR-0009, ADR-0010). No licence means no permission. |
| `rsteckler/applaud` | MIT | excluded | would be required if code were used | relies on undocumented web APIs and browser session tokens | Not a direct-device implementation and not evidence of cloud-free transport. We use neither its code nor its endpoints. |
| `Sumatoshi-tech/plaude-cli` | MIT | excluded (no code vendored) | recorded here and in `docs/protocol/plaud-legacy-ble.md` | reverse-engineered protocol; its token bootstrap emulates a BlueZ peripheral, which we do not ship | Protocol *facts* only — GATT service `0x1910`, characteristics `0x2BB1`/`0x2BB0`, `0x01`/`0x02` framing, opcodes `0x0001`/`0x001A`/`0x001C`, the `0xFFFFFFFF` sentinel. Our codec is written from those facts. |
| Official PLAUD CLI (`@plaud-ai/cli`) | **none declared on the package listing** | excluded | n/a | spawned as a user-installed executable only | Never bundled, never redistributed, never patched. The user installs it; we invoke it with an argument array and a restricted environment. |
| PLAUD mobile application assets | proprietary | excluded | n/a | extracting keys from the APK is out of scope | No asset is copied, no key is extracted, no encrypted handshake is bypassed (App. B.11). |

## Runtime components

| Component | Licence | Redistribution | Attribution | Security note | Decision |
|---|---|---|---|---|---|
| `egui` 0.36 | MIT or Apache-2.0 | bundle | notice file | immediate-mode toolkit; renders our own data, parses no untrusted format | Bundle (ADR-0023). It replaced the Tauri webview at the EPIC O cutover on 2026-10-12 (ADR-0024); `tauri`, `wry` and the platform webview bindings are no longer in the graph, and the Tauri row that stood here was removed with them. |
| `eframe` 0.36 (`default_fonts`, `wgpu`, `accesskit`; `x11`/`wayland` on Linux) | MIT or Apache-2.0 | bundle | notice file | pulls `winit` 0.30, `wgpu` 30 and the AccessKit platform adapters; the GPU driver becomes part of the trusted computing base | Bundle (ADR-0023). |
| `epaint_default_fonts` (via `eframe/default_fonts`) | OFL-1.1 and UFL-1.0 font licences, crate itself MIT or Apache-2.0 | bundle | font licence notices | fonts are data we render, not code | Bundle. Checked with `cargo deny` as part of O1.2; see the audit note below if the allowlist needs a font-licence entry. |
| AccessKit (`accesskit`, `accesskit_winit`, platform adapters, via `eframe`) | MIT or Apache-2.0 (Apache-2.0 with the upstream NOTICE) | bundle | notice file | exposes our widget tree to VoiceOver, Narrator and AT-SPI; it is how §5.3 is met without a webview | Bundle (ADR-0023). |
| `wgpu` 30 and its tree (`naga`, `gpu-allocator`, …) | MIT or Apache-2.0 | bundle | notice file | translates our shaders only; no user content reaches the shader compiler | Bundle. |
| `egui_kittest` 0.36 (`wgpu`, `snapshot`) and `kittest` | MIT or Apache-2.0 | development only | — | drives the interface headlessly in tests; never linked into a release build | Development dependency (`crates/ui-testkit`). |
| `rfd` 0.17 | MIT | bundle | notice file | opens the OS file dialog; the returned paths are still validated against the approved roots in Rust (spec §7.4) | Bundle. Used only by the binary as a host hook, never by view code. |
| `parking_lot` | MIT or Apache-2.0 | bundle | notice file | — | Bundle. |
| `dirs`, `sysinfo`, `keyring` 3 | MIT or Apache-2.0 | bundle | notice file | `keyring` is the only credential path (ADR-0014); `dirs` resolves the library location (migration risk R7) | Bundle. |
| craft-libs `craft-ui` layout primitives, **vendored** at `third_party/craft-ui` (revision `1aa81a7`) | MIT or Apache-2.0 | bundle | **required**: `NOTICE`, `third_party/craft-ui/{LICENSE-MIT,LICENSE-APACHE,NOTICE,README.md}` and a provenance header in every file | pure geometry and one widget; no asset, no network, no unsafe | Vendor, do not track. `deny.toml` denies unknown git sources and the crate is unpublished; see `docs/EGUI_REFERENCE_COMPONENTS.md` §6. |
| `cpal` 0.18 (audio output) | Apache-2.0 | bundle | notice file | realtime callback thread; opens an **output** stream only, so no microphone permission is ever requested; device loss is reported through `CpalOut::last_error` and stops the clock rather than playing silence | Bundle. Introduced by O1.5 (#203) at the version measured against: 0.18.2, newer than the 0.16 prototyped in `docs/spikes/O1.2-audio-playback-and-opus.md`. Builds on macOS (CoreAudio) and in the MARS Linux container (ALSA, `libasound2-dev`, already installed by `scripts/e2e-remote.sh`). Confined to `crates/audio-playback` behind the `cpal-output` feature, which `plaudon-ui` does not enable.
| `opus-pure` 0.2.2 (Opus decoder) | BSD-3-Clause | bundle | **required**: `NOTICE` carries the inherited Xiph.Org/Skype/restsend/Berry copyrights and the BSD conditions | decodes untrusted device audio; pure Rust with no FFI and no C build, so ADR-0015's out-of-process requirement for *native* codecs is not triggered; `unsafe` is confined to SIMD kernels the crate pins against their scalar definitions; `plaudon_audio::opus` caps packet size (64 KiB), frame length (5 760 samples/channel) and channel count (8) before any allocation | **Bundle. Introduced by O1.6 (#214)**, replacing the "not yet introduced" row O1.5 left. Chosen over `opus-decoder` 0.1.1, `rusty-opus` 1.0.1, `moosicbox_opus_native` 0.4.0 and libopus bindings (`opus`/`audiopus`/`symphonia-adapter-libopus`) on measured evidence, recorded in `docs/spikes/O1.2-audio-playback-and-opus.md` §4: against a libopus reference it decodes every committed fixture to within 1 LSB, and 4 323 hostile packets produced no panic and no non-finite sample. BSD-3-Clause is already on the `deny.toml` allowlist, so no policy change was needed. No system library on macOS arm64, Windows x64 or Linux: the crate has no dependencies and no build script. |
| Rust standard library and toolchain | MIT or Apache-2.0 | bundle | notice file | — | Bundle. |
| `sqlx` + bundled SQLite | MIT or Apache-2.0 (SQLite itself is public domain) | bundle | notice file | SQLite parses no untrusted input beyond our own schema | Bundle. |
| Symphonia | MPL-2.0 | bundle | notice file, and MPL source availability for the crate itself | pure-Rust decoders, but still parse untrusted input; run them under the out-of-process policy of ADR-0015 | Bundle. MPL-2.0 is file-level copyleft and does not reach our own sources. |
| `btleplug` | MIT or Apache-2.0 | bundle (behind the `hardware-scan` feature) | notice file | wraps platform Bluetooth APIs; the radio is an untrusted input domain | Bundle only when the device feature ships. |
| `serde`, `serde_json`, `thiserror`, `uuid`, `sha2`, `hex`, `tokio`, `tracing`, `time` | MIT or Apache-2.0 | bundle | notice file | — | Bundle. |
| `serde_norway` | MIT or Apache-2.0 | bundle | notice file | parses `compatibility.yaml`, which we author | Bundle. |
| `egui`, `epaint`, `emath`, `ecolor` 0.36 | MIT or Apache-2.0 | bundle | notice file | immediate-mode GUI; renders only data our own code produced | Bundle (EPIC O, ADR range 0020–0029). |
| `accesskit`, `accesskit_consumer` | Apache-2.0 | bundle | notice file | publishes the accessibility tree to the platform; carries widget labels and values, which for the transcript editor means transcript text — the same text the window already shows | Bundle. The accessibility tree is what makes the pure-Rust UI auditable against spec §5.3. |
| `epaint_default_fonts` (Hack, Ubuntu-Light, NotoEmoji, emoji-icon-font) | (MIT or Apache-2.0) **and** OFL-1.1 **and** Ubuntu-font-1.0 | bundle | notice file must carry the OFL-1.1 and Ubuntu Font Licence texts and the font copyright lines | font files are data, parsed by `ab_glyph`; they are not executed | Bundle. OFL-1.1 and Ubuntu-font-1.0 both permit redistribution of the unmodified fonts inside a larger work with their licence texts; neither reaches our own sources. Reviewed for EPIC O and added to `deny.toml`. |
| `egui_kittest`, `kittest` | MIT or Apache-2.0 | development only | — | drives the accessibility tree in tests | Development dependencies. |
| `tray-icon` 0.25 and `muda` 0.20 (macOS and Windows only) | MIT or Apache-2.0 | bundle, on macOS and Windows only | notice file | draws the status item and its menu through `NSStatusItem` / `Shell_NotifyIcon`, and on macOS the application menu through `NSMenu`; every row is our own — a `plaudon_platform::tray::TrayModel` row, or a label and chord read from the shell's command registry — so no user content, path or credential reaches a platform menu API | Bundle on macOS and Windows (EPIC O task O2.4, matrix row C11). **`muda` is also named directly** by `apps/plaudon-egui` under `cfg(target_os = "macos")` for the application menu (parity row S8; see the 2026-10-12 amendment to ADR-0023). That direct entry adds nothing to the graph: it is the same package and version `tray-icon` already pulls — `cargo tree -i muda --target aarch64-apple-darwin -e normal` reaches `muda` 0.20.0 through `tray-icon` 0.25 and through `tauri` 2 — and it is taken with `default-features = false`, exactly as `tray-icon` takes it, which is what keeps muda's GTK and `libxdo` defaults out of the build. `cargo deny --all-features check licenses bans sources advisories` was run before and after and reports the same findings, none of which names either crate. **Not** a Linux dependency: the `tray-icon` Linux backends link GTK and `libappindicator`, which the headless build and the MARS container do not carry, so Linux reports `TraySupport::UnavailableInBuild` with that reason instead and keeps the in-window menu bar. |
| `arboard` 3 | MIT or Apache-2.0 | bundle | notice file | the only clipboard path; Plaudon writes non-secret facts (a digest, a path the user can already see, a version string) and never a credential | Bundle (EPIC O task O2.4, matrix row C10). Already reached through `egui-winit`; `crates/platform` now names it directly so the clipboard has a testable port. |
| `crossbeam-channel` (via `tray-icon`) | MIT or Apache-2.0 | bundle, on macOS and Windows only | notice file | carries tray and menu events to the render loop | Bundle with `tray-icon`. |
| `tauri-winrt-notification` 0.8 (Windows only) | MIT or Apache-2.0 | bundle, on Windows only | notice file | the Windows toast backend (`crates/notifications/src/windows.rs`); payloads are counts, fixed phrases and a typed target id only, per ADR-0021's privacy rule | Bundle on Windows. The crate is a WinRT binding over `ToastNotificationManager` published by the Tauri organisation; it depends only on the `windows` crate family and pulls no `tauri`, `wry` or webview package, so it survives the ADR-0024 cutover on its own merits rather than as a leftover. |
| `jsonschema` | MIT | development only | — | test-time schema conformance | Development dependency. |
| `proptest`, `tempfile` | MIT or Apache-2.0 | development only | — | — | Development dependencies. |
| whisper.cpp | MIT | bundle (binary we build) | notice file | C++ parsing untrusted audio; sandbox per ADR-0015 | Bundle the binary we build ourselves. |
| Apple `Speech` / `FoundationModels` | Apple platform frameworks | linked on macOS only, never redistributed | n/a | availability is a runtime probe | Link on macOS; the Windows build links nothing Apple. |
| FFmpeg | LGPL-2.1+ or GPL depending on build | **excluded for now** | would require source offer and dynamic linking care | large C codebase parsing untrusted input | Not introduced, and no longer needed for Opus: Symphonia covers WAV, MP3, M4A/AAC, FLAC and Ogg Vorbis, and `opus-pure` covers Ogg Opus (O1.6, #214). A format outside that set is still refused with a typed error rather than reached for with an LGPL C library. |
| filmcraft (`spdrman/filmcraft`) | MIT or Apache-2.0 | bundle (adapted source) | **`NOTICE`, per file** | `std`-only ring buffer and mixing thread; no `unsafe` | Adapt, not depend: `crates/audio-playback/src/ring.rs` is derived from its `play_ahead.rs`, and `cpal_out.rs` and the waveform mesh follow its structure. Every adapted file names the origin in its header and in `NOTICE`. A git dependency is impossible here — `deny.toml` sets `unknown-git = "deny"` — and the crate is unpublished. |
| `objc2` 0.6, `objc2-app-kit` 0.3, `objc2-foundation` 0.3 (macOS only) | MIT or Apache-2.0 | bundle, on macOS only | notice file | typed bindings to the Objective-C runtime; `apps/plaudon-egui/src/mac_open.rs` uses them for one thing — adding `application:openURLs:` to the live `NSApplicationDelegate` so a `.plaudon` document or a `plaudon://` link reaches the window. The handler copies URL strings into a queue and nothing else; the strings are then parsed by `plaudon_platform::activation::open_request`, which refuses anything outside the approved roots, and no path is logged | Bundle on macOS (parity row C10). **No package is added to the graph**: `tray-icon` 0.25 and `muda` 0.20 already pull all three at exactly these versions on macOS (`Cargo.lock`), and naming them directly only removes the pretence that the tray crate is where AppKit types come from. Needed because winit 0.30.13 installs its own application delegate (`platform_impl/macos/event_loop.rs`) which implements only `applicationDidFinishLaunching:` and `applicationWillTerminate:`, and neither winit nor eframe 0.36 surfaces an open event. |

## Packaging and installer tooling

Build-time tools. None of them puts *its* bytes inside an artifact we hand to
a user — a claim that has to be re-checked whenever one of them changes, and
that changed once already: see the WiX row.

| Component | Licence | Redistribution | Attribution | Security note | Decision |
|---|---|---|---|---|---|
| WiX Toolset v5 (`wix` .NET tool, pinned to 5.0.2) | MS-RL | **never redistributed**: an `.msi` is a database the operating system's own installer service interprets, and carries no WiX code | n/a | the package is per-user, writes only `HKCU`, requests no elevation, runs no custom action of its own, and is Authenticode-signed when a certificate is available; `scripts/package-windows-egui.ps1` re-asserts all of that against the built database | Chosen for the Windows installer (`apps/plaudon-egui/packaging/windows/plaudon-egui.wxs`, O2.10, #250). **This reverses the O2.6 decision and the reason it was taken.** NSIS 3 was chosen then because its zlib/libpng licence is permissive while WiX is MS-RL, a reciprocal licence — but that comparison only holds for NSIS, whose installer stub really is inside the `setup.exe` we hand to a user. MS-RL binds what is distributed; the `.msi` contains none of WiX, so WiX sits in the same category as `signtool` and `codesign`: a build tool that stays on the build machine. What the `.msi` buys is Windows' own install, upgrade and uninstall path, a Programs-and-Features entry the system writes, and `MajorUpgrade`. NSIS is no longer used anywhere in this repository, and its notice-file entry went with it. `cargo-packager` remains rejected: it would add a generator between us and the installer definition, and the registry claim has to stay comparable with `plaudon_platform::associations`. |
| Apple `codesign`, `notarytool`, `stapler`, `iconutil`, `sips` | Apple developer tools, part of Xcode / the Command Line Tools | never redistributed | n/a | they sign and verify; nothing of theirs ships | Use on macOS only (`scripts/package-macos-egui.sh`). |
| Microsoft `signtool` (Windows SDK) | Microsoft Windows SDK licence | never redistributed | n/a | Authenticode signing only | Use on Windows only (`scripts/package-windows-egui.ps1`). |
| `rsvg-convert` / ImageMagick (`magick`) | LGPL-2.1+ / ImageMagick licence (Apache-2.0-style) | never redistributed | n/a | run once by a developer to rasterise our own SVG; no third-party artwork is involved | Developer tools for `scripts/make-app-icon.sh`. The application icon (`apps/plaudon-egui/packaging/icon.svg`) is original artwork authored in this repository; no third-party icon set, font or trace is used. |

## Model weights

Weights are **never bundled** unless the entry below says otherwise. §12
requires the model manager to record size, licence, checksum, source and
hardware recommendation, and to install into a versioned user data directory
outside the application bundle.

| Weights | Licence | Redistribution | Checksum recorded | Decision |
|---|---|---|---|---|
| Whisper `ggml` conversions (`tiny`, `base`, `small`, `medium`, `large-v3`) | MIT for the original OpenAI weights; each redistributed conversion must be checked individually | user-installed by default | required before install; `scripts/verify-model-pack.sh` refuses an unverified file | A conversion whose redistribution terms are not clearly MIT is marked **not bundlable** and is user-downloaded only. |
| Apple on-device speech and language assets | Apple platform assets | never redistributed | n/a | Downloaded by the operating system on the user's instruction. |
| Any diarization model (for example pyannote) | gated, varies | excluded pending a separate review | n/a | Not introduced; §8.7 requires a feasibility and licensing spike first. |

## Audit hook

`scripts/license-audit.sh` runs `cargo deny check licenses bans sources
advisories` against `deny.toml`. The allowlist is the set of licences reviewed
above. A new dependency with any other licence fails the build until it is
reviewed and added here, which is the mechanism spec §18.8 asks for.
