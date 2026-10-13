# ADR-0080: Voice profiles are biometric data — off by default, local-only, user-deletable to the key

- **Status:** Proposed; amended 2026-10-12 for the master speaker list (spec v1.1): the rules
  below govern the *biometric tier*; the name-only roster is explicitly outside them (rule 5)
- **Date:** 2026-10-12
- **Spec:** VOICE_PROFILES_SPEC §1, §7, §9A, §11, §12 (incl. §12.2A), §13 (VP-002/003, VP-010/011, VP-071, VP-095, VP-110, VP-120–133, VP-210–217); base spec §1.7, §8.7, §16.3, §16.4, §13.5, §4.6

## Context

The voice-profiles feature ([docs/specs/VOICE_PROFILES_SPEC.md](../specs/VOICE_PROFILES_SPEC.md))
stores speaker embeddings so that attendees enrolled once can be suggested as labels in later
recordings. A speaker embedding used to recognise a person across recordings is a voiceprint:
special-category biometric data under GDPR Art. 9, an enumerated biometric identifier under
Illinois BIPA, Texas CUBI and Washington's statutes, and sensitive personal information under
CCPA/CPRA. The people enrolled are meeting attendees — third parties, not the consenting app user.
BIPA carries a private right of action and a certified ~1.2M-member class exists on the
voice-enrolment fact pattern (*Wilcosky v. Amazon*); the Texas AG has settled biometric actions for
over a billion dollars. The one strong vendor-side shelter in current case law (*G.T. v. Samsung*,
7th Cir. 2026, with *Barnett v. Apple* and *Bhavilai v. Microsoft*) protects a vendor whose
software keeps biometric data solely on the user's device, inaccessible to and never transmitted to
the vendor, with user-controlled disable and delete.

## Decision

Voice profiles are governed by five structural rules, none of them waivable by settings:

1. **Off by default, consent-gated on.** No embedding is computed until the user enables the
   feature, passes a versioned activation disclosure, and — per enrolment — attests that the named
   person agreed. Disclosure version and attestation are journalled (`voice_profile_events`).
2. **Local-only by construction.** Embeddings get their own egress payload class
   (`PayloadClass::VoiceBiometric`) that `OutboundPolicy::authorize` refuses in every network mode
   for every subsystem. There is no consent scope that could permit it: the class is unconsentable,
   unlike `TranscriptText`/`AudioBytes`. Voice data is likewise excluded from notifications,
   error envelopes and diagnostics.
3. **Encrypted at rest, deletable to the key.** Embedding vectors exist at rest only as
   XChaCha20-Poly1305 ciphertext under a feature-scoped key in the OS secret store (new
   `SecretScope::VoiceProfileKey`; ADR-0014's no-plaintext-fallback rule applies — an unavailable
   store is a locked feature). Per-profile delete destroys the profile's rows; "forget everyone"
   additionally destroys the key, rendering every remaining ciphertext — including any inside
   encrypted backups — permanently undecryptable. Retention defaults to 12 months unused
   (the Texas CUBI outer bound), shortenable, never extendable.
4. **Exports exclude biometric data by default; plaintext voice export is unrepresentable.** The
   `.recording-archive` voice namespace exists only inside password-encrypted backups (ADR-0071
   stack), and restore re-runs the consent gate.
5. **The name-only roster is not biometric data, and must never become it by drift.** The master
   speaker list (`people`: display name, aliases, plain email/organisation) and manual assignments
   are ordinary personal data: no statute's biometric definition reaches a typed name (GDPR
   Art. 4(14) requires data "resulting from specific technical processing" of bodily
   characteristics; BIPA/CUBI/RCW 19.375 enumerate closed identifier classes; CCPA requires
   template-extractable material — spec §12.2A). Manual assignment therefore works with biometric
   matching off, carries no consent gate, no retention clock and no AEAD requirement — and the
   manual code path is structurally unable to call embedding code (tested). The limits are stated,
   not hidden: the roster is still ordinary personal data under GDPR; the audio it points at could
   be enrolled later, which is why enrolment always runs its own gate and is never implied by an
   assignment; "forget everyone" destroys every enrolment and the key but keeps the roster, and
   says so.

