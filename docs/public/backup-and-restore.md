# Backup, restore and getting your data out

Plaudon keeps everything it manages on your machine. Two different operations
move it around, and they are not interchangeable.

## Export: one recording, in a format other tools read

An export writes a single recording's transcript or captions to a file you
choose — plain text, Markdown, JSON or subtitle formats. An export is for
reading elsewhere. It is not a backup: it does not carry the original media,
the revision history, the job record or the provenance of the recording.

## Portable backup: the whole library, restorable

A portable backup is a single archive containing the original media bytes, every
transcript revision, derived outputs, annotations and the structured records
that tie them together, plus a manifest listing each entry with its SHA-256.

What a backup never contains: your OpenRouter key, device authorization
material, PLAUD session state, or any other secret. Those live in the platform
secret store and stay there. After a restore you re-enter the key you want to
use.

## Making a backup

Settings → Backup → Export portable backup, then choose a destination. The
archive is written to a temporary file and renamed into place only once it is
complete, so an interrupted export never leaves a file that looks finished. A
plaintext archive is announced as plaintext before it is written.

## Restoring

Restore into a clean install, or into a library you are prepared to merge with.
Before anything is written, the archive is validated:

- every path is checked for traversal and absolute components, and symlinks are
  refused;
- duplicate entries are refused;
- decompressed size and entry count are bounded, so a compression bomb is
  rejected rather than unpacked;
- every media blob is hashed and compared against the manifest;
- the schema version must be one this build understands.

A failure at any of those points aborts the restore before your existing library
is modified. A restore either completes or changes nothing.

After a successful restore, media hashes and structured records match the source
manifest. Any entry that does not match is reported; none is silently dropped.

## Moving between macOS and Windows

The archive format is the same on both platforms, and the manifest is
path-normalised, so a backup made on one can be restored on the other. Available
engines may differ — an Apple on-device model exists only on qualifying macOS
hardware — so a transcript produced by an engine the new machine does not have is
restored as data and labelled with the engine that produced it, not regenerated.

## Deleting

Deleting a recording removes the managed media and the derived outputs that
belong to it, and leaves no dangling references behind. Files you imported from
your own folders are never touched: the import copied them, and the original is
yours. A recording you deleted stays deleted — a later rescan of the same source
does not resurrect it without an explicit confirmation from you.

## Related reading

- [Privacy disclosure](./privacy-disclosure.md)
- [Troubleshooting](./troubleshooting.md)
