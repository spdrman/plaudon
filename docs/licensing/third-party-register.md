# Third-party provenance and licensing register

Spec §2.6 requires a recorded licence and distribution decision for every
component we intend to use, and spec §18.8 requires a licence audit in the
release evidence bundle. An entry whose licence is unknown is **excluded from
bundling**; it is never shipped on the assumption that absence of a licence
means permission.

`scripts/license-audit.sh` enforces the Rust half of this register against
`deny.toml`: a dependency carrying a licence outside the reviewed allowlist, an
unknown registry or an open advisory fails the build.

Reviewed on 2026-10-12. Two passes that day: O1.6 made the Opus decoder row a
shipped dependency, and P2.5 (#248) added the speaker-embedding runtime, the
two bundle-tier model artifacts and their checksum table.

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
| `ort` 2.0.0-rc.14 and `ort-sys` (ONNX Runtime bindings, behind `plaudon-ai-speaker`'s off-by-default `onnx` feature) | MIT or Apache-2.0 for the bindings; the ONNX Runtime binary they link is MIT (© Microsoft) | **not in any shipped build today**; bundle-with-the-feature when ADR-0081 §4 passes | `NOTICE` carries both | links a prebuilt ONNX Runtime **statically**, so a release has no extra dylib to sign or notarize; the model file is parsed by that native code, which is why the digest is verified before the session is created (`crates/ai-speaker/src/onnx.rs`) and why ADR-0015's out-of-process requirement still stands as an open gate item | **Chosen for the runtime (ADR-0081 §3 as amended 2026-10-12).** Measured here against the alternatives on macOS arm64 at `-j2`: `ort` + a first-party feature front end builds clean in **15 s**, adds 15 crates to a normal build and produces a single self-contained binary. `sherpa-rs` 0.6.8 — the first-party sherpa-onnx fallback — takes **80 s**, needs bindgen (libclang) and CMake on every build host, and ships **three dylibs** (`libsherpa-onnx-c-api`, `libsherpa-onnx-cxx-api`, `libonnxruntime.1.17.1`, 26 MB) pinned to an ONNX Runtime two years older than the one `ort` links. Two caveats are recorded rather than smoothed over: the 2.0 line is still a release candidate (pinned with `=`), and `download-binaries` fetches the runtime over TLS at build time, which a release build must replace with `ORT_LIB_LOCATION` pointing at a pinned, checksummed artifact (release-evidence item, `docs/release/voice-profiles-evidence.md`). |
| `ureq` 3, `ureq-proto`, `socks`, `lzma-rust2`, `hmac-sha256` (build dependencies of `ort-sys` only) | MIT or Apache-2.0; `hmac-sha256` is ISC | never shipped: they exist only in `ort-sys`'s build script | n/a | this is the build-time download path for the prebuilt ONNX Runtime — an HTTP client, an LZMA decompressor and a digest check running on the build machine, not in the product | Accepted only as long as the build-time download is accepted; pinning `ORT_LIB_LOCATION` removes all five from the build. |
| `sherpa-rs` 0.6.8 / sherpa-onnx (the ADR-0081 §3 fallback) | Apache-2.0 (sherpa-onnx), MIT (`sherpa-rs`) | **excluded** | n/a | drags a second C++ codebase, a bindgen/libclang build-host requirement and three runtime dylibs into the artifact path | Rejected on the measured build burden above. Still the fallback if `ort` cannot be made to work on Windows x64, which is not yet measured; the decision is recorded in ADR-0081 so a reversal has to argue with numbers. |
| `knf-rs` 0.3.2 / `knf-rs-sys` (Rust bindings to `kaldi-native-fbank`) | MIT | **excluded** | n/a | vendors the kaldi-native-fbank C++ sources and builds them with CMake **and** bindgen, so every build host needs libclang and a C++ toolchain, and a second native parser of untrusted audio enters the process | Rejected: `crates/ai-speaker/src/fbank.rs` computes the same features in ~300 lines of dependency-free Rust, the same precedent as choosing `opus-pure` over libopus bindings (O1.6). The two implementations were compared on this machine over a 3 s broadband signal with the same window (`povey`): identical frame and bin counts, **max absolute difference 5.6e-4, RMS 1.2e-5** — f32 rounding, not a behavioural difference. `knf-rs` is a verification tool, never a dependency. |

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
| pyannote `segmentation-3.0`, ONNX export | MIT, © 2022 CNRS; the export is published un-gated with the licence text beside it | **bundlable** in the offline model pack, user-installed by default | required: exact file name and sha256 in the table below, enforced by `crates/ai-speaker/src/checkpoint.rs` and `scripts/fetch-speaker-models.sh` | Reviewed and accepted (ADR-0081 §1). Not installed by any release today: the voice-profiles feature is off and its §4 gates are not all passed. The gated Hugging Face original is **excluded** as a default path (ADR-0400 condition 3); it remains permitted as an explicit user download with the user's own token. |
| WeSpeaker `ResNet34-LM` (VoxCeleb), ONNX | CC-BY-4.0 — WeSpeaker publishes pretrained weights under the licence of their training corpus, and VoxCeleb is CC-BY-4.0 | **bundlable** with attribution, user-installed by default | required: exact file name and sha256 in the table below | Reviewed and accepted (ADR-0081 §1, §2). Where upstreams disagree about whether VoxCeleb-trained weights inherit CC-BY-4.0 we follow the stricter claim; the attribution lands in `NOTICE` and on the model screen. |
| NVIDIA TitaNet-L and any model trained on LDC corpora (Fisher, Switchboard, NIST SRE) | labelled CC-BY-4.0 on Hugging Face, Apache-2.0 on NGC; training corpora are per-seat LDC licences | **excluded** | n/a | Excluded from bundling regardless of the label (ADR-0081 §2): the two licence theories give opposite answers and we do not need the model. |
| Rev Reverb diarization models (carried in the same sherpa-onnx release tags) | non-commercial | **excluded** | n/a | Excluded. This is why every acquisition above is by exact file name: a glob over a release tag would fetch one of these. |

### Model-weights checksums (§12 "checksum recorded")

These four values — file name, size, digest, licence — are the install
contract. They are compiled into `crates/ai-speaker/src/checkpoint.rs`, pinned
in `scripts/fetch-speaker-models.sh`, reproduced in `NOTICE`, and checked
against this table by
`plaudon-integration-tests --test licensing_register`, so a drift in any one
of the four fails the build rather than a review.

| File (exact name) | Bytes | sha256 | Licence | Fetched from |
|---|---|---|---|---|
| `wespeaker_en_voxceleb_resnet34_LM.onnx` | 26530550 | `e9848563da86f263117134dfd7ad63c92355b37de492b55e325400c9d9c39012` | CC-BY-4.0 | `https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx` (upstream tag spelling preserved) |
| `model.onnx` → installed as `pyannote-segmentation-3.0.onnx` | 5992913 | `220ad67ca923bef2fa91f2390c786097bf305bceb5e261d4af67b38e938e1079` | MIT | `https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx` |

Provenance note, verified by byte comparison on 2026-10-12: the WeSpeaker file
above is the upstream export
(`https://wespeaker-1256283475.cos.ap-shanghai.myqcloud.com/models/voxceleb/voxceleb_resnet34_LM.onnx`,
26530309 bytes, sha256
`7bb2f06e9df17cdf1ef14ee8a15ab08ed28e8d0ef5054ee135741560df2ec068`) with 241
bytes of ONNX `metadata_props` appended — `framework`, `language`, the upstream
`url`, `sample_rate=16000`, `output_dim=256`, `normalize_samples=0`. The first
26530309 bytes are identical, so the graph and the weights are upstream's and
only the metadata is the mirror's. The mirror is the acquisition target
because that metadata states the feature contract the engine implements.

## Evaluation corpora (speaker matching, VOICE_PROFILES_SPEC §17.2)

The speaker-matching harness (`crates/eval/src/speaker/`,
[docs/benchmarks/speaker-match-eval.md](../benchmarks/speaker-match-eval.md))
scores embeddings, never audio, so the repository carries fixture *ground
truth* and checksums — never corpus samples. The allowlist below is enforced in
code: `FixtureCorpus` has exactly these variants, and `FixtureManifest::parse`
additionally rejects the forbidden names wherever they appear in a manifest,
including in free text.

| Corpus | Licence | Redistribution | Attribution | Decision |
|---|---|---|---|---|
| LibriSpeech `test-clean` (openslr.org/12) | CC-BY-4.0, derived from public-domain LibriVox recordings | never vendored; a contributor downloads it and builds fixtures locally | "LibriSpeech (Panayotov et al.), CC-BY-4.0" in any artifact built from it | Allowed as the source of seeded multi-speaker sessions with exact RTTM. Real readers' voices, so the consent-free tier stays TTS-only (§17.5). |
| MUSAN (openslr.org/17) | CC-BY-4.0 | never vendored; local noise variants only | "MUSAN (Snyder et al.), CC-BY-4.0" | Allowed as the noise condition. |
| Kokoro-82M TTS (`hexgrad/Kokoro-82M`) | Apache-2.0 | never vendored; generated locally | "Kokoro-82M, Apache-2.0" | Allowed as the zero-human-voices smoke tier. |
| Scripted vectors (`plaudon-eval`) | n/a — generated from a seed | committed (no recorded material) | none | The hermetic CI tier. Produces no accuracy claim about any model. |
| AMI, ICSI | CC-BY-4.0 | **download-on-demand, developer-local only** | required if figures are published | Never in the repository and never in CI: gigabytes of real meetings, used as an accuracy gate a developer runs deliberately. |
| VoxCeleb 1/2, VoxConverse | form-gated, "research purposes", underlying YouTube copyright never licensed to the publisher | **never in the repository or CI** | n/a | Excluded. Developer-local evaluation only, never a committed fixture or a derived one. |
| AISHELL-4 (openslr.org/111) | CC-BY-**SA**-4.0 | **never vendored, never mixed into derived fixtures** | n/a | Excluded: ShareAlike contamination risk for anything derived from it. |
| sherpa-onnx release-tag test wavs | no stated licence | never vendored | n/a | Excluded. No licence means no permission. |

The measured tier is built locally by `plaudon-speaker-embed build-fixtures`
(`crates/ai-speaker`): it decodes LibriSpeech `test-clean` with the product's
own decoder, cuts fixed-length single-speaker utterances, and derives the
channel conditions. Three of the four are pure Rust in that tool (clean,
telephone band-pass, room impulse response); the low-bitrate Opus condition
shells out to a developer-local **FFmpeg** (`-c:a libopus -b:a 12k`), which is
never redistributed and never linked — the same standing as `rsvg-convert`
above. The MUSAN condition is **not built**: the corpus is an 11 GB download
this run did not take, so `noise_musan` is absent from the measured plan rather
than approximated. Every built file's digest is in
[docs/benchmarks/speaker-match-wespeaker-fixtures.json](../benchmarks/speaker-match-wespeaker-fixtures.json);
no audio and no embedding is committed.

## Audit hook

`scripts/license-audit.sh` runs `cargo deny check licenses bans sources
advisories` against `deny.toml`. The allowlist is the set of licences reviewed
above. A new dependency with any other licence fails the build until it is
reviewed and added here, which is the mechanism spec §18.8 asks for.
