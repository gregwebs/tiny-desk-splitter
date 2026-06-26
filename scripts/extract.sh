#!/bin/bash
set -euo pipefail

# Resolve the sibling download.sh by this script's own location, so extract.sh
# works regardless of the caller's CWD. Data files (*.json, *.mp4) are still
# read/written in the CWD, as before.
script_dir="$(cd "$(dirname "$0")" && pwd)"

# Check if a URL was provided
if [ $# -eq 0 ]; then
  echo "Please provide a URL to a Tiny Desk concert" >&2
  echo "Usage: ./scripts/extract.sh <URL or json file>" >&2
  exit 1
fi

while [ $# -gt 0 ]; do
    url="$1"
    shift

    found=""

    # file given instead of url
    if [[ -e $url ]] ; then
        found="$url"
        url="$(jq -r .source "$found")"
    else
        # Check if already downloaded file or its metadata
        for file in *.json ; do
            if ! echo "$file" | grep listing ; then
                if [[ "$(jq -r .source "$file")" == "$url" ]] ; then
                    echo "found existing metadata file $file"
                    mp4="$(jq -r .album "$file" | sed 's|:||').mp4"
                    if ! test -f "$mp4" ; then
                        "$script_dir/download.sh" "$url"
                    else
                        echo "already downloaded $mp4"
                    fi
                    found="$file"
                    break
                fi
            fi
        done
    fi

    if [[ -z $found ]] ; then
        "$script_dir/download.sh" "$url"
        for file in *.json ; do
            if [[ "$(jq -r .source "$file")" == "$url" ]] ; then
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