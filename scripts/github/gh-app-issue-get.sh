#!/usr/bin/env bash
# Read an issue, authenticated as a GitHub App installation.
#
# Usage: gh-app-issue-get.sh --issue NUMBER [--repo OWNER/REPO]
#
# --repo defaults to the current directory's github.com origin remote.
# Requires a GitHub App set up per docs/change/2026-06-20-github-app-push.md
# (client-id, private-key.pem under ~/.config/github-app/, not tracked).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/lib.sh"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/gh-app-token.sh"

usage() {
  echo "usage: $0 --issue NUMBER [--repo OWNER/REPO]"
}

repo="" issue=""
while [ $# -gt 0 ]; do
  case "$1" in
    --help|-h) usage; exit 0 ;;
    --repo) repo="$2"; shift 2 ;;
    --issue) issue="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

[ -n "$repo" ] || repo=$(gh_app_default_repo) || { echo "--repo required (not in a github.com git repo)" >&2; exit 1; }
if [ -z "$issue" ]; then
  usage >&2
  exit 1
fi

gh_app_api_get "repos/${repo}/issues/${issue}"
