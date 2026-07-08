# Fix Player error/status text: never visible in a real browser since the Foldkit port

## Summary

Fixes `e2e/player-queue.spec.js:1114` ("a failing delete keeps playback going and shows an
error"), failing on unmodified `main` since the Foldkit player port (`2e570373`, #21). The bug is
more than a test failure: `#player-status` ("Preparing…" progress text) has been invisible in a
real browser since the same port, currently masked because the only e2e assertion on it
(`automate-splitting.spec.js:121`) checks `toContainText`, which passes on hidden elements.

## Bug: `#player-error` / `#player-status` render with no `display` style

`style.css` gives both spans a `display: none` baseline (lines 581/584). The pre-Foldkit
implementation (`player.ts`'s `showError`/`hideError`/`setStatus`) toggled `el.style.display =
"inline"` / `"none"` explicitly. The Foldkit port (`view.ts`) set the text content but never set a
display style, so the CSS baseline always won — confirmed in the Playwright failure log, which
showed the correctly-populated `<span id="player-error">Delete failed</span>` resolving as
`hidden`.

This is the exact hazard the same view function already documents for the action-button group
(`#player-watch`/`#player-open`/`#player-delete`, which use explicit `"inline-block"` shown
values) — the two status spans just didn't get the same treatment during the port.

**Fix:** `h.Style({ display: errorText ? "inline" : "none" })` / `busyText ? "inline" : "none"` on
the two spans (`view.ts`), matching the pre-Foldkit shown value.

## Known follow-up (not fixed here, out of scope): `#player-status` still invisible during prepare-before-play

Manual in-browser verification (`getComputedStyle`) found a second, unrelated cause of the same
symptom: while a track is preparing (download/split) but hasn't started playing yet,
`playback.concertId` is still `null`, so `#player-bar` never gets the `.active` class and stays
`display: none` at the bar level — the now-correctly-styled `#player-status` span is invisible
inside a hidden ancestor. The pre-Foldkit `preparePlay` called `showBar()` (forcing `.active`)
*before* `setStatus(...)`; the Foldkit port has no equivalent call, so the bar only appears once
real playback begins. Not caused by this change, and no existing e2e test catches it (same
`toContainText`-only blind spot). Left for a follow-up since it's a distinct root cause from the
one this change fixes.

## Tests

- `view.scene.test.ts`: added `toHaveStyle("display", ...)` assertions to the existing idle/error/
  busy-status cases, pinning the exact CSS-baseline-vs-inline-style contract (Scene's `toBeVisible`
  passes on a vnode with no display style at all — exactly the gap that shipped this bug — so
  `toHaveStyle` is used instead of `toBeVisible` here).

## Verification

- `npx vitest run` (frontend) — 219 passed (unchanged count; 6 new `toHaveStyle` assertions added
  inside 3 existing tests — those 3 tests fail against unmodified `view.ts`, confirming the fix is
  necessary before they were made to pass).
- `node build.mjs` + `cargo build` re-embedded `concert-tracker/static/player.js`.
- `npx playwright test e2e/player-queue.spec.js:1114` — now passes.
- `npx playwright test e2e/player-queue.spec.js e2e/automate-splitting.spec.js` — 88/89 pass; the
  one failure (`player-queue.spec.js:386`, "Space in an interactive control...") is pre-existing
  and unrelated (tracked separately; `#player-seek` is `h.Disabled(true)`).
- Manual: in a real browser, clicked delete after killing the server and confirmed the red error
  text now actually renders inline in the player bar (not just present in the DOM).
