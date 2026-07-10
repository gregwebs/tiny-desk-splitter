# Add GitHub App issue update script

## What changed

Added `scripts/github/gh-app-issue-update.sh`, a small GitHub App-authenticated
wrapper around the existing shared `gh_app_api_patch` helper.

The script updates existing issues by number and supports:

- `--title`
- `--body` or `--body-file`
- `--state open|closed`
- repeated `--label` arguments
- `--clear-labels`

Passing labels replaces the issue's full label set, matching GitHub's issue
update API semantics. `--clear-labels` makes that replacement explicit when an
issue should have no labels.

## Why

Issue bodies are the canonical source for ticket specifications. When a ticket
needs corrected acceptance criteria or dependency notes, updating the issue body
keeps the actionable instructions in one place instead of splitting them across
comments.

## Verification

- `bash -n scripts/github/gh-app-issue-update.sh`
