#!/usr/bin/env bash
# Regenerate the audio fixture corpus (tasks A1.4 and O1.6).
#
# The valid fixtures are real encoder output, not hand-written bytes, so the
# decoder coverage matrix is a measurement. The hostile fixtures are derived
# from them by targeted corruption, so each one isolates exactly one failure.
# The Opus reference WAVs under `reference/` are decoded by **libopus** (via
# ffmpeg's `libopus` decoder, not its native one) and are what
# `crates/audio/tests/opus_decode.rs` differentially compares our pure-Rust
# decoder against; regenerating them changes the reference, so do it
# deliberately.
#
# Requires ffmpeg with libmp3lame, libopus and libvorbis. The committed
# fixtures are small (a 1-second tone), so this script rarely needs to run.
set -euo pipefail
cd "$(dirname "$0")/../tests/fixtures/audio"

command -v ffmpeg >/dev/null 2>&1 || { echo "ffmpeg is required" >&2; exit 127; }
mkdir -p valid hostile reference
cd valid

ffmpeg -hide_banner -loglevel error -y -f lavfi \
  -i "sine=frequency=440:duration=1:sample_rate=16000" -ac 1 -c:a pcm_s16le tone-mono-16k.wav
ffmpeg -hide_banner -loglevel error -y -f lavfi \
  -i "sine=frequency=440:duration=1:sample_rate=44100" -ac 2 -c:a pcm_s16le tone-stereo-44k.wav
ffmpeg -hide_banner -loglevel error -y -i tone-mono-16k.wav -c:a libmp3lame -b:a 64k tone-mono-16k.mp3
ffmpeg -hide_banner -loglevel error -y -i tone-mono-16k.wav -c:a aac        -b:a 64k tone-mono-16k.m4a
ffmpeg -hide_banner -loglevel error -y -i tone-mono-16k.wav -c:a libopus    -b:a 24k tone-mono-16k.opus
ffmpeg -hide_banner -loglevel error -y -i tone-mono-16k.wav -c:a libvorbis  -q:a 2   tone-mono-16k.ogg
ffmpeg -hide_banner -loglevel error -y -i tone-mono-16k.wav -c:a flac                tone-mono-16k.flac

# O1.6: the Opus corpus the decoder is tested against. Opus always codes at
# 48 kHz, so the three source rates below differ in what libopus resamples on
# the way in, which is exactly the case a device file exercises.
ffmpeg -hide_banner -loglevel error -y -f lavfi \
  -i "sine=frequency=440:duration=1:sample_rate=48000" -ac 1 -c:a libopus -b:a 32k tone-mono-48k.opus
ffmpeg -hide_banner -loglevel error -y -f lavfi \
  -i "sine=frequency=440:duration=1:sample_rate=8000"  -ac 1 -c:a libopus -b:a 24k tone-mono-8k.opus
# Two different tones, one per channel, so a channel swap or a downmix cannot
# pass as correct stereo.
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "sine=frequency=440:duration=1:sample_rate=48000" \
  -f lavfi -i "sine=frequency=660:duration=1:sample_rate=48000" \
  -filter_complex "[0:a][1:a]join=inputs=2:channel_layout=stereo[a]" -map "[a]" \
  -c:a libopus -b:a 64k tone-stereo-48k.opus

# The differential reference: each Opus fixture decoded by libopus itself,
# 48 kHz signed 16-bit, no resampling and no filtering. `-c:a libopus` before
# `-i` forces ffmpeg's libopus binding rather than its own native decoder, so
# the reference really is the reference implementation.
cd ../reference
for name in tone-mono-16k tone-mono-48k tone-mono-8k tone-stereo-48k; do
  ffmpeg -hide_banner -loglevel error -y -c:a libopus -i "../valid/$name.opus" \
    -c:a pcm_s16le "$name.libopus.wav"
done
cd ..
python3 - <<'PY'
import pathlib, struct

