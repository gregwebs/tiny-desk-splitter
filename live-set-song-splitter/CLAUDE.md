# Live Set Song Splitter - Claude Guide

## Build/Run Commands
- Rust: `cargo build --release` to compile
- Rust: `cargo run -- <input_file> <num_songs>` to execute

## Dependencies
- FFmpeg: Used for audio analysis
- Install with: `brew install ffmpeg` (macOS) or `apt install ffmpeg` (Ubuntu)

## Code Style Guidelines
- **DRY**: Avoid code duplication. Use variables, functions, and modules to share code
- **Rust**: Use standard Rust formatting (`cargo fmt`)
- **Naming**: snake_case for variables/functions, CamelCase for types
- **Error Handling**: Use Result/Option in Rust, try/except in Python
- **Documentation**: Document functions with comments explaining purpose
- **Types**: Use strong typing where possible (type annotations in Python)
- **Imports**: Group standard library, external crates/packages, then local modules
- **Constants**: Define thresholds and parameters as named constants

## Project Structure
- `src/main.rs`: Rust implementation
- `src/*.rs`: Rust modules

## Logic

- Look for the overlay with the artist and song title
  - Use FFmpeg to analyze the video and extract images of frames for every 2 seconds
  - Use OCR to extract text from the images
  - Fuzzy match the text to the expected overlay text
- Refine the starting point by looking backwards frame by frame
- There is audio processing with FFmpeg that is available.
  - This code was able to successfully split up some sets just based on the audio.
  - It wasn't accurate enough to be used by itself, but it could be used to help find a better split point.

## Features

- Command-line interface with positional and optional arguments
- Reads in JSON, outputs JSON and files
