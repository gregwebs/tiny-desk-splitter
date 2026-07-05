#!/usr/bin/env bash
# Report the GitHub Actions Playwright check for a commit.
#
# Usage: ./scripts/check-playwright-job.sh [--wait] [COMMIT]
# COMMIT defaults to HEAD. With --wait, poll until the check completes.
set -euo pipefail
cd "$(dirname "$0")/.."

wait_for_result=false
if [ "${1:-}" = "--wait" ]; then
  wait_for_result=true
  shift
fi
if [ $# -gt 1 ]; then
  echo "usage: $0 [--wait] [COMMIT]" >&2
  exit 2
fi

commit="${1:-HEAD}"
sha=$(git rev-parse "$commit")
origin=$(git config --get remote.origin.url)
repo=${origin#*github.com[:/]}
repo=${repo%.git}
interval=${CHECK_INTERVAL_SECONDS:-10}

if [ -x /opt/homebrew/opt/curl/bin/curl ]; then
  curl_bin=/opt/homebrew/opt/curl/bin/curl
else
  curl_bin=curl
fi

while true; do
  check=$(
    "$curl_bin" -fsS \
      -H "Accept: application/vnd.github+json" \
      "https://api.github.com/repos/$repo/commits/$sha/check-runs?per_page=100" |
      jq -c '
        [.check_runs[] | select(.name == "playwright")]
        | sort_by(.started_at)
        | last
      '
  )

  if [ "$check" = "null" ]; then
    echo "playwright: not found for $sha" >&2
    exit 2
  fi

  status=$(jq -r .status <<<"$check")
  conclusion=$(jq -r '.conclusion // "pending"' <<<"$check")
  url=$(jq -r .html_url <<<"$check")
  printf 'playwright: %s (%s)\n%s\n' "$status" "$conclusion" "$url"

  if [ "$status" = "completed" ]; then
    [ "$conclusion" = "success" ]
    exit
  fi
  if [ "$wait_for_result" = false ]; then
    exit 2
  fi
  sleep "$interval"
done
