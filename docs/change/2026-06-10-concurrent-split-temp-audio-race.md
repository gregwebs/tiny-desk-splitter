# Fix race condition between concurrent splits: shared `temp_audio.wav`

## Problem

Splitting two concerts at once (triggered from concert-web) could fail one of
the splits with:

```
Failed to extract audio waveform from ./concerts/...mp4
  0: Failed to open temporary WAV file: temp_audio.wav
  1: No such file or directory (os error 2)
```

Retrying the failed concert alone succeeded, which pointed at a concurrency
problem (captured in `dual-split-failure.log`: concert 3 failed at the WAV
open while concert 4's split was mid-flight).

## Root cause

`extract_audio_waveform` (`live-set-song-splitter/src/audio.rs`) decoded audio
through a hardcoded CWD-relative temp file: ffmpeg wrote `temp_audio.wav`, the
function read it back, then deleted it. Every split job spawned by
concert-tracker runs in the same working directory, so concurrent splitter
processes shared that single path. All other temp artifacts were already
namespaced per concert (`temp_frames/{album}`, smart-cut `.work` dirs); this
was the only shared path.

### Failing interleaving (before)

```
Process A (concert 4)              Process B (concert 3)        temp_audio.wav
---------------------              ---------------------        --------------
ffmpeg -y -> temp_audio.wav                                     A's audio
read temp_audio.wav                ffmpeg -y -> temp_audio.wav  B's audio (overwrote A mid-read!)
remove_file(temp_audio.wav)                                     (gone)
                                   File::open(temp_audio.wav)   -> ENOENT  ❌
```

Besides the observed ENOENT, the overwrite window could silently feed one
concert's audio into the other's analysis (wrong song boundaries, no error).

## Fix

Eliminate the temp file entirely. ffmpeg now emits raw PCM (`-f s16le -`) to
stdout and `extract_audio_waveform` reads it from a pipe:

```
Process A                          Process B                    shared state
---------                          ---------                    ------------
ffmpeg | pipe -> Vec<f32>          ffmpeg | pipe -> Vec<f32>    none  ✅
```

This also removes:
- the assumption that a WAV header is exactly 44 bytes,
- the leaked temp file if the process died between write and cleanup,
- a full disk write+read of the decoded audio (~100 MB for a typical concert).

Error handling: the exit status is checked before the bytes are trusted (a
failed ffmpeg may emit a truncated stream), errors name the input file and
exit code, and a successful-but-empty decode is logged at info level and
returned as an empty waveform (downstream recovery then fails loudly listing
the missing songs).

The PCM→samples conversion was extracted into a pure, unit-tested function
`pcm_s16le_to_samples`.

## Tests

In `audio.rs`:
- unit tests for `pcm_s16le_to_samples` (known values, empty input, odd
  trailing byte),
- integration test decoding a generated 1-second sine file (ffmpeg `lavfi`),
- **concurrency regression test**: two threads extract different sine inputs
  from the same CWD; asserts both succeed, the waveforms differ (catches the
  silent cross-read), and no `temp_audio.wav` is created,
- failure-path test: missing input yields an error naming the file and exit
  code.

## Known limitation (out of scope)

Two concurrent splits of the *same* concert would still collide on
`temp_frames/{album}`; the job queue prevents duplicate jobs per concert.
