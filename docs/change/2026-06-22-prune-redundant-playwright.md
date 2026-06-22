# Prune Playwright tests redundant with Foldkit Story/Scene/core coverage

## Summary

With both Foldkit widgets (splitter, add-to-playlist panel) now carrying `Story`/`Scene`
coverage (`docs/change/2026-06-21-foldkit-add-panel.md`,
`docs/change/2026-06-22-splitter-foldkit-tests.md`) on top of the existing pure `core.ts` unit
tests, several Playwright e2e tests had become pure duplicates: real-browser tests of widget
logic or static rendering that an in-process Story/Scene/core test already pins down, with no
real-server, real-drag, or cross-widget assertion of their own. This change removes those nine
tests and backfills one Scene test to close the only resulting coverage gap.

## Test-layer policy

Going forward:

- **Story / Scene / core (`js-tests/*.test.ts`, `src/**/*.{story,scene}.test.ts`)** own Foldkit
  widget *logic* (`update.ts`, `core.ts`) and *rendering* (`view.ts`). Fast, in-process,
  happy-dom — no real browser, no real server.
- **Playwright (`e2e/*.spec.js`)** is reserved for what only a real browser + real server can
  prove: real-server round-trips (asserted via `page.request`/`fetch` against the actual JSON
  API, not mocked), browser-only interaction (real `pointerdown`/pointer-capture drag, hover
  reveals), cross-widget/host integration (the global player bar, sidebar open/close state,
  add-panel ↔ track-list/concert-card/playlist-row glue), and non-Foldkit code. Per the project's
  general caution about mocking, Playwright tests against the actual server (including
  `page.route`-delayed real responses for race conditions) are kept rather than re-created as
  mocked Story tests.

## Removed (9 tests)

| Removed Playwright test | File | Now covered by |
| --- | --- | --- |
| `editing a linked boundary moves both adjacent times` | `e2e/splitter.spec.js` | `update.story.test.ts`: `MovedDragPointer while Dragging moves the linked boundary it targets`; `js-tests/splitter.test.ts`: `setEnd on a linked boundary moves the next track's start too` |
| `editing below the 1s minimum is clamped, keeping submit enabled` | `e2e/splitter.spec.js` | `js-tests/splitter.test.ts`: `setEnd clamps to neighbour and minimum segment` |
| `add panel lists existing playlists with membership checks` | `e2e/add-to-playlist.spec.js` | `view.scene.test.ts`: `renders the context label, a filter combobox, and a row per playlist` (asserts the `✓` member checkmark and option count) |
| `filter input narrows the playlist list` | `e2e/add-to-playlist.spec.js` | `view.scene.test.ts`: `typing in the filter narrows the list (no auto-highlight Command for a partial match)` |
| `member playlists appear at the top when filter is empty` | `e2e/add-to-playlist-ordering.spec.js` | `js-tests/playlists-core.test.ts`: `buildRows with no filter puts members on top, then non-members` |
| `member playlists move to the bottom when filter has text` | `e2e/add-to-playlist-ordering.spec.js` | `js-tests/playlists-core.test.ts`: `buildRows when filtered: non-members, then create row, then members` |
| `clearing the filter moves members back to the top` | `e2e/add-to-playlist-ordering.spec.js` | `js-tests/playlists-core.test.ts`: `buildRows with no filter puts members on top, then non-members` (same rule, re-applied) |
| `arrow-key navigation follows display order (empty filter: members-first)` | `e2e/add-to-playlist-ordering.spec.js` | `js-tests/playlists-core.test.ts`: `nextRow: from null selects the first row, then advances, then clamps`; backfilled DOM-wiring coverage below |
| `arrow-key navigation follows display order (with filter: members-last)` | `e2e/add-to-playlist-ordering.spec.js` | same as above |

## Backfill

`view.scene.test.ts` (add-panel) gained one Scene test, `ArrowDown on the filter highlights the
first row in display order (members first)`: renders a loaded panel with one member and one
non-member playlist, dispatches a real `keydown` ArrowDown on the filter combobox, and asserts
the rendered `option` with `aria-selected="true"` is the member row — the one piece of behavior
(`PressedArrowDown` wired to a real DOM keydown, rendering `aria-selected`/`add-pl-row-active`)
that the removed e2e arrow-nav tests exercised and that `nextRow`'s pure ordering test alone
doesn't prove.

## Explicitly kept (not redundant)

- All real-server round-trips: splitter's `loads auto timestamps`, `discard my edits restores…`,
  `detach … submit re-cuts … reset`, `no listen events recorded`; every add-to-playlist test that
  `fetch`es `/api/playlists` to confirm a real add/create/remove/422 cycle; the ordering-spec's
  `Enter … removes it (unlike click no-op)`.
- Real-browser-only interaction: splitter's `dragging the tail handle … clamps` (real
  pointer-capture drag); the hover-reveal `+` tests.
- Cross-widget/host glue: the player-bar tests, sidebar open/close state,
  `e2e/sidebar-close-resize.spec.js`.
- The network-race staleness test (`a slow membership fetch for a superseded target does not
  clobber the current one`) — uses `page.route` to delay a real server response, not to mock
  one; this is exactly the kind of real-async behavior Playwright should own.

## Verification

- `cd concert-tracker/frontend && npm run test:story` — 4 vitest files, 47 tests (up from 46:
  +1 backfilled Scene test), all green.
- `just test-ts` — 32 `node:test` core unit cases + 47 Story/Scene cases, all pass.
- `just lint` — fmt, clippy, `tsc --noEmit` (frontend + `js-tests`), the esbuild rebuild, and the
  `ts-verify` diff guard all clean.
- `git diff --stat` — only the three trimmed e2e spec files, the one Scene test addition, and
  this change note; no production code touched.
