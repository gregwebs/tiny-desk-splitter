# Fix Watch button hidden during concert-reconstruction video playback

## Summary

Regression reported after the Foldkit player port (`2e570373`, #21): the player-bar **Watch**
button — which folds up the inline video panel — disappeared during concert-reconstruction
playback of video items. Root-causing this surfaced two further, deeper bugs in the same code
that made the visibility gate fix insufficient on its own: a CSS/JS convention mismatch that left
Watch, Open, and Delete permanently invisible in a real browser regardless of state, and a missing
`liked` exclusion on Delete once it started rendering. All three are fixed here; verified against
a real running server (Playwright driving an isolated test db/workdir) and the tracked
`e2e/player-queue.spec.js` + `e2e/concert-reconstruction.spec.js` suites (see Verification).

## Bug 1: Watch's visibility gate required a `watchUrl` that concert playback never has

The old player (`frontend/src/player.ts`'s `updateMediaButtons`) showed Watch whenever `isVideo`
was true, independent of `watchUrl`. `playConcertItem()` always passed `watchUrl: null` — even for
video items, since concert-reconstruction items (including interludes, which have no `trackIdx`)
have no per-item watch endpoint — so Watch was correctly visible during concert playback.

The Foldkit port's view (`concert-tracker/frontend/src/player/widget/view.ts`) gated Watch on
`p.isVideo && p.watchUrl !== null`. `watchUrlFor` (`widget/update.ts`) still deliberately returns
`null` for `ConcertItem` playback, so the extra `watchUrl` conjunct hid Watch exactly during
concert playback of video items — the one path the button most needs to work, since concert
playback is how most video content actually plays.

The Watch click handler never reads `watchUrl`; it only toggles the `open` class on the
host-owned `#player-video-panel`, which wraps the already-playing `#player-audio` element. So the
button works correctly with `watchUrl: null` — only the gate needed fixing.

**Fix:** Watch's visibility now gates on `isVideo` alone, restoring pre-Foldkit behavior. The
Open-external button (⊞, launches the system player) keeps its `watchUrl` gate: the old player
showed it whenever `isVideo` was true but its handler no-opped when `watchUrl` was null — a latent
dead-button bug. Hiding it when there's nothing to open is an intentional improvement, not
something to "fix back" later.

## Bug 2 (found during manual verification): CSS `display: none` baseline defeats the `""` show-value convention

Fixing Bug 1 alone did not make Watch appear in a real browser. `style.css` (predates the Foldkit
port, from commit `19f704e0`) declares `#player-watch, #player-open { display: none; }` and
`#player-delete { display: none; }` as baseline rules. The Foldkit view's "shown" state used
`h.Style({ display: cond ? "" : "none" })` — but setting an inline style to `""` via the DOM API
*removes* the property rather than overriding it, so the CSS baseline `display: none` kept
winning. These buttons were therefore never actually visible in a real browser regardless of
state; only jsdom-based Scene/vitest tests reported them as visible, because jsdom doesn't load
the external stylesheet, so there's no competing rule there. Confirmed this is a pre-existing bug
from the original port (not introduced by this change) by running the tracked Playwright test
`player-queue.spec.js:1058` ("delete button is shown when a track is playing") against unmodified
`main` — it fails there too.

**Fix:** changed the "shown" value from `""` to `"inline-block"` for `#player-watch`,
`#player-open`, `#player-delete`, matching exactly what the pre-Foldkit imperative code used
(`btn.style.display = "inline-block"`). Also found `#player-track` (the "N." track-number span)
has the same CSS baseline but had *no* JS style override at all — added one, using the same
`"inline-block"`/`"none"` pattern.

## Bug 3 (found while fixing Bug 2): Delete's visibility never excluded liked tracks

Once Delete started actually rendering (previously permanently hidden by Bug 2, which masked
this), three tracked e2e tests failed: they expect Delete hidden for a liked/starred track — a
starred track's files are protected from the player-bar delete button until unstarred. The
pre-Foldkit condition was `trackIdx == null || liked` (hide if no track *or* liked); the Foldkit
port's `hasTrack` (`hasMedia && p.trackIdx !== null`) never carried the `liked` exclusion.

**Fix:** changed just the delete button's condition to `hasTrack && !p.liked ? "inline-block" :
"none"` — left `hasTrack` itself untouched since Like/Add-to-playlist/Track-number don't have this
exclusion in the old code either.

## Tests

- `view.scene.test.ts`: new Scene case for the concert-playback shape (`isVideo: true, watchUrl:
  null`) → `#player-watch` visible, `#player-open` hidden; new case for a liked track → `#player-delete`
  hidden while like/add-to-playlist stay visible.
- `update.story.test.ts`: new Story case pinning that `ReceivedConcertPlaybackItems` for a video
  item produces `playback.isVideo === true && playback.watchUrl === null` — the state the Bug 1
  view gate depends on.
- Bug 2 and Bug 3 are structurally invisible to jsdom-based Scene tests (no external stylesheet
  loaded), so their regression coverage is the tracked Playwright e2e suite, not new unit tests.

## Verification

- `npx vitest run` (concert-tracker/frontend) — 156 green, including the three new cases.
- `npx tsc --noEmit` (frontend + js-tests) — clean.
- `just fmt-check` / `just clippy` — clean (`shellcheck` binary absent in this dev sandbox, a
  pre-existing environment gap unrelated to this change — no shell scripts were touched).
- `node build.mjs` + `cargo build --bin concert-web` re-embedded `concert-tracker/static/player.js`.
- Manual: started the server on a separate port/db/workdir (via `examples/make_test_fixture`),
  played a concert item that is a video, confirmed Watch appears and toggles the panel; confirmed
  audio-only items still hide it, and Open-external stays hidden during concert playback.
- `e2e/player-queue.spec.js` + `e2e/concert-reconstruction.spec.js` (78 tests), measured
  before/after against a real Chromium: unmodified `main` — **42 failed / 36 passed**; with this
  fix — **22 failed / 56 passed**. 20 net new passes (the literal reported bug, all "Inline video"
  tests, several keyboard-Escape tests, and five Delete tests), zero new failures — verified the
  two borderline cases (`:382`, `:568`) are pre-existing flaky tests by rerunning each 3x against
  unmodified `main`, where they also fail intermittently (unrelated to this change; owned by
  `subscription.ts`, which was not touched here).

## Follow-up (not in this PR)

The remaining 22 e2e failures are pre-existing and unrelated to this code path — filed as tracked
issues rather than left in prose:
- [#28](https://github.com/gregwebs/tiny-desk-splitter/issues/28) — `subscription.ts`'s
  `isKeyboardShortcutIgnoredTarget` uses a blanket `target.closest("#player-bar")` check, where the
  pre-Foldkit code had a more nuanced two-tier check (`isPlayerPlaybackShortcutTarget` distinguished
  playback-control targets like `#player-title` from actually-editable ones like `#player-seek`).
  Several Space/Escape keyboard-shortcut tests fail because of this drift.
- [#29](https://github.com/gregwebs/tiny-desk-splitter/issues/29) — `concert-reconstruction.spec.js:88`
  fails at its `delete-redundant-source` precondition (`source_redundant` reports "not yet fully
  covered" for an auto-split source the test expects to be redundant).
- [#30](https://github.com/gregwebs/tiny-desk-splitter/issues/30) — three Player like-star e2e tests
  (reflects-liked, POST /like flip, track-list reverse sync).
- [#31](https://github.com/gregwebs/tiny-desk-splitter/issues/31) — a failing delete doesn't show
  `#player-error`.
- [#32](https://github.com/gregwebs/tiny-desk-splitter/issues/32) — several untriaged "basic playback
  start" and "inline video minimize" e2e failures, possibly sharing a root cause with #28.
- [#33](https://github.com/gregwebs/tiny-desk-splitter/issues/33) — meta issue: the e2e suite isn't a
  CI merge gate (42/78 fail on unmodified `main`), which is how Bugs 2 and 3 above shipped silently
  in the original port and went unnoticed until this investigation.

These are real bugs but a different code path (`subscription.ts` keyboard handling, splitter
redundant-source logic) from this PR's `view.ts` display fixes.
