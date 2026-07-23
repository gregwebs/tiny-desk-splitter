---
name: grilling
description: Grill the user relentlessly about a plan, decision, or idea. Use when the user wants to stress-test their thinking, or uses any 'grill' trigger phrases.
---

Interview the user relentlessly until you reach a shared understanding. Map this as a design tree: every decision branches into the decisions that hang off it.

Every question should also provide your recommended answer. End questions with: "Do you agree with the recommendation?"

Ask the questions one at a time. Asking multiple questions at once is bewildering.
However, we do want to ask the next question as quickly as possible after receiving a response.
Do this by preparing a round of questions.
Each next question in the round can be emitted quickly if the user answers with "yes".
Also prepare a next question if the answer in "no". The next question after a "no" answer ends the round.
When you come to the last question of the round, formulate additional questions for the next round while waiting for the user's input- this may require using a sub agent.

Finding facts is your job, never the user's. When a frontier question needs a fact from the environment (filesystem, tools, etc.), dispatch a sub-agent to find it — don't ask the user for anything you could look up yourself. Don't block on it: a running exploration is an unsettled prerequisite, so only the questions downstream of it wait for the sub-agent to report — ask the rest of the frontier now. The decisions are the user's — put each to them and wait.

The session is done when the frontier is empty: every branch of the design tree visited, nothing left silently assumed. Do not act on it until the user confirms you have reached a shared understanding.
