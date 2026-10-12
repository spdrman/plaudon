# ADR-0002: OpenRouter is the only cloud AI gateway in V1

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §1.2(3), §2.2, §10, §8.6

## Context

Supporting several cloud providers multiplies the contract surface: each has its own transcription endpoint shape, pricing unit, retention policy and error vocabulary. Spec §2.5B requires per-model verification of request family, codecs, timestamps, billing units, routing and retention before a model may be offered at all.

## Decision

V1 integrates exactly one cloud gateway, OpenRouter, with the user's own key. Models are offered only after a probe observes a real exchange and resolves a privacy route for that exact endpoint. There are two narrowly typed request builders, `multipart_stt` and `audio_chat`, and no generic fallback.

## Rejected alternatives

- **Direct integrations with several vendors.** multiplies the verification work in §2.5B by the number of vendors and leaves the user managing several keys.
- **A provider-abstraction layer with automatic fallback.** spec §10.6 forbids automatic fallback because it changes data handling and can incur charges without approval.
- **Shipping with a vendor-supplied key.** turns us into a reseller with a cost centre and removes the user's control over their own data routing.

## Consequences

- A model that we cannot classify stays **Not verified** and cannot be selected.
- Adding a second gateway later is a new adapter behind `SpeechToText`/`TextModel`, not a refactor.
- Users need their own OpenRouter credit; a consumer AI subscription is not API credit.
