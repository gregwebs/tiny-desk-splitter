@AGENTS.md

## Agent Review

When running from Claude Code, if the codex plugin is installed, use Codex for review as defined below.
If the codex plugin is installed but not working (probably needs re-authentication), stop and ask the user if they want to fix this.

Otherwise ask the engineering-lead agent for a review.

### Codex review

Use /codex:adversarial-review to question the plan/implementation.
* Plan/spec review. Pass the plan/spec as text, starting with "Review the following plan/spec". If the review suggests a good alternative approach that is significantly different, ask the user to choose between them, and recommended a choice.
* Code review: only ask the user to help resolve review issues if a resolution would end up altering the plan/spec.

After making changes based on the adversarial review, perform a non-adversarial review using /codex:review.

Do not substitute Claude's own review for these checkpoints.
Do not invoke Codex for trivial documentation or formatting-only changes.

Use the engineeering-lead in .claude/agents/engineering-lead.md as the persona for the review.