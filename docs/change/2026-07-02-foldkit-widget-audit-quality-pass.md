# Foldkit widget quality pass: purity sweep + mechanical idiom batch

## Summary

Follow-up to [docs/change/2026-07-02-foldkit-widget-audit-fixes.md](2026-07-02-foldkit-widget-audit-fixes.md),
which fixed the 18 BLOCKER-level findings from the `foldkit-skills:audit-program` audit (PR #36,
merged). This change addresses Groups 4-5 of the QUALITY-tier findings: an Effect-idiom purity
sweep of `player/widget/update.ts` and a mechanical naming/typing batch across `player/` and
`playlists/`. No behavior changes are intended — the existing 185-test suite is the regression
safety net, and it passes unchanged before and after.

Since the blocker-fix PR merged, `origin/main` advanced independently via an unrelated "adopt
Foldkit oxlint plugin" commit (#35), which already fixed some of the same mechanical issues this
audit flagged (the `SyncNowPlayingMirrorCmd`→`SyncNowPlayingMirror` rename, 8 of 9 empty-`{}`
Command definitions, most `.length === 0` checks). This change re-surveyed the codebase against the
audit's findings before implementing, to avoid redoing already-fixed work and to catch drift in
line numbers / message field names (`FailedConcertPlayback`/`FailedMutation`'s `message` field was
independently renamed to `errorMessage`).

## Group 4: `player/widget/update.ts` purity sweep

Foldkit's architecture requires `update` to be pure and to route all Model changes through `evo()`
rather than object spread — spread-inside-`evo` silently bypasses that single-codepath invariant.

- **23 spread-inside-`evo` sites** converted to nested `evo()` calls (e.g. `sidebar: (s) => ({ ...s,
  loadGen })` → `sidebar: () => evo(model.sidebar, { loadGen: () => loadGen })`), including inside
  the three like-sync helper functions (`flipSidebarTrackLiked`, `flipSidebarTrackAvailable`,
  `flipConcertItemLiked`).
- **Extracted `applyLikedEverywhere(model, concertId, trackIdx, liked): Model`**, replacing 5
  `let model1 = ...; model1 = ...` reassignment chains (`CompletedLikeToggle`, `FailedLikeToggle`,
  `SyncLikeFromSwap`, `ToggleLike`, `SidebarLikeTrack`) that all independently re-implemented "flip
  the bar star (if this is the current track) + the sidebar list + the concert-reconstruction list."
- **Extracted `findCurrentLiked(model, concertId, trackIdx): Option<boolean>`**, replacing
  `SidebarLikeTrack`'s `let currentLiked: boolean | null` imperative lookup (which also had a
  shadowed `(t)` parameter) with an `Option.orElse` chain over the sidebar list then the concert list.
- **6 `cmds.push(...)` mutation sites** converted to immutable array construction (spread +
  ternary/`Option.match`), in `beginPlayback`, `ReassertUi`, and `PlayAlbumAt`.
- **Removed the duplicated `sameTargetLocal`** function; `update.ts` now imports the identical
  `sameTarget` already exported from `model.ts` (the removal comment claiming "reuse" was itself
  inaccurate — nothing was actually being reused before this change).
- **`stopPlaybackPure`** now reuses the exported `initialPlayback` constant instead of re-listing
  all twelve `Playback` fields inline, removing a duplicate-maintenance hazard.

## Group 5: mechanical idiom batch

- **Removed dead code**: `ClearPlayingExternal` (a Command that only cleared the "playing" CSS
  marker) was unreferenced outside its own definition — every call site that marks a new element
  "playing" already unconditionally clears all others first (`MarkPlayingExternal`,
  `MarkPlayingInterludeExternal`), so the standalone clear-only Command was pure dead weight. Deleted
  it and its `{}` args declaration, with cross-references in `message.ts`/`command.ts` comments updated.
- **`LoadSidebarWidthCmd` → `LoadSidebarWidth`**: dropped the last remaining `Cmd`-suffixed Command
  name (the project convention names Commands by action, not `fetchWeatherCommand`-style).
- **`tracks.length === 0` → `Array.isReadonlyArrayEmpty(tracks)`** (`ReceivedPlaylistTracks`) — the
  one remaining native length-check the independent oxlint-adoption commit hadn't already caught.
  (Note: this Effect version's `Array` module exports `isArrayEmpty`/`isReadonlyArrayEmpty`, not the
  `isEmptyArray` name some documentation references — matched the naming already in use elsewhere in
  this codebase, e.g. `splitter/widget/view.ts`'s `Array.isArrayNonEmpty`.)
- **`T[]` → `ReadonlyArray<T>`**: 6 remaining `Command<Message>[]` sites in `player/widget/update.ts`
  (`beginPlayback`'s and `refetchSidebarIfConcertChanged`'s signatures, `playConcertItemPure`'s
  `extraCommands`, `ReceivedPrepareStart`'s command list) and one `readonly Member[]` in
  `playlists/widget/update.ts`.
- **Single-letter callback parameters renamed to full words** across `player/widget/update.ts`
  (`(s)`→`sidebar`, `(p)`→`playback`, `(q)`→`queue`, `(t)`→`track`), `player/widget/command.ts`
  (`(t)`→`track`, `(b)`→`button`, and a genuine bug caught mid-rename: a mechanical find/replace
  briefly collided a `(b)` button-loop rename with an unrelated `(lb)` "like button" parameter,
  corrupting `lb.classList` into `lbutton.classList`; caught by re-reading the diff before running
  tests, fixed by renaming `(lb)` to `(likeButton)` properly instead of leaving the collision), and
  `playlists/widget/update.ts` (`(p)`→`phase`, `(l)`→`loaded`, `(t)`→`target`, `(r)`→`row`).
- **`cmds` → `commands`** throughout both files' local variables and destructured tuple elements.

## Verification

- `npx vitest run` (concert-tracker/frontend) — 185 tests green, unchanged from before this pass
  (no behavior change intended; the existing suite is the regression net for the whole sweep).
- `npx tsc --noEmit` — clean.
- `cargo build` + `just lint` (`cargo fmt --check`, `cargo clippy -D warnings`, shellcheck,
  ts-check, oxlint) — clean.
- `node build.mjs` re-embedded `concert-tracker/static/player.js` (the only bundle with a
  byte-level diff — `playlists.js`/`splitter.js` are unchanged because esbuild's minifier already
  collapses local variable names, so pure identifier renames with no behavioral difference produce
  identical minified output).

## Deferred (not in this change)

Group 6 (structural refactors — view/`handleCommand` decomposition, `switch`→`Match` conversion,
Message renames, `Mount.defineStream` conversion, dead `PlayTargetValue.Album` removal, playlists
combobox ARIA fixes, Scene-locator migration) and the `Playback` discriminated-union redesign remain
as follow-up work, per `/Users/claude/.claude/plans/resilient-chasing-lovelace.md`.
