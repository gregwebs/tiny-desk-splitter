#!/bin/bash
set -euo pipefail

# Check if a URL was provided
if [ $# -eq 0 ]; then
  echo "Please provide a URL to a Tiny Desk concert"
  echo "Usage: ./download.sh <URL>"
  exit 1
fi

URL="$1"

# Scrape the set list
echo "Scraping set list..."
json_file=$(cargo run --bin scraper "$URL" | tee | grep -po '\S\+.json')
# remove the colon- ffprobe doesn't like that
album=$(sed -nE 's/"album": "(.*)",/\1/p' "$json_file" | sed 's/^ *//' | sed 's|:||')

# TODO: Use the album name from the JSON file that is emitted.

# Download the video using yt-dlp
echo "Downloading video..."
yt-dlp --use-extractors "generic,-Npr" "$URL" -o "${album}.mp4"

echo "Done!"