The vendor-facing posture follows from the same rules: the vendor never receives, holds keys to,
or can access voiceprints, and the public privacy documents say so affirmatively (and never say
"we may collect biometric data").

## Rejected alternatives

- **Treat embeddings as ordinary derived data (like transcripts).** Rejected: every applicable
  regime singles voiceprints out from the recording they derive from; transcript-grade handling
  (plaintext at rest, exportable, consentable to cloud) would be the *Hazlitt v. Apple*
  counter-model and would forfeit the user's and the vendor's defensible position.
- **Full-library encryption instead of a feature-scoped key.** Rejected: ADR-0402 already decided
  against library-wide encryption (FileVault/BitLocker posture), and a library-wide key could not
  provide per-feature cryptographic erasure; a 33-byte-per-dim blob store is small enough that
  scoped AEAD costs nothing measurable.
- **Opt-out instead of opt-in, or a one-click global consent.** Rejected: GDPR Art. 9(2)(a)
  explicit consent, BIPA §15(b) written release and Washington MHMD's "not via general terms"
  standard all make bundled or default-on consent legally worthless; and §16.3 already established
  the scoped-consent discipline this follows.
- **Vendor-assisted sync/backup of profiles ("convenience").** Rejected: any vendor path to the
  data destroys the *G.T.* posture, makes the vendor a GDPR controller/processor, and adds a BIPA
  "possession" theory — the entire feature is only shippable because the vendor never touches it.

## Consequences

- The feature is strictly less convenient than competitors' cloud voice-ID: no cross-device
  profiles, no automatic back-catalogue scan, re-enrolment after key loss. These are accepted
  costs, stated in the docs.
- The egress gate grows an unconsentable payload class, which is new surface in
  `crates/security/src/outbound.rs` and must keep its refusal tests green forever; threat-model
  rows TM-044–TM-048 track those controls to implementation.
- Users carry the legal duty to obtain attendee consent; the product's job is to make that duty
  visible, dischargeable (templates, exportable consent records) and bounded (retention
  auto-expiry). This is stated bluntly in-product; it cannot be engineered away.
- A privacy-counsel review of the spec's §12 is a release-evidence item before the feature reaches
  a public build.
- **Implemented so far (P2.3):** the export/backup/deletion leg of this decision now exists.
  A recording export carries display-name text only; portable backups carry the name-only roster
  like any other library data and exclude the biometric tier unless the user opts in to an
  *encrypted* archive — a plaintext backup carrying voiceprints is unrepresentable, because the
  `voiceprofiles/` namespace can only be staged with a permit that `Protection::Plaintext` cannot
  produce. Restoring voice data re-arms the consent gate in the target library. "Forget everyone"
  destroys every enrolment, exemplar and suggestion, destroys the `SecretScope::VoiceProfileKey`
  entry (cryptographic erasure reaching ciphertexts inside backups already written) and switches
  the feature off, keeping the roster. Retention expiry runs as a journalled, resumable
  maintenance pass. Threat-model row TM-046 is `implemented`; TM-045 and TM-048 record what their
  remaining legs still owe.
- **Implemented so far (P2.2):** the egress leg exists. `PayloadClass::VoiceBiometric` is refused
  by `OutboundPolicy::decide` before the network mode, the grant, the subsystem, the purpose or
  the destination are considered, so the dedicated `DenyReason::VoiceBiometricNeverEgresses` is
  the answer in Local-only Mode and with the network permitted alike, and the class has no
  `ContentType`, so no consent grant can name it. Every effective-policy snapshot states the
  refusal in both modes. Diagnostics gained the embedding shapes (a named field, a long numeric
  vector, a long opaque blob) and a process-wide display-name registry fed from the roster, so a
  name, a vector and a sealed blob are scrubbed from the log sink, the crash hook and the
  notification reducer alike. Threat-model row TM-044 is `implemented`. What this does not claim:
  the gate governs traffic this process initiates, and redaction cannot recognise a name nobody
  registered.
