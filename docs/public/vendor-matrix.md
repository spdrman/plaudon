# Vendor dependency matrix

Plaudon is an independent product. It is not produced by, affiliated with or
approved by PLAUD, OpenRouter or Apple, and it uses no vendor branding beyond
naming the services it can talk to.

Every status below uses the fixed evidence vocabulary: `Documented`,
`Observed in upstream code`, `Validated`, `Supported`, `Experimental`,
`Unsupported`. `Supported` appears only where an archived evidence record backs
it.

## Services and tools

| Dependency | What it is | Required? | Network | Status | Licensing |
|---|---|---|---|---|---|
| OpenRouter REST API | Optional cloud gateway for transcription and text models, using **your** API key | No | Yes, per authorized request | **Documented** | Commercial service; you buy prepaid API credit directly from OpenRouter. Not a subscription to this product, and this product takes no cut |
| official PLAUD Cloud CLI | PLAUD's own command-line tool, installed by you, used to list and download recordings from your PLAUD account | No | Yes, by that tool itself | **Unsupported** | Published by PLAUD; we neither bundle nor redistribute it, and its package listing declares no licence |
| Direct device access over Bluetooth or USB | Unofficial interoperability with a compatible PLAUD recorder, built from protocol observation, not from a vendor SDK | No | No | **Unsupported** | Our own implementation; see the compatibility matrix for the exact cells |
| Apple Speech and Foundation Models | On-device transcription and text generation on qualifying macOS hardware | No | No | **Documented** | Apple system frameworks; availability is probed at runtime, never assumed |
| whisper.cpp | Local speech-to-text engine | For local transcription | No | **Validated** | MIT; the model weights you download carry their own licence, shown before download |
| Tauri 2 | Desktop application shell | Yes | No | **Validated** | MIT / Apache-2.0 |
| SQLite (via sqlx) | Local database | Yes | No | **Validated** | Public domain / MIT |
| Symphonia | Audio decoding | Yes | No | **Validated** | MPL-2.0 |

The full dependency list, with the licence each crate declares, ships in the
release evidence bundle as a software bill of materials.

## Official cloud tool versus unofficial direct access

These are different things and the application never blurs them:

- The **official PLAUD Cloud CLI** is PLAUD's own program. It authenticates to
  your PLAUD account and talks to PLAUD's servers. Using it requires a PLAUD
  account and an internet connection. We launch it with fixed, typed arguments
  and never ask you to paste a bearer token or install a browser extension.
- **Direct device access** is unofficial interoperability: our own code speaking
  to a recorder over Bluetooth or reading a mounted USB volume. It needs no
  PLAUD account and no internet connection, it is strictly read-only, and it
  works only on the exact hardware, firmware and OS combinations listed in the
  compatibility matrix.

Neither path implies that PLAUD endorses, supports or has reviewed this
product.

## Cost model

- OpenRouter charges you for what you send. The application shows the model's
  advertised price before dispatch and the provider-reported usage afterwards;
  where the provider reports no usage, the cost is shown as unknown rather than
  estimated.
- Local engines have no per-call fee. They use your hardware.
- This product sells nothing and has no subscription.

## Related reading

- [Privacy disclosure](./privacy-disclosure.md)
- [Compatibility matrix](./compatibility.md)
- [Model and offline artifact licensing](./model-licensing.md)
