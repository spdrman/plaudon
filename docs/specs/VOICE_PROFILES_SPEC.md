# Speaker Voice Profiles — Product & Engineering Specification

**Status:** Proposed; no code exists for this feature. Implementation is gated on the ADR-0400 provider
conditions, on ADR-0080/0081/0082, and on the measured-evidence rules below.
**Version:** 1.1 — adds the global master speaker list (the *people roster*), manual assignment
with biometric matching off, suggestion correction with negative evidence, and propagation rules
(§2, §7, §8, §9A, §12.2A); 1.0 was the first consensus draft.
**Spec lineage:** extends the PLAUD BYOK Desktop specification v1.3. Section references of the form
§N.M refer to that document; references of the form VP-NNN are requirements of this document.
**ADRs:** [ADR-0080](../adr/ADR-0080-voice-profiles-are-biometric-data-off-by-default-local-only.md),
[ADR-0081](../adr/ADR-0081-speaker-embedding-runtime-and-model-licensing.md),
[ADR-0082](../adr/ADR-0082-a-voice-match-is-a-suggestion-never-an-identity.md).

Honesty key, used throughout: **[VERIFIED: url]** — claim taken from that source during the
2026-10-12 research pass. **[MEASURED-RESEARCH]** — measured during that research pass on one
Apple M5 machine (macOS 27.0.1, onnxruntime 1.31.0, CPU provider, synthetic fbank input); these
are *scoping* numbers, not release benchmarks, and MUST be re-measured by the EPIC on reference
hardware before any claim ships. **[UNVERIFIED]** — no primary source found; must not be quoted
downstream. Section 22 collects every unverified claim in one register.

---

## 1. Problem, users, and non-goals

### 1.1 Problem

Plaudon can label *who-spoke-when* inside a single recording (the diarization contract of §8.7 and
ADR-0400, `crates/ai-diarization`), but every recording starts from zero: "Speaker 1" in Monday's
stand-up and "Speaker 1" in Tuesday's stand-up are unrelated labels. Users who record the same
people every week re-name the same colleagues every time. The feature in this document gives the
app **one global master speaker list** (the people roster) that any recording can assign speakers
from, and — optionally, consent-gated — lets a user *enroll* a roster person's voice once, with
that person's knowledge and consent, so later recordings *suggest* "this cluster sounds like
Priya (87%)" for one-click confirmation.

- **VP-001.** Voice profiles exist to reduce repeated manual labelling across recordings. They are
  a convenience, not an identification system; the product never presents a match as proof of
  identity (ADR-0082, §16 of this document).
- **VP-002.** The entire feature is local-only. Voice embeddings are biometric data; they are
  computed, stored, matched, and deleted exclusively on the user's machine. No embedding, score,
  or profile metadata is ever a permissible payload for any network subsystem (see §11).
- **VP-003.** Biometric matching is **off by default**. Until the user explicitly enables it and
  passes the consent gate (§12), no embedding is computed — not even transiently — and the whole
  §4 pipeline is dormant. Per-recording diarization without cross-recording persistence remains
  the default behaviour.
- **VP-010.** There is exactly **one** master speaker list per library, independent of any
  recording (§9A). Every assignment surface — per-cluster, per-segment, bulk — picks from it;
  there are no per-recording name silos beyond the existing per-recording `speakers` rows, which
  become *links* to roster people plus a text snapshot.
- **VP-011.** Manual assignment is a first-class mode: the roster and every picker work with
  biometric matching **off**, and in that mode **no embedding is stored or computed** — a
  name-only roster is ordinary personal data, not biometric data (§12.2A). Nothing in the manual
  path may call into the embedding/matching code, enforced by the same structural test as the
  consent gate (§17.1).

### 1.2 Users

The §1.4 user groups apply unchanged. Within them, the feature targets the recurring-meeting user:
a consultant, project manager or team lead who records the same group weekly. It explicitly does
NOT target investigative, forensic, security or workforce-monitoring use; §16 and ADR-0082 forbid
presenting the feature as suitable for those uses.

### 1.3 Non-goals

- **VP-004.** No speaker *verification* ("prove this is Priya"). The feature is 1:N open-set
  suggestion, and the spec never uses the word "verification" for it (EU AI Act Art. 3(35) vs
  3(36) draw exactly this line; see §12.6).
- **VP-005.** No emotion, sentiment, age, gender, health, accent, ethnicity or any other attribute
  inference from voice — ever, including "while we're at it" additions. Attribute inference would
  move the feature into EU AI Act Art. 5 / Annex III(1)(b)-(c) territory and is out of scope for
  this product line, not merely for V1 (§12.6).
- **VP-006.** No cloud or cross-device sync of profiles, no vendor-hosted "model improvement", no
  telemetry containing voice data. The feature never creates a reason for the vendor to possess
  biometric data (§12.7).
- **VP-007.** No automatic enrolment. A profile only comes into existence through an explicit user
  action naming a person; the system never silently promotes a recurring cluster to a profile, and
  never retroactively scans the back catalogue without an explicit, scoped user command.
- **VP-008.** No real-time/live identification during recording in V1. Matching runs as a job over
  imported media.
- **VP-009.** Not a security control: profiles must not gate playback, deletion, export or any
  other capability.

## 2. Terminology

These five words are distinct and the code must keep them distinct (the DTO layer, the database
schema and the UI copy all use them in exactly these senses):

- **Speaker turn** — a time span `[start_ms, end_ms)` attributed to one local cluster inside one
  recording. Already modelled: `SpeakerTurn { cluster, start_ms, end_ms }` in
  `crates/ai-diarization/src/contract.rs`.
- **Cluster** — an anonymous within-recording grouping of turns ("Speaker 2"). Clusters carry no
  identity and no biometrics; they are what §8.7 diarization produces. Already modelled:
  `ClusterKey`, the `speakers` table row with `provenance = 'inferred'`
  (`crates/persistence/migrations/0002_core.sql`, `0400_transcript_tools.sql`).
- **Person** — one entry in the app-global **master speaker list** (the *roster*): a display
  name, optional aliases and optional plain contact fields (email, organisation). A person row is
  ordinary personal data, not biometric data; it exists so the same colleague can be picked in any
  recording, with or without voice matching (§9A, §12.2A). New in this spec (§7).
- **Voice profile (voice enrolment)** — a person's *optional* biometric attachment, created only
  by explicit consent-gated enrolment: one `person_voice_enrollments` row owning one or more
  *exemplar embeddings*. The enrolment is the only place embeddings live; a person without an
  enrolment is a name-only roster entry.
- **Identity** — the real-world person. Out of the system's reach. A person row *names* someone;
  it does not *establish* that a voice belongs to them. UI copy must say "sounds like", never
  "is" (VP-120).

Supporting terms: an **exemplar** is one stored embedding with provenance (which recording,
segment range, model and version produced it). An **embedding** is a fixed-length f32 vector
(256–512 dims for the candidate models, §5) derived from speech audio; it is a biometric template
in the legal sense (§12.1) regardless of its non-invertibility in practice. A **suggestion** is a
persisted, pending match between a cluster and an enrolled person with a similarity score; it has
no effect on transcripts until confirmed. An **assignment** is the user-made (manual or
confirmed-suggestion) link from a recording's cluster or segment to a person; assignments are
ordinary data and exist in both modes.

## 3. User stories

1. *Enrol from a transcript.* Dana selects a segment where only Priya speaks, chooses
   "Create voice profile from this speaker…", reads the consent gate, confirms that Priya has
   agreed, names the profile, and the app computes exemplars from that cluster's turns.
2. *Recognise next week.* Dana imports the next stand-up. After transcription and diarization, a
   speaker-matching job compares each cluster's embedding to enrolled profiles and records
   suggestions. The recording's speaker panel shows "Speaker 1 — sounds like Priya (strong match)"
   with Confirm / Not them buttons. Nothing is renamed until Dana confirms.
3. *Not them.* A suggestion is wrong. Dana rejects it; the rejection is remembered for that
   cluster and the score contributes nothing anywhere else.
4. *Improve a profile.* After confirming, Dana chooses "Add this recording's speech to Priya's
   profile" to add exemplars from a new microphone/room, improving cross-device robustness (§6.5).
5. *Merge/split.* Two profiles turn out to be the same person: Dana merges them (exemplars are
   combined, both names kept as aliases until she picks one). One profile turns out to contain two
   people's exemplars: Dana splits it by exemplar provenance.
6. *Leave no trace.* Priya leaves the company and asks Dana to remove her data. Dana deletes the
   profile; every exemplar, suggestion and match score is destroyed (§12.4), and transcript name
   labels she confirmed earlier remain only as ordinary text she may also remove.
7. *Forget everyone.* Dana stops using the feature. One button destroys every profile, exemplar,
   suggestion and the feature's encryption key, and turns the feature off (VP-095).
8. *Bulk review.* After enabling the feature, Dana runs matching over selected past recordings
   (explicit, scoped — never automatic) and reviews suggestions in a single queue.
9. *Manual-only shop.* Lee never enables voice matching. Lee still keeps the master list —
   "Priya", "Sam (client)" — and in every recording assigns clusters and single segments by
   picking from it. No embedding is ever computed or stored (VP-011, §12.2A).
10. *Correct a wrong suggestion.* The panel says "sounds like Sam (0.74)" but it is actually
    Priya. Dana presses Not them, the picker opens pre-filtered, Dana picks Priya. The wrong
    match is recorded as negative evidence against Sam's enrolment for calibration; because Priya
    is enrolled, the cluster's embedding may be offered as a new exemplar for Priya (VP-216).
11. *Rename once, everywhere — visibly.* Dana fixes a typo in a person's name; every recording
    shows the corrected name because assignments reference the person. Dana is told so before
    saving ("name shown in 14 recordings"). Assignments themselves are never changed by a rename,
    and no other recording's assignment is ever rewritten by an action in this one (VP-213).

## 4. Pipeline

```text
 audio (imported, immutable original — §6.3B)
   │ decode_to_canonical_pcm (crates/audio/src/decode.rs, 16 kHz mono,
   │ CanonicalPcmProfile::whisper_mono_16k)
   ▼
 [stage 1] diarization        DiarizationProvider::diarize via run_guarded
   │                          (crates/ai-diarization/src/provider.rs) → SpeakerTurn list
   ▼
 [stage 2] reconciliation     reconcile(revision, result) (crates/app-core/src/diarization.rs)
   │                          → clusters, segment assignments (already implemented, test-only today)
   ▼
 [stage 3] per-cluster        for each cluster: select clean turns (no overlap, ≥ minimum
   │        embeddings        speech), window, embed, aggregate → cluster embedding
   ▼
 [stage 4] matching           cosine similarity (+ AS-Norm, §6.4) against enrolled profile
   │                          exemplars → scores → open-set decision (§6.2)
   ▼
 [stage 5] suggestions        persist speaker_suggestions rows; emit event; UI shows
   │                          Confirm / Not them. NOTHING is auto-assigned (VP-060).
   ▼
 [stage 6] confirmation       user confirms → speakers.name set via the existing rename path;
                              per-segment reassignment via the NEW wire surface (§8, closing T12)
```

- **VP-020.** Stages 1–5 run as one resumable job per recording (§10) with a checkpoint per stage
  (`JobStore::prepare_checkpoint`/`commit_checkpoint`, `crates/jobs/src/store.rs`). Stage 6 is
  user interaction, never a job.
- **VP-021.** Stage 1 uses the existing `DiarizationProvider` contract unchanged. A real local
  provider must satisfy all five ADR-0400 conditions before entering `VALIDATED_PROVIDERS`
  (`crates/ai-diarization/src/provider.rs`); this spec adds no bypass. The deterministic fake
  provider keeps the whole pipeline testable end-to-end (§17).
