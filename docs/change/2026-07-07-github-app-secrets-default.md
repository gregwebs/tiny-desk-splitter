# GitHub App secrets directory default

## Problem

`scripts/github/gh-app-token.sh` documented `$HOME/.config/github-app` as the
default secret directory, but one read still referenced `GITHUB_APP_SECRETS_DIR`
directly. With `set -u` and the environment variable unset, GitHub operations
failed before minting a token.

## Change

The token helper now resolves `secrets_dir` once with:

```sh
${GITHUB_APP_SECRETS_DIR:-$HOME/.config/github-app}
```

Both `client-id` and `installation-id` are read from that resolved directory.
This preserves the override behavior while making the documented default work.

## Verification

- `/bin/bash -c 'source scripts/github/gh-app-token.sh && gh_app_token | wc -c'`
- `./scripts/shellcheck.sh`
- `just lint`
