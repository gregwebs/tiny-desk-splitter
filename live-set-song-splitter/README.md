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

## How it works

It looks for the text overlay with the artist and song title in the video frames.


## Bad data

This concert is missing the overlays: Carminho
