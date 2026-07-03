# Fix player keyboard shortcuts: restore pre-Foldkit Space/Escape target detection

Fixes #28.

## Summary

The Foldkit port of the player (`2e570373`, #21) collapsed the pre-Foldkit `player.ts`'s
three-tier keyboard-shortcut target detection into a blanket rule: any keydown whose target was
inside `#player-bar` was ignored by the global Space/Escape shortcuts. That broke a cluster of
Playwright tests in `e2e/player-queue.spec.js`'s "Player keyboard shortcuts" describe block —
Space on a focused player-bar control (queue toggle, play/pause, Next, video-close, title) got
natively re-clicked instead of pausing playback. Root-causing this surfaced two further,
independent bugs that made the target-detection fix insufficient on its own to pass the listed
tests: a stale-model race in the Space pause/resume decision, and a widget-quality-pass regression
that had (re-)broken Enter/Space activation on the title/track-number spans in the opposite
direction. All three are fixed here; verified with the full `player-queue.spec.js` "Player
keyboard shortcuts" describe block (including `--repeat-each=5` for the two that were flaky rather
than deterministic) plus the frontend's vitest suite (see Verification).

## Bug 1: blanket `#player-bar` ignore rule replaced the old three-tier check

Pre-Foldkit `player.ts` (`git show 2e570373^:concert-tracker/frontend/src/player.ts`, ~lines
220–247) distinguished three tiers for a keydown target:

1. **Playback-shortcut targets** — the media element, or anything non-editable inside
   `#player-bar` / `#player-video-panel` — Space still controls playback.
2. **Editable targets** — `input`/`textarea`/`select`/contenteditable — native key behavior wins.
3. **Other interactive controls** (`INTERACTIVE_SELECTOR`, outside the bar) — native activation
   wins.

The Foldkit port's `subscription.ts` collapsed this to `isEditableTarget(target) ||
target.closest("#player-bar")`, which incorrectly ignored Space on every non-editable player-bar
control (the queue toggle, play/pause, Next, video-close, title) instead of using it to pause, and
never excluded interactive controls outside the bar at all.

**Fix:** `isEditableTarget` / `isPlayerPlaybackShortcutTarget` / `isKeyboardShortcutIgnoredTarget`
are restored verbatim in `concert-tracker/frontend/src/player/core.ts` (next to the existing
`INTERACTIVE_SELECTOR` and `clickShouldDismiss`, which use the same target-argument-predicate
shape) and unit-tested directly in a new `core.unit.test.ts` (vitest's `include` pattern extended
with `*.unit.test.ts` in `vitest.config.ts` for plain DOM-fixture predicate tests that aren't
Story/Scene/Command tests). `subscription.ts`'s `keyboard` entry now uses
`isKeyboardShortcutIgnoredTarget` for Space and adds:

- a `defaultPrevented` top guard (an earlier handler — e.g. the title/track spans' own
  `OnKeyDownPreventDefault`, which fires first since it's attached lower in the bubble chain —
  already claimed the key),
- `preventDefault()` *before* the repeat check (old code had this backwards, so a held Space
  never suppressed page-scroll on repeat keydowns — see Bug 1a),
- Escape's `preventDefault`/dispatch now gated on the video panel actually being open (a
  `videoOpen` model dependency, mirroring the existing `outsideVideo` entry) — old code never
  suppressed native Escape when the panel was closed; the port did unconditionally.

### Bug 1a: repeated-Space `preventDefault()` ordering (test 428)

Old code called `preventDefault()` before checking `e.repeat`; the port swallowed repeats first,
so a held Space stopped suppressing page-scroll after the first keydown. Fixed by reordering (see
above). Verifying this via the existing synthetic-event test required an additional fix: the test
dispatches a `KeyboardEvent` via `document.dispatchEvent()` and reads `event.defaultPrevented` in
the same synchronous script block, but the player's keydown handling runs through an Effect
`Stream` (queue → async pull → `preventDefault()`), so `defaultPrevented` isn't observable
synchronously the way it would be with a plain `addEventListener` callback. `e2e/player-queue.spec.js`
now awaits a zero-delay `setTimeout` (one macrotask — enough for the Stream to drain its internal
microtask hops) before reading it, with a comment explaining why.

## Bug 2: `PressedSpace` decided pause-vs-resume from a laggy `model.isPlaying`

Tests 331 and 363 ("body-focused Space resumes paused media" / "video-focused Space toggles
playback") failed deterministically (100% reproducible via `--repeat-each=5`) even after Bug 1's
fix. `update.ts`'s `PressedSpace` handler branched on `model.isPlaying`, which only updates
asynchronously via the `audioEvents` subscription's native `play`/`pause` DOM event round-trip.
Two Space presses in quick succession (first pauses, second should resume — confirmed via an
instrumented debug script showing real keydown events ~8ms apart) could both see a stale
`model.isPlaying === true` and both dispatch `PauseAudio`, so the second press was a no-op instead
of a resume.

**Fix:** `PressedSpace` now carries an `audioPaused: boolean` payload, sampled live from
`#player-audio.paused` in the subscription at dispatch time — exactly mirroring pre-Foldkit
`player.ts`'s `togglePause()`, which read `audio.paused` directly rather than a cached flag.
`update.ts`'s handler branches on the payload instead of `model.isPlaying`.

