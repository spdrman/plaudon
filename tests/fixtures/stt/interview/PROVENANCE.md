# Interview fixture provenance (issue #308)

`hwhap-407-excerpt.mp3` is a 2 minute 32 second excerpt of a real two-person
interview: NASA's *Houston We Have a Podcast*, episode 407, "A Place in Space
to Call Home: Part 1", recorded 30 September 2025 and released 16 January
2026. Host Nilufar Ramji talks to NASA astronaut Tracy Dyson.

## Licence

The episode is a work of NASA, produced by the Johnson Space Center, so it is
a work of the United States federal government and is in the public domain in
the United States (17 U.S.C. 105). Wikimedia Commons holds it under its
`PD-USGov-NASA` tag and lists the licence as public domain. NASA's media
guidelines ask that NASA material is not used to imply endorsement; this
fixture is test data only and implies nothing of the sort.

The reference transcript is NASA's own published transcript, which is a US
government work in the public domain for the same reason.

## Source

| | |
|---|---|
| Item page | https://commons.wikimedia.org/wiki/File:HWHAP_Ep407_A_Place_in_Space_to_Call_Home-_Part_1_(Ep407_A_Place_in_Space_to_Call_Home_Part_1).mp3 |
| Download | https://upload.wikimedia.org/wikipedia/commons/6/67/HWHAP_Ep407_A_Place_in_Space_to_Call_Home-_Part_1_%28Ep407_A_Place_in_Space_to_Call_Home_Part_1%29.mp3 |
| Episode page and transcript | https://www.nasa.gov/podcasts/houston-we-have-a-podcast/a-place-in-space-to-call-home-part-1/ |
| Source format | MP3, 32 kHz stereo, 96 kbit/s, 1:00:28.99 |
| Source SHA-1 (matches Commons) | `6fbbdc919e42c20a8275a8274da4e34248bc2d8c` |
| Source SHA-256 | `a6b65f84a64161ca52ab5d6146db69c55a4add90bc0b914abfa84de0e61f60bb` |

## The excerpt

| | |
|---|---|
| Span of the source | 130.3 s to 282.7 s (152.4 s) |
| Cut with | `ffmpeg -c copy` (the source's own MP3 frames, no re-encode), metadata and Xing header stripped |
| Size | 1,829,088 bytes |
| SHA-256 | `90a533a5cfefc05a5ffbf0d5f7c5c13a3a0673c2e5a468bc44c6fb23b3e7e630` |

The span starts as the intro music fades out under "Tracy, welcome to Houston
We Have a Podcast" and stops in the pause after "Tell us a little bit about
what that was like.", before the next answer begins. It holds seven turns:
two long answers, a "Wow." interjection, and a "Yeah," that Dyson says over
the host's question.

The excerpt is committed because it is small and public domain, so no test
ever touches the network. `scripts/make-interview-fixture.sh` re-derives it:
it downloads the source, checks the SHA-256 above, makes the same cut and
compares the result with the pinned digest. With ffmpeg 9.0.2 the bytes match
exactly; another ffmpeg could land the cut on a neighbouring frame, in which
case the script says so and leaves the committed file alone.

## Reference transcript

`hwhap-407-excerpt.reference.txt` is the official transcript text for exactly
this span, copied verbatim from the episode page (curly apostrophes and all).
It is near-verbatim speech, false starts and repeated words included ("the
the year of 1997", "fellow fellow fellowship"). It is not perfect, and I left
it uncorrected on purpose so the reference is NASA's rather than mine. The
one known quirk is that Dyson's "Yeah," over the host's last question is
folded into the host's line; WER ignores speakers, so that costs nothing.

The excerpt was checked against the transcript by running whisper.cpp
`ggml-tiny.en` over the excerpt and diffing its output word by word with the
reference: the first and last words line up with the cut points, and every
remaining difference is a recognition slip ("space station" vs "this space
station", "astronaut core") rather than missing or extra speech.

## Character of the audio

It is a clean, studio-miked conversation with two speakers, natural turn
taking and one short overlap. It has little room noise, so it proves the
conversational side of the issue (turns, interjections, disfluencies, a
second voice) better than the noisy-room side.
