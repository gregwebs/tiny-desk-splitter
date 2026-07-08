# Fix player-bar artist link: full-page navigation kills playback since the Foldkit port

## Summary

Fixes `e2e/back-navigation.spec.js:87` ("player-bar artist link -> detail keeps the same audio
node playing"), failing on unmodified `main` since the Foldkit player port (`2e570373`, #21). A
plain click on `#player-artist` (the link to the currently-playing concert's detail page) was
doing a full-page browser navigation instead of an htmx partial swap of `#content`, which
detaches and re-creates `<video id="player-audio">` and stops playback — the exact regression
`2ed2144d` ("Keep audio player alive across in-app navigation") originally fixed.

## Bug: the anchor's click handler was dropped during the port

The pre-Foldkit implementation (`866fc4e9`) had two cooperating pieces: the anchor carried
`onclick="Player.openConcert(event)"` alongside `hx-target`/`hx-select`/`hx-swap`/`hx-push-url`,
and `openConcert(e)` skipped modifier clicks, else called `e.preventDefault()` +
`window.htmx.ajax("GET", url, { source: e.currentTarget })` — the `source` option is what makes
htmx read those `hx-*` attributes from the anchor.

The Foldkit port kept the `hx-*` attributes and a `Player.openConcert` shim
(`frontend/src/player/index.ts`), but:

- **dropped the `onclick` attribute** from the anchor (`widget/view.ts`) — with no click handler
  and no `hx-get`, htmx never intercepted the click, so a plain click fell through to a full-page
  navigation. Nothing in the codebase called `Player.openConcert`.
- **routed navigation through the widget's command Port** (`OpenConcert` → `NavigateToConcert`),
  which called `window.htmx.ajax("GET", url, {})` with no `source` — even if reached, this would
  have swapped `document.body` (htmx's default with no source/target) and pushed no history.

## Fix: navigation moves entirely into the host shim

The click event (modifier keys, `preventDefault`, the htmx `source` element) can't cross the
widget's Schema-encoded Port boundary (this was already documented in `port.ts`'s `OpenConcert`
comment). Since navigating the host page doesn't touch widget state, `Player.openConcert` now
does the full job itself instead of forwarding to the widget:

- `widget/view.ts`: `#player-artist` gets `onclick="Player.openConcert(event)"` back.
- `player/index.ts`: `openConcert(e)` reads `e.currentTarget`, checks a new pure guard
  `nativeClickShouldWin` (`player/core.ts` — non-primary button, any modifier key, an
  already-handled event, a non-`_self` target, or a download link; mirrors Foldkit's own
  link-router guard), and if the click should be intercepted, calls
  `window.htmx.ajax("GET", href, { source: anchor })`.
- The now-dead `OpenConcert` command and `NavigateToConcert` were removed from
  `widget/port.ts`, `widget/update/handleHostCommand.ts`, and `widget/command.ts`.

## Tests

- `player/core.unit.test.ts`: new `nativeClickShouldWin` suite (8 cases — plain click, non-primary
  button, each modifier key, already-prevented, non-`_self` target, `_self` target, download link).
- `widget/view.scene.test.ts`: new case asserting `#player-artist` carries
  `onclick="Player.openConcert(event)"`.
- `e2e/back-navigation.spec.js:87` is the end-to-end guard (already existed, was red).

## Verification

- `npx tsc --noEmit`, `npx vitest run` (230 passed), `npm run lint` (oxlint, clean).
- `just ts-build` + `cargo check` — re-embedded `concert-tracker/static/player.js`.
- `npx playwright test e2e/back-navigation.spec.js` — 4/4 pass.
- Full `npx playwright test` — 167 passed, 4 pre-existing failures (`concert-reconstruction.spec.js:88`,
  `interlude-tracks.spec.js:66`, `player-queue.spec.js:386`, `player-queue.spec.js:593`), confirmed
  by reproducing the same 4 failures against unmodified `origin/main` in a separate worktree.
- `just lint` — clean.
- Manual: ran `concert-web` against a copy of the e2e fixture DB on a separate port/workdir and
  drove the exact scenario with a standalone Playwright script — plain click does a partial swap
  (same audio node, exactly one `#content`, playback keeps advancing), Back returns to the list
  with the same node, and a Cmd-click is correctly declined by `openConcert` (`htmx.ajax` not
  called, original tab does not navigate, audio node untouched).
