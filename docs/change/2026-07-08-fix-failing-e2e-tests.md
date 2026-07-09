# Fix the six failing e2e tests

## Problem

The Playwright CI job has been red since it was added on July 5. Six tests
failed:

- `concert-reconstruction.spec.js:88` — delete-interlude flow
- `interlude-tracks.spec.js:66` — source-redundant gate
- `player-queue.spec.js:386` — Space in an interactive control
- `player-queue.spec.js:593` — Enter on focused player-title
- `splitter.spec.js:77` — detach/gap/reset
- `delete-track.spec.js:114` — deleting every track

None were caused by any specific in-flight branch; all six reproduced against
`main`. Four had deterministic root causes (two product bugs, two
Foldkit-port-era regressions); two were races that only reproduced under CPU
contention (parallel local runs / CI), not in this sandbox's serialized
Playwright config.

## Root causes and fixes

### 1. `source_redundant` never checked that a source file exists

`concert-tracker/src/model.rs`'s `source_redundant()` determined whether the
downloaded source file was safe to delete purely from split-timestamp
coverage — it never checked that a source file was still *present*. Once a
user deleted the redundant source, the concert card re-rendered with
`source_redundant` still `true` (coverage hadn't changed), so the 🗑️ "Source
redundant" button reappeared and stayed forever, and the server-side delete
handler would happily report a second "deletion" of a file that no longer
existed.

This regressed in commit `7251f22a` (June 16, concert reconstruction
playback), which loosened the template gate from
`{% if source_redundant && can_listen %}` to `{% if source_redundant %}`
without moving the dropped `can_listen`-equivalent check (source presence)
into `source_redundant` itself.

**Fix**: `source_redundant()` now returns `false` immediately when
`find_downloaded_file` can't locate a source file. `find_downloaded_file`
moved from `jobs/mod.rs` to `model.rs` (re-exported from `jobs` for existing
callers) to avoid a circular `model → jobs → model` dependency, and now lives
next to the interlude-file lookups it's conceptually paired with.

Affected: `render_card`/`render_detail_card` (button visibility) and
`lifecycle::delete_redundant_source` (the 409-Conflict gate), both call
through the same function — one change covers both paths.

### 2. The seek slider and time display were never wired up in the Foldkit player port

`concert-tracker/frontend/src/player/widget/view.ts`'s `#player-seek` input
was hardcoded `Disabled(true)` with a comment marking it as a known gap: the
Foldkit port never implemented the `timeupdate`/`loadedmetadata` audio
subscription the pre-Foldkit `player.ts` had. `#player-time` was likewise
frozen at `"0:00 / 0:00"`. Playwright's `.focus()` silently no-ops on a
disabled element, so a test focusing `#player-seek` actually left focus
wherever it already was, and the subsequent Space keypress hit the global
pause shortcut instead of being swallowed by the (unfocused) seek control.

**Fix**: restored the missing subscription. Getting a stale-event race
genuinely closed took five rounds of review — two intermediate designs
(gating the Subscription's own acquisition on a model-side counter; then
adding a separate `expectedAudioSrc`/message-`src` check) each narrowed the
window but left a real gap, because both compared against state the *model*
tracked, updated at message-processing time, while the actual resource swap
(`audio.src = url`) happens later, in a separately forked Command's Effect.
The final design ties the correctness check directly to that Effect instead:

- `subscription.ts`'s existing `audioEvents` entry (unchanged dependencies,
  `{}`) now also listens for `timeupdate`/`loadedmetadata` on `#player-audio`
  and dispatches a new `UpdatedAudioTime({ currentTime, duration, loadGen })`
  message (guarded the same way the old `onTimeUpdate` was: no message until
  `duration` is finite and positive).
- `command.ts`'s `PlayAudio` now takes `{ url, loadGen }` and, in the same
  synchronous statement as `audio.src = url`, stamps `loadGen` onto
  `audio.dataset.audioLoadGen`. Since both mutations happen atomically in one
  `Effect.sync` block, the DOM element's own dataset always tells the truth
  about "which resource is actually loaded right now" — no dependency on
  message-processing order, Subscription re-acquisition timing, or same-URL
  replays (all of which defeated the earlier model-only designs).
