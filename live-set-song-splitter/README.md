# live-set-song-splitter
Split a live performance set into individual songs

## Overview
This tool uses 2 techniques to split a live recording into separate song tracks.
* image analysis- look for an overlay that has the artist and song (Tiny Desk)
* audio analysis- detect silence between songs

The image analysis, with some heuristics works for most Tiny Desk concerts.
Silence analysis is used
* as a fallback if an overlay is missing for a track
* to further refine the song start timing since the overlay is usually late

## Requirements
- Rust (with Cargo)
- FFmpeg (for audio analysis)
- OCR Engine
  - *(default)* a C/C++ toolchain — only to build the **PaddleOCR** backend (`--features paddle-ocr`),
    a more accurate OCR option selectable at runtime with `--ocr-engine paddle`. See
    [docs/change/2026-06-04-adopt-paddle-ocr.md](docs/change/2026-06-04-adopt-paddle-ocr.md).
  - *(alternative)* **leptonica** and **tesseract** — (`--features leptess-ocr`)

## Usage
```bash
cargo run -- <input_file> <concert_description>
```

Where:
- `input_file` is the path to your audio/video file
- `concert_description` is JSON describing the recording and expected songs

```sh
cargo run --bin live-set-splitter -- <json_file> [output_dir]

# Optional: choose the OCR backend (default tesseract; paddle needs --features paddle-ocr)
cargo run --features paddle-ocr --bin live-set-splitter -- <json_file> --ocr-engine paddle

# Optional: frame-accurate video cuts (slower, re-encodes video). Default is `copy`.
cargo run --bin live-set-splitter -- <json_file> --video-cut-mode reencode
```

The JSON file uses the same format produced by the `scraper` crate.

### Video cut mode

`--video-cut-mode` controls how each track's video is cut from the source. Both modes
keep audio and video in sync:

| Mode | Speed | Cut precision | Notes |
|---|---|---|---|
| `copy` *(default)* | Fast, lossless | Snaps the start back to the nearest preceding keyframe (up to one GOP — a few seconds — early) | Stream copy; no re-encode |
| `reencode` | Slow | Frame-accurate at the detected start | Re-encodes video with x264; audio is still copied |

Both modes seek on the **input** side (`-ss` before `-i`). An earlier version placed
`-ss` *after* `-i` with `-c copy`, which let the video start at the first keyframe
*after* the cut while the audio started exactly at the cut — desyncing every track not
cut on a keyframe by up to one GOP. See
[docs/change/2026-06-06-video-audio-sync-fix.md](docs/change/2026-06-06-video-audio-sync-fix.md).


## Bad data

This concert is missing the overlays: Carminho
