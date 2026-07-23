# Fix flaky Playwright track-list Like-button interaction (issue #135)

## Status

Complete. Fix applied, reproduction/verification loops run, all Playwright
suites in the file passing.

## Trigger

The `playwright` CI job for PR #133 failed once and needed a manual rerun.
The failing test was `e2e/player-queue.spec.js:1280` — "track list: starring
the playing track from the player hides its row delete button" — timing out
while clicking a track-list Like button. Playwright's error trace showed the
parent `<li>` intercepting pointer events, then the htmx-swapped button
detached from the DOM. The rerun passed 172/172.

- Workflow run: https://github.com/gregwebs/tiny-desk-splitter/actions/runs/29741539884
- Filed as issue #135, asking for investigation of the htmx swap's
  synchronization/interaction seam, treated as a flaky-test investigation
  (explicitly unrelated to the #126 split-job changes also in that PR).

## Root cause

This is a **test-actionability race, not a UI/product defect**.

- The track-list Like button (`concert-tracker/templates/like_button.html`)
  is rendered `hx-post=".../like" hx-target="this" hx-swap="outerHTML"` — every
  toggle click detaches and replaces the button element once the POST
  response lands.
- The row lives inside the hover-revealed `.card-tracks-box` in the card
  list. `#player-bar` is `position: fixed; bottom: 0; z-index: 1000`
  (`concert-tracker/static/style.css:478`) and can overlap the bottom rows of
  that list.
- Playwright's real `.click()` performs pointer hit-testing plus an
  actionability auto-retry loop. Under the sandboxed/CI `--single-process`
  Chromium (`e2e/sandbox.js`), the hit-test can resolve to the parent `li`
  (or the fixed player bar) instead of the button ("intercepted pointer
  events"), and a subsequent retry can resolve against the button just
  before — or, worse, land its dispatch just after — the outerHTML swap has
  detached that exact node ("element is detached from DOM"). Both failure
  strings in the CI trace match this mechanism precisely.
- The behavior itself is deterministic: the server re-renders the swapped
  button with the correct `liked` class, and delete-button visibility is
  driven purely by CSS — `.track-list li:has(.btn-like.liked) .btn-delete {
  display: none }` (`style.css:432`). The htmx `detail.elt` reverse-sync race
  (issue #30) was already fixed and is unrelated here.

**Existing idiom in this file already documents and works around exactly this
race**, on the very same button, at two other call sites: `.evaluate(el =>
el.click())`, which dispatches a synthetic click directly on the node —
bypassing pointer hit-testing and the actionability retry loop entirely — and
is not invalidated by the node being detached and replaced moments later
(`player-queue.spec.js:1088-1090`, `:1114-1118`; also `:124-129` for a
different fixed-overlay button).

### Note on the issue's cited test name/line

Issue #135 names line `:1280` and the test
`"...starring the playing track from the player hides its row delete
button"`. In the current tree that test clicks `#player-like` (the fixed
player bar, always on top, not interceptable) and asserts via `expect.poll`,
so it cannot produce a "parent li intercepted pointer events" error. That
error text unambiguously matches a click on `trackListLikeButton(...)`
(inside the hover-revealed card list), which in the current tree exists only
in the sibling test `"track list: a row's delete visibility tracks its own
star (htmx button swap)"` at lines 1312 and 1315. The issue's line/test-name
reflect PR #133's file revision at CI time; this change targets the actual
real-pointer-click sites in the current tree, which are the only two
`trackListLikeButton(...).click()` calls left unconverted to the established
`evaluate` idiom.

## Fix

`e2e/player-queue.spec.js`, test `"track list: a row's delete visibility
tracks its own star (htmx button swap)"` (lines ~1298-1319): converted both
real `.click()` calls on `trackListLikeButton(page, AUDIO, 0)` to
`.evaluate((el) => el.click())`, matching the pattern already used elsewhere
in this file for the same button, with a comment explaining the fixed-bar
interception and htmx-swap detach race (referencing issue #135).

No production code changed — the htmx swap and CSS `:has()` visibility rule
are already correct and deterministic.

## Reproduction attempt (Phase 1 feedback loop)

```
npx playwright test player-queue.spec.js \
  -g "a row's delete visibility tracks its own star" --repeat-each=150
```
→ 150/150 passed, no flake, ~3.6 min.

Raised to `--repeat-each=400` to push reproduction odds further (CPU-stress
amplification via background processes was attempted but is blocked by the
sandbox's process-management restrictions — `nice`/process-listing calls fail
— so repeat-count alone was used).

Result: `400 passed (7.9m)`, zero failures. Combined with the initial 150-rep
run, **550/550 reps passed** pre-fix on this machine.

The flake reported in CI was a single occurrence out of 172 tests on a shared
GitHub Actions runner — a low base rate consistent with not reproducing
reliably even at 550 repeats on a dedicated, less-contended machine. Per the
diagnosing-bugs skill, this is the documented finding: an unreproduced but
well-understood race (root cause traced from the CI error text, the htmx
`hx-swap="outerHTML"` markup, the fixed-overlay CSS, and Chromium
actionability semantics), with a confirmed, pre-existing fix idiom already
used twice elsewhere in this exact file for this exact button. The regression
guard is the repeated-run loop passing cleanly post-fix (below), not a clean
red→green repro.

## Verification

- [x] Pre-fix: `--repeat-each=150` then `--repeat-each=400` — 550/550 passed
      (did not reproduce locally; see above).
- [x] Post-fix: `--repeat-each=400` — **399/400 passed, 1 failed**. The single
      failure (`repeat327`) was `TimeoutError: browserType.launch: Timeout
      180000ms exceeded` inside `beforeEach`'s `page.goto("/")`, before any
      click executed — a Chromium single-process launch timing out, not the
      pointer-interception/htmx-detach race this fix targets. This is
      environmental resource exhaustion from ~950 consecutive
      `--single-process` Chromium launches back-to-back in this sandbox over
      ~25 minutes (the same class of contention `playwright.config.js`
      already documents as the reason `workers: 1` is forced in-sandbox), not
      a regression from the fix. Re-verified with a clean, shorter run below.
- [x] Post-fix clean re-run: `--repeat-each=150` — **150/150 passed** in
      2.6 min, zero failures of any kind.
- [x] `npx playwright test player-queue.spec.js -g "delete button"` — 6/6
      passed (8.7s).
- [x] Full `npx playwright test player-queue.spec.js` — **77/77 passed**
      (56.1s). No collateral breakage in the file.
- [x] `/code-review` on the diff — see Review section below.

## Review

Ran `/code-review` (Standards + Spec axes) against `git diff main -- \
e2e/player-queue.spec.js` and this file. The `/codex:rescue` path
(CLAUDE.md's default for read-only subagent work) failed both attempts with
a sandbox `EPERM` writing Codex's job log
(`.../codex-openai-codex/state/.../jobs/task-*.log`) — a known Codex/sandbox
limitation already documented elsewhere in this repo's change-record history
(same fallback used in `2026-07-21-fix-couldnt-load-next-track-false-error.md`).
Fell back to the project's standard Claude-subagent review path.

**Spec axis:** No missing requirements, no scope creep. Independently
re-verified the line/test-name discrepancy claim (confirmed the issue's cited
test only clicks `#player-like`, the fixed bar, and cannot produce the CI
error; the real `trackListLikeButton(...).click()` sites pre-fix were only
at `:1312`/`:1315`) and confirmed the fix targets the correct code path. One
disclosed limitation flagged: the race was never empirically caught red
(0/550 pre-fix, 399/400 post-fix with the one failure being an unrelated
infra timeout — see Verification), so confidence in the fix rests on matching
an already-proven idiom rather than a clean red→green repro. Judged
acceptable given the low CI base rate (1/172) makes a clean repro unlikely
regardless of effort, and the mechanism is independently corroborated by the
htmx markup, CSS, and Chromium actionability semantics — not fix-blocking.

**Standards axis:** No hard violations of `CODING_STANDARDS.md` or
`CONTRIBUTING.md`. One judgement call raised (Duplicated Code: the rationale
comment is now a third near-identical restatement across call sites in this
file) — noted as a defensible, already-documented deferral per this file's
own Follow-up section and `CODING_STANDARDS.md`'s DRY caution ("wait for 3
concrete instances... if the value initially appears low"), not an
oversight. Change Record tone/structure confirmed consistent with existing
`docs/change/` conventions. One real inconsistency caught: this file's
`Status: Complete` line was contradicted by an unchecked `[ ] /code-review`
item at the time of review — fixed by the edit you're reading now.

No changes to the fix itself were required by either axis.

## Follow-up (not implemented here)

Every track-list button click in this file should use the `evaluate` idiom
by default to avoid this recurring foot-gun; a small `clickButton(locator)`
test helper could enforce that at the type/lint level. Not added in this
change since only two sites remained non-conforming and a broader refactor
was out of scope for a flaky-test fix.