- **VP-022.** Stage 3 computes embeddings only from spans where exactly one cluster is present
  (`SegmentAssignment::Single`); overlapping speech is excluded from embedding extraction, the
  same rule pyannote applies (`embedding_exclude_overlap: true`)
  [VERIFIED: https://github.com/pyannote/hf-speaker-diarization-3.1/blob/main/config.yaml].
- **VP-023.** Stage 3 enforces minimum speech durations (VP-065) and skips clusters below them
  with an explicit "not enough clean speech" outcome, never a low-quality embedding.
- **VP-024.** Stage 4 never runs when the feature is disabled, when no profiles exist, or when the
  consent gate has not been passed; in those states the job plan simply has no stage-4/5 entries.
- **VP-025.** Model inference (stages 1 and 3) is an ADR-0015 out-of-process candidate: the ONNX
  helper runs under `plaudon_security::sandbox::SandboxedHelper` once that wiring lands (today the
  sandbox has no caller — open finding F-012 — and this EPIC must not widen that gap by adding a
  new in-process parser of untrusted model output).
- **VP-026.** Every embedding records the model id, model version/sha256 and feature-extraction
  parameters that produced it (§7). Embeddings from different models are never compared
  (thresholds are properties of a checkpoint, not an architecture — §6.3). A model upgrade
  triggers re-embedding (§7.6), never silent mixed-version matching.

### 4.1 T12: per-segment speaker reassignment gets wire support

The egui migration honestly shipped T12 short: "nothing on the wire can persist a segment→speaker
assignment: `SegmentEdit` is `{index, text}` and `TranscriptSegmentDto` carries only
`speaker_label`" (`docs/EGUI_MIGRATION_PLAN.md` §4.4 row T12, `docs/EGUI_MIGRATION_REPORT.md`,
`crates/ui/tests/speaker_identity.rs`). The in-memory editor model already supports it:
`Op::SetSpeaker`, `session::set_speaker`, `reassign_speaker_everywhere`
(`crates/transcript-doc/src/{ops,session}.rs`). Confirmation (stage 6) needs exactly this leg, so
this spec closes T12 as a prerequisite:

- **VP-030.** `TranscriptSegmentDto` (`crates/app/src/dto.rs`) gains `speaker_id: Option<String>`
  alongside the resolved `speaker_label`, with `#[serde(default)]` for compatibility.
- **VP-031.** A new command `set_segment_speaker` (request: `recording_id`,
  `transcript_revision_id`, `seq_index`, `speaker_id: Option<_>`) persists one segment's speaker
  in `transcript_segments.speaker_id`, validated against the recording's `speakers` rows. The name
  obeys the §15.1 guard in `crates/app/src/command.rs` (no `run_`/`exec`/`shell`/`http_`/
  `read_file`/`write_file` prefix); schemas land under `schemas/commands/v1/`, the
  `COMMAND_NAMES.len()` assertion and `EGUI_ONLY_COMMANDS` are updated, per the guard tests.
- **VP-032.** Bulk reassignment ("this whole cluster is Priya") reuses the same persistence
  through the suggestion-confirmation command (§8), not through N individual segment writes.
- **VP-033.** T12's parity row and `docs/EGUI_MIGRATION_REPORT.md` are updated when the wire leg
  lands, in the same evidence-citing style ("done — …" with file and test references).

## 5. Model and library evaluation

Everything in this section was researched 2026-10-12; every number carries a source or an
[UNVERIFIED] mark, and §22 aggregates the unverified set. The licence bar is the repository's
existing one: `deny.toml` allows MIT/Apache-2.0/CC-style permissive only, the third-party register
(`docs/licensing/third-party-register.md`) requires a decision per component, and
`docs/public/model-licensing.md` promises that no model ships without a displayed licence and a
verified checksum. "Bundled" below still means *installed via the offline model-pack flow*
(`install_model_from_pack`, `crates/app/src/backend/models.rs`) or an explicit user-approved
download — never a silent download (§12 of the base spec).

### 5.1 Candidate speaker-embedding models