valid = pathlib.Path("valid")
hostile = pathlib.Path("hostile")
hostile.mkdir(exist_ok=True)

wav = (valid / "tone-mono-16k.wav").read_bytes()
mp3 = (valid / "tone-mono-16k.mp3").read_bytes()
opus = (valid / "tone-mono-16k.opus").read_bytes()

# A filename that lies about its contents.
(hostile / "mp3-bytes-named.wav").write_bytes(mp3)
# A stream that stops halfway through its data.
(hostile / "truncated.wav").write_bytes(wav[: len(wav) // 2])
# A RIFF size field claiming four gigabytes.
forged = bytearray(wav)
struct.pack_into("<I", forged, 4, 0xFFFFFFF0)
(hostile / "forged-riff-size.wav").write_bytes(bytes(forged))
# A data chunk length claiming two gigabytes.
forged = bytearray(wav)
index = bytes(forged).find(b"data")
struct.pack_into("<I", forged, index + 4, 0x7FFFFFFF)
(hostile / "forged-data-length.wav").write_bytes(bytes(forged))
# An absurd declared sample rate.
forged = bytearray(wav)
index = bytes(forged).find(b"fmt ")
struct.pack_into("<I", forged, index + 12, 0xFFFFFFFF)
(hostile / "forged-sample-rate.wav").write_bytes(bytes(forged))
# No bytes at all.
(hostile / "zero-length.wav").write_bytes(b"")
# A header with no samples behind it.
(hostile / "header-only.wav").write_bytes(wav[:44])
# Text wearing an audio extension.
(hostile / "text-named.mp3").write_bytes(b"This is a plain text file, not audio at all.\n" * 8)
# Padding before a real header, to defeat a naive prefix check.
(hostile / "leading-nulls.wav").write_bytes(b"\x00" * 4096 + wav)
# A PLAUD-style .ASR sidecar: Opus payload with no container.
(hostile / "device-sidecar.asr").write_bytes(opus[0x100 : 0x100 + 2048])

# ---------------------------------------------------------------------------
# O1.6: hostile Ogg Opus containers and the raw-packet fuzz corpus.
#
# A byte flip inside an Ogg page breaks the page CRC, and the demuxer would
# then reject the file before the codec ever saw it — which would test the
# wrong layer. These fixtures therefore edit the page and recompute its CRC,
# so each one reaches exactly the component it is aimed at.
# ---------------------------------------------------------------------------

CRC_TABLE = []
for byte in range(256):
    value = byte << 24
    for _ in range(8):
        value = ((value << 1) ^ 0x04C11DB7) & 0xFFFFFFFF if value & 0x80000000 else (value << 1) & 0xFFFFFFFF
    CRC_TABLE.append(value)


def ogg_crc(page: bytes) -> int:
    crc = 0
    for byte in page:
        crc = ((crc << 8) & 0xFFFFFFFF) ^ CRC_TABLE[((crc >> 24) & 0xFF) ^ byte]
    return crc


def split_pages(data: bytes):
    """Yield (start, header_len, segment_table, payload_len) for each page."""
    offset = 0
    while offset + 27 <= len(data) and data[offset : offset + 4] == b"OggS":
        segments = data[offset + 26]
        table = data[offset + 27 : offset + 27 + segments]
        header_len = 27 + segments
        payload_len = sum(table)
        yield offset, header_len, table, payload_len
        offset += header_len + payload_len


def reseal(page: bytearray) -> bytes:
    page[22:26] = b"\x00\x00\x00\x00"
    page[22:26] = struct.pack("<I", ogg_crc(bytes(page)))
    return bytes(page)


def pages(data: bytes):
    return [
        bytearray(data[start : start + header_len + payload_len])
        for start, header_len, table, payload_len in split_pages(data)
    ]


def packets(data: bytes):
    """Reassemble the Opus packets of an Ogg stream, headers excluded."""
    out, pending = [], bytearray()
    for start, header_len, table, _payload in split_pages(data):
        cursor = start + header_len
        for length in table:
            pending.extend(data[cursor : cursor + length])
            cursor += length
            if length != 255:
                out.append(bytes(pending))
                pending = bytearray()
    # The first two packets are OpusHead and OpusTags, not audio.
    return out[2:]


opus_pages = pages(opus)
head_page = bytearray(opus_pages[0])
head_offset = bytes(head_page).find(b"OpusHead")

# A reserved channel mapping family: RFC 7845 says a decoder must refuse it.
forged = bytearray(head_page)
forged[head_offset + 18] = 255
(hostile / "opus-reserved-mapping.opus").write_bytes(
    reseal(forged) + b"".join(bytes(page) for page in opus_pages[1:])
)
# A channel count of zero: no layout exists for it.
forged = bytearray(head_page)
forged[head_offset + 9] = 0
(hostile / "opus-zero-channels.opus").write_bytes(
    reseal(forged) + b"".join(bytes(page) for page in opus_pages[1:])
)
# Headers with no audio behind them: nothing to decode, and it must say so.
(hostile / "opus-header-only.opus").write_bytes(
    b"".join(bytes(page) for page in opus_pages[:2])
)
# A stream that stops in the middle of an audio page.
(hostile / "opus-truncated.opus").write_bytes(opus[: len(opus) * 55 // 100])
# A code-3 packet claiming zero frames (RFC 6716 3.2.5 requires 1..=48).
forged_pages = [bytearray(page) for page in opus_pages]
audio = forged_pages[2]
payload_at = 27 + audio[26]
audio[payload_at] = (audio[payload_at] & 0xF8) | 0x03
audio[payload_at + 1] = 0x00
forged_pages[2] = bytearray(reseal(audio))
(hostile / "opus-zero-frame-count.opus").write_bytes(
    b"".join(bytes(page) for page in forged_pages)
)

# The raw-packet fuzz corpus: real packets plus the hostile shapes the packet
# decoder must refuse. `crates/security/tests/fuzz_corpus_replay.rs` replays
# every entry on stable and `fuzz/fuzz_targets/fuzz_opus_packet.rs` fuzzes the
# same function.
corpus = pathlib.Path("../fuzz-corpus/opus_packet")
corpus.mkdir(parents=True, exist_ok=True)
for existing in corpus.glob("*.bin"):
    existing.unlink()

real = packets(opus)
stereo = packets((valid / "tone-stereo-48k.opus").read_bytes())
entries = {
    "valid-mono-first.bin": real[0],
    "valid-mono-middle.bin": real[len(real) // 2],
    "valid-mono-last.bin": real[-1],
    "valid-stereo-middle.bin": stereo[len(stereo) // 2],
    "empty.bin": b"",
    "toc-only.bin": real[0][:1],
    "truncated-frame.bin": real[len(real) // 2][: max(1, len(real[0]) // 2)],
    "code3-zero-frames.bin": bytes([real[0][0] | 0x03, 0x00]) + real[0][1:],
    "code3-forty-nine-frames.bin": bytes([real[0][0] | 0x03, 49]) + real[0][1:],
    "code1-odd-length.bin": bytes([real[0][0] | 0x01]) + real[0][1:2],
    "code2-forged-length.bin": bytes([real[0][0] | 0x02, 0xFF, 0xFF]) + real[0][1:],
    "all-zero-frame.bin": bytes(64),
    "all-ones.bin": b"\xff" * 64,
    "oversized.bin": b"\xfc" + b"\x00" * 70_000,
    "max-frames-truncated.bin": bytes([0xFB, 48]) + b"\x01" * 48,
}
for name, payload in entries.items():
    (corpus / name).write_bytes(payload)
PY

echo "regenerated $(ls valid | wc -l | tr -d " ") valid, $(ls hostile | wc -l | tr -d " ") hostile, \
$(ls reference | wc -l | tr -d " ") reference fixtures and \
$(ls ../fuzz-corpus/opus_packet | wc -l | tr -d " ") opus fuzz corpus entries"
