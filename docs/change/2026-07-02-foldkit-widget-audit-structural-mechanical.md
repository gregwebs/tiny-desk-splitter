# Foldkit widget quality pass: dead-code removal, switch→Match, Message renames, combobox ARIA

## Summary

Continuation of the `foldkit-skills:audit-program` remediation (blockers in #36, purity/mechanical
sweep in #37). This change covers the "mechanical batch" half of Group 6 — items #14, #15, #17, #18
from the remediation plan (`/Users/claude/.claude/plans/resilient-chasing-lovelace.md`) — reviewed
and approved by the engineering-lead agent before implementation (decisions recorded in
`.claude/agent-memory/engineering-lead/project_group6_review_decisions.md`).

Group 6's remaining items — #19 (migrating 96 CSS-selector Scene-test locators to accessible
role/label queries), #12 (decomposing the 238-line `view()` function and its duplicated row-button
blocks), and #13 (splitting the 211-line `handleCommand` function) — are deliberately deferred to a
follow-up, per explicit user direction to ship this checkpoint first. Item #16 (the
`sidebarResize`-Subscription-to-`Mount.defineStream` conversion) was pulled out of Group 6 entirely
during the engineering-lead review — it's the only item that touches real drag behavior and needs
Playwright verification, so it gets its own follow-up PR.

## #17: removed the dead `PlayTargetValue.Album` variant

`PlayTarget` (`player/widget/model.ts`) modeled a track/album discriminant, but only tracks ever
enter the prepare/poll flow — `PlayTargetValue.Album` was never constructed anywhere (whole-album
fetches are always either playable or a hard failure; concert-reconstruction items are never
prepared). Collapsed `PlayTarget` from `S.Union([TrackTarget, AlbumTarget])` to just `TrackTarget`,
and simplified `sameTarget` from a tag-switching function to a plain field comparison (no
discriminant left to switch on).

## #14: `switch` → `Match`

Converted 7 of 9 remaining raw `switch` statements to `Match`/`M.tagsExhaustive`/
`M.discriminatorsExhaustive` across `player/widget/update.ts` (`targetIdFor`, `listenUrlFor`,
`watchUrlFor` — over `PlaySource._tag`; `applyAdvanceFailure` — over the string-literal `AdvancePlan`,
using `M.whenOr` for the two plans sharing a handler) and `playlists/widget/command.ts`
(`fetchMembershipJson`, over `AddTarget.type` via `M.discriminatorsExhaustive("type")`).

**2 sites kept as `switch` with a `// NOTE:`**: `playlists/widget/{update,view}.ts`'s three
`row.kind` matches. `Row` (`playlists/core.ts`) is a flat interface — every kind shares the exact
same `{id, kind, name}` shape, `kind` is just a plain literal-union field, not a true per-variant
discriminated union. `Match.discriminatorsExhaustive` requires genuine per-variant type narrowing;
attempting it here made TypeScript infer `never` inside every branch (confirmed by trying it and
reverting). A plain `switch` is the structurally correct tool for this specific type shape.

## #15: `Received*` → `Succeeded*` Message renames (one pass)

Renamed 17 Messages in `player/widget/message.ts` and every reference across `update.ts`,
`command.ts`, `model.ts`, `subscription.ts`, and the Story/Scene/command test files:

- 8 straightforward `Received*` → `Succeeded*` renames: `SucceededMediaInfo`,
  `SucceededTrackInfoForEnqueue`, `SucceededPrepareStart`, `SucceededPrepareStatus`,
  `SucceededConcertItems`, `SucceededConcertPlaybackItems`, `SucceededPlaylistTracks`,
  `SucceededTrackDetails`.
- 2 exceptions flagged by engineering-lead review: `ReceivedQueueDrainResult` → `DrainedQueue`
  (outcome-neutral — carries an `Option`, no `Failed*` twin, so "Succeeded" is misleading) and
  `ReceivedDeleteTrackResult` → `CompletedDeleteTrack` (carries `ok: boolean`, folding success and
  failure into one message, so neither "Succeeded" nor "Failed" alone fits).
- `ReassertUi` → `SettledHtmxContent`, `SyncLikeFromSwap` → `SwappedLikeButton` (imperative → past-tense
  event names).
- `AudioPlaying`/`AudioPaused`/`AudioEnded`/`AudioErrored`/`AudioPlayRejected` → `StartedAudio`/
  `PausedAudio`/`EndedAudio`/`ErroredAudio`/`RejectedAudioPlay` (noun-first → verb-first past-tense).

Also fixed a stale doc comment claiming "No Subscription dispatches these yet" for the audio
messages — `subscription.ts`'s `audioEvents` Subscription already wires all five to the native
`<audio>` element's events; the comment predated that wiring landing.

Post-rename, grepped every old name across `src/` for stragglers (comments, doc strings) — none found.

## #18: playlists combobox — trash button no longer a descendant of `role="option"`

`memberRowView` (`playlists/widget/view.ts`) nested a "remove from playlist" `<button>` inside the
`<li role="option">` element. ARIA forbids interactive descendants of `option` — a screen reader
can't reliably reach a focusable child of an option the way it reaches a sibling. The
`aria-expanded="true"` finding from the original audit (on the filter `<input role="combobox">`) was
assessed as a near-non-issue during the engineering-lead review — the listbox is unconditionally
visible while the panel renders, so hardcoding it isn't wrong.

Fix: the `<li>` now carries `role="presentation"` (it's the visual flex-row wrapper only); the
`role="option"` / `id="add-pl-opt-<id>"` / `aria-selected` semantics moved to a new inner `<span>`,
with the trash `<button>` as that span's sibling, not its descendant. The inner span uses
`display: contents` so it stays invisible to `.add-pl-row`'s flex layout — `.add-pl-check`/
`.add-pl-name` remain direct flex items exactly as before (verified against `style.css`: `.add-pl-row`
is `display: flex`, `.add-pl-trash` pushes right via its own `margin-left: auto`, independent of the
option wrapper). The existing Scene test (`Scene.expectAll(Scene.all.role("option")).toHaveCount(2)`)
passed unchanged, confirming `display: contents` doesn't remove the role from the accessibility tree.

## Verification

- `npx vitest run` (concert-tracker/frontend) — 185 tests green, unchanged before/after.
- `npx tsc --noEmit` — clean.
- `cargo build` + `just lint` (`cargo fmt --check`, `cargo clippy -D warnings`, shellcheck,
  ts-check, oxlint) — clean.
- `node build.mjs` re-embedded `concert-tracker/static/{player,playlists}.js`.
- **Not independently browser-verified**: the #18 combobox restructure only has jsdom/happy-dom
  Scene-test coverage in this change, not a live-browser or Playwright check, per this session's
  scope. The CSS layout preservation is reasoned from `style.css`'s flex rules, not visually
  confirmed — flag for the next real-browser pass on this widget.

## Deferred (not in this change)

- Group 6 #19 (Scene-locator role/label migration), #12 (view/row-button decomposition), #13
  (`handleCommand` split) — explicit user direction to ship this checkpoint first; tracked in the
  plan file for a follow-up session.
- Group 6 #16 (`sidebarResize` → `Mount.defineStream`) — pulled into its own follow-up PR by the
  engineering-lead review (real drag-behavior change, needs Playwright verification).
- The `Playback` discriminated-union redesign remains its own future plan, as established in #36.
