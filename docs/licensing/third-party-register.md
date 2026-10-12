# Third-party provenance and licensing register

Spec §2.6 requires a recorded licence and distribution decision for every
component we intend to use, and spec §18.8 requires a licence audit in the
release evidence bundle. An entry whose licence is unknown is **excluded from
bundling**; it is never shipped on the assumption that absence of a licence
means permission.

`scripts/license-audit.sh` enforces the Rust half of this register against
`deny.toml`: a dependency carrying a licence outside the reviewed allowlist, an
unknown registry or an open advisory fails the build.

Reviewed on 2026-10-11.

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
| Tauri 2 | MIT or Apache-2.0 | bundle | notice file | the capability manifest is an allowlist we audit, not a default | Bundle. |
| `egui` 0.36 | MIT or Apache-2.0 | bundle | notice file | immediate-mode toolkit; renders our own data, parses no untrusted format | Bundle (ADR-0023). Replaces the Tauri webview at the EPIC O cutover. |
| `eframe` 0.36 (`default_fonts`, `wgpu`, `accesskit`; `x11`/`wayland` on Linux) | MIT or Apache-2.0 | bundle | notice file | pulls `winit` 0.30, `wgpu` 30 and the AccessKit platform adapters; the GPU driver becomes part of the trusted computing base | Bundle (ADR-0023). |
| `epaint_default_fonts` (via `eframe/default_fonts`) | OFL-1.1 and UFL-1.0 font licences, crate itself MIT or Apache-2.0 | bundle | font licence notices | fonts are data we render, not code | Bundle. Checked with `cargo deny` as part of O1.2; see the audit note below if the allowlist needs a font-licence entry. |
| AccessKit (`accesskit`, `accesskit_winit`, platform adapters, via `eframe`) | MIT or Apache-2.0 (Apache-2.0 with the upstream NOTICE) | bundle | notice file | exposes our widget tree to VoiceOver, Narrator and AT-SPI; it is how §5.3 is met without a webview | Bundle (ADR-0023). |
| `wgpu` 30 and its tree (`naga`, `gpu-allocator`, …) | MIT or Apache-2.0 | bundle | notice file | translates our shaders only; no user content reaches the shader compiler | Bundle. |
| `egui_kittest` 0.36 (`wgpu`, `snapshot`) and `kittest` | MIT or Apache-2.0 | development only | — | drives the interface headlessly in tests; never linked into a release build | Development dependency (`crates/ui-testkit`). |
| `rfd` 0.17 | MIT | bundle | notice file | opens the OS file dialog; the returned paths are still validated against the approved roots in Rust (spec §7.4) | Bundle. Used only by the binary as a host hook, never by view code. |
| `parking_lot` | MIT or Apache-2.0 | bundle | notice file | — | Bundle. |
| `dirs`, `sysinfo`, `keyring` 3 | MIT or Apache-2.0 | bundle | notice file | `keyring` is the only credential path (ADR-0014); `dirs` resolves the library location (migration risk R7) | Bundle. |
| craft-libs `craft-ui` layout primitives, **vendored** at `third_party/craft-ui` (revision `1aa81a7`) | MIT or Apache-2.0 | bundle | **required**: `NOTICE`, `third_party/craft-ui/{LICENSE-MIT,LICENSE-APACHE,NOTICE,README.md}` and a provenance header in every file | pure geometry and one widget; no asset, no network, no unsafe | Vendor, do not track. `deny.toml` denies unknown git sources and the crate is unpublished; see `docs/EGUI_REFERENCE_COMPONENTS.md` §6. |
| `cpal` (audio output) | Apache-2.0 | **not yet introduced** | notice file when it lands | realtime callback thread; device loss must surface honestly | Prototyped and measured in `docs/spikes/O1.2-audio-playback-and-opus.md`; the row is completed by O1.5 (#203) with the version actually shipped. |
| An Opus decoder (`opus-decoder`, `rusty-opus`, `moosicbox_opus_native` or libopus bindings) | MIT or Apache-2.0 / BSD-3-Clause / MPL-2.0 depending on the choice | **not yet introduced** | notice file when it lands | decodes untrusted device audio; ADR-0015 applies to any C implementation | Undecided. Candidates, licences and a measured decode are in `docs/spikes/O1.2-audio-playback-and-opus.md`; the decision is O1.5's. |
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
| `jsonschema` | MIT | development only | — | test-time schema conformance | Development dependency. |
| `proptest`, `tempfile` | MIT or Apache-2.0 | development only | — | — | Development dependencies. |
| whisper.cpp | MIT | bundle (binary we build) | notice file | C++ parsing untrusted audio; sandbox per ADR-0015 | Bundle the binary we build ourselves. |
| Apple `Speech` / `FoundationModels` | Apple platform frameworks | linked on macOS only, never redistributed | n/a | availability is a runtime probe | Link on macOS; the Windows build links nothing Apple. |
| FFmpeg | LGPL-2.1+ or GPL depending on build | **excluded for now** | would require source offer and dynamic linking care | large C codebase parsing untrusted input | Not introduced. Symphonia covers WAV, MP3, M4A/AAC and FLAC; see `docs/spikes/A1.4-audio-decoder-coverage.md` for the Opus gap and the options. |

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
