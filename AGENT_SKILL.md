---
name: omascribe
description: Inspect OmaScribe music transcription sessions and audio files.
---

Use `omascribe check FILE --json` without a display to validate a session's
structure and probe its referenced audio. Exit 0 means success; errors are
JSON on stderr with exit 1. `omascribe FILE` opens audio or a `.omascribe`
session in the GUI. FFmpeg and ffprobe must be on PATH.

Sessions are JSON with an absolute `audio` path, `markers` (`time` in original
song seconds and `kind` of `Measure` or `Beat`), `selection` (two original-song
seconds or null), `looping`, and `effects` (`speed` 0.25–1.5, five `gains` in
-12–12 dB at 80/250/1000/4000/10000 Hz, and `vocal_cut`). Keep markers sorted.
Use an atomic write if editing a session, validate it afterward, and never
modify the source audio. Vocal reduction is stereo center cancellation.
