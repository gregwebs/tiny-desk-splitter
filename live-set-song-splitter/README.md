# live-set-song-splitter
Split a live performance set into individual songs

## Overview
This tool analyzes audio files to detect silence between songs in a live recording and splits the file into separate song tracks.

## Requirements
- Rust (with Cargo)
- FFmpeg (for audio analysis)

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
