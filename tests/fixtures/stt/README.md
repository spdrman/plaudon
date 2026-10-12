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

## `models/`

Not committed — see `.gitignore`. Seed it with:

```bash
scripts/fetch-stt-fixtures.sh
```

which downloads `ggml-tiny.en.bin` once, verifies its SHA-256, and stops if it
does not match. Tests never reach the network: when the model is absent they
print `SKIPPED` with this instruction instead of passing silently.

Set `PLAUDON_WHISPER_MODEL` to use a model from elsewhere.
