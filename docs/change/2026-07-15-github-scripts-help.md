# GitHub App Script Help

The GitHub App helper scripts under `scripts/github/` now accept `--help` and
`-h` without requiring repository discovery, credentials, or GitHub API access.

This includes the issue, pull request, credential-helper, token, and shared
library script entry points. The issue read helper
`scripts/github/gh-app-issue-get.sh` is included with the same help behavior.

Verification:

- `shellcheck scripts/github/*.sh`
- `scripts/github/*.sh --help`