| Candidate | Code licence | Weights licence | Gated | Size | Dim | EER (VoxCeleb1-O) | Verdict |
|---|---|---|---|---:|---:|---|---|
| WeSpeaker ResNet34-LM (ONNX via sherpa-onnx zoo) | Apache-2.0 [VERIFIED: github.com/wenet-e2e/wespeaker/blob/master/LICENSE] | CC-BY-4.0 per WeSpeaker's own policy ("pretrained model on VoxCeleb follows CC-BY-4.0") [VERIFIED: github.com/wenet-e2e/wespeaker/blob/master/docs/pretrained.md]; some HF repos tagged apache-2.0 — treat as CC-BY-4.0, attribute | No | 26.53 MB | 256 | 0.797% as shipped (LM, no AS-Norm); 0.723% requires AS-Norm [VERIFIED: wespeaker examples/voxceleb/v2 README] | **Default candidate** (§5.4) |
| WeSpeaker CAM++-LM | Apache-2.0 | CC-BY-4.0 (same policy) | No | 29.29 MB | 512 | 0.707% as shipped [VERIFIED: same README] | Alternate: ~2.8× faster than ResNet34 single-threaded [MEASURED-RESEARCH], but no published calibrated threshold |
| 3D-Speaker CAM++ en (VoxCeleb) | Apache-2.0 [VERIFIED: github.com/modelscope/3D-Speaker LICENSE] | Apache-2.0 asserted on ModelScope despite VoxCeleb training data [VERIFIED: modelscope.cn/models/iic/speech_campplus_sv_en_voxceleb_16k] | No | 29.60 MB | 512 | 0.73% (vendor-reported) [VERIFIED: ModelScope card]; vendor CPU RTF 0.013, CPU model unspecified | Alternate if an Apache-2.0 *label* is a hard requirement; ships a vendor threshold (0.39 cosine similarity) |
| SpeechBrain ECAPA-TDNN | Apache-2.0 | Apache-2.0 (HF card) | No | [UNVERIFIED] | 192 (standard; [UNVERIFIED] by inspection) | 0.80% [VERIFIED: huggingface.co/speechbrain/spkrec-ecapa-voxceleb] | **Reject for V1: no official ONNX export** (open issue speechbrain#2661); self-export shifts the fidelity burden onto us |
| pyannote/embedding | MIT | MIT, HF-gated | Yes | — | 512 | 2.8% (card's own figure) | **Reject**: gated and ~4× worse than the above |
| NVIDIA TitaNet-L | Apache-2.0 (NeMo) | CC-BY-4.0 on HF; NGC pages cite the Apache-2.0 NeMo licence — NVIDIA is inconsistent | No | 101.41 MB (includes a dead 16,681-way classifier head) | 192 | 0.66% [VERIFIED: huggingface.co/nvidia/speakerverification_en_titanet_large] | **Reject for bundling**: trained on LDC Fisher/Switchboard/SRE corpora, which are per-seat and non-redistributable; if the "dataset licence taints weights" theory applies anywhere, it applies here. Counsel question, not an engineering one |
| Resemblyzer / GE2E d-vector | Apache-2.0 repo | No explicit weights statement | No | 17.09 MB (PyTorch) | 256 | No published EER at all [UNVERIFIED] | **Reject**: 2017-era architecture, no ONNX, dormant since 2023 |
| Whisper-encoder embedding reuse | — | — | — | — | — | Whisper-PMFA reaches 1.42% only with a *newly trained* aggregation head [VERIFIED: arxiv.org/abs/2408.15585] | **Reject**: worse than purpose-built 26 MB models at far higher cost; no redistributable trained head |

The dataset-taint asymmetry deserves one honest sentence: WeSpeaker, 3D-Speaker and SpeechBrain all
train on VoxCeleb and publish three different weight-licence positions (CC-BY-4.0, Apache-2.0,
Apache-2.0). All three are commercially usable; the conservative action — attribute VoxCeleb and
the toolkit under CC-BY-4.0 in `NOTICE` and the third-party register — costs nothing and is what
this spec requires (VP-041).

### 5.2 Candidate diarization/segmentation components

| Candidate | Licence (code / weights) | Gated | Size | Accuracy | Verdict |
|---|---|---|---:|---|---|
| pyannote segmentation-3.0, ONNX via sherpa-onnx release | MIT / **MIT © 2022 CNRS — licence text verified inside the sherpa-onnx tarball**, obtained from a non-gated GitHub release [VERIFIED: github.com/k2-fsa/sherpa-onnx/releases/tag/speaker-segmentation-models] | HF: gated-auto; sherpa-onnx mirror: no gate | 5.99 MB fp32 / 1.54 MB int8 | Component, not a pipeline — DER belongs to pipelines built on it | **Default candidate**. Raw-waveform input, no fbank needed; int8 was *not* faster in research measurements, its value is download size |
| pyannote speaker-diarization-community-1 (pipeline recipe) | MIT code / CC-BY-4.0 weights | Yes (HF gate; an unofficial non-gated mirror exists — supply-chain risk, pin+hash if ever used) | — | AMI-IHM 17.0%, AMI-SDM 19.9%, VoxConverse 11.2% DER, fully automatic, no collar, overlap scored [VERIFIED: community-1 model card] | User-download tier only; these DER numbers are the honest "what good looks like" bar for meeting audio |
| sherpa-onnx diarization pipeline (segmentation-3.0 + embedding + clustering) | Apache-2.0 code | No | runtime | Published end-to-end RTF 0.119 (with titanet_small) on an unspecified macOS machine [VERIFIED: k2-fsa.github.io/sherpa/onnx/speaker-diarization/models.html] | Integration path A (§5.3) |
| speakrs (pure-Rust community-1-style pipeline) | Apache-2.0 | Model repo has **no licence tag** and disclaims upstream compliance onto the integrator | No | VoxConverse dev 7.1% DER at 529× realtime on an M4 Pro under **CoreML**; **no CPU-mode DER/RTF published** [VERIFIED: github.com/avencera/speakrs README; CPU figures UNVERIFIED] | Watch-list. CPU-only path (the Windows x64 case) unmeasured; model provenance chain weaker than sherpa-onnx's |
| pyannote-rs | MIT | No | — | A third party (speakrs) measured DER 80.2% and empty output on 183/216 VoxConverse files [VERIFIED: speakrs README — competitor benchmark, treat sceptically] | **Reject as a diarizer** |
| Revai Reverb v1/v2 (also present in the sherpa-onnx release tag) | Rev Model Non-Production License | — | — | — | **Disqualified: non-commercial.** Model downloads must allow-list by filename, never glob the sherpa-onnx release tag (VP-043) |
| silero-vad | MIT / MIT, weights in-repo | No | 1.29 MB | n/a | Optional pre-filter only; segmentation-3.0 already includes a non-speech class. Add only on a measured win. The `voice_activity_detector` crate shows `license: non-standard` on crates.io — audit before depending [VERIFIED: crates.io API] |

### 5.3 Runtime / integration options

- **Path A — `sherpa-onnx` crate** (Apache-2.0, first-party Rust bindings, v1.13.8 on crates.io):
  a working segmentation → embedding → clustering pipeline today
  (`rust-api-examples/examples/offline_speaker_diarization.rs`,
  `speaker_embedding_manager.rs`). Cost: a large C++ dependency (ONNX Runtime) entering the
  supply chain; `deny.toml` review and a third-party-register row required; native code parsing
  untrusted audio/model bytes strengthens the ADR-0015 out-of-process requirement (VP-025).
- **Path B — `ort` (MIT OR Apache-2.0) + `kaldi-native-fbank` (MIT) + our own windowing,
  AS-Norm and clustering**: full control over the matching layer, which this spec needs anyway for
  profile identity (open-set logic, AS-Norm, per-profile thresholds — §6). Cost: `ort` 2.0 is
  still a release candidate [VERIFIED: crates.io]; we own numerical-equivalence validation.
- There is **no maintained pure-Rust/candle speaker-embedding crate** (searched 2026-10-12; none
  found). A from-scratch port is feasible but buys nothing over `ort` on CPU.

### 5.4 Decision and fallback

- **VP-040.** **Decision:** Path B for matching plus the two bundled-tier artifacts —
  `pyannote segmentation-3.0` ONNX (MIT, 5.99 MB) and `WeSpeaker ResNet34-LM` ONNX (CC-BY-4.0,
  26.53 MB, 256-dim) — distributed through the existing model-pack flow with recorded sha256 and
  displayed licence, exactly like whisper weights. ResNet34-LM wins the default slot over the
  faster CAM++ because it is the exact checkpoint pyannote published a calibrated clustering
  threshold for (cosine *distance* 0.7045654963945799, `min_cluster_size: 12`
  [VERIFIED: github.com/pyannote/hf-speaker-diarization-3.1/blob/main/config.yaml]), so V1 starts
  from a real operating point instead of a guess. Total weight footprint ≈ 32.5 MB.
- **VP-041.** Attribution: `NOTICE` and `docs/licensing/third-party-register.md` gain rows for
  CNRS (MIT, segmentation), WeSpeaker + VoxCeleb (CC-BY-4.0, embedding), and the runtime crates;
  the register's `## Model weights` table records the checksum requirement
  (`no_model_weight_is_bundled_without_a_checksum_requirement`,
  `tests/integration/tests/licensing_register.rs`).
- **VP-042.** **Fallback:** if Path B misses its §14 budgets on reference hardware, fall back to
  Path A (sherpa-onnx pipeline) for stages 1–3 while keeping our matching layer; if the ResNet34-LM
  threshold calibration does not transfer to meeting audio in the §17 evaluation harness, switch
  the default embedder to CAM++-LM (same licence family) and re-calibrate on the harness. If no
  candidate passes the ADR-0400 DER gate on measured fixtures, the feature does not ship — the
  diarization allowlist keeps only the fake provider and the UI never offers enrolment.
- **VP-043.** Model acquisition allow-lists exact filenames + sha256; globbing a release tag is
  forbidden (the sherpa-onnx speaker-model tags contain non-commercial Rev models and unlicensed
  test wavs side by side with the permissive files).
- **VP-044.** The gated `pyannote/speaker-diarization-community-1` recipe (CC-BY-4.0 weights, HF
  token required) is permitted only as a documented *user-download* tier per ADR-0400 condition 3:
  user-supplied token, never stored by the app, with the Hugging Face disclosure. Not in V1.
- **VP-045.** Apple-Silicon and Windows-x64 CPU performance for the chosen stack has **no
  published figures** beyond the single-machine research measurements above [UNVERIFIED for
  release purposes]; the EPIC must measure RTF, peak RSS and startup on both reference platforms
  (`docs/hardware/`) before the feature is offered, mirroring ADR-0400 condition 1.

## 6. Matching algorithm

### 6.1 Scoring

- **VP-050.** Similarity is cosine over L2-normalised embeddings. The score polarity is encoded in
  the type system (`CosineSimilarity` newtype, higher = more similar); pyannote publishes a cosine
  *distance* threshold while sherpa-onnx uses cosine *similarity* — copying "0.7" across polarities
  inverts the decision, and the research pass found both conventions deployed
  [VERIFIED: pyannote 3.1 config.yaml vs sherpa-onnx speaker_embedding_manager.rs].
- **VP-051.** Thresholds are per-checkpoint, never per-architecture: the same CAM++ architecture
  ships with vendor thresholds 0.39 (VoxCeleb-en) and 0.31 (zh-cn corpus)
  [VERIFIED: ModelScope cards]. The threshold set is stored beside the model version and replaced
  with it.
- **VP-052.** Score normalisation: adaptive S-norm (AS-Norm, top-K cohort) is implemented in V1.
  It is worth a consistent ~7–17% relative EER reduction across WeSpeaker models
  [VERIFIED: wespeaker voxceleb/v2 README] and — the reason it matters here — it makes scores
  comparable across enrolment conditions (different microphones, rooms, devices), which is exactly
  the cross-channel variance problem of §6.5. Canonical reference: Matějka et al., Interspeech
  2017. The impostor cohort ships with the model pack as a licence-clean `[N, D]` f32 matrix
  derived from LibriSpeech/synthetic fixtures (§17.2), a few hundred KB.

### 6.2 Open-set decision: two thresholds, three outcomes

- **VP-060.** Matching is open-set: "none of the enrolled people" is the expected common case and
  must be cheap to produce. Two thresholds define three outcomes per (cluster, profile) best
  score:
  - `score ≥ T_suggest` → a **suggestion** is recorded for user review;
  - `T_floor ≤ score < T_suggest` → a **weak signal**: shown only inside the per-recording speaker
    panel's detail view as "possible", never in a notification, badge or queue;
  - `score < T_floor` → nothing is recorded beyond the job's run metadata.
  **In no outcome is a transcript label changed without user confirmation.** There is no
  "high-confidence auto-assign" tier, deliberately: VoxCeleb-trained models carry measured
  demographic error disparities (§16), and an auto-assignment above any threshold converts a
  fairness problem into silently wrong records.
- **VP-061.** Unknown speakers must stay unknown: a cluster that matches nobody keeps its neutral
  "Speaker N (inferred cluster)" display (`crates/domain/src/speaker.rs`); the UI never invites
  "closest match" labelling of a below-threshold score.
- **VP-062.** False-accept vs false-reject: the two thresholds are set from the §17 evaluation
  harness's DET curve such that at `T_suggest` the false-accept rate on the harness is ≤ 1% —
  favouring false rejects, because a missed suggestion costs one manual rename while a wrong
  suggestion teaches distrust and risks mislabelled records. The shipped default values are an
  output of the harness, not of this document; this spec deliberately publishes no threshold
  number for an unmeasured pipeline.
- **VP-063.** Calibration and drift: every confirm/reject decision is recorded (as a boolean
  outcome and score, not as new audio) and may adjust the *per-profile* threshold offset within
  harness-validated bounds — per-speaker calibration, a privacy-preserving cousin of AS-Norm. A
  profile whose recent exemplars are older than 12 months surfaces an "aging profile" notice
  offering re-enrolment or expiry (§12.4's retention clock also applies).
- **VP-064.** Exemplar aggregation: a profile is matched as multi-exemplar, not single-centroid —
  the score is the maximum of (a) similarity to the profile centroid and (b) the top-2 mean of
  per-exemplar similarities. Centroid-only matching degrades when exemplars span devices/rooms;
  multi-exemplar matching is the standard mitigation. [Design decision; comparative literature on
  centroid-vs-exemplar for this exact stack: UNVERIFIED — validated by the harness before ship.]
- **VP-065.** Minimum durations (from the short-utterance literature): embeddings are computed only
  over ≥ 1.0 s of clean single-speaker speech; identification requires ≥ 3 s accumulated speech
  per cluster (expect roughly 1.4–2.3× nominal EER at 3 s, 2.0–3.9× at 2 s
  [VERIFIED: ERes2NetV2 paper, arxiv.org/html/2406.02167v1 Table 3]); enrolment requires ≥ 10 s
  accumulated clean speech and recommends ≥ 2 recordings. The 1.0 s floor is derived
  ([INFERENCE] from `min_duration_on = 0.3 s` pipeline defaults plus the 2 s EER data; no single
  source states it). Architectural minima (ResNet34 accepts 25 ms) are not reliability minima and
  must not be relied on.
- **VP-066.** Overlapping speech never contributes to embeddings (VP-022) and a cluster whose
  speech is mostly overlap reports "not enough clean speech" rather than a low-confidence match.
- **VP-067.** Cross-channel/device variance is addressed by (a) AS-Norm (VP-052), (b)
  multi-exemplar profiles spanning devices (VP-064, user story 4), and (c) the §17 harness
  including cross-device trials (same synthetic speaker through different codec/EQ chains). No
  claim of device invariance is made anywhere in UI or docs.

## 7. Data model and migration

### 7.1 Placement

- **VP-070.** New migration file `crates/persistence/migrations/0900_voice_profiles.sql`, claiming
  the **0900–0999** range ("voice profiles", currently unallocated — the range comment in
  `crates/persistence/src/migrations.rs` is extended, per its own "claim a range in this comment"
  rule; the highest applied migration today is 0800 `source_reconciliation`). All tables are
  `STRICT` with `CHECK` constraints and explicit `ON DELETE` behaviour, like the existing files.

### 7.2 Tables

```sql
-- The master speaker list (roster). Ordinary personal data: names and plain contacts,
-- never biometrics. Exists and is useful with biometric matching off (VP-011).
CREATE TABLE people (
    id            TEXT PRIMARY KEY,               -- UUID
    display_name  TEXT NOT NULL CHECK (length(display_name) BETWEEN 1 AND 120),
    aliases_json  TEXT NOT NULL DEFAULT '[]',     -- JSON array of alternate names (search only)
    email         TEXT CHECK (email IS NULL OR length(email) <= 254),
    organisation  TEXT CHECK (organisation IS NULL OR length(organisation) <= 200),
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('active','merged'))
) STRICT;

-- A person's optional biometric attachment (0..1 per person). Only exists after the
-- consent gate; carries the consent/retention state. Deleting it leaves the person
-- as a name-only roster entry.
CREATE TABLE person_voice_enrollments (
    person_id        TEXT PRIMARY KEY REFERENCES people(id) ON DELETE CASCADE,
    consent_version  TEXT NOT NULL,               -- disclosure text version shown at creation
    consent_note     TEXT,                        -- optional free-text basis the user recorded
    retention_months INTEGER NOT NULL DEFAULT 12 CHECK (retention_months BETWEEN 1 AND 12),
    last_matched_at  INTEGER,                     -- drives the §12.4 expiry clock
    status           TEXT NOT NULL CHECK (status IN ('active','expired')),
    created_at       INTEGER NOT NULL
) STRICT;

-- One stored embedding with full provenance. Vector bytes are AEAD-encrypted (§12.5).
CREATE TABLE voice_exemplars (
    id                 TEXT PRIMARY KEY,
    person_id          TEXT NOT NULL REFERENCES person_voice_enrollments(person_id)
                       ON DELETE CASCADE,
    recording_id       TEXT REFERENCES recordings(id) ON DELETE SET NULL,  -- provenance, not ownership
    diarization_run_id TEXT REFERENCES diarization_runs(id) ON DELETE SET NULL,
    cluster_key        TEXT,                      -- which cluster it was cut from
    source_start_ms    INTEGER, source_end_ms INTEGER,
    speech_ms          INTEGER NOT NULL CHECK (speech_ms >= 1000),
    model_id           TEXT NOT NULL,             -- e.g. "wespeaker-resnet34-lm"
    model_version      TEXT NOT NULL,
    model_sha256       TEXT NOT NULL,
    embedding_dim      INTEGER NOT NULL,
    embedding_cipher   BLOB NOT NULL,             -- XChaCha20-Poly1305, key in OS secret store
    embedding_nonce    BLOB NOT NULL,
    created_at         INTEGER NOT NULL
) STRICT;

-- Pending/settled match suggestions. Scores, never labels. A rejected row doubles as
-- negative evidence for per-enrolment calibration (VP-216): score + decision, no embedding.
CREATE TABLE speaker_suggestions (
    id                 TEXT PRIMARY KEY,
    recording_id       TEXT NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    diarization_run_id TEXT NOT NULL REFERENCES diarization_runs(id) ON DELETE CASCADE,
    cluster_key        TEXT NOT NULL,
    person_id          TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    score              REAL NOT NULL,
    score_kind         TEXT NOT NULL CHECK (score_kind IN ('cosine_similarity_asnorm')),
    band               TEXT NOT NULL CHECK (band IN ('suggest','weak')),
    state              TEXT NOT NULL CHECK (state IN
                       ('pending','confirmed','rejected','corrected','expired')),
    corrected_person_id TEXT REFERENCES people(id) ON DELETE SET NULL,  -- VP-216 reject→pick flow
    decided_at         INTEGER,
    model_id           TEXT NOT NULL, model_version TEXT NOT NULL,
    created_at         INTEGER NOT NULL,
    UNIQUE (diarization_run_id, cluster_key, person_id)
) STRICT;

-- Journal of roster- and consent-relevant events; append-only, exportable (§12.3).
CREATE TABLE voice_profile_events (
    id          TEXT PRIMARY KEY,
    person_id   TEXT NOT NULL,                    -- deliberately NOT a FK: survives deletion
    kind        TEXT NOT NULL CHECK (kind IN
                ('person_created','person_renamed','person_merged','person_deleted',
                 'enrolled','exemplar_added','enrollment_split','enrollment_expired',
                 'enrollment_deleted','suggestion_confirmed','suggestion_rejected',
                 'suggestion_corrected','assignment_set','assignment_cleared')),
    detail_json TEXT NOT NULL,                    -- no embeddings, no scores against third people
    created_at  INTEGER NOT NULL
) STRICT;
```

- **VP-071.** The raw embedding vector exists in plaintext only in process memory during
  embed/match; at rest it is always `embedding_cipher`. Keys: one feature-scoped data key,
  generated locally, stored via the existing OS secret store
  (`crates/security/src/secrets.rs` — new `SecretScope::VoiceProfileKey` variant; unavailable
  store = locked feature, no plaintext fallback, exactly ADR-0014's rule). AEAD is the
  XChaCha20-Poly1305 already in the workspace (ADR-0071 stack). This is additional to — not a
  replacement for — the ADR-0402 recommendation to run FileVault/BitLocker, and it enables
  cryptographic erasure (VP-095). Consistent with the full-library-encryption threat model
  (`docs/threat-model/full-library-encryption.md`): it protects data at rest, not data in use.
- **VP-072.** `speakers` rows (per-recording, `crates/persistence/migrations/0002_core.sql`) gain
  nothing biometric. An assignment — manual pick or confirmed suggestion — sets the existing
  `speakers.name`/`user_confirmed` through the existing rename/confirm path and records the link
  in a new nullable `speakers.person_id` column (ON DELETE SET NULL). The name stays a **text
  snapshot** beside the link: deleting the person from the roster severs the link and leaves the
  snapshot, so no recording silently loses its labels (VP-213). Per-segment assignments
  (`transcript_segments.speaker_id` → that recording's `speakers` row, VP-031) are unchanged in
  shape; the roster is reached through the `speakers` row's `person_id`.
- **VP-073.** Suggestion rows reference `diarization_runs`/`speaker_turns`
  (`0400_transcript_tools.sql`) rather than transcript revisions, so re-transcription does not
  orphan them; reconciliation to segments happens at display time through the existing
  `segment_speaker_assignments`.
- **VP-074.** `schema_migrations` discipline, pre-migration backup (`VACUUM INTO`), and
  `crates/persistence/tests/schema_contract.rs` coverage apply as for every migration.

### 7.3 Model-version binding and re-embedding

- **VP-075.** Every exemplar and suggestion is bound to (model_id, model_version, model_sha256).
  Matching only ever compares equal versions.
- **VP-076.** Installing a new embedding-model version offers a re-embed job: for exemplars whose
  source recording still exists, embeddings are recomputed from audio; exemplars whose source
  audio was deleted cannot be re-embedded and are flagged "legacy — will not match until
  re-enrolled" (the original audio is never retained just for re-embedding). Old-version
  ciphertexts are destroyed after a successful re-embed.

### 7.4 Export, backup and sync

- **VP-077.** **Default: biometric data is excluded from every export and portable backup; roster
  rows are not biometric and follow ordinary rules.** `export_recording` (all `ExportFormat`s)
  never contains embeddings, scores, suggestion history or roster contact fields — a recording
  export carries only the resolved display-name **text** per speaker, exactly as today
  (`SpeakerRow` keeps label/name/confirmed). The roster itself (names, aliases, email/org) **is**
  included in portable backups, plaintext or encrypted, like any other library data — it is a
  contact list, not a voiceprint (decision; see §12.2A for the boundary). Only the
  `person_voice_enrollments` + `voice_exemplars` + `speaker_suggestions` tables are
  biometric-constrained.
- **VP-078.** An explicit "include voice profiles" toggle exists only for **encrypted** portable
  backups (ADR-0071 Argon2id + STREAM; `Protection::Encrypted` required — a plaintext backup with
  voice data is unrepresentable, enforced in `ExportService::create_portable_backup`). The archive
  gains a `voiceprofiles/` namespace with its own document schema
  (`archive-voiceprofile/1`, `#[serde(deny_unknown_fields)]`), under the existing
  `ArchiveSecretScanner` plus a new rule treating embedding blobs outside that namespace as a
  refusal. Restore of a voice-data archive re-runs the consent gate before activating profiles.
- **VP-079.** There is no sync. Profiles are per-library. Cross-library correlation is a threat,
  not a feature (TM row, §13): the export default (VP-077), the per-library AEAD key (VP-071) and
  the absence of any stable cross-library identifier are the mitigations.
- **VP-080.** Deletion integrates with the ADR-0072 journal: deleting a recording cascades its
  suggestions and nulls exemplar provenance (an exemplar belongs to the enrolment and survives
  unless the enrolment is deleted — the deletion disclosure says so). Deleting an **enrolment**
  destroys its exemplars/suggestions and leaves the person as a name-only roster entry; deleting a
  **person** additionally severs `speakers.person_id` links (name snapshots remain, disclosure
  states in how many recordings) — both journalled with exact counts (`DeletionService`,
  `crates/app-core/src/deletion.rs`).

## 8. Command surface and events

All names obey the §15.1 guard test
(`no_generic_shell_http_or_filesystem_command_is_published`, `crates/app/src/command.rs`): no
`run_`/`exec`/`shell`/`http_`/`read_file`/`write_file` prefixes. Each command adds a table line to
`command_surface!`, functions in `crates/app/src/commands/{people,voice_profiles}.rs` (new modules:
roster commands live in `people`, biometric commands in `voice_profiles`), request/
response DTOs, two JSON schemas under `schemas/commands/v1/`, an `EGUI_ONLY_COMMANDS` entry and the
count bump — the mechanism the guard tests enforce.

| Command | Purpose |
|---|---|
| **Roster (works with matching off, VP-011)** | |
| `list_people` | the master list with assignment counts and enrolment presence — never embeddings |
| `create_person`, `update_person` | roster CRUD: display name, aliases, optional email/org (plain contacts) |
| `merge_people` | merge duplicates: assignments, enrolments and exemplars repointed; disclosure shows affected-recordings count (VP-213) |
| `delete_person` | removes the roster entry; severs `speakers.person_id` links, name snapshots remain (VP-080) |
| `assign_cluster_person` | bulk "this cluster is X" in one recording: sets `speakers.name` + `person_id` for that cluster, nothing beyond the recording |
| `set_segment_speaker` | the T12 wire leg (VP-031); the segment picker offers roster people, "create new person" and "unknown/none" (VP-210) |
| **Biometric tier (consent-gated, feature-flagged — except the four reads/deletes marked below, which must stay reachable when the gate is shut)** | |
| `get_voice_profile_settings` / `set_voice_profiles_enabled` | feature gate + consent-gate state (§12.2) |
| `enroll_person_voice` | attach an enrolment to a roster person from (recording, cluster) after the consent gate |
| `add_voice_exemplar` | extend an enrolment from another recording's cluster |
| `split_voice_enrollment` | split an enrolment's exemplars by provenance onto another (new) person |
| `list_voice_exemplars` | what one enrolment is made of: exemplar ids, provenance and durations — never vectors (drives the split UI, VP-091) |
| `get_cluster_speech` | a cluster's clean speech before anything is computed, for the enrolment meter (VP-094); ungated, because it reads the stored reconciliation and calls no model |
| `delete_voice_enrollment` | per-person biometric delete + journal (§12.4); person stays as name-only |
| `forget_all_voice_profiles` | the "forget everyone" button (VP-095): every enrolment + the key; roster survives |
| `list_speaker_suggestions` | pending suggestions for a recording, or the global review queue |
| `confirm_speaker_suggestion` | applies the person to the cluster via the assignment path |
| `reject_speaker_suggestion` | records the rejection (negative evidence, VP-216); optional `corrected_person_id` for the reject-then-pick flow |
| `start_speaker_matching` | enqueue the §10 job for a recording (or an explicit selection for bulk review) |

- **VP-085.** DTOs follow `crates/app/src/dto.rs` conventions (`#[serde(rename_all="snake_case")]`,
  `#[serde(default)]` for added fields). **No DTO ever carries embedding bytes or key material**;
  `assert_no_secret_in_response` (`crates/security/src/response.rs`) gains an embedding-shaped
  check, and the `Debug`-prints-no-arguments rule applies (`a_command_debug_line_never_contains_its_arguments`).
- **VP-086.** Events: `voiceprofile.suggestions_ready { recording_id, pending_count }`,
  `person.changed { person_id }`, plus the ordinary `job.updated`. Payloads are ids and
  counts only — names and scores are fetched by command, never pushed into the event/notification
  path (ADR-0021 keeps notification text generic; a lock-screen toast never contains a person's
  name — see §11).

## 9. UI (egui)

Screens follow the `Screen` trait registration pattern (`crates/ui/src/shell/screen.rs`,
`ScreenRegistry`) and the design system (`docs/EGUI_DESIGN_SYSTEM.md`): AccessKit roles,
status = glyph plus word never colour alone, 24 pt minimum targets, Escape-closes-modal,
`confirm_modal` for destructive actions.

- **VP-090.** **Settings → Privacy** gains a "Voice profiles" block in `PrivacySection`
  (`crates/ui/src/settings/privacy.rs`): the biometric-matching master toggle (off by default),
  the consent-gate summary, retention default, a link to the People screen, and the "Forget all
  voice profiles" danger button. Copy states plainly: local-only, biometric, off by default, your
  legal responsibility to get attendees' consent (§12.3) — and that the People list itself works
  without any of this (VP-011).
- **VP-091.** **People screen** (new `plaudon-ui::screens::people`, a top-level destination —
  `Destination::People`, extending `Destination::ALL` and the Mod+N shortcuts — also reachable
  from Settings and the speaker panel): searchable list (name + aliases) with per-person
  assignment count ("in 14 recordings"), enrolment presence badge ("voice: enrolled/none"),
  exemplar count, device-diversity hint, last matched and expiry countdown where enrolled;
  actions: rename, edit aliases/contacts, merge (with affected-recordings disclosure), enrol
  voice / delete enrolment, split enrolment by provenance, delete person. The split dialog is fed
  by `list_voice_exemplars` (ids, provenance, durations — never vectors) and ticks exemplars by
  provenance; moving every one is refused as a merge, and the move is attested for the person the
  speech goes *to*. Dedupe aid: the list surfaces same-name/same-alias groups as merge candidates
  — never auto-merged. Every control named for AccessKit (`assert_every_control_is_named`,
  `crates/ui-testkit`).
- **VP-092.** **Per-recording speaker panel** (extends the mounted T12 surface,
  `plaudon_ui::ai::OutputsViewer` / `LivePanels`): per cluster — neutral label, the **person
  picker** (VP-210), suggestion (if any) with qualitative band + numeric score ("sounds like
  Priya — strong match, 0.82"), Confirm / Not them (reject-then-pick, VP-216), "Enrol this
  speaker's voice…" where gated-on, and the per-segment person picker (now wired, VP-031) in the
  transcript chips (`crates/ui/src/transcript/chips.rs::speaker`). The panel works identically
  with matching off — minus the suggestion rows.
- **VP-093.** **Bulk review queue**: a single list of pending suggestions across recordings
  (opt-in run, VP-007), keyboard-completable (j/k navigation, Enter confirm, Del reject),
  announced via `Role::Status`/`Live::Polite` like existing toasts.
- **VP-094.** Enrolment flow: segment-anchored ("create from this speaker"), shows what audio
  span the exemplars come from, enforces VP-065 minima with a progress meter ("12 s of 10 s clean
  speech collected", fed by `get_cluster_speech` as the flow opens, and the enrolment control
  stays unavailable while the measurement is under the minimum), and ends at the consent gate
  (§12.2) before anything is computed.
- **VP-095.** **Forget everyone:** one action destroys every enrolment, exemplar and suggestion,
  destroys the `SecretScope::VoiceProfileKey` entry (cryptographic erasure of every
  `embedding_cipher`, including any copy inside encrypted backups the key never left the device
  to protect), disables biometric matching, and writes one journal event. **The roster and all
  assignments survive** — they are names, not voiceprints; a separate ordinary action deletes
  people. The disclosure lists counts before confirmation, in the ADR-0072 disclosure style, and
  states the honest limitation: previously exported *names* in transcripts/exports are plain text
  and are not recalled, and we make no forensic-erasure claim for SSD sectors (§4.6 of the base
  spec applies).
- **VP-096.** A11y: the manual screen-reader script (`docs/accessibility/manual-screen-reader-script.md`)
  gains a voice-profiles chapter; suggestion confirm/reject is fully keyboard-driven; score bands
  have text names ("strong/possible"), not colour-only bars.

## 9A. Master speaker list: pickers, correction, and propagation

The roster is the single assignment vocabulary across the app. This section is normative for both
modes (matching on and off).

- **VP-210.** **The person picker** is one widget used everywhere a speaker can be set — the
  per-cluster row in the speaker panel, the per-segment transcript chip, and the bulk-review
  queue. It is a searchable combobox over the roster (matches display name and aliases,
  most-recently-assigned first, then alphabetical), with three fixed entries: **"Create new
  person…"** (inline name field; creates the roster row and assigns in one step),
  **"Unknown/none"** (clears the assignment back to the neutral cluster label), and — when a
  suggestion exists — the suggested person pinned on top with its band and score. The picker is
  fully keyboard-driven: open on Enter/Space, type-to-filter, arrows, Enter assigns, Escape
  cancels; AccessKit `Role::ComboBox` with the filtered-count announced politely.
- **VP-211.** **Scope of an assignment action** is exactly what the control names: the segment
  picker sets one segment (`set_segment_speaker`); the cluster row's "Assign all of Speaker 2 to
  …" sets that cluster in that recording (`assign_cluster_person`); nothing reaches beyond the
  open recording. A cluster assignment that would override per-segment exceptions says so
  ("3 segments were set individually — keep or overwrite?").
- **VP-212.** **Undo/redo:** every assignment action in the open recording goes through the
  transcript session's undo stack (`Op::SetSpeaker`, `crates/transcript-doc/src/ops.rs`) — one
  undo step per user action (a bulk cluster assignment is one step), Cmd/Ctrl+Z works, and undo
  restores both the segment links and the cluster link. Roster edits (rename, merge) are not in
  the transcript undo stack; they are journalled, and merge is reversible only via the journalled
  split, with the disclosure saying so.
- **VP-213.** **Propagation rules — nothing rewrites other recordings silently.**
  (a) *Rename/alias/contact edits* propagate by reference: every recording shows the new display
  name because display joins through `people`, and the edit dialog states the blast radius
  ("name shown in 14 recordings") before saving. The underlying assignments are untouched.
  (b) *Assignment changes* (pick, correct, clear) affect only the open recording — never another
  recording's assignments, suggestions or exemplars.
  (c) *Merge* repoints assignments and enrolments from both people to the survivor after a
  disclosure that counts affected recordings, exemplars and pending suggestions; it is journalled
  (`person_merged`) and the loser row keeps `status='merged'` tombstone semantics for the journal.
  (d) *Person deletion* severs links and leaves per-recording name snapshots (VP-080); it never
  erases transcript text.
  (e) *Enrolment changes* (new exemplars, expiry, deletion) change future matching only; they
  never touch existing assignments.
- **VP-214.** **Dedupe:** the People screen groups same-normalised-name/alias rows as merge
  candidates; merging is always explicit (VP-213c); the picker's create-new entry warns inline
  when the typed name equals an existing person ("assign to existing Priya instead?").
- **VP-215.** **Manual-only parity:** with biometric matching off, everything in this section
  behaves identically except that suggestion rows and enrolment affordances are absent; the
  picker, bulk assignment, undo, propagation and export behaviour are byte-for-byte the same code
  paths (no forked manual implementation).
- **VP-216.** **Correcting a wrong suggestion** is one flow: *Not them* marks the suggestion
  `rejected` and opens the picker; picking a person records `corrected_person_id` on the
  suggestion (state `corrected`), assigns that person to the cluster, and the rejected
  (run, cluster, person, score) tuple becomes **negative evidence** for the wrongly-matched
  enrolment's per-profile calibration (VP-063). **Deliberate deviation, for privacy:** the
  negative is the *score record*, never a stored embedding — keeping the rejected cluster's
  embedding against the wrong person's profile would retain a voiceprint of whoever actually
  spoke, who may be unenrolled and unconsented (§12.1). If the corrected person **is** enrolled,
  the flow offers "also add this speech to Priya's exemplars" (ordinary consented exemplar add,
  VP-064 rules); if not, no embedding outlives the job.
- **VP-217.** **Suggestion-queue correction parity:** the bulk review queue offers the same
  reject-then-pick (keyboard: `n` rejects and opens the picker), so correction never requires
  leaving the queue.

## 10. Job integration

- **VP-100.** New `JobKind::SpeakerMatch` (`crates/app/src/dto.rs`), executor module
  `crates/app/src/backend/processing/speaker_match.rs` beside `stt.rs`/`text.rs`, a
  `dispatch_executor` arm and preflight arm (`processing/mod.rs` — today any kind other than
  Transcribe/Summarize returns "this build has no executor"), and `ProcessingStage` additions
  `SpeakerEmbed`, `SpeakerMatch` beside the existing (currently executor-less) `Diarize`
  (`crates/jobs/src/key.rs`, `crates/app/src/cloud_presentation.rs`).
- **VP-101.** The job key (`ProcessingJobKey`) covers audio sha256, diarization run,
  embedding-model id+version, threshold-set version and enrolled-profile-set digest, so enqueueing
  the same work twice deduplicates (`EnqueueOutcome::Deduplicated`) and enrolling a new profile
  naturally makes a fresh key.
- **VP-102.** Resumable and idempotent: one checkpoint per pipeline stage (§4) through
  `prepare_checkpoint`/`commit_checkpoint`; a crash between embed and match resumes at match;
  re-running a committed stage is refused by the store. Suggestion insertion is idempotent via the
  `UNIQUE (diarization_run_id, cluster_key, person_id)` constraint.
- **VP-103.** Cancellation and progress use the existing `JobContext { cancel, progress }`
  (`plaudon_ai_contracts::job`) and the coalescing `ProgressReporter`; cancel points at every
  window boundary during embedding, the most expensive loop.
- **VP-104.** Resources: the job takes a `ResourceClass::Cpu` slot like transcription
  (`ResourceScheduler::admit`), and defers to the §14.4 arbitration rules; it never runs
  concurrently with local STT on the same recording (shared decode scratch).
- **VP-105.** Chaos coverage: `tests/chaos` gains kill/resume cases for the new stages, and
  `coverage-policy.toml` gains `[[transitions]]` entries naming positive, negative and
  crash-recovery tests for the new stage transitions, as the policy requires.

## 11. Local-only mode and egress

The egress gate (ADR-0013, `crates/security/src/outbound.rs`) is the enforcement point, and it
needs *no relaxation* for this feature — only hardening:

- **VP-110.** `PayloadClass` (today: None/Metadata/TranscriptText/AudioBytes/Credential — only
  the middle two consentable) gains **`VoiceBiometric`, which is unconsentable by construction**:
  `OutboundPolicy::authorize` refuses it in every `NetworkMode`, for every subsystem, with a
  dedicated `DenyReason`. There is no UI that could grant it; the refusal is structural, like the
  sealed `EgressTicket`. Embeddings therefore cannot reach OpenRouter, PLAUD Cloud, the update
  checker, model distribution or telemetry even by a future coding mistake that requests them.
- **VP-111.** `apps/plaudon-egui/tests/local_only_egress.rs` gains a scenario driving
  enrol + match end-to-end behind the cloud-transport watch, asserting zero outbound attempts —
  in local-only mode **and out of it** (this feature is local-only in both modes).
- **VP-112.** Redaction: profile display names and scores are treated like transcript text in
  diagnostics — never in error envelopes (§15.4, `crates/security/src/domain_map.rs` detail rules),
  never in notifications (ADR-0021 generic text), and the diagnostics redactor
  (`plaudon_diagnostics::redact`) registers profile names as redactable terms. Embedding bytes
  never reach any log path; `fuzz_redactor`/`fuzz_error_envelope` corpora gain embedding-shaped
  inputs.
- **VP-113.** The §16.2A egress-evidence suite (`tests/egress/`) and
  `docs/threat-model/egress-evidence.md` are extended with the voice-profile scenario, so the
  published evidence covers the new subsystem.

## 12. Privacy, security and legal

This section is the heart of the feature and the reason ADR-0080 exists. It stands on two
sentences the base spec already commits to: §8.7 — "We will not promise speaker identity from
voice without explicit opt-in" and "We will distinguish identified speakers (user-confirmed) from
inferred speaker clusters" — and §1.7 — the app "will not make legal-compliance claims or assume a
PLAUD purchase establishes rights to third-party voices" (both verified against the source text).
Research pass 2026-10-12; primary sources cited; open legal questions are marked CONTESTED and
collected in §20/§22. None of this is legal advice; a counsel review is a release gate (VP-131).

### 12.1 A voice embedding is biometric data, in every regime that matters

- **GDPR.** Art. 4(14) + Art. 9(1): biometric data processed "for the purpose of uniquely
  identifying a natural person" is special-category data; a stored, matchable speaker embedding
  used to label "who spoke" is exactly that purpose
  [VERIFIED: https://gdpr-info.eu/art-9-gdpr/; EDPB Guidelines 3/2019 on video devices draw the
  recording-vs-template line]. The only workable lawful basis for a meeting tool is **Art. 9(2)(a)
  explicit consent of the enrolled person** — the attendees, who are third parties, not the app
  user. The household exemption (Art. 2(2)(c)) must be treated as unavailable: CJEU C-212/13
  *Ryneš* reads it narrowly, and work meetings are by definition connected to a professional
  activity [VERIFIED: curia.europa.eu press release 175/14]. The *user* is the controller
  (Art. 4(7)); the vendor of offline software that never receives the data is neither controller
  nor processor ([INFERENCE] from EDPB Guidelines 07/2020's essential-means analysis and its
  IT-consultant example; no EDPB example squarely covers offline shrink-wrap software — CONTESTED).
- **Illinois BIPA (740 ILCS 14).** "Voiceprint" is an enumerated biometric identifier; §15(b)
  requires written notice + specific purpose and retention term + a **written release** before
  collection; §15(a) requires a public retention/destruction policy (destroy when purpose
  satisfied or 3 years after last interaction, whichever is first); §20 gives a private right of
  action ($1,000/$5,000 per person after SB 2979 clarified per-person rather than per-scan
  accrual) [VERIFIED: ilga.gov statute text]. BIPA's "private entity" expressly includes "any
  individual", so the *user* is on the hook on the statute's face (no case law on purely personal
  users — CONTESTED). A certified class of ~1.2 M users exists on precisely the voice-enrolment
  fact pattern (*Wilcosky v. Amazon*, Alexa Voice ID, class certified Nov 2025)
  [VERIFIED: duanemorris class-action blog].
- **Texas CUBI (§503.001).** Voiceprint enumerated; inform + consent before capture for a
  commercial purpose; **destruction within one year of the purpose expiring** — the strictest
  retention clock in any applicable statute, and the reason for the 12-month default (VP-130);
  AG-only enforcement, up to $25,000 per violation, and the AG has extracted $1.4 B (Meta) and
  $1.375 B (Google) [VERIFIED: texasattorneygeneral.gov]. The 2026 TRAIGA AI carve-out explicitly
  does **not** cover systems "used or deployed for the purpose of uniquely identifying a specific
  individual" — i.e. it does not help this feature [VERIFIED: statute text].
- **Washington.** RCW 19.375 (HB 1493) covers voiceprint enrolment but its "commercial purpose"
  is narrowly sale/marketing-oriented, and its definition excludes data derived from an audio
  recording (whether an embedding falls in that carve-out is unresolved — CONTESTED); My Health
  My Data (RCW 19.373) separately treats voice-derived biometric data as consumer health data with
  a strict opt-in consent standard that "may not be obtained via general terms of use"
  [VERIFIED: app.leg.wa.gov RCW texts].
- **CCPA/CPRA.** Voiceprint-extractable voice recordings are "biometric information"; processing
  it to uniquely identify a consumer is **sensitive personal information** (§1798.140(bbb)(2)).
  Obligations attach to covered "businesses", not to individuals; relevance is to enterprise
  customers, whose SPI obligations the feature's deletion/disclosure controls must not obstruct
  [VERIFIED: privacy.ca.gov statute PDF].
- **EU AI Act (Reg. 2024/1689).** 1:N matching against an enrolled set is **biometric
  identification** (Art. 3(35)), not verification (Art. 3(36)) — the spec and UI never say
  "verification" (VP-004). It is not *remote* biometric identification on the text (enrolled,
  consented, actively-speaking participants; not a publicly accessible space), so Annex III(1)(a)
  high-risk classification is not engaged on the text — CONTESTED at the margin for recognising a
  previously-consented person in a later meeting, which is a reason the feature never runs
  automatically over new recordings without the user having enabled it and a reason enrolment
  consent names future recognition explicitly. Art. 50(3) transparency covers emotion recognition
  and biometric categorisation, which the product forbids (VP-005), not identification. Art. 2(10)
  exempts purely personal non-professional deployers; professional users are deployers
  [VERIFIED: artificialintelligenceact.eu texts].
- **Recording consent is a separate, additional layer.** All-party consent states (CA, FL, IL, MD,
  MA, MT, NH, PA, WA, plus DE; MI for third-party recordings; OR for in-person after the Ninth
  Circuit en banc reversal of *Project Veritas v. Schmidt*, 7 Jan 2025; split-modality rules in
  MO, CT, NV, HI, ME) [VERIFIED: rcfp.org recording guide; en banc opinion at cdn.ca9.uscourts.gov].
  A valid "this meeting is recorded" notice does **not** satisfy BIPA §15(b), CUBI, MHMD or GDPR
  Art. 9(2)(a). Two consent layers, never bundled (VP-121).

### 12.2 Consent model

- **VP-120.** The **biometric tier** has a three-level gate, each strictly stronger: (1) the
  matching toggle
  (off by default, VP-003); (2) a one-time **activation disclosure** (versioned text, §16-style
  honest language) covering what an embedding is, where it is stored, retention defaults, and the
  sentence "People you enrol must agree first. In Illinois, Texas, Washington, the EU and other
  places, their explicit — sometimes written — consent is legally required, and that is your
  responsibility, not this software's"; (3) a **per-enrolment confirmation** in which the user
  attests the named person has agreed, chooses/accepts the retention term, and may attach a note
  of the consent basis. The attestation, disclosure version and timestamp land in
  `voice_profile_events` (§7.2).
- **VP-121.** Consent texts are never bundled with unrelated terms, never pre-ticked, and
  re-prompt when the disclosure version changes — the same scoped-consent discipline as §16.3
  cloud consent (`crates/security/src/consent.rs` patterns), applied to a local feature.
- **VP-122.** The app ships an exportable, printable consent-request template (plain text) the
  user can hand to attendees, and an exportable consent record per profile
  (`voice_profile_events` filtered) — making the user's §15(b)/Art. 9 duty dischargeable without
  making the vendor a party to it.
- **VP-123.** Withdrawal: "this person withdrew consent" is a first-class action on the profile
  (delete with reason recorded); the disclosure explains what deletion does and does not reach
  (VP-095 honesty rules).

### 12.2A The name-only roster is not biometric data — reasoning and limits

- **VP-126.** **Boundary.** The master list without an enrolment is a contact list: display name,
  aliases, optional email/organisation, plus links from recordings the user already possesses.
  None of the biometric definitions reaches it: GDPR Art. 4(14) requires data "resulting from
  specific technical processing relating to the physical, physiological or behavioural
  characteristics" of a person — a typed name is not; BIPA's identifier list is "retina or iris
  scan, fingerprint, voiceprint, or scan of hand or face geometry" and expressly **excludes**
  writing samples, demographic data and physical descriptions; CUBI and RCW 19.375 enumerate the
  same closed classes; CCPA's "biometric information" requires characteristics from which "an
  identifier template, such as a … voiceprint, can be extracted" — a name is not extractable
  material [VERIFIED: statute texts cited in §12.1]. Therefore the roster, manual assignment and
  manual-only mode carry **no** consent gate, no retention clock, no AEAD requirement, and no
  VoiceBiometric egress class.
- **VP-127.** **Limits of that reasoning — stated, not hidden.** (a) The roster is still
  *personal data* under GDPR: names, employer, email and "who attended which meeting" need an
  ordinary Art. 6 basis and fall under access/erasure rights; the household exemption is as
  narrow here as in §12.1. The product's duties are the ordinary ones it already honours for
  transcripts (local-only storage, export, delete). (b) The *audio* still exists: a name-only
  assignment plus the recording would let a future user enrol that person — which is why enrolment
  always re-runs its own gate and is never implied by an assignment. (c) A name linked to
  transcript content can still be sensitive in the ordinary sense (who said what); nothing in
  this section weakens §16.6-style operational honesty. (d) This boundary is jurisdiction-tested
  only as far as the cited statute texts go — flagged for the same counsel review as §12.1
  (VP-131), but it is the reason manual mode ships without the biometric machinery (VP-011), not
  a loophole for storing anything voice-derived: the structural test of §17.1 proves the manual
  path cannot reach embedding code.
- **VP-128.** **Consent-text clarity.** The activation disclosure (VP-120) states the two-tier
  boundary in one sentence: "Keeping a list of names and assigning them to recordings needs no
  special consent; computing a voiceprint to recognise someone automatically does." ADR-0080 is
  amended to carry the same distinction.

### 12.3 What the vendor never has

- **VP-124.** The vendor-safe posture is a design contract, not a policy: embeddings and all
  derived voice data (a) exist only on the user's device, (b) are never transmitted anywhere
  (VP-110's unconsentable payload class), (c) are not accessible to the vendor by any mechanism
  (no remote admin, no escrowed key, no "model improvement" path), and (d) can be disabled and
  destroyed by the user alone. This is the fact pattern the Seventh Circuit held does not trigger
  BIPA's possession/collection verbs for a device maker (*G.T. v. Samsung*, 7th Cir. 2026;
  likewise *Barnett v. Apple*, *Bhavilai v. Microsoft*) [VERIFIED: case citations in the research
  dossier; TX/WA transfer of that reasoning UNVERIFIED — no parallel case found]. The
  counter-model to never resemble: *Hazlitt v. Apple* — on-device biometrics the **user could not
  see, disable or delete**.
- **VP-125.** `docs/public/privacy-disclosure.md` gains a voice-profiles section that states what
  the feature does, that the vendor never collects/receives/can-access voiceprints, and the
  user-responsibility sentence — and the vendor's own privacy documents must never contain "we
  may collect biometric data" boilerplate (vague collection language was treated as pleading
  ammunition in *G.T.*).

### 12.4 Retention and deletion

- **VP-130.** Default retention: a profile unused (no match, no exemplar added, no confirmation)
  for **12 months** expires automatically — exemplar ciphertexts destroyed, profile row kept as a
  tombstoned name for 30 days for user visibility, then removed. 12 months is the CUBI outer
  bound; the user may shorten it per profile, never lengthen beyond 12 (VP-070 CHECK constraint).
  Expiry runs as a maintenance job and its deletions are journalled (ADR-0072).
- **VP-131.** Release gate: the retention policy text (BIPA §15(a)'s publicly available policy)
  ships in `docs/public/`, and a privacy-counsel review of §12 is a named release-evidence item —
  the feature does not reach a public build without it.

### 12.5 Encryption at rest

- **VP-132.** As VP-071: per-feature AEAD key in the OS secret store; `embedding_cipher` is the
  only at-rest form; destroying the key is part of "forget everyone" (cryptographic erasure —
  rendering ciphertexts permanently undecryptable by destroying the key). Honest limits apply and
  are stated where users see them: it protects data at rest, not data in use (debugger/malware in
  the user session reads process memory — `docs/threat-model/full-library-encryption.md`); an
  unavailable secret store is a locked state (ADR-0014), so voice features refuse to run rather
  than fall back to plaintext.

### 12.6 Forbidden adjacent capabilities

- **VP-133.** The codebase must not grow: emotion/sentiment-per-speaker, demographic inference
  from voice, voice cloning/synthesis from exemplars, continuous/ambient identification, or any
  API that returns raw embeddings to callers (including the §14.5/ADR-0403 automation bridge —
  the automation surface gets suggestion/confirm verbs at most, never embedding access). Each of
  these is individually a legal regime-changer (EU AI Act Art. 5/Annex III, CCPA ADMT, Art. 50(3))
  and collectively mission creep this product rejects.

## 13. Threat model additions

New rows land in `docs/threat-model/traceability.json` as `planned` (control not present yet;
owner names the future module; no invented test names), restated in `open-findings.md`, in the
existing traceability style:

| Row | Threat | Planned control | Owner (future) |
|---|---|---|---|
| TM-044 | Voice embedding leaves the device through any egress path (cloud dispatch, model download, telemetry, diagnostics, notification payload) | Unconsentable `PayloadClass::VoiceBiometric` refused by `OutboundPolicy::authorize` in every mode; egress watch test extended to the enrol/match flow; embedding-shaped redaction in diagnostics | `crates/security/src/outbound.rs` |
| TM-045 | Embedding inversion / re-identification from a stolen profile store (research shows speech characteristics are partially recoverable from embeddings) | AEAD encryption at rest keyed from the OS secret store; no plaintext fallback (locked state); embeddings excluded from exports by default; honest docs: encryption protects at rest, not in use | `crates/app-core/src/voice_profiles/store.rs` |
| TM-046 | Profile export leaks voiceprints outside the user's control (plaintext backup, export format, archive restore on another machine) | Voice namespace excluded from exports and plaintext backups by construction; encrypted-backup-only opt-in; archive secret scanner refuses embedding blobs outside the voice namespace; restore re-runs the consent gate | `crates/export/src/archive/voiceprofiles.rs` |
| TM-047 | Poisoning: the wrong person enrolled into a profile, mislabelling them in every later recording | Enrolment minima and clean-speech checks; suggestions never auto-assign; per-profile provenance list with split-by-provenance repair; rejection feedback; the UI never presents a match as identity | `crates/app-core/src/voice_profiles/enroll.rs` |
| TM-048 | Cross-library correlation: two libraries' profile stores combined to track one voice across contexts | No sync, no stable cross-library identifier, per-library AEAD key, export-off-by-default; documented as a residual risk that two *plaintext*-era exports could be compared — which is why plaintext voice export is unrepresentable | `crates/app-core/src/voice_profiles/mod.rs` |

- **VP-140.** These rows follow the repository's traceability rules: `planned` status carries no
  verification test names; when the EPIC implements a control, the row flips with named Rust
  tests, checked by `scripts/check-threat-model.py` and
  `crates/security/tests/threat_model_traceability.rs`.

## 14. Performance budgets and resource limits

Engineering targets in the §17.5 sense — set now, measured before any claim ships:

- **VP-150.** End-to-end (diarize + embed + match) ≤ 0.25× realtime single-threaded on the
  reference machines in `docs/hardware/` (i.e. a 60-minute recording adds ≤ 15 minutes CPU-bound
  work), and ≤ 0.1× with 4 threads. Plausibility, not proof: research-pass measurements put
  segmentation ≈ 1.9 ms/audio-second and ResNet34-LM embedding ≈ 12.7 ms/audio-second
  single-threaded on one Apple M5 [MEASURED-RESEARCH]; the only published end-to-end figure for a
  comparable stack is RTF 0.119 on unspecified Apple hardware [VERIFIED: sherpa-onnx docs].
  **No Windows x64 figure exists anywhere — measuring it is a release gate** (VP-045).
- **VP-151.** Peak additional RSS during the job ≤ 500 MB (models ≈ 33 MB + ONNX runtime +
  windows); the job streams PCM via the existing `ScratchPcm`/chunking (`crates/audio`), never
  holding a full decoded recording in memory (base-spec §17.5 bounded-memory rule).
- **VP-152.** Matching cost is trivial by design (N profiles × E exemplars × 256-dim cosine);
  with the VP-130 retention cap and realistic libraries (≤ 200 profiles), matching stays < 1 ms
  per cluster on any supported CPU; the binding constraints are embedding and segmentation.
- **VP-153.** UI: suggestion rendering joins the §17.5 10k-recordings/100k-segments scale tests;
  the review queue virtualises like the transcript lists.
- **VP-154.** The job yields under `ResourceScheduler` pressure and never degrades interactive
  latency beyond the §17.5 100 ms budget — enforced by the existing kittest frame-budget tests
  extended to a library with pending suggestions.

## 15. Diagnostics and observability

- **VP-160.** Job steps report stage, window counts and timings — never scores attached to names.
  The quality-metrics CLI (`crates/ai-diarization/src/bin/quality_metrics.rs`) grows EER/DET
  reporting alongside DER so release evidence can cite harness numbers (§17.2) from a committed
  artifact (`docs/benchmarks/speaker-match-<model>-eer.json`, mirroring ADR-0400's
  `diarization-<provider>-der.json` convention).

## 16. Accuracy, fairness, and honest UX

- **VP-170.** Measured fact: speaker-verification accuracy is demographically skewed. The FAccT '22
  study of VoxCeleb-trained systems found error rates substantially worse for female and non-US
  speakers (training data 61% male, 29% US; at the overall operating point, some subgroups' false
  positive rate reached 13× the average), and found that *smaller* models — exactly what a CPU
  desktop app ships — can widen the gap [VERIFIED: Hutiri & Ding, arxiv.org/abs/2201.09486]. There
  is no NIST demographic-differentials report for speaker recognition (the published ones cover
  face recognition) — nobody should cite one [VERIFIED: absence checked against NIST FRVT reports].
- **VP-171.** Consequences, all mandatory: no auto-assignment at any score (VP-060); per-profile
  calibration from the user's own confirmations (VP-063) as the only personalisation — the app
  never collects demographic labels to "correct" bias; no single advertised accuracy number in UI
  or marketing; the §17 harness reports per-condition (duration, noise, codec) error bars, and the
  docs state the demographic caveat in plain language.
- **VP-172.** UX wording rules, enforced by kittest string assertions: "sounds like", "suggestion",
  "match score" — never "identified", "verified", "recognised as", "is". The speaker display
  qualifier stays in one place (`plaudon_domain::speaker::display`), extended with a
  "named via voice match (confirmed by you)" provenance qualifier. Confidence is shown as both a
  band and the number, with a tooltip explaining that scores are not probabilities of identity.
- **VP-173.** `docs/public/` accuracy language follows the §23 reference-accuracy discipline: the
  published claim is the committed harness artifact, not a model card's number; model-card numbers
  are quoted only with their protocol (VoxCeleb1-O full-length EER is not meeting-audio accuracy;
  oracle-VAD DER like TitaNet's AMI 1.73% is never compared to fully-automatic DER like
  community-1's AMI 17.0% — a documented spec-writing trap [VERIFIED: both model cards]).

## 17. Test plan

Strictly the house stack: egui_kittest for UI (`crates/ui-testkit`, no browser, no other UI
runner), hermetic Rust tests everywhere else, no real external service, nothing gated on network
in CI. Sentence-style test names as throughout the repo.

### 17.1 Unit and property tests

- Embedding windowing/aggregation invariants (property tests, `proptest` as used in
  `property_invariants.rs`): window plans cover exactly the clean single-speaker spans; aggregation
  is permutation-invariant; VP-065 minima refuse short inputs.
- Matching: cosine + AS-Norm known-answer tests against committed fixture vectors; threshold-band
  partitioning is total and stable; per-profile offset bounds respected; model-version mismatch
  refuses to score.
- Open-set: with zero profiles the match stage is a no-op; unknown clusters stay unknown
  (VP-061); the `UNIQUE` suggestion constraint makes re-runs idempotent.
- Persistence: schema-contract tests for the five tables; AEAD round-trip; key-absent = locked
  (not plaintext); migration 0900 up-from-0800 on a populated fixture library; deletion cascades
  (enrolment vs person, VP-080) and `voice_profile_events` survivability; merge repoints
  assignments/enrolments and is journalled.
- Consent gate and mode separation: disclosure-version bump re-prompts; no embed call is reachable
  while the gate is unpassed **or while biometric matching is off** (a scripted embedder that
  panics on call, driven through the full engine in both states — the VP-011/VP-215 structural
  test); roster CRUD and assignment work end-to-end with matching off.

### 17.2 Evaluation harness (DER/EER), fixtures and their licences

- **VP-180.** `plaudon-eval` grows a speaker-recognition harness: trial lists over labelled
  fixtures → DET curve, EER, and the two operating points (VP-062), plus DER via the existing
  `diarization_error_rate` (`crates/ai-diarization/src/der.rs`). Reports are committed JSON
  (`docs/benchmarks/`), schema'd like `extraction-eval-report.v1.schema.json`.
- **VP-181.** **Fixture licensing is a hard rule.** Committed-to-repo fixtures are only:
  (a) **synthetic multi-speaker audio built from LibriSpeech** (CC-BY-4.0, derived from
  public-domain LibriVox, so no third-party copyright underneath
  [VERIFIED: openslr.org/12 info.txt]) — N speakers from `test-clean`, concatenated/cross-faded on
  a fixed seed with controlled overlap, which yields exact ground-truth RTTM deterministically;
  (b) optionally MUSAN noise variants (CC-BY-4.0 [VERIFIED: openslr.org/17]); and (c) fully
  TTS-generated fixtures (Kokoro-82M, Apache-2.0 [VERIFIED: HF API]) for a zero-human-voices smoke
  tier. Attribution rows land in the third-party register.
- **VP-182.** **VoxCeleb is not redistributable and never enters the repo or CI**: its access is
  form-gated and wrapped in "for research purposes", and the underlying YouTube copyright was never
  licensed to VGG [VERIFIED: mm.kaist.ac.kr/datasets/voxceleb licence page]. The same wrapper
  applies to VoxConverse (direct download, same caveat) — developer-local evaluation only.
  **AISHELL-4 is CC-BY-**SA**-4.0: ShareAlike contamination risk — never vendored, never mixed
  into derived fixtures** [VERIFIED: openslr.org/111]. The sherpa-onnx release tags' test wavs
  carry no stated licence — not vendored. Corpora that *are* clean for download-on-demand
  accuracy gates (not in CI): **AMI** (CC-BY-4.0, 100 h of real meetings — the genuine target
  domain) and **ICSI** (CC-BY-4.0, hard set, stress tier)
  [VERIFIED: groups.inf.ed.ac.uk/ami licence pages].
- **VP-183.** Golden tests: committed fixture → committed expected suggestions (ids, bands —
  scores asserted within tolerance), pinned to model sha256, so a silent model or feature-extraction
  change fails loudly. The deterministic fake provider (ADR-0400) plus a scripted embedder keeps
  the full pipeline testable without any model file; model-dependent tests are `#[ignore]`d behind
  the same opt-in pattern as hardware suites.
- **VP-184.** Cross-device trials: the harness re-encodes fixture speakers through codec/EQ chains
  (Opus low-bitrate via `crates/audio/src/opus.rs`, telephone band-pass, room IR) and reports EER
  per condition — the §6.5 variance story is measured, not asserted.

*Implementation status (2026-10-12, P2.4):* the harness exists in `crates/eval/src/speaker/`
(`plaudon-eval speaker-match`), with the licence gate (VP-181/VP-182) enforced in code, the
DET/EER/operating-point maths, calibration at FA ≤ 1%, per-condition reporting, golden suggestions
pinned to the model sha256, DER via `crates/ai-diarization::der`, and the committed artifact plus
its schema (`schemas/speaker-match-report.v1.schema.json`). It runs hermetically in CI on scripted
embeddings and on real ones as soon as a model pack supplies `eval-embeddings.json`. **No
checkpoint, no audio tier and no platform runtime figure has been measured**, so the committed
artifact is explicitly not release evidence and the shipped thresholds remain underived — see
[docs/benchmarks/speaker-match-eval.md](../benchmarks/speaker-match-eval.md).

### 17.3 Fuzzing

- **VP-185.** New cargo-fuzz targets in `fuzz/` with stable-replay twins in
  `crates/security/tests/fuzz_corpus_replay.rs` (the house pattern): `fuzz_embedding_blob`
  (ciphertext/nonce/dim parsing from the DB — hostile lengths, truncation, dim mismatch, NaN/Inf
  smuggling in decrypted plaintext), `fuzz_voiceprofile_archive_doc` (the `archive-voiceprofile/1`
  document against the restorer), and ONNX-output shape handling (hostile tensor shapes from the
  out-of-process helper). Corpora committed under `tests/fixtures/fuzz-corpus/`.

### 17.4 UI (egui_kittest)

- Person picker (VP-210): searchable filtering over names and aliases; "Create new person…"
  creates and assigns in one step; "Unknown/none" clears; duplicate-name inline warning (VP-214);
  full keyboard path (Enter/Space opens, type-to-filter, arrows, Enter, Escape); AccessKit
  ComboBox role with announced filtered count.
- Suggestion panel: confirm dispatches `confirm_speaker_suggestion` with the right ids; reject
  never renames; reject-then-pick records `corrected_person_id` and assigns (VP-216); wording
  assertions for VP-172 ("sounds like", never "identified"); the qualifier renders from the
  single domain function.
- Assignment scope and undo: cluster assignment is one undo step and restores segment exceptions
  (VP-211/VP-212); per-segment exception warning; nothing dispatches against a non-open recording.
- Enrolment: gate text must be scrolled/acknowledged; minima meter; refusal states (not enough
  clean speech) announced via `Role::Status`.
- People screen: rename obeys the 120-char rule (same as `speaker_identity.rs` tests); rename
  dialog shows the affected-recordings count (VP-213a); merge shows the disclosure and repoints
  (VP-213c); delete-person leaves name snapshots; delete-enrolment keeps the person;
  forget-everyone requires typed confirmation and keeps the roster (VP-095); dedupe groups are
  suggestions only; every control named (a11y sweep).
- Manual-only parity: the full-shell scenario runs once with matching on and once off; with it
  off, pickers/bulk-assign/undo behave identically and no suggestion UI exists (VP-215).
- Full-shell (`PlaudonShell`, `apps/plaudon-egui/tests/support`): enrol → import second fixture →
  job runs with scripted providers → suggestion appears → confirm → transcript chips update in
  the same frame (the T12 acceptance), with the egress watch asserting zero outbound attempts.

### 17.5 What is explicitly not tested against

No live model downloads, no Hugging Face, no cloud STT, no real voiceprints of real people in the
repository (synthetic/LibriSpeech-derived only — and LibriSpeech voices are real readers, which is
why the consent-free fixture tier is TTS-only and LibriSpeech fixtures stay within their CC-BY-4.0
research-appropriate use as test material, attributed in NOTICE).

## 18. Rollout plan, feature flag, migration

- **VP-190.** Cargo feature `voice-profiles` on `plaudon-egui` → `plaudon-app` (the
  `local-whisper` pattern), **off by default**; without it the commands are absent from
  `command_surface!`, the screens unregistered, and migration 0900 still applies (schema is
  harmless when dormant — same posture as the 0400 diarization tables, which shipped before any
  real provider).
- **VP-191.** Even with the build feature on, the runtime toggle is off until the §12.2 gate is
  passed. Two independent switches, both honest in `get_capabilities` (an absent engine is
  "unavailable in this build", like whisper today).
- **VP-192.** Order of shipping (phases, §19): wire-support + schema first (useful alone — closes
  T12), then matching behind the flag with the fake provider for tests, then the model-pack tier
  and measured enablement per ADR-0400 conditions, then docs/public updates and the counsel gate
  (VP-131), then default-on *for the build flag* (never for the runtime toggle, which stays
  opt-in forever).
- **VP-193.** Rollback: the feature flag can be dropped from a release without data loss (tables
  dormant); "forget everyone" is the user-level rollback; migrations are forward-only per house
  rules, so 0900 ships with the first flagged build, not before the schema is final.

## 19. Phased task breakdown (EPIC P)

Small, independently mergeable, each with acceptance criteria; ordered by the dependency graph
below. Issue ids follow the house convention (EPIC P, ≤ 2 phases, ≤ 5 tasks per phase).

### Phase 1 — wire support, schema, pipeline skeleton

| Task | Deliverable | Acceptance |
|---|---|---|
| P1.1 | T12 wire leg: `speaker_id` on `TranscriptSegmentDto`, `set_segment_speaker`, schemas, guard-test updates | per-segment reassignment persists and survives reload; kittest chip test asserts dispatch; T12 parity row updated; `COMMAND_NAMES` guard green |
| P1.2 | Migration 0900 (`people`, `person_voice_enrollments`, `voice_exemplars`, `speaker_suggestions`, `voice_profile_events`, `speakers.person_id`) + repositories + AEAD store + `SecretScope::VoiceProfileKey` | schema-contract tests; AEAD round-trip; locked-store = refusal; migration from populated 0800 library; merge/delete cascade semantics (VP-080, VP-213) |
| P1.3 | `JobKind::SpeakerMatch` executor skeleton with stages Diarize→Embed→Match over scripted providers (fake diarization, scripted embedder) | checkpoint/resume/cancel/idempotency tests; chaos kill/resume; coverage-policy transitions entries |
| P1.4 | Matching core: cosine + AS-Norm + bands + open-set rules + per-profile calibration bounds incl. negative evidence (pure `app-core`, no model) | known-answer + property tests; polarity encoded in types; version-mismatch refusal; rejected/corrected tuples feed calibration (VP-216) |
| P1.5 | Roster + biometric commands and DTOs (`people` + `voice_profiles` modules: list/create/update/merge/delete people, assign_cluster_person, enrol/split/delete enrolment, suggestions confirm/reject-with-correction, forget-all), consent-gate state machine | schema files; no-embedding-in-DTO response scan; consent-gate + matching-off unreachable-embed tests (VP-011); roster CRUD works with the voice feature flag compiled out |

### Phase 2 — UI, egress hardening, models, evidence

| Task | Deliverable | Acceptance |
|---|---|---|
| P2.1 | People screen (new `Destination::People`) + person picker everywhere + speaker-panel suggestions + reject-then-pick + bulk review + enrolment flow + Settings block | kittest suites incl. picker/undo/propagation-disclosure/manual-parity, wording and a11y sweeps; full-shell enrol→match→confirm scenario in both modes |
| P2.2 | `PayloadClass::VoiceBiometric` (unconsentable) + egress watch scenario + redaction + fuzz targets | outbound refusal in every mode; local_only_egress extended; corpus replay green |
| P2.3 | Export/backup/deletion integration: voice namespace (encrypted-only), deletion disclosures, forget-everyone, retention expiry job | plaintext-with-voice unrepresentable; restore re-gates consent; journalled deletes; archive fuzz doc target |
| P2.4 | Evaluation harness + LibriSpeech/TTS fixture generator + EER/DER reports + threshold calibration | committed benchmark JSON; golden tests pinned to model sha256; cross-device conditions reported |
| P2.5 | Model-pack tier for the two ONNX artifacts, licence register + NOTICE rows, threat-model row flips, `docs/public` updates, counsel-review evidence item | register tests green; ADR-0400-style conditions documented per artifact; TM-044..048 flipped or re-scoped with named tests |
| P2.6 | The two reads P2.1 had no command for: `list_voice_exemplars` (ids, provenance, durations — never vectors) behind the split-by-provenance dialog (VP-091), and `get_cluster_speech` behind the enrolment flow's clean-speech meter (VP-094) | schemas + response scan proving no vector; kittest for the split dialog (ticking, "moving everything is a merge", target attestation) and for the meter (unmeasured ≠ zero, under-minimum disables the enrolment) |

Dependency graph (arrows = "needs"):

```text
P1.1 ──────────────┐
P1.2 ─→ P1.3 ─→ P1.5 ─→ P2.1
          │        │  └─→ P2.3
P1.4 ─────┘        └────→ P2.2
P1.2 ──────────────────→ P2.3
P1.4 ─→ P2.4 ─→ P2.5   (P2.5 also needs P2.2, P2.3)
```

P1.1 is independently valuable (closes T12) and has no voice dependency; P1.2/P1.4 can proceed in
parallel; nothing user-visible ships before P2.1 + P2.2 land together.

## 20. Open questions

1. **Counsel questions (block release, not development):** the TX/WA transfer of the
   *G.T. v. Samsung* control analysis; whether a WA-RCW-19.375 embedding falls in the
   "derived from a recording" carve-out; BIPA exposure of purely personal individual users; the
   EU AI Act "remote" reading for later-meeting recognition of a consented enrollee (all CONTESTED,
   §12).
2. **Model gate:** will ResNet34-LM's pyannote-calibrated threshold transfer to meeting audio on
   the §17 harness, or does V1 ship CAM++-LM with harness-derived thresholds (VP-042)?
3. **Windows x64 CPU budget:** no published figures exist for any candidate stack; if reference
   hardware misses VP-150, is the fallback fewer threads + background-only, or feature
   unavailability on low-end hardware (the §3.1 "measured runtime capability" rule)?
4. **ADR-0015 wiring:** the sandbox has no callers today (open finding F-012). Does this EPIC land
   the first `SandboxedHelper` integration for ONNX inference, or does it inherit the F-012
   remediation? (This spec assumes the former; it must not ship a new in-process untrusted-input
   surface.)
5. **Enrolment from imported vendor transcripts** (recordings with vendor speaker labels): allowed
   as provenance, or restricted to app-diarized recordings in V1? (Spec assumption: restricted.)
6. **Suggestion expiry:** do pending suggestions expire with the enrolment retention clock or
   independently at 90 days? (Spec assumption: with the recording's lifetime, bounded by the
   enrolment's.)
7. **Roster in recording exports:** VP-077 decides display-name text only (no contacts/aliases).
   Should an explicit "contact card" export exist for a person (vCard)? (Spec assumption: not in
   V1; it is ordinary data and could ship later without touching the biometric tier.)

## 21. Risks

| Risk | Severity | Mitigation / decision |
|---|---|---|
| Legal exposure lands on users who skip attendee consent | High | Off-by-default, three-level gate, consent templates and exportable records, retention auto-expiry, blunt in-product responsibility language (§12.2); counsel-reviewed docs are a release gate |
| Accuracy on real meeting audio far below VoxCeleb headline numbers (far-field, overlap, short turns) | High | Honest targets (AMI-class DER ~17–20%); AMI download-on-demand gate; no shipped claim without the committed harness artifact; suggestion-only UX degrades gracefully |
| Demographic error disparity harms specific users | High | VP-170/171: no auto-assignment, per-profile calibration, per-condition reporting, plain-language caveat |
| ONNX runtime supply chain (`ort` is a release candidate; sherpa-onnx is a large C++ dep) | Medium | Path A/B fallback pair; `deny.toml` + register review; out-of-process inference (ADR-0015) contains parser risk |
| Model licence theories shift (dataset-taint argument against VoxCeleb-trained weights) | Medium | CC-BY-4.0 attribution posture (the stricter claim); 3D-Speaker Apache-2.0 alternate; user-download tier as escape hatch |
| Threshold calibration does not transfer; users drown in wrong suggestions | Medium | Two-threshold bands, false-accept-biased operating point, per-profile adaptation, harness-derived defaults (VP-062) |
| Feature creep toward surveillance (bulk retroactive scans, ambient ID, attribute inference) | Medium | VP-005/007/008/133 are normative prohibitions; ADR-0082 records the decision so the pressure has to overturn an ADR, not a sentence |
| Key loss (OS keychain reset) destroys profiles | Low | That is the designed failure mode (fail toward privacy); re-enrolment path; documented honestly |

## 22. Unverified-claims register

Claims this spec relies on that have **no primary source** and must be verified (or struck) by the
EPIC before anything ships:

1. CPU speed of every candidate on **Windows x64**: no published figures at all; research
   measurements cover one Apple M5 machine only (and are not release evidence).
2. SpeechBrain ECAPA model file sizes and true embedding dimension (not inspected).
3. 3D-Speaker vendor RTF 0.013 — CPU model unspecified by the vendor.
4. TitaNet-S accuracy — NVIDIA's NGC page mislabels TitaNet-L prose under the TitaNet-S heading.
5. speakrs CPU-mode (non-CoreML) DER/RTF — unpublished; its model mirror carries no licence tag.
6. Resemblyzer EER — no published figure anywhere.
7. Centroid-vs-multi-exemplar superiority for this exact stack (VP-064) — standard practice,
   validated locally by the harness, no citable benchmark.
8. The 1.0 s hard floor for any embedding (VP-065) — derived, no single source.
9. Legal CONTESTED items of §12 and §20(1) — flagged for counsel, not engineering.
10. `voice_activity_detector` crate licence ("non-standard" on crates.io) — audit before use, if
    used at all.
11. The claim that per-profile threshold adaptation meaningfully reduces demographic disparity —
    plausible mechanism, no published measurement for this stack; the harness must report it.

Verification duties for the measured-but-single-machine numbers ([MEASURED-RESEARCH]) are part of
VP-045/VP-150, on reference hardware, both platforms, committed to `docs/benchmarks/`.

### 22.1 Base-spec section references: verification status

Version 1.0 of this document was written while the original
`PLAUD_BYOK_Desktop_SPEC_v1.3_Consensus.md` was unreadable (file permissions); roughly half of it
was recovered verbatim from prior read sessions and the rest of the §-references were grounded
only in repository documents restating them. **For version 1.1 the original became readable and
every §-reference in this document and in ADR-0080/0081/0082 was re-checked against the source
text (1703 lines).** Outcome:

- All previously indirect references held up: §1.4 (user groups; includes the no-compliance-claim
  sentence quoted by `docs/threat-model/full-library-encryption.md`), §1.7, §3.1 (diarization is
  P1-conditional, "the core product does not wait on … diarization"), §8.7 (quoted in §12 above),
  §12 (opt-in model manager, "never silently download multi-gigabyte weights"), §13.5–§13.8
  (backup/encryption posture as restated by ADR-0070/0071/0072/0402), §14.1–§14.7 (job engine as
  restated by ADR-0030), §15.1/§15.2/§15.4, §16.1–§16.6 (threat table column shape and the 25
  threat rows match `traceability.json`'s `spec_threats`), §17, §18, §22, §23, §24. No reference
  in this document needed correction.
- One divergence worth recording, in the repository rather than in this document: base-spec §15.2
  *proposes* a command named `run_transcript_tool`, while the shipped guard
  (`no_generic_shell_http_or_filesystem_command_is_published`, `crates/app/src/command.rs`)
  forbids the `run_` prefix and the shipped surface names it differently. The guard, not the
  proposal table, governs new command names (VP-031 and §8 follow the guard).
- Base-spec §21 lists ADR-001 through ADR-022 in prose; the repository's `docs/adr/README.md`
  reserves numbers 0001–0012 for the "§21 initial set" and its test checks exactly those twelve.
  This document's 0080–0089 range claim is unaffected.
- The base spec contains no biometric, voiceprint or privacy-statute treatment (checked by
  search); §12 of this document is additive, constrained by §1.7 and §8.7, and contradicts
  nothing in the source text.
