# ADR-0070: An explicitly versioned `.recording-archive` container, not a ZIP

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §13.6, §13.8, §16.5, §18.6A

## Context

Spec §13.6 requires "a versioned ZIP **or another explicitly versioned
container**" holding `manifest.json`, `records/*.json`, `media/<sha256>.<ext>`,
transcript revisions, generated outputs, source metadata, tool definitions and
user annotations, with a per-entry SHA-256 and an overall integrity digest.
Spec §13.8 additionally requires streaming creation and extraction, size
ceilings, inspection before any write, and an authenticated encrypted mode.

ZIP brings features we would then have to defend against on every restore:
symlink and device entries, external attributes, per-entry encryption flags,
two conflicting directory listings, and data-descriptor modes that force a
reader to trust a stream it has not verified. Writing a ZIP also needs `Seek`
to place the central directory, which is awkward for a strictly streaming
writer.

## Decision

We define `.recording-archive` as our own container, documented in
`crates/export/src/archive/manifest.rs`:

```text
"PLAUDARC" | u16 format_version | u8 protection | u8 reserved
u32 header_len | header_json          <- authenticated metadata (AEAD AAD)
body := frame* terminator
frame := u32 header_len | frame_header_json | stored_length bytes
```

The **manifest is the first frame**, so a reader validates the complete entry
list — paths, kinds, digests, lengths, compression, namespaces, ceilings —
before it sees one payload byte. Entry payloads then stream in manifest order
and each digest is verified before the entry reaches a sink. An entry kind
other than `file` is refused; there is no representation for a link, a device
or a directory, so a traversal or symlink attack has to be expressed as a path
or a kind, and both are rejected by `plaudon_security::fsguard`.

The integrity digest covers the canonical entry list and is documented in the
manifest itself as an integrity check, not an authenticity signature.

## Rejected alternatives

- **A versioned ZIP.** every ZIP feature we do not want (links, device entries,
  attributes, per-entry encryption flags, two directories) becomes an attack
  surface we must explicitly refuse, and the central directory forces `Seek`.
- **A tar stream.** tar carries ownership, modes and link entries, has no
  manifest, and cannot be inspected before extraction without a full pass.
- **SQLite file plus a media folder.** version-locked to one schema, not
  streamable, and it would ship the credential-adjacent tables we exclude.
- **Putting the manifest last, like a ZIP central directory.** it would force
  either a seek or a full buffering pass before validation, which defeats
  "inspect before writing".

## Consequences

- We own the format and its compatibility: the version gate refuses an archive
  written by a newer build rather than guessing, and `format_version` lives in
  both the preamble and the authenticated header.
- Creation and extraction hold one 64 KiB buffer plus the manifest, whatever
  the archive size.
- Third-party tools cannot open an archive. That is acceptable: the archive is
  a restore artifact, and `export_recording` covers reading data elsewhere.
- Media entries are stored uncompressed, so a restore reproduces bit-identical
  bytes with no codec in the path.
