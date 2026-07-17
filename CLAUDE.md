@AGENTS.md

## Spawning Subagents

For any read only tasks, spawn subagents using /codex:rescue rather than a normal claude subagent spawn.
The Agent Review is documented below, but agents analyzing the codebase should also use this.

## Addendum: Agent Review

When running from Claude Code, if the codex plugin is installed, use Codex for review as defined below.
If the codex plugin is installed but not working (probably needs re-authentication), stop and ask the user if they want to fix this.

Otherwise ask the engineering-lead agent for a review.

### Codex review

The codex:rescue subagent can be invoked but will likely fail due to sandbox nesting. Instead you can invoke:

```
node $HOME/.claude/plugins/cache/openai-codex/codex/1.0.5/scripts/codex-companion.mjs task --cwd "$(pwd)"
```

If your code is already committed, add a base argument:  --base main

Pass the instructions from the codex:adversarial-review skill to the agent.
* Plan/spec review. Pass the plan/spec as --prompt-file, starting with "Review the following plan/spec".
  If the review suggests a good alternative approach that is significantly different, ask the user to choose between them, and recommended a choice.
* Code review: Pass the plan/spec for the changes. If there is only a conversation to go off of, summarize the conversation to a spec.
  Only ask the user to help resolve review issues if a resolution would end up altering the plan/spec.

If changes are made based on the adversarial review, perform a non-adversarial review using the same procedure as above but passing instructions from the codex:review skill rather than codex:adversarial-review.

Do not substitute Claude's own review for these checkpoints.
Do not invoke Codex for trivial documentation or formatting-only changes.

Use the engineeering-lead in .claude/agents/engineering-lead.md as the persona for the review.
