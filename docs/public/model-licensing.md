# Model and offline artifact licensing

No model weights ship inside the application. Local transcription uses an
artifact you choose and download; the licence and the publisher are shown before
the download starts, and the downloaded file is verified against its published
checksum before it is used.

## What this means in practice

| Artifact | Where it comes from | Licence | Shipped with the app? |
|---|---|---|---|
| whisper.cpp engine | Built from source as part of this product | MIT | Yes, as compiled code |
| Whisper model weights (`ggml-*.bin`) | Downloaded by you from the publisher you select | The publisher's licence, displayed before download | No |
| Apple Speech / Foundation Models | Provided by macOS on qualifying hardware | Apple's platform terms | No; probed at runtime |
| OpenRouter-hosted models | Run by the provider you route to | That provider's terms | No; nothing is downloaded |
| pyannote segmentation-3.0 (speaker segmentation) | Downloaded by you, from the publisher, verified against a published checksum | MIT, © 2022 CNRS | No |
| WeSpeaker ResNet34-LM (English, VoxCeleb) (voice matching) | Downloaded by you, from the publisher, verified against a published checksum | CC-BY-4.0 | No |

The release evidence bundle records the checksum and declared licence of every
offline artifact a release was tested against, so a published claim can be
checked rather than taken on trust.

## The two voice models

The last two rows above belong to the optional voice-profiles feature. They are
listed here before the feature is available to anyone, because the licence facts
are the same whether or not you ever turn it on:

- **pyannote segmentation-3.0** finds who is speaking when. MIT licence,
  © 2022 CNRS.
- **WeSpeaker ResNet34-LM** turns a stretch of one person's speech into the
  numeric summary used for matching. The weights are published under CC-BY-4.0,
  the licence of the VoxCeleb dataset they were trained on, which obliges
  attribution to the WeSpeaker authors and to VoxCeleb (Nagrani, Chung and
  Zisserman, University of Oxford). That attribution is in this product's
  `NOTICE` file.

Neither file is part of the application. Each is fetched by its exact published
file name and refused unless its SHA-256 matches the value recorded in the
product, so a mirror that serves different bytes is rejected rather than used.
Nothing is sent anywhere to use them: both run on your machine, and the voice
feature refuses network egress outright.

At the time of writing the voice-profiles feature is **off and not shipped**:
it is published only when its measured performance, accuracy and licence
conditions are all met, and one of those measurements (Windows x64) does not
exist yet.

## Third-party code

The full dependency graph, with the licence each component declares, is
published as a software bill of materials with every release and is audited in
continuous integration. A dependency carrying an unreviewed licence fails the
build rather than shipping.

Designs described in public reverse-engineering write-ups informed the
direct-device protocol work. No code was copied from a project whose licence is
incompatible with this one, and no vendor SDK is bundled.

## Related reading

- [Vendor dependency matrix](./vendor-matrix.md)
- [Privacy disclosure](./privacy-disclosure.md)
