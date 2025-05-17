#!/bin/bash
set -euo pipefail

# Check if a URL was provided
if [ $# -eq 0 ]; then
  echo "Please provide a URL to a Tiny Desk concert"
  echo "Usage: ./extract.sh <URL>"
  exit 1
fi

url="$1"
shift

# Check if already downloaded file or its metadata
for file in *.json ; do
    if [[ "$(jq -r .source "$file")" == $url ]] ; then
        echo "found existing metadata file $file"
        mp4="$(jq -r .album "$file").mp4"
        if ! test -f "$mp4" ; then
            ./download.sh "$url"
        else
            echo "already downloaded $mp4"
        fi
        cargo run --bin live-set-splitter -- --analyze-images "$file" 
        exit 0
    fi
done

./download.sh "$url"
for file in *.json ; do
    if [[ "$(jq -r .source "$file")" == $url ]] ; then
        cargo run --bin live-set-splitter -- --analyze-images "$file" 
        exit 0
    fi
done