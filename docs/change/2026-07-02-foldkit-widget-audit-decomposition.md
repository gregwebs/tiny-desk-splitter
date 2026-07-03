# Foldkit widget quality pass: handleCommand split + view decomposition

## Summary

Third and final commit in this session's Group 6 work (blockers in #36, purity/mechanical sweep in
#37, mechanical batch in #38). This change covers items #13 and #12 from the remediation plan
(`/Users/claude/.claude/plans/resilient-chasing-lovelace.md`) — the two structural decompositions,
engineering-lead-reviewed before implementation.

Item #19 (migrating the 96 CSS-selector locators in `view.scene.test.ts` to accessible role/label
queries) is deferred: several elements share the same accessible name (e.g. "Like" appears on the
player bar *and* every sidebar track row; "Toggle queue and tracks sidebar" labels two separate
spans), so a correct migration needs per-test `Scene.within` scoping decisions, not a mechanical
find-replace. Doing it hastily risked silently weakening test coverage — explicit user direction was
to ship the two structural refactors now and leave #19 for a follow-up.

## #13: split `handleCommand` out of `update.ts`

`update.ts` was 932 lines: the top ~650 lines were pure decision-logic helpers, followed by the
211-line `handleCommand` function dispatching on the `PlayerCommand` host-Port union. Both the main
`update` function and `handleCommand` called into the same helpers (`beginPlayback`,
`dispatchPlayTrack`, `applyLikedEverywhere`, the concert-reconstruction functions, etc.), so splitting
`handleCommand` into its own file naively would have created an `update.ts` ↔
`update/handleHostCommand.ts` import cycle.

Fix (per engineering-lead review): extracted the shared pure helpers to a third file first.

- **`player/widget/update/helpers.ts`** (440 lines): `UpdateReturn`, `withUpdateReturn`,
  `toCoreState`, `withError`/`withBusy`/`withPlayback`, `beginPlayback`,
  `refetchSidebarIfConcertChanged`, `dispatchPlayTrack`, `stopPlaybackPure`, the concert-reconstruction
  functions (`playConcertItemPure`, `advanceConcertPure`, `playConcertPosOrEnd`), the like-sync
  helpers (`applyLikedEverywhere`, `findCurrentLiked`, `flipSidebarTrackAvailable`), etc.
- **`player/widget/update/handleHostCommand.ts`** (255 lines): the `PlayerCommand` dispatch,
  extracted as a curried `(model: Model) => (command: PlayerCommand): UpdateReturn`, matching the
  project's curried-extracted-handler convention. Imports its shared logic from `./helpers`.
- **`player/widget/update.ts`** (315 lines): the top-level `update` function only,
  `CommandReceived: ({ command }) => handleHostCommand(model)(command)`. Imports the same shared
  helpers from `./update/helpers`.

No behavior change intended. Verified by running the full 185-test suite after the split — the 80
`update.story.test.ts` tests exercise every Message handler *and* every `PlayerCommand` case (via
`CommandReceived` wrapper messages), giving direct coverage of exactly the code that moved.

## #12: decomposed `view()` and removed duplicated row-button blocks

`view()` was 238 lines rendering the entire player bar + sidebar in one function; `reconstructionList`
(~110 lines) and `wholeAlbumList` (~200 lines) each inlined their own copies of the
like/delete/add-to-playlist button trio.

- Extracted `playerBarView(model)` and `sidebarView(model)` from `view()`, which is now a 3-line
  composition of the two.
- Extracted named per-row view functions: `concertTrackRowView`, `concertInterludeRowView`,
  `availableTrackRowView`, `unavailableTrackRowView` (replacing inline `.map()` callback bodies).
- Extracted shared button builders — `likeButton`, `deleteTrackButton`, `addToPlaylistButton` — used
  by both `concertTrackRowView` and `availableTrackRowView`, removing the duplicated button markup
  between the concert-reconstruction and whole-album sidebar lists.

**Hard invariant preserved**: the Group-1 keyed-row identity fixes (`track-${trackIdx}`,
`interlude-${interludeIdx}`, `avail-${track.index}`, `unavail-${track.index}`, the `song-`/`group-`
queue keys) are unchanged, byte-for-byte, through the decomposition — verified by the 3 keyed-identity
tests added in the original blocker-fix PR, which assert the exact key strings and still pass
unchanged.

## Verification

- `npx vitest run` (concert-tracker/frontend) — 185 tests green, unchanged before/after both commits.
- `npx tsc --noEmit` — clean.
- `cargo build` + `just lint` (`cargo fmt --check`, `cargo clippy -D warnings`, shellcheck,
  ts-check, oxlint) — clean.
- `node build.mjs` re-embedded `concert-tracker/static/player.js`.

## Deferred (not in this change)

- Group 6 #19 (Scene-locator role/label migration) — needs a per-test audit with `Scene.within`
  scoping for ambiguous same-named elements; tracked for a follow-up session.
- Group 6 #16 (`sidebarResize` → `Mount.defineStream`) — pulled into its own follow-up PR by the
  engineering-lead review (real drag-behavior change, needs Playwright verification).
- The `Playback` discriminated-union redesign remains its own future plan, as established in #36.
