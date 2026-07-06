# Push and PR/issue automation via a GitHub App

## Summary

Git pushes, pull request creation, issue creation, and issue/PR comments from
this machine now authenticate as a GitHub App installation (`greg-weber-claude-agent`,
App ID `4103109`) instead of a personal GitHub account. This is local tooling
(not part of the deployed application) but the API-calling scripts are
repo-tracked so the pattern is reusable by anyone who sets up their own App
credentials.

## Why

A GitHub App's installation token is short-lived (1 hour), scoped to exactly
the permissions granted to the installation, and revocable independent of any
human account - a better fit for automated/agent-driven git and API operations
than a personal access token.

## Credentials (not tracked in this repo)

```
~/.config/github-app/
├── app-id                 # plain text App ID
├── private-key.pem        # App private key, chmod 600
├── gh-app-token.sh         # shared lib: signs a JWT, mints an installation
│                           # access token (~9 min expiry). Provides gh_app_token().
└── credential-helper.sh   # git credential helper, chmod 700, sources gh-app-token.sh
```

`~/.config/github-app/` is `chmod 700`; secret files within it are `chmod 600`.
None of this directory is part of the git repo.

## Git push

This repo's `.git/config` (local, not tracked) points `credential.https://github.com`
at `credential-helper.sh` and sets `username = x-access-token`, overriding the
global `osxkeychain` helper for `github.com` in this repo only. Every `git push`
mints a fresh token - nothing is cached or can go stale.

Commit authorship is set separately via local `user.name`/`user.email`:
```
greg-weber-claude-agent[bot] <4103109+greg-weber-claude-agent[bot]@users.noreply.github.com>
```
This is the standard GitHub bot-identity format (matches `dependabot[bot]` etc.)
and makes GitHub render commits/PRs with the App's bot badge. It applies to
future commits only in this repo; it does not rewrite existing history.

## PR / issue scripts (`scripts/github/`)

Three thin wrappers over the GitHub REST API, all minting their own token via
`gh_app_token()` (sourced from `~/.config/github-app/gh-app-token.sh`):

- `gh-app-pr-create.sh --base BASE --head HEAD --title TITLE [--repo OWNER/REPO] [--body TEXT | --body-file FILE]`
- `gh-app-issue-create.sh --title TITLE [--repo OWNER/REPO] [--body TEXT | --body-file FILE] [--label LABEL]...`
- `gh-app-issue-comment.sh --issue NUMBER [--repo OWNER/REPO] (--body TEXT | --body-file FILE)`

`--repo` defaults to the current directory's `github.com` origin remote
(`scripts/github/lib.sh:gh_app_default_repo`). All three print the resulting
`html_url` on success.

These call the REST API directly with `curl`/`jq` rather than the `gh` CLI:
in this project's sandboxed dev environment, `gh` fails TLS verification
against `api.github.com` (`x509: OSStatus -26276`, a Go/macOS Security
framework interaction) even though `curl` succeeds against the same host.
`gh` works normally outside that sandbox.

## Required App permissions

The installation needs, at minimum: `Contents: write` (push), `Pull requests: write`
(PR create/comment), `Issues: write` (issue create/comment - also gates PR
*conversation* comments, since GitHub treats those as issue comments). Verify
via `GET /app/installations` (lists granted `permissions`) and confirm the
target repo appears in `GET /installation/repositories` for that installation.

Note: editing an App's declared permissions in its settings page is not
enough by itself - the installation owner must separately accept the
upgraded permission grant (a prompt/notification under
`https://github.com/settings/installations`) before tokens actually carry
the new scope. `Issues` started as read-only here and required this second
acceptance step before comments worked (see Verification below).

## Verification performed

- `credential-helper.sh get` mints a token; `git ls-remote` and a real
  `git push origin foldkit-splitter-spike` succeeded end-to-end.
- `gh_app_default_repo` correctly resolves `gregwebs/tiny-desk-splitter` from
  this repo's origin remote.
- PR #1 was opened against this repo via the same token-minting approach
  (initially inline, then extracted into `gh-app-pr-create.sh`).
- `bash -n` syntax-checked on all four `scripts/github/*.sh` files.
- `gh-app-issue-create.sh` opened issue #2 as a live test.
- `gh-app-issue-comment.sh` initially failed on both issue #2 and PR #1 with
  `403 Resource not accessible by integration` - the installation's `issues`
  permission was `read`, not `write`. After granting `Issues: write` in the
  App settings and accepting the upgraded permission grant for the
  installation, both comments succeeded:
  issue #2's [comment](https://github.com/gregwebs/tiny-desk-splitter/issues/2#issuecomment-4760288332)
  links to PR #1; PR #1's [comment](https://github.com/gregwebs/tiny-desk-splitter/pull/1#issuecomment-4760288521)
  links back to issue #2.
