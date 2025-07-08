#!/bin/bash
set -euo pipefail

# Check if a URL was provided
if [ $# -eq 0 ]; then
  echo "Please provide a URL to a Tiny Desk concert" >&2
  echo "Usage: ./extract.sh <URL>" >&2
  exit 1
fi

while [ $# -gt 0 ]; do
    url="$1"
    shift

    # Check if already downloaded file or its metadata
    found=""
    for file in *.json ; do
        if [[ "$(jq -r .source "$file")" == $url ]] ; then
            echo "found existing metadata file $file"
            mp4="$(jq -r .album "$file" | sed 's|:||').mp4"
            if ! test -f "$mp4" ; then
                ./download.sh "$url"
            else
                echo "already downloaded $mp4"
            fi
            found="$file"
            break
        fi
    done

    if [[ -z $found ]] ; then
        ./download.sh "$url"
        for file in *.json ; do
            if [[ "$(jq -r .source "$file")" == $url ]] ; then
                found="$file"
                break
            fi
        done
        if [[ -z $found ]] ; then
            echo "downloaded json file not found for $url" >&2
            exit 1
        fi
    fi

    cargo run --bin live-set-splitter -- --analyze-images "$found"
done