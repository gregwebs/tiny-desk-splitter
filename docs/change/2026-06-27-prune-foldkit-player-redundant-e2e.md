# Prune Playwright tests redundant with the Foldkit player in-process coverage

## Summary

The player Foldkit port's in-process suites (Story/Scene/core) plus the backfills added with the
e2e drift fix (`docs/change/2026-06-27-foldkit-player-e2e-drift-fix.md`) now pin down several queue
behaviors that Playwright tests were still exercising in a real browser with no real-server,
real-`<audio>`, or cross-widget assertion of their own. This removes those four tests, following
the test-layer policy in `docs/change/2026-06-22-prune-redundant-playwright.md` (Story/Scene/core
own widget logic and rendering; Playwright is reserved for real-server, real-browser, and
cross-widget/host glue).

## Removed (4 tests)

| Removed Playwright test | File | Now covered by |
| --- | --- | --- |
| `multiple tracks can be queued` (badge "2") | `e2e/player-queue.spec.js` | core `enqueueDedupe` append (`js-tests/player-core.test.ts`) + Scene `queue badge shows count when queue is non-empty` (queue of 2 → "2") |
| `duplicate tracks are not enqueued` (badge "1") | `e2e/player-queue.spec.js` | core `enqueueDedupe` skips-duplicate (`js-tests/player-core.test.ts`) + Scene `queue badge shows 1 for a single-entry queue (post-dedup render)` |
| `empty queue shows 'Nothing queued' message` | `e2e/sidebar.spec.js` | Scene `empty queue shows Nothing queued` (asserts the exact "Nothing queued" text) |
| `queued track appears in the sidebar list` | `e2e/sidebar.spec.js` | Scene `queue with one song shows title` + `remove button shows the × glyph, not a trash icon` |

## Explicitly kept (not redundant)

- `remove button deletes queue entry without affecting playback` (`e2e/sidebar.spec.js`) and
  `play-now button removes entry and immediately plays that track` (`e2e/sidebar.spec.js`): both
  assert real-`<audio>` continuity (playback keeps running / immediately plays the chosen entry),
  which Story/Scene cannot reproduce. They also still drive a real enqueue through the card track
  buttons, so the real enqueue → queue-render wiring stays covered end-to-end.

## Verification

- `npx playwright test e2e/player-queue.spec.js e2e/sidebar.spec.js --list` — both specs parse;
  91 tests (down from 95), the kept queue tests present.
- `just test-ts` + `just ts-check` — unchanged and green (no production or in-process test code
  touched; only e2e `.js` removed).
