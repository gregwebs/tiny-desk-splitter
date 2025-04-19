# Live Set Song Splitter - Claude Guide

## Build/Run Commands
- Rust: `cargo build --release` to compile
- Rust: `cargo run -- <input_file> <num_songs>` to execute

## Dependencies
- FFmpeg: Used for audio analysis
- Install with: `brew install ffmpeg` (macOS) or `apt install ffmpeg` (Ubuntu)

## Code Style Guidelines
- **Rust**: Use standard Rust formatting (`cargo fmt`)
- **Naming**: snake_case for variables/functions, CamelCase for types
- **Error Handling**: Use Result/Option in Rust
- **Documentation**: Document functions with comments explaining purpose
- **Types**: Use strong typing where possible
- **Imports**: Group standard library, external crates/packages, then local modules
- **Constants**: Define thresholds and parameters as named constants
- **DRY**: Avoid code duplication, use functions and modules to share code
- **Testing**: Refactor code into small testable functions. Write lots of tests without using mocks.

## Project Structure
- `src/main.rs`: Rust implementation
- `src/*.rs`: Rust modules

## Logic

- Scrape for information
- Bash scripts and just for orchestration
- Downloads videos using yt-dlp

## Testing
- Manual testing by running on various NPR Tiny Desk Concert URLs
- Check output files for correct song listings