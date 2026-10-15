# Local STT fixtures

## `audio/`

`jfk.wav` is an excerpt of the 1961 United States presidential inaugural
address. As a work of the US federal government it is in the public domain;
the particular 11-second 16 kHz mono encoding is the sample distributed with
`ggerganov/whisper.cpp` (MIT). `jfk.mp3` and `jfk.m4a` are derived from it with
`ffmpeg` at 64 kbit/s, 16 kHz mono, so the same content exercises the MP3 and
MP4/AAC decode paths.

Everything else the audio suite uses is generated at test time by
`plaudon_audio::testing`, so the corpus stays reproducible and licence-clean
(§17.1: no confidential material in the repository).

## `interview/`

A 2.5 minute excerpt of a public-domain NASA podcast interview (two speakers,
real turn taking) with NASA's own transcript of it, used by
`tests/integration/tests/f_interview_stt.rs` to score `ggml-tiny.en` on an
actual conversation. `interview/PROVENANCE.md` has the source URL, licence,
digests and how the excerpt was cut; `scripts/make-interview-fixture.sh`
re-derives it from the pinned source and checks the bytes.

## `models/`

Not committed — see `.gitignore`. Seed it with:

```bash
scripts/fetch-stt-fixtures.sh
```

which downloads `ggml-tiny.en.bin` once, verifies its SHA-256, and stops if it
does not match. Tests never reach the network: when the model is absent they
print `SKIPPED` with this instruction instead of passing silently.

Set `PLAUDON_WHISPER_MODEL` to use a model from elsewhere.
