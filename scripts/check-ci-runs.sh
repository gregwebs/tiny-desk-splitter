#!/usr/bin/env bash
# Report GitHub Actions check runs for a commit.
#
# Usage: ./scripts/check-ci-runs.sh [--wait] [--job JOB_NAME] [COMMIT]
# COMMIT defaults to HEAD. With --wait, poll until the selected check runs complete.
set -euo pipefail
cd "$(dirname "$0")/.."

usage() {
  echo "usage: $0 [--wait] [--job JOB_NAME] [COMMIT]" >&2
}

wait_for_result=false
job_filter=""
commit="HEAD"
while [ $# -gt 0 ]; do
  case "$1" in
    --wait)
      wait_for_result=true
      shift
      ;;
    --job)
      if [ -z "${2:-}" ]; then
        usage
        exit 2
      fi
      job_filter="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    -*)
      usage
      exit 2
      ;;
    *)
      if [ "$commit" != "HEAD" ]; then
        usage
        exit 2
      fi
      commit="$1"
      shift
      ;;
  esac
done

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

# shellcheck disable=SC2016 # jq sees $job; the shell must not expand it.
latest_checks_query='
  [.check_runs[]
   | select(.app.slug == "github-actions")
   | select($job == "" or .name == $job)]
  | sort_by(.name, (.started_at // .created_at // ""))
  | group_by(.name)
  | map(max_by(.started_at // .created_at // ""))
  | sort_by(.name)
'

while true; do
  checks=$(
    "$curl_bin" -fsS \
      -H "Accept: application/vnd.github+json" \
      "https://api.github.com/repos/$repo/commits/$sha/check-runs?per_page=100" |
      jq -c --arg job "$job_filter" "$latest_checks_query"
  )

  if [ "$(jq 'length' <<<"$checks")" -eq 0 ]; then
    if [ -n "$job_filter" ]; then
      echo "$job_filter: not found for $sha" >&2
    else
      echo "CI checks: not found for $sha" >&2
    fi
    if [ "$wait_for_result" = true ]; then
      sleep "$interval"
      continue
    fi
    exit 2
  fi

  jq -r '.[] | "\(.name): \(.status) (\(.conclusion // "pending"))\n\(.html_url)"' <<<"$checks"

  if jq -e 'all(.[]; .status == "completed" and .conclusion == "success")' \
    >/dev/null <<<"$checks"; then
    exit
  fi

  if jq -e 'any(.[]; .status == "completed" and .conclusion != "success")' \
    >/dev/null <<<"$checks"; then
    exit 1
  fi

  if [ "$wait_for_result" = false ]; then
    exit 2
  fi
  sleep "$interval"
done
