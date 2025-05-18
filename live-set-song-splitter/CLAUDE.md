# Live Set Song Splitter - Claude Guide

## Build/Run Commands
- Rust: `cargo build --release` to compile
- Rust: `cargo run -- <input_file> <num_songs>` to execute

## Dependencies
- FFmpeg: Used for audio analysis
- Install with: `brew install ffmpeg` (macOS) or `apt install ffmpeg` (Ubuntu)

## Project Structure
- `src/main.rs`: Rust implementation entrypoint
- `src/*.rs`: Rust modules

## Logic

- Split the video of the concert up into separate tracks.
- The videos have overlays that state the track title.
- Look for the overlay with the artist and song title
  - Use FFmpeg to analyze the video and extract images of frames for every 2 seconds
  - Use OCR to extract text from the images
  - Fuzzy match the text to the expected overlay text
- The overlay is shown a few seconds after the song has started.
  - Refine the starting point by looking backwards frame by frame for a previous frame with overlay.
  - Use audio processing to look for silent points a few seconds before the overlay.

## Features

- Command-line interface with positional and optional arguments
- Reads in JSON, outputs JSON and files
