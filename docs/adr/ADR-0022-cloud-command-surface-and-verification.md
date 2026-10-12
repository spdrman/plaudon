# ADR-0022: The cloud command surface, and what `Verified` is allowed to mean

- **Status:** accepted
- **Date:** 2026-10-12
- **Deciders:** EPIC G (OpenRouter BYOK)
- **Spec:** §10.1–§10.8, §8.6/§8.6A, §15.1–§15.4, §16.3, App. A.3

## Context

EPIC G adds the first cost-bearing, content-exporting feature in the product.
Three decisions had to be made explicitly rather than left to the shape of the
code:

1. **Which commands JavaScript may call.** ADR-0020 fixed the Phase-2 surface at
   16 commands and required later EPICs to take further commands from the §15.2
   table. Cloud AI needs five of them (`list_models`, `start_processing`,
   `get_usage`, `grant_cloud_consent`, `revoke_cloud_consent`) plus write paths
   for two settings the spec mandates but the §15.2 table has no command for:
   the editable spending cap (§4.4(4), §10.4) and the per-task default model /
   per-recording override (§10.2).
2. **What `Verified` means.** §10.1 and §10.2 say a model is `Verified` only by
   "passing our actual capability smoke test on that API contract, not
   appearing in a model search response". The A1.2 evidence is that the public
   catalogue reports audio as an *input modality* and nothing else: it names no
   endpoint family, no timestamp support and no retention policy. So the
   catalogue can never promote a model, and our own hermetic fixtures prove our
   decoder, not the provider's behaviour.
3. **Where consent, the ledger and the wire client meet.** Only one place may
   answer "may this leave, what will it cost, what did it cost".

## Decision

### 1. Seven commands, two of them additions to §15.2

From the §15.2 table: `list_models`, `start_processing`, `get_usage`,
`grant_cloud_consent`, `revoke_cloud_consent`.

Two additions, for state the spec requires and the table has no write path for:

- `set_cloud_budget(per_job_limit, monthly_limit, currency) -> CloudBudget`
  — §4.4(4) requires an *editable* spending cap and §10.4 requires per-job and
  monthly local budgets.
- `set_model_preference(stage, scope, engine, model_id) -> ModelPreference`
  — §10.2 requires independent per-task defaults and per-recording overrides.

Neither widens the trust boundary: both accept enumerated values and decimal
strings only — no paths, no URLs, no free-form commands — and both are
validated in Rust before reaching a port. `Capabilities` gains
`cloud_consents` so grants are revokeable from settings without an eighth
command.

### 2. Verification is evidence-gated, and the gate is a type

`ModelRegistry` accepts three inputs with strictly different authority: the
pinned allowlist may contribute a *candidate* family and a fixture id; the live
catalogue may contribute display metadata, prices and the single downgrade
"declares no audio modality at all"; only an observed exchange
(`ProbeEvidence::ObservedExchange`) with a resolved provider route may set
`Verification::Verified`. `ModelCard::is_selectable()` is false for everything
else, and the two request builders refuse a family without a builder, so an
unverified model has no dispatch path rather than a discouraged one.

The pinned allowlist therefore ships with **no verified models**. A user
verifies a model by running the capability smoke test, which is a real,
consented, cost-bearing request. The migration enforces the same rule in SQL:
`model_capability_records` rejects an `observed_exchange` row without a fixture
id and a validation timestamp.

### 3. One dispatch path, in a fixed order

`plaudon-ai-openrouter` owns the wire contracts and no state. Every dispatch
goes: credential → `CloudDispatchAuthority` (egress gate, then scoped consent,
then retention posture for the exact route *and content type*) → **committed
ledger row** → transmit → settle. The ledger row is obtained through
`AttemptLedger::open`, which returns the `AttemptId` the client needs, so a code
path that skips the ledger does not compile. `plaudon-app-core::cloud` is the
only crate that implements both the authority and the ledger.

## Consequences

- The frontend contract, the JSON schemas and the Rust DTOs enumerate the same
  23 commands; drift fails CI on both sides.
- Shipping a verified model out of the box would require a captured real
  exchange in the repository; until then the first run of a cloud model costs
  the user one smoke-test request, which the consent sheet discloses.
- Local budgets are guardrails: the UI states, in fixed text, that they cannot
  prevent charges made elsewhere with the same key and recommends an
  OpenRouter-side key limit.
- A retention posture is resolved per route **and** content type, so enabling
  "require documented zero retention" currently denies every audio upload: the
  endpoints metadata discloses no retention field. That is the fail-closed
  direction §10.5 requires, and it is visible rather than silent.

## Rejected alternatives

- **Ship a pre-verified model list.** Rejected: §10.1 forbids treating catalogue
  presence as proof, and a verification claim we cannot evidence is exactly the
  "optimistic success message" §3 rules out. The honest cost is one smoke-test
  request per model.
- **Let the catalogue set the request family when `architecture.input_modalities`
  contains `audio`.** Rejected: the A1.2 capture shows the catalogue never says
  which endpoint contract a model implements, so this would route chat-only
  models at the transcription endpoint and vice versa (§8.6).
- **Treat an absent retention field as the provider's default policy.**
  Rejected: §10.8 requires the policy to resolve for the exact endpoint and
  route; "nothing was published" is `Unknown`, and `Unknown` satisfies no
  requirement in either direction.
- **Automatically retry a request whose response was lost.** Rejected by §10.7:
  once bytes have been transmitted the provider may have billed, so the attempt
  becomes `OutcomeUnknown` and needs explicit user approval to be re-sent.
- **One generic `cloud_request` command with a model id and a payload.**
  Rejected: §15.1 forbids a generic HTTP proxy from JavaScript, and a generic
  command cannot be consent-scoped per purpose and content type.
- **Fold the budget and model-preference writes into `complete_onboarding`.**
  Rejected: onboarding completes once, while both settings are edited
  repeatedly, and overloading it would make a settings change re-run the
  first-run flow.