The same `model.isPlaying`-staleness pattern also exists, unfixed, in the play/pause **button**'s
`TogglePause` host command (`widget/update/handleHostCommand.ts:66`) — out of scope here (no
keyboard event to sample `audioPaused` from; needs its own Command-layer fix), tracked as #40.

## Bug 3: a later, unrelated commit had re-broken Enter/Space activation the other way

A "Foldkit widget quality pass" that landed on `main` after this issue was filed added a generic
`onActivateKey` helper (Enter *and* Space, the usual ARIA button-activation convention) to
`#player-title`/`#player-track`. That directly conflicts with the tests this issue is fixing:
Space on a focused title/track span must pause (tier-1 playback shortcut), not toggle the sidebar.
Renamed to `onEnterKey` and restricted to Enter only, with a doc comment explaining the exclusion;
updated the Scene test that had asserted Space activation and added one asserting it's now a
no-op for the sidebar.

## Out of scope (confirmed pre-existing, unrelated to this issue)

- **Test 382** ("Space in an interactive control does not trigger the global pause shortcut")
  still fails on this branch and on unmodified `main`. `#player-seek` is `h.Disabled(true)` in the
  current view (comment: "static until audio Subscription adds currentTime/duration"), so
  Playwright's `.focus()` on it silently no-ops and the test's premise doesn't hold. Pre-Foldkit
  `#player-seek` was not disabled; this is a separate, already-documented incomplete-feature state
  from the port, not part of #28.
- Three further pre-existing e2e failures unrelated to keyboard shortcuts (a `#player-track`
  text-format mismatch, a like-star class assertion, a delete-error-visibility timing issue),
  confirmed identical on unmodified `main` via `git stash`, in files this change doesn't touch.

## State table

| keydown target                                               | Space                                                  | Escape (panel open) |
|----------------------------------------------------------------|--------------------------------------------------------|----------------------|
| media element / non-editable in `#player-bar` or `#video-panel`| toggle playback, live `audio.paused` decides (`preventDefault`) | fold panel     |
| editable (input/textarea/select/contenteditable)                | native (type a space)                                  | native (clear/blur)  |
| other interactive control (`INTERACTIVE_SELECTOR`, outside bar) | native activation                                       | fold panel           |
| anything else (body, plain elements)                            | toggle playback if media loaded, else native scroll     | fold panel           |

## Verification

- `cd concert-tracker/frontend && npm run check && npm run lint && npm run test:story` — 204
  vitest tests pass, including the new `core.unit.test.ts` (17 tests) and a new `PressedSpace`
  regression test for the stale-`isPlaying` race.
- `just lint` — clean.
- `npx playwright test e2e/player-queue.spec.js -g "Player keyboard shortcuts"` — 21/22 pass (the
  22nd is the pre-existing, out-of-scope `#player-seek`-disabled test above); the full describe
  block was also run with `--repeat-each=5` to confirm the previously-flaky tests (331, 363) now
  pass reliably (10/10 combined).
- `npx playwright test e2e/player-queue.spec.js` (full file) — no new failures versus unmodified
  `main`; the 4 failures present on both branches are confirmed pre-existing and unrelated.
