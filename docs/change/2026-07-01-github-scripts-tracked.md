# Move GitHub App scripts into the tracked repo; keep only two secrets local

## Motivation

Per `docs/change/2026-06-20-github-app-push.md`, `gh-app-token.sh` and
`credential-helper.sh` lived entirely in `~/.config/github-app/` (untracked),
alongside the App's `app-id`, `installation-id`, and `private-key.pem`. That
meant the actual token-minting and git-credential-helper *logic* wasn't
version controlled — only the thin `gh-app-pr-create.sh`/etc. wrapper scripts
that sourced it were.

Separately, while debugging a broken `git push` this session, the JWT `iss`
claim in `gh-app-token.sh` turned out to need quoting as a JSON string, and
the App's `app-id` file needed to hold the client ID rather than the numeric
App ID. Both were fixed directly in the untracked copy, which highlighted the
problem: a fix to shared logic living outside the repo has nowhere to be
recorded or reviewed.

## What changed

All GitHub-interaction code now lives in `./scripts/github/` (tracked). Only
the two actual secrets stay outside the repo, in `~/.config/github-app/`
(unchanged location, `chmod 700`/`600`, not tracked):

- `client-id` — the App's client ID, used as the JWT `iss` claim (renamed
  from the old `app-id`, which held the numeric App ID before this session's
  fix switched to the client ID)
- `private-key.pem` — the App's private key, signs the JWT

The installation ID is **not** a secret — it's just an opaque identifier for
this repo's installation of the App, not usable without the private key — so
it's now committed as `./scripts/github/installation-id`.

- `scripts/github/gh-app-token.sh` (new, tracked) — moved from
  `~/.config/github-app/gh-app-token.sh`. Reads `client-id` and
  `private-key.pem` from `$GITHUB_APP_SECRETS_DIR` (renamed from
  `GITHUB_APP_CONFIG_DIR`; default unchanged: `~/.config/github-app`), reads
  `installation-id` from its own directory. Includes this session's fix: the
  JWT `iss` claim is quoted as a JSON string
  (`'{"iat":%d,"exp":%d,"iss":"%s"}'`, was unquoted before).
- `scripts/github/credential-helper.sh` (new, tracked) — moved from
  `~/.config/github-app/credential-helper.sh`; sources the now-local
  `gh-app-token.sh` via `$SCRIPT_DIR` instead of `$GITHUB_APP_CONFIG_DIR`.
- `scripts/github/gh-app-pr-create.sh`, `gh-app-pr-update.sh`,
  `gh-app-issue-create.sh`, `gh-app-issue-comment.sh` — each now sources
  `"$SCRIPT_DIR/gh-app-token.sh"` instead of
  `"${GITHUB_APP_CONFIG_DIR:-$HOME/.config/github-app}/gh-app-token.sh"`.
- `scripts/github/lib.sh` — doc comment updated to point at the local
  `gh-app-token.sh` instead of the old `~/.config/github-app/` path.
- This repo's local (untracked) `.git/config` now points
  `credential."https://github.com".helper` at
  `scripts/github/credential-helper.sh` (absolute path) instead of the old
  `~/.config/github-app/credential-helper.sh`.
- `.claude/skills/github/SKILL.md` — "Setup reference" section rewritten to
  describe the new split (tracked scripts vs. two local secrets) and point
  at `GITHUB_APP_SECRETS_DIR`.

`docs/change/2026-06-20-github-app-push.md` is left as-is (a snapshot of the
original setup); this doc supersedes it as the current reference.

## Now-unused files

These can be deleted from `~/.config/github-app/` (superseded by the tracked
copies above): `app-id`, `installation-id`, `gh-app-token.sh`,
`credential-helper.sh`. Keep `client-id` and `private-key.pem`.

## Verification

- `bash -n` on every script in `scripts/github/` — all pass.
- Sourced the new `scripts/github/gh-app-token.sh` directly and called
  `gh_app_token()` — successfully minted a token (verified by length only,
  never printed).
- Ran the new `scripts/github/credential-helper.sh get` end-to-end (stdout
  redacted before inspection) — returned `username=x-access-token` and a
  non-empty `password=`.
- Re-pointed local `.git/config` at the new script and ran `git ls-remote
  origin HEAD` — authenticated successfully and returned the current HEAD sha.

## Files changed

- `scripts/github/gh-app-token.sh` (new)
- `scripts/github/credential-helper.sh` (new)
- `scripts/github/installation-id` (new)
- `scripts/github/gh-app-pr-create.sh`, `gh-app-pr-update.sh`,
  `gh-app-issue-create.sh`, `gh-app-issue-comment.sh` — source path updated
- `scripts/github/lib.sh` — comment updated
- `.claude/skills/github/SKILL.md` — setup reference rewritten
