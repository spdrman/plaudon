#!/usr/bin/env bash
# Regenerate the A1.4 audio fixture corpus (task A1.4).
#
# The valid fixtures are real encoder output, not hand-written bytes, so the
# decoder coverage matrix is a measurement. The hostile fixtures are derived
# from them by targeted corruption, so each one isolates exactly one failure.
#
# Requires ffmpeg with libmp3lame, libopus and libvorbis. The committed
# fixtures are small (a 1-second tone), so this script rarely needs to run.
set -euo pipefail
cd "$(dirname "$0")/../tests/fixtures/audio"

command -v ffmpeg >/dev/null 2>&1 || { echo "ffmpeg is required" >&2; exit 127; }
mkdir -p valid hostile
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
PY

echo "regenerated $(ls valid | wc -l | tr -d " ") valid and $(ls hostile | wc -l | tr -d " ") hostile fixtures"
