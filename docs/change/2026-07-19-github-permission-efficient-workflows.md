# Permission-efficient GitHub workflows

## Purpose

Repeated GitHub operations were using command forms that prevented narrow,
persistent sandbox approval rules from matching. In particular, callers had to
prepend a Homebrew curl directory to `PATH`, and CI monitoring was performed
without a dedicated workflow skill.

This change makes the stable repo scripts self-contained and documents the
command boundaries an agent should use. It does not weaken the sandbox or grant
permissions: the user still controls persistent approvals.

## Implementation plan

- [x] Make GitHub App authentication choose the host-compatible curl internally.
- [x] Remove the need for environment-prefixed GitHub commands.
- [x] Add one literal GitHub App dispatcher prefix for persistent approval.
- [x] Authenticate CI polling when App credentials are available and back off
  anonymous polling.
- [x] Make the GitHub API helper library source its token dependency directly.
- [x] Extend the GitHub App skill with read, push, and permission-efficient
  command guidance.
- [x] Add a GitHub Actions CI skill around the existing check-run helper.
- [x] Validate the new skill and both changed shell scripts.
- [x] Exercise token minting, issue reading, and CI status without a `PATH`
  prefix.
- [x] Review the changes.

## Permission state

```text
user authorizes GitHub task
          |
          v
stable repo dispatcher command
          |
          v
matching persistent prefix approval? -- no --> one narrow approval prompt
          | yes                                |
          v                                    |
network call runs <----------------------------+
          |
          v
future matching calls run without a new prompt
```

Skills select stable commands and request the smallest appropriate boundary.
They cannot create or broaden sandbox approval rules themselves.

## Canonical documentation

- `.claude/skills/github-app/SKILL.md` owns GitHub App read/write workflow.
- `.claude/skills/github-actions-ci/SKILL.md` owns CI monitoring workflow.
- `docs/playwright.md` remains canonical for Playwright CI and host failures.

## Approval audit

| Previous prompt source | Durable path |
|---|---|
| Raw GitHub API curl for issue reads | `gh-app.sh issue-get` |
| Environment-prefixed App API commands | `gh-app.sh COMMAND` |
| Environment-prefixed authenticated push | plain `git push` |
| CI status probes followed by escalated retries | start with `check-ci-runs.sh` at its persisted network boundary |
| Repeated local Chromium escalation after host `SIGTRAP` | record the host limitation and use Playwright CI |
| Git metadata writes (`fetch`, `switch`, `merge`, `add`, `commit`) | existing narrow persisted git prefixes |

## Verification results

- `bash -n scripts/github/*.sh scripts/check-ci-runs.sh` passed.
- `/opt/homebrew/bin/bash scripts/shellcheck.sh` passed.
- The new `github-actions-ci` frontmatter passed the skill-creator validator's
  structural rules using the repository's installed YAML parser. The official
  Python validator could not start because its environment lacks `PyYAML`.
- `gh_app_curl --version` resolved Homebrew curl 8.21.0 without a `PATH`
  override.
- `gh-app.sh issue-get --issue 130` minted an App token and read the issue
  without a `PATH` override.
- `check-ci-runs.sh --job playwright origin/main` reported the successful job
  without a `PATH` override.
- `PATH=/opt/homebrew/bin:$PATH just lint` passed Rust formatting, Clippy,
  ShellCheck, TypeScript checking, and oxlint.
- Initial adversarial engineering-lead review found approval-prefix,
  rate-limit, and source-order problems; the non-adversarial follow-up review
  approved their corrections with no remaining findings.
