#!/usr/bin/env bash
# Update an existing pull request, authenticated as a GitHub App installation.
#
# Usage: gh-app-pr-update.sh --pr NUMBER [--repo OWNER/REPO]
#          [--title TITLE] [--body TEXT | --body-file FILE]
#          [--base BASE] [--state open|closed]
#
# --repo defaults to the current directory's github.com origin remote.
# At least one of --title, --body/--body-file, --base, or --state is required.
# Requires a GitHub App set up per docs/change/2026-06-20-github-app-push.md
# (app-id, installation-id, private-key.pem under ~/.config/github-app/,
# installation granted Contents:write + Pull requests:write on the repo).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib.sh"
source "${GITHUB_APP_CONFIG_DIR:-$HOME/.config/github-app}/gh-app-token.sh"

repo="" pr="" title="" body="" body_file="" base="" state=""
while [ $# -gt 0 ]; do
  case "$1" in
    --repo)      repo="$2";      shift 2 ;;
    --pr)        pr="$2";        shift 2 ;;
    --title)     title="$2";     shift 2 ;;
    --body)      body="$2";      shift 2 ;;
    --body-file) body_file="$2"; shift 2 ;;
    --base)      base="$2";      shift 2 ;;
    --state)     state="$2";     shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

[ -n "$repo" ] || repo=$(gh_app_default_repo) || { echo "--repo required (not in a github.com git repo)" >&2; exit 1; }
if [ -z "$pr" ]; then
  echo "usage: $0 --pr NUMBER [--repo OWNER/REPO] [--title TITLE] [--body TEXT | --body-file FILE] [--base BASE] [--state open|closed]" >&2
  exit 1
fi
if [ -z "$title" ] && [ -z "$body" ] && [ -z "$body_file" ] && [ -z "$base" ] && [ -z "$state" ]; then
  echo "at least one of --title, --body, --body-file, --base, or --state is required" >&2
  exit 1
fi

payload="{"
sep=""
if [ -n "$title" ]; then
  payload+=$(jq -n --arg v "$title" '"title": $v'); sep=","
fi
if [ -n "$body" ] || [ -n "$body_file" ]; then
  resolved_body=$(gh_app_resolve_body "$body" "$body_file")
  payload+="${sep}"$(jq -n --arg v "$resolved_body" '"body": $v'); sep=","
fi
if [ -n "$base" ]; then
  payload+="${sep}"$(jq -n --arg v "$base" '"base": $v'); sep=","
fi
if [ -n "$state" ]; then
  payload+="${sep}"$(jq -n --arg v "$state" '"state": $v')
fi
payload+="}"

gh_app_api_patch "repos/${repo}/pulls/${pr}" "$payload"
