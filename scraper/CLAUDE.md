# Tiny Desk Concert Downloader - Claude Guide

## Build/Run Commands
- Rust: `cargo build --release` to compile
- Rust: `cargo run -- <input_file> <num_songs>` to execute

## Dependencies
- yt-dlp for downloading the mp4

## Project Structure
- `src/main.rs`: Rust implementation
- `src/*.rs`: Rust modules

## Logic

- Scrape for information
- Downloads videos using yt-dlp
- Bash scripts are available for orchestration

## Testing
- Manual testing by running on various NPR Tiny Desk Concert URLs
- Check output files for correct song listings