- `subscription.ts`'s `audioTimeMessage` reads that DOM-stamped value back
  live (`Number(audio.dataset.audioLoadGen)`, defaulting to `-1` — which can
  never match a real generation — if PlayAudio has never touched the
  element) for every `UpdatedAudioTime` it emits.
- `model.ts` gained `audioTime: { currentTime, duration }` and a monotonic
  `audioLoadGen` counter (same staleness-guard idiom as the existing
  `sidebar.loadGen`). Both `beginPlayback` and `stopPlaybackPure` reset
  `audioTime` to zero and bump `audioLoadGen`; `beginPlayback` passes the new
  value straight to its `PlayAudio` command. `update.ts`'s `UpdatedAudioTime`
  handler compares the message's `loadGen` against `model.audioLoadGen` and
  discards a mismatch.
- `view.ts`'s seek input is now driven by `model.audioTime`: disabled only
  while `duration <= 0`, `max`/`value` in seconds (matching the pre-Foldkit
  behavior), and wired to dispatch `Seek` on `OnInput`. `#player-time` renders
  `formatTime(currentTime) / formatTime(duration)` (`formatTime` already
  existed in `core.ts`, unused until now).

### 3 & 4. Two e2e-test synchronization gaps

- **`playTrack` helper** (duplicated in `player-queue.spec.js`,
  `sidebar.spec.js`, `sync-player-persistence.spec.js`): only waited for
  `<audio>.paused` to flip, which happens before the Foldkit view re-renders
  `#player-title`. A test that immediately called `.focus()` on a player-bar
  element could race that render and land on an empty 0×0 span, where
  `.focus()` again silently no-ops. Fixed by also waiting for
  `#player-title` to be non-empty before returning.
- **Splitter gap-edit sequencing** (`splitter.spec.js`,
  `interlude-tracks.spec.js`'s `submitGapSplit`): committing a time-input
  edit fires on `blur` (`change` event); under contention, a second
  fill+blur could race a re-render still in flight from the first, and its
  `change` event would be lost. The tests previously asserted
  `toHaveValue(...)` on the very input just typed into — which stays showing
  the typed value in the DOM regardless of whether the model actually
  committed it, so that wait didn't prove anything. Fixed by polling the
  `.splitter-gap` block's rendered width instead — a different element,
  driven only by the committed model state, so it can only reflect a value
  once the corresponding `change` has actually landed.
- **`delete-track.spec.js`**: strengthened the per-iteration wait to also
  assert the swapped card's tracks-count text (server-rendered), not just
  the clicked button's absence, so the next click only fires once the full
  card replacement has settled.

### 5. A masked second bug in `concert-reconstruction.spec.js`

Fixing #1 let this test run past its original failure point for the first
time, which exposed a second, previously-unreached issue: the fixture's auto
split (`concert-tracker/examples/make_test_fixture.rs`) deliberately stops
at 19s of a ~20s source file, leaving a permanent trailing gap. Any test that
creates its own gap (as this one does, 5s–8s) therefore ends up with *two*
interludes — the deliberate one plus the fixture's baseline trailing one —
not the one the test's locators assumed. Fixed by asserting on the count (2)
and targeting the specific interlude index (1, the deliberate gap) for the
delete-and-verify step, leaving the baseline trailing interlude (2) untouched.

## Verification

- `cargo fmt --all -- --check`, `cargo build --locked --workspace
  --all-targets`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --locked --workspace` (434 lib tests + 75 web_integration
  tests, all passing) — clean
- `./scripts/ts-check.sh`, `./scripts/ts-lint.sh`, `./scripts/ts-test.sh` —
  clean (243 vitest + 68 node:test)
- `./scripts/ts-build.sh` then confirmed a second rebuild is byte-identical
  to the first (build determinism), and the resulting `static/player.js` is
  what CI's `ts-verify.sh` will diff against once committed
- `npx playwright test` — full local suite, 171/171 passing, including all
  six originally-failing tests
- Manual: played a track against a separate `--db`/`--workdir`, watched the
  seek thumb advance during playback and confirmed dragging it seeks;
  exercised the delete-redundant-source flow and confirmed the button
  disappears permanently while "Play concert" (reconstruction) stays visible
