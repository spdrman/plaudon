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

The release evidence bundle records the checksum and declared licence of every
offline artifact a release was tested against, so a published claim can be
checked rather than taken on trust.

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
