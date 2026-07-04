# Fix player: track-number text, queue-badge visibility/tooltip, video minimize-button idle timer

Fixes #32.

## Summary

Issue #32 listed 8 `e2e/player-queue.spec.js` tests failing on unmodified `main`, never
individually root-caused, with a note that they might share a cause with #28's keyboard-shortcut
drift. They don't: three independent bugs, all introduced by the Foldkit player port (`2e570373`,
#21), account for all 8. All three are fixed here; verified against the full `player-queue.spec.js`
suite (73/75 pass — the 2 remaining failures are confirmed pre-existing and unrelated, see below).

## Bug A: `#player-track` renders `"1."` instead of the pre-Foldkit `"#1"`

Old `player.ts`'s `updateInfo()`: `n.textContent = "#" + (trackIdx + 1)`. The port changed this to
`` `${p.trackIdx + 1}.` `` in `view.ts`, and the widget's own Scene test was written against the
regressed text (`toContainText("1.")`) rather than catching the drift.

**Fix:** restored `` `#${p.trackIdx + 1}` `` in `view.ts`; corrected the Scene test's assertion.

## Bug B: queue badge never becomes visible, never gets a tooltip

Old `updateQueueBadge()` toggled `badge.style.visibility` between `"visible"`/`"hidden"` and set
`badge.title` to the queued titles joined by `\n` (empty when the queue is empty). The CSS
baseline is `visibility: hidden` (deliberately visibility-not-display, so the bar layout never
shifts when a track is enqueued). The port's view only ever rendered the count text — never
touched `visibility` or `title` — so the badge was permanently invisible and tooltip-less.

**Fix:** the badge span now sets `h.Style({ visibility: ... })` and `h.Title(...)` from
`model.queue`, mirroring the old two-branch behavior exactly. Extended the three existing Scene
tests for the badge with visibility and title assertions.

Note for future test-writers: Foldkit's Scene `toBeVisible` matcher only inspects `vnode.data`
(explicit style/hidden/aria-hidden) — it has no knowledge of a CSS-baseline rule like this badge's
`visibility: hidden`. So the *positive* case (non-empty queue → visible) can't be driven red by a
missing fix the way the *negative* case (empty queue → not visible) and the `title` assertions can;
the e2e test is the real acceptance check for the positive case.

## Bug C: video-controls-idle subscription was never ported (missing feature)

The whole "reveal the minimize button on mouse movement over the open video panel, fade it after
2.5s idle" feature from old `player.ts` (`showVideoControls()`/`videoControlsTimer`) was never
carried into the Foldkit widget — `subscription.ts`'s own header comment still flagged it as
pending. `VIDEO_CONTROLS_IDLE_MS` was already ported to `core.ts` but sat unused.

**Fix, three parts:**
- `subscription.ts` gains `attachVideoControlsIdle(panel)`: attaches `mousemove`/`touchstart`
  directly on `#player-video-panel`, adds `controls-visible` on activity, and (re)starts a
  `VIDEO_CONTROLS_IDLE_MS` timeout that removes it. Returns a cleanup that detaches both
  listeners, cancels the pending timer, and removes the class.
- A new `videoControlsIdle` subscription entry, gated on a `{ videoOpen: S.Boolean }` model
  dependency exactly like the existing `outsideVideo`/`keyboard` entries: Foldkit tears down and
  re-acquires the stream whenever the panel opens/closes, so the listeners and timer only exist
  while the panel is open — the old always-attached-with-a-runtime-guard approach is unnecessary
  here. No new Message or model field: this is transient CSS-only state, exactly like
  `sidebarResize`'s inline `document.body.classList` mutation.
- `command.ts`'s `HideVideoPanel` now also removes `controls-visible` (parity with old
  `hideVideoPanel()`'s cleanup), so the minimize button can't linger revealed across a
  close/reopen — this runs deterministically on every close path, independent of
  subscription-teardown timing.

### State diagram

```
                 ┌──────────────────────────────────────────────────────┐
                 │ PANEL CLOSED  (model.video.open = false)             │
                 │ no listeners attached · no timer · class absent      │
                 └──────────────────────────────────────────────────────┘
   ShowVideoPanel/open=true│                        ▲ HideVideoPanel/open=false
   (subscription acquires: │                        │ (Escape, outside click, Watch
    listeners attach)      ▼                        │  toggle → command removes
                 ┌──────────────────────────┐        │  controls-visible; subscription
        ┌───────►│ OPEN · CONTROLS HIDDEN   │────────┤  release clears listeners+timer)
        │        └──────────────────────────┘        │
        │ timer fires (2500ms)   │ mousemove/touchstart:
        │                        │ add class, start timer
        │                        ▼
        │        ┌──────────────────────────┐
        └────────│ OPEN · CONTROLS VISIBLE  │────────┘
                 │                          │◄──┐ mousemove/touchstart:
                 └──────────────────────────┘   │ clear+restart timer
                              └──────────────────┘
```

## Out of scope (confirmed pre-existing, unrelated)

Two `player-queue.spec.js` failures remain, present identically before and after this change:
- "Space in an interactive control does not trigger the global pause shortcut" — `#player-seek`
  is `h.Disabled(true)` pending a later commit (real-time currentTime/duration sync), so
  Playwright's `.focus()` on it silently no-ops; unrelated to this fix or to #28.
- "a failing delete keeps playback going and shows an error" — a timing issue unrelated to any
  file this change touches.

## Verification

- `npx vitest run` (concert-tracker/frontend) — 219 green, including 7 new
  `attachVideoControlsIdle` unit tests (fake timers, no mocks — one specifically proves the idle
  timer is cancelled on cleanup, not just that cleanup happens to remove the class once) and 2 new
  `HideVideoPanel` command tests, plus the corrected/extended Scene tests for Bugs A and B.
- `npm run check` / `npm run lint` / `just lint` — all clean.
- `npx playwright test e2e/player-queue.spec.js` — 73/75 pass; all 8 tests originally listed in
  #32 pass, including with the full file run (not just in isolation). The 2 remaining failures are
  the confirmed pre-existing/unrelated ones above.
