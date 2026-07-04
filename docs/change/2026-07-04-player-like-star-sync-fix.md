# Fix Player like-star: missing bar-star CSS class + dead htmx reverse sync

## Summary

Fixes [#30](https://github.com/gregwebs/tiny-desk-splitter/issues/30): three `e2e/player-queue.spec.js`
"Player like star" tests failing on unmodified `main` (also `sidebar.spec.js:198`, one test the
issue didn't mention but shares the same root cause). Both bugs date to the Foldkit player port
(`2e570373`, #21). Root-caused via in-browser instrumentation (event-listener wrapping, a
`MutationObserver` on `#player-like`, and logging injected into the served bundle at the exact
minified call sites) rather than guesswork — see per-bug detail below.

## Bug 1: `#player-like` never gets the `liked` CSS class

`concert-tracker/frontend/src/player/widget/view.ts` rendered the bar star with a hard-coded
`h.Class("btn-like")`. The model updates correctly (★/☆ text, `aria-pressed`), but the class stays
`btn-like` forever. `style.css` has `#player-like.liked { color: var(--like-color); }` — so this
isn't test-only, the bar star never turns gold in a real browser either. The sidebar/track-list
`likeButton` helper in the same file did this correctly (`h.Class(liked ? "btn-like liked" :
"btn-like")`); the bar star just never got the same treatment.

**Fix:** extracted a shared `likeButtonClass(liked): string` helper, used at both call sites, so
the class can't drift out of sync between them again.

## Bug 2: htmx reassigns `detail.elt` after dispatch, so the reverse-sync subscription never fires

The `htmxSwap` subscription (`subscription.ts`) read `evt.detail.elt` inside a `Stream.mapEffect`
stage. `Stream.fromEventListener`'s DOM listener only enqueues the event; the `mapEffect` body runs
later, on the Effect fiber. Instrumented probes (wrapped `addEventListener`, a `MutationObserver`,
and logging patched into the served `player.js` bundle at its exact call sites) proved:

- At synchronous dispatch time, `detail.elt` is the swapped-in `<button>` carrying
  `hx-post="/concerts/:id/tracks/:idx/like"`.
- One macrotask later, htmx 1.9.12 has reassigned the same shared `detail` object's `elt` to the
  parent `<li>` (it reuses its `responseInfo` object for the settle phase; the `outerHTML` swap
  replaces the original element). The parent has no `hx-post`, so the regex never matches and
  `SwappedLikeButton` is never dispatched.
- Dispatching a synthetic `htmx:afterSwap` with the original `detail.elt` still attached proved the
  rest of the pipeline (`update.ts`'s `SwappedLikeButton` handler → `applyLikedEverywhere`) was
  already correct — only the timing of the read was broken.

**Fix:** moved the parsing (regex match, `parseInt`, DOM re-query, `classList.contains("liked")`)
into a new exported `parseLikeSwapEvent(evt): Option<{ concertId, trackIdx, liked }>`, called
synchronously from inside the DOM listener via `Stream.callback` (mirroring the existing
`sidebarResize` entry's pattern in the same file, including the `Effect.flatMap(() =>
Effect.never)` that keeps the acquired scope — and thus the listener — alive).

**General constraint, documented once:** anything that must observe an event's payload as it was
*at dispatch time* — `preventDefault`, or reading a framework's mutable event `detail` like htmx's
— must run inside the DOM listener itself, not in a downstream Stream stage. The `keyboard` and
`outsideVideo` subscriptions in the same file call `e.preventDefault()` inside `Stream.mapEffect`
and likely have the same no-op timing issue; that overlaps the already-tracked keyboard-shortcut
drift in [#28](https://github.com/gregwebs/tiny-desk-splitter/issues/28) and is out of scope here.

**Follow-up not taken (flagged in review, deliberately deferred):** the like-endpoint URL shape
(`/concerts/:id/tracks/:idx/like`) now appears in three places — the regex + re-query in
`subscription.ts`, and a similar selector in `command.ts:564` — that a shared
`likeEndpointPath(concertId, trackIdx)` helper could unify. Left as-is here to keep this PR scoped
to the two reported bugs; the pre-existing unanchored regex (`/like` also matches `/like-foo`)
would be worth tightening in that same follow-up.

## Tests

- `view.scene.test.ts`: extended the existing "liked track shows filled star" case with
  `toHaveClass("liked")` / `not.toHaveClass("liked")` assertions — this is the coverage gap that
  let Bug 1 ship (jsdom-based Scene tests never caught the missing class because they were only
  asserting text/state, not the class list).
- `subscription.unit.test.ts` (new): unit tests for `parseLikeSwapEvent` against real `CustomEvent`s
  — a liked button, an unliked button, `detail.elt` reassigned to a parent (no `hx-post`, the exact
  failure mode), a non-like `hx-post`, and a plain `Event` with no `detail`. Named `.unit.test.ts` to
  match the category [#28](https://github.com/gregwebs/tiny-desk-splitter/issues/28)'s fix
  introduced in `vitest.config.ts` for plain DOM-dependent unit tests that aren't Story/Scene/Command
  harness tests (e.g. `player/core.unit.test.ts`'s keyboard-target predicates) — no config change
  needed here, it already covers this file.
- The three originally-reported e2e tests plus `sidebar.spec.js:198` are the acceptance criteria;
  no changes needed to any of them.

## Verification

- `npm run check` / `npm run lint` (frontend) — clean.
- `npx vitest run` — 210 green (was 205 on updated `main`; +5 new `parseLikeSwapEvent` cases in
  `subscription.unit.test.ts`; the 2 new scene-class assertions extend an existing test rather
  than adding new ones).
- `node build.mjs` + `cargo build --bin concert-web` re-embedded `concert-tracker/static/player.js`.
- `just lint` (`cargo fmt --all -- --check` + `cargo clippy --workspace --all-targets -- -D
  warnings` + shellcheck + ts-check + ts-lint) — clean.
- `npx playwright test e2e/player-queue.spec.js -g "Player like star"` — all 7 pass (was 4/7).
- Full e2e suite, measured before/after via two clean checkouts of the same commit set (updated
  `main` vs. this branch): **26 failed / 145 passed** on `main`, **22 failed / 149 passed** on this
  branch. The 4 newly-passing tests are exactly the 3 reported (`:959`, `:977`, `:1009`) plus
  `sidebar.spec.js:198` (same root cause). All 22 remaining failures are byte-for-byte the same
  test names in both runs — pre-existing, already tracked in #29/#31/#32/#33 (and the one flaky
  `#28`-adjacent keyboard test that #28's own fix didn't fully stabilize), unrelated to this fix.
- This branch was originally developed on top of `foldkit-widget-group-6` before that branch merged
  to `main` via #39; the one new commit was cherry-picked onto fresh `main` (which had since also
  picked up #28's keyboard-shortcut fix, PR #41) to keep this PR scoped to just the like-star fix.
  `subscription.ts`'s auto-merge combined both fixes without conflict; `vitest.config.ts` conflicted
  only on the *name* of the new test-file category (this PR proposed `*.test.ts` generally, #28's
  PR had already landed a narrower `*.unit.test.ts` convention) — resolved by adopting #28's
  convention and renaming `subscription.test.ts` → `subscription.unit.test.ts`.
- Manual: started `concert-web` on a separate port against a copied fixture db/workdir, played a
  track, clicked the bar star, and confirmed via `getComputedStyle(...).color` that it visibly
  changes from gray (`rgb(170,170,170)`, the default text color) to gold (`rgb(240,165,0)`, the
  liked color) — then toggled it back off via the track-list star and confirmed the bar star
  reverts, exercising the reverse-sync path end to end in a real browser.
