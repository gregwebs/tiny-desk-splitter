# Fix Foldkit player port-behavior bugs + restore bar visibility (issue #23)

## Summary

PR #22's e2e drift fix surfaced four Playwright failures that were genuine Foldkit-port behavior
regressions (not migration artifacts), tracked in issue #23. This fixes the three behaviors, and —
discovered while verifying them — a fourth, critical regression: the player bar was invisible
because its `active` class was never set. The three behavior fixes live in
`concert-tracker/frontend/src/player/widget/update.ts` and reuse existing helpers; each gets Story
coverage, the bar fix gets Scene coverage, and the pre-existing e2e tests validate end-to-end.

### 1. Playlist label not shown after `playPlaylist` (`playlists.spec.js:152`, `:210`)
`ReceivedQueueDrainResult`'s play branch passed `defaultPlayOpts` (`playlistName: null`) to
`beginPlayback`, discarding the queued entry's `playlistName`, so `playback.playlistLabel` was
never set and `#player-playlist` stayed hidden. Now it threads
`{ ...defaultPlayOpts, playlistName: entry.playlistName }`. Clearing on a later non-playlist track
already worked (a plain `startTrack` flows `playlistName: null`), so 210's broken precondition
(the label never appearing) is fixed by the same change.

### 2. Sidebar didn't reload when the playing concert changed mid-open (`sidebar.spec.js:215`)
`beginPlayback` rewrites `playback` but never touched `sidebar`, and the track list is only fetched
on sidebar open. Changing concert via `startTrack` with the sidebar open left stale `sidebar.tracks`
rows stamped with the new `playback.concertId`. A new helper `refetchSidebarIfConcertChanged` bumps
`sidebar.loadGen` and dispatches `FetchTrackDetails` when the sidebar is open and the concert id
changed, applied at the two whole-album `beginPlayback` call sites (`ReceivedMediaInfo`,
`ReceivedQueueDrainResult`). The `concertId`-changed guard is load-bearing: the helper also runs on
every intra-album next/prev advance, and only that guard prevents a spurious refetch there.
Reconstruction plays do not use the helper (the sidebar renders from concert items). The existing
`ReceivedTrackDetails` stale-guards (loadGen / concertId) make a late response safe.

### 3. Sidebar delete in whole-album mode: advance + grey the row (`sidebar.spec.js:257`, `:232`)
`ReceivedDeleteTrackResult` advanced for `source:"bar"` but, for `source:"sidebar"`, only handled
concert-reconstruction mode and returned `[model, []]` in whole-album mode — so deleting the playing
track didn't advance (`:257`), and the deleted row never greyed out because nothing flips
`sidebar.tracks` availability (the delete command only swaps the concert card HTML), which `:232`
asserts. The sidebar/whole-album branch now flips the deleted track to `available: false` (new
`flipSidebarTrackAvailable`, mirroring `flipSidebarTrackLiked`) and, when the deleted track is the
playing one, calls the same `advanceAfterDelete` the bar source uses. The concert-reconstruction
branch (`RefreshConcertItems`) is unchanged.

### 4. Player bar was invisible — `active` class never set (critical PR #21 regression)
Found while verifying #1: the Foldkit port dropped what the old imperative player did on play/stop —
`bar.classList.add("active")` and `body.classList.add("player-active")`. CSS hides the bar until it
has the `active` class (`#player-bar { display: none }` / `#player-bar.active { display: flex }`), so
the player bar was effectively invisible in the app, and the playlist label (which lives in the bar)
could never show. The view now adds `active` to `#player-bar` reactively when media is loaded
(`hasMedia`), and `SyncNowPlayingMirror` — already appended by `withPlayback` on every
playback-identity change — toggles `body.player-active`. Both are reactive off playback identity, so
they can't desync from a missed code path (the failure mode of the original imperative approach).

## Tests

`update.story.test.ts` gains a `port-behavior fixes (#23)` block: playlist label set + null control;
sidebar refetch on concert change + the same-concert no-refetch control (pins the load-bearing
guard); sidebar delete advance+grey for the playing track, grey-without-advance for a non-playing
track, and a concert-mode regression guard that the branch still emits `RefreshConcertItems`.
`view.scene.test.ts` gains two cases for the bar `active` class (present with media, absent when
idle).

## Verification

- `just test-ts` — 152 green (148 prior + 7 new player Story cases... see suite).
- `just lint` — fmt/clippy/ts-check clean (`shellcheck` binary absent in the dev sandbox).
- `just ts-build` + `cargo build` — `static/player.js` re-embedded.
- Playwright: `playlists.spec.js:152/:210` and `sidebar.spec.js:215/:257` pass against a fresh
  fixture server.

## Follow-up (not in this PR)

Issue #23 stays open for adding the e2e suite to CI (so this drift class can't recur) — preference
recorded there: smoke subset on PRs, full suite on push-to-main, with `--single-process` made
conditional on CI. The two `--single-process` sandbox-crash tests (`sidebar:198/:232`) are to be
re-confirmed once real-browser CI runs them.
