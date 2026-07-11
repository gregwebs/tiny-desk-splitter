# Change: Project wrapper for /to-tickets with parent tracking issue

Status: implemented.

This change adds a project-local `to-tickets` wrapper skill. For this repo,
multi-ticket publications now create a parent tracking issue, create child
implementation issues, attempt native GitHub sub-issue links, and then update
the parent with a canonical child index.

## Context

The personal `/to-tickets` skill (`~/.agents/skills/to-tickets/SKILL.md`, symlinked at `~/.claude/skills/to-tickets`) breaks a spec/plan/conversation into tracer-bullet tickets and publishes one GitHub issue per ticket. This project wants a customization: **when more than one issue is created, also create a parent issue that (a) contains the spec the tickets were derived from and (b) has every created issue attached as a GitHub sub-issue.**

Decisions made during planning:

- Project skill named `to-tickets` (shadows the personal skill inside this repo; other repos keep the plain personal version).
- The parent issue gets **no** label (children keep `ready-for-agent` per the base skill).
- The skill must also be usable from **Codex**. The repo now carries a
  `.codex/skills/to-tickets` symlink to the shared project skill; a fresh Codex
  session can discover it if project-level skill discovery is enabled in the
  host.

The repo's GitHub automation goes through App-authenticated scripts in `scripts/github/` (see `.claude/skills/github-app/SKILL.md`). No sub-issue support exists yet, so one new script is needed.

*(Plan revised after Codex adversarial review: added issue-number extraction, parent-body child index, per-child failure tolerance, and API version header. A follow-up non-adversarial review added the missing-base-skill guard.)*

## Shipped changes

### 0. Small edit: `scripts/github/lib.sh`

Added a pinned `X-GitHub-Api-Version` header via the shared
`GH_APP_API_VERSION` constant. All three `gh_app_api_*` helpers now send the
same API version header.

### 1. New script: `scripts/github/gh-app-issue-sub-add.sh`

Links an existing issue as a sub-issue of a parent, following the sibling script
conventions (`set -euo pipefail`, source `lib.sh` + `gh-app-token.sh`, arg
loop, header comment with usage):

```
usage: gh-app-issue-sub-add.sh --parent NUMBER --child NUMBER [--repo OWNER/REPO]
```

- Resolves repo with `gh_app_default_repo` from `scripts/github/lib.sh`.
- Fetches the child's database id with
  `gh_app_api_get "repos/${repo}/issues/${child}" | jq -r '.id'`.
- Calls `gh_app_api_post "repos/${repo}/issues/${parent}/sub_issues"` with
  `{"sub_issue_id": <id>}`.
- Requires the App installation's existing Issues:write scope; no new secrets.

### 2. New project skill, shared between Claude Code and Codex

Canonical file: **`.agents/skills/to-tickets/SKILL.md`** (new tracked dir,
mirroring the personal convention where `~/.agents/skills/` is canonical and
agent dirs symlink into it). Wired up via:

- `.claude/skills/to-tickets` -> committed relative symlink to
  `../../.agents/skills/to-tickets`.
- `.codex/skills/to-tickets` -> committed relative symlink to
  `../../.agents/skills/to-tickets`.

The frontmatter mirrors the base skill (`name: to-tickets`, adapted
description, `disable-model-invocation: true`, plus Codex's
`metadata.short-description`; unknown keys are ignored by each tool). The body
is a thin, agent-neutral wrapper, not a copy:

1. **Delegate**: "Read `~/.agents/skills/to-tickets/SKILL.md` (the base skill's canonical path, valid from any agent) and follow its full process (gather context → draft vertical slices → quiz the user → publish), with the project-specific publishing changes below." No duplication of the base process, so upstream edits flow through.
   - **Guard**: if that base file doesn't exist on this machine, stop and tell the user the personal `to-tickets` skill is missing rather than improvising the process. The wrapper intentionally depends on the personal skill (a wrapper, not a vendored copy). Alternative if zero machine dependency is preferred: vendor the base skill's process into the repo file — noted as a trade-off, not the default.
2. **Publishing overrides (replaces base step 5 for this repo):**
   - All issues are created with `./scripts/github/gh-app-issue-create.sh`, bodies via `--body-file` from a temp file (never `--body`; in Claude Code write the file with the Write tool to `$TMPDIR` — the sandbox mangles `!` in inline shell strings), per the existing `/github` skill conventions. Keep all tool references agent-neutral so the same instructions work under Codex.
   - **Issue-number extraction**: the create script prints only the `html_url`. After each create, extract the issue number strictly from that URL (must match `https://github.com/<owner>/<repo>/issues/<digits>` for this repo's origin); if extraction fails, stop and report rather than passing a garbage number onward.
   - **If the approved breakdown has 2+ tickets:** first create a **parent tracking issue** — title is the short name of the work; body is the spec the tickets were derived from (the referenced spec/issue content verbatim, or a synthesized spec if working purely from conversation), prefixed with a one-line note that this is a tracking issue whose sub-issues are the implementation tickets; **no labels**. Then create each child issue in dependency order as the base skill directs (`ready-for-agent` label, base issue template with the `## Parent` section referencing the parent issue number), and after each child is created attempt `./scripts/github/gh-app-issue-sub-add.sh --parent N --child M`.
   - **Linking is failure-tolerant**: a failed sub-issue link (403/422/rate limit) must not abort the publish — record the failure, keep creating the remaining children, and report link outcomes at the end. Native sub-issue links are an enhancement, not the source of truth.
   - **Parent child-index (canonical, always)**: after all children are created, update the parent with `./scripts/github/gh-app-issue-update.sh --issue N --body-file …`, re-sending the full body (spec + an appended `## Implementation tickets` section listing every child URL in dependency order, noting any that failed to link natively). This makes the parent complete even when native linking partially fails, and closes the gap that the parent is created before its children exist.
   - **If only 1 ticket:** no parent issue; behavior identical to the base skill.
   - Relay all created issue URLs back to the user, parent first, including any sub-issue link failures.

### 3. Doc updates

- `.claude/skills/github-app/SKILL.md`: added a row to the action table:
  "Link sub-issue | `gh-app-issue-sub-add.sh` |
  `--parent NUMBER --child NUMBER`".
- This file: updated from plan to shipped-change description, including the
  Codex wiring note.

## Files

| File | Action |
|---|---|
| `scripts/github/lib.sh` | edit (pinned API version header in the 3 helpers) |
| `scripts/github/gh-app-issue-sub-add.sh` | new (executable) |
| `.agents/skills/to-tickets/SKILL.md` | new (canonical skill) |
| `.claude/skills/to-tickets` | new committed symlink → `../../.agents/skills/to-tickets` |
| `.codex/skills/to-tickets` | new committed symlink → `../../.agents/skills/to-tickets` |
| `.claude/skills/github-app/SKILL.md` | edit (one table row) |
| `docs/change/2026-07-11-to-tickets-parent-issue.md` | this plan, updated after implementation |

## Verification

Completed locally:

1. `shellcheck scripts/github/gh-app-issue-sub-add.sh scripts/github/lib.sh`
   passed.
2. Confirmed the project skill symlinks resolve to
   `.agents/skills/to-tickets`.
3. Confirmed `scripts/github/gh-app-issue-sub-add.sh` is executable.

Not run automatically:

1. Live GitHub API test creating throwaway issues, linking one under the other,
   verifying `repos/.../issues/{parent}/sub_issues`, and closing both issues.
   This is outward-facing and should only run after explicit confirmation.
2. Runtime skill resolution in Claude Code and Codex. The committed symlinks are
   present, but each host may cache skill discovery until a fresh session.
