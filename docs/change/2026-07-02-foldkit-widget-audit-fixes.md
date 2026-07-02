# Fix correctness, accessibility, and test-coverage blockers found by a Foldkit quality audit

## Summary

Ran the `foldkit-skills:audit-program` skill against the three embedded Foldkit widgets
(`concert-tracker/frontend/src/{player,playlists,splitter}/widget/`), grading them against the
live typing-game/examples exemplars along five dimensions in parallel: structural correctness,
Effect-TS idioms, naming/decomposition, accessibility, and testing. The audit reported 18
BLOCKER-level findings; this change fixes all of them (correctness bugs, accessibility, and
test-coverage gaps), engineering-lead-reviewed before implementation. The audit's remaining
QUALITY/NICE-TO-HAVE findings (Effect-idiom cleanup, decomposition, a `Playback` discriminated-union
redesign) are deliberately deferred to a separate follow-up — this change is scoped to blockers only.

## Correctness fixes

### Keyed-list identity bugs (`player/widget/view.ts`)

Three row lists used position- or non-unique keys, violating Foldkit's keyed-list identity
contract (vdom patches by key, not position — a key collision or a position-based key across a
mid-list delete can patch the wrong physical DOM node's event handlers onto the wrong row):

- **Queue rows**: keyed `song-${concertId}-${trackIdx}`. `ReceivedPlaylistTracks` appends
  playlist-group entries without deduping against an already-queued solo entry
  (`enqueueDedupe` is only used for solo `Enqueue`), so the same track queued once solo and once
  via a playlist produced two rows sharing one key. Fixed: key now includes `groupId` (`song-${groupId
  ?? "solo"}-${concertId}-${trackIdx}`).
- **Concert-reconstruction rows**: keyed `String(pos)` — a pure array-index position, not an
  identity. A sidebar delete mid-concert reindexes every item after it, so the item at a given
  `pos` can flip between a track row (4 buttons) and an interlude row (2 buttons) across renders
  while keeping the same key. Fixed: `track-${trackIdx}` / `interlude-${interludeIdx}`.
- **Whole-album rows**: keyed `String(track.index)` for both the available and unavailable
  branches — the same track index shares one key across its available/unavailable states, which
  have different button counts. Fixed: `avail-${index}` / `unavail-${index}`.

### `PlayAudio` Command could crash instead of degrading (`player/widget/command.ts`)

`PlayAudio` looked up `#player-audio` with the throwing `byId` helper inside `Effect.sync`, so a
missing element raised a defect that bypassed the Command's own `Effect.catch` entirely — the one
Command in the file inconsistent with its siblings (`PauseAudio`/`ResumeAudio`/`SeekAudio`/
`ClearAudioSrc` all use `byIdOrNull`). Fixed to use `byIdOrNull` and resolve to `AudioPlayRejected()`
when the element is absent, matching the rest of the file and satisfying the architecture's "Commands
never throw" invariant.

### Splitter sticky validation error (`splitter/widget/update.ts`)

`ChangedTimeInput`'s invalid-timecode branch set a `StatusError`, but the valid branch never
cleared it — a corrected entry left the stale error message on screen until an unrelated submit
or reset. Fixed: a valid `ChangedTimeInput` now clears an existing `StatusError` back to `NoStatus`.

## Accessibility fixes

- **Keyboard-dead controls**: `#player-track`/`#player-title` are `Role("button")` spans (not
  native `<button>`s, to match existing CSS), so Enter/Space did nothing — a pure mouse-only
  control. Added `OnKeyDownPreventDefault` activation via a shared `onActivateKey` helper.
- **Nameless controls**: play/pause (`#player-play-pause`) had no accessible name at all; queue
  "×" buttons (remove-from-queue, remove-group), the seek slider, and the playlists filter input
  were unlabeled. Added dynamic `AriaLabel` (play/pause reflects `isPlaying`) and per-row
  `AriaLabel`s naming the affected track/group.
- **Title-only accessible names**: mirrored `Title` text into `AriaLabel` on the remaining
  icon-only buttons across all three widgets (like stars, delete/add-to-playlist rows, open-external,
  prev/next, splitter's audition "▶").
- **Live regions**: none of the three widgets announced dynamic status text to screen readers.
  Added `Role("alert")` to error containers (`#player-error`, `add-pl-error`, splitter's toolbar
  status when `StatusError`) and `AriaLive("polite")` to non-error status text.
- **Toggle semantics**: added `AriaPressed` to every like-star toggle button (player bar, both
  sidebar lists) reflecting `liked` state.
- **`window.open` missing `noopener`**: `OpenInNewTab` now passes `"noopener"` as the third argument.
- **`@foldkit/ui` NOTE**: `@foldkit/ui` is not a project dependency (not in `package.json`).
  Per engineering-lead review, that's a separate dependency decision from this a11y pass — added a
  `// NOTE:` at the top of each widget's `view.ts` recording why controls are hand-rolled with
  explicit ARIA rather than adopting `Ui.*`, satisfying the audit checklist's requirement that
  hand-rolling be justified inline.
- The seek slider (`#player-seek`) was previously a live-looking but inert control (no `OnInput`,
  hardcoded `Value("0")`); it's now `Disabled(true)` until a later audio-Subscription commit wires
  it, rather than presenting broken interactivity.

## Test-coverage fixes

- **`playlists/widget`**: `FailedLoad` (both the matching and superseded-target paths) and
  `FailedMutation` (for all three mutation Commands — `AddItem`/`RemoveItem`/`CreateAndAdd`, both
  matching and stale-target) had no coverage at all — every fallible Command's failure path was
  untested. Also fixed a mislabeled test: "a failed mutation for a superseded target is ignored"
  actually exercised `CompletedMutation`, not `FailedMutation`; renamed and added a real
  stale-`FailedMutation` test alongside it.
- **`player/widget`**: `TrackMissing` (the prepare-flow entry point), `NotPlayable` (the
  fallback-to-external-URL path), and `PostDeleteInterlude`'s both outcomes (`CompletedDeleteInterlude`
  in and out of concert mode, `FailedDeleteInterlude`) had zero coverage — the sidebar
  interlude-delete flow the engineering-lead previously flagged as highest bug density. Also added
  the six previously-untested `Failed*` status-error paths: `FailedPrepareStart`,
  `FailedConcertPlayback`, `FailedPlaylistLoad`, `FailedOpenExternal`, `AudioPlayRejected`, and
  `ReceivedDeleteTrackResult{ok: false}`.

### New test category: `*.command.test.ts`

Foldkit's Story/Scene test harnesses never execute a Command's real `Effect` body — Commands are
resolved abstractly via `Story.Command.resolve`/`Scene.Command.resolve` by design, so neither
harness could exercise the actual `byId`-vs-`byIdOrNull` defect in `PlayAudio`. Added a new
`*.command.test.ts` file glob to `vitest.config.ts` for the rare Command whose Effect has
DOM-dependent branching that only a real Effect run against happy-dom can prove; used it for
`player/widget/playAudio.command.test.ts` (element present → `Acked()`, absent → `AudioPlayRejected()`,
never a throw).

## Process

Audit and remediation followed the `foldkit-skills:audit-program` skill: five parallel subagents
graded the three widgets against `packages/typing-game/client/src/` and `examples/embedding/`,
report delivered as BLOCKERS/QUALITY/NICE-TO-HAVE before any code changed. The remediation plan
was reviewed by the engineering-lead agent before implementation, which confirmed the top
correctness findings against the code, endorsed fixing Group 1 (correctness) standalone with its
own regression tests before any broader sweep, resequenced test-coverage additions ahead of the
(deferred) purity sweep so future refactors land on real coverage, and trimmed two findings out of
scope: the outbound-Port findings for `setNowPlaying`/`window.Playlists.openAdd` (host-glue for the
still-mid-port legacy imperative side) and the `@foldkit/ui` adoption question (a separate
dependency decision, not an a11y fix).

Each correctness fix (keyed-list identity, `PlayAudio`) was verified with a regression test
confirmed to fail against the pre-fix code and pass post-fix, via `git stash` on the production
file and rerunning the specific test.

## Deferred (not in this change)

The audit's QUALITY and NICE-TO-HAVE findings — Effect-TS idiom cleanup (spread-inside-`evo`,
`let`-based mutation, raw `switch` vs `Match`, empty-arg Command calls, `T[]` syntax), naming
(`Received*`→`Succeeded*`, `maybe*`/`is*` prefixes, single-letter callback params), decomposition
(919-line `update.ts`, 220-line `view` function), and the `Playback` discriminated-union redesign —
are out of scope for this change and left for follow-up work.

## Verification

- `npx vitest run` (concert-tracker/frontend) — 185 tests green (up from 156 pre-audit), across 7
  test files including the new `playAudio.command.test.ts`.
- `npx tsc --noEmit` — clean.
- `cargo build` + `just lint` (`cargo fmt --check`, `cargo clippy -D warnings`, shellcheck,
  ts-check) — clean.
- `node build.mjs` re-embedded `concert-tracker/static/{player,playlists,splitter}.js`.
