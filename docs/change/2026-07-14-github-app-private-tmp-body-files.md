# GitHub App private tmp body files

The GitHub App workflow now documents the Codex-specific way to create pull
request, issue, and comment body files: write them directly under
`/private/tmp` with `exec_command`, then pass the path with `--body-file`.

This avoids using repository patch tooling for temporary files outside the
workspace, which can trigger unnecessary approval prompts even though
`/private/tmp` itself is writable.
