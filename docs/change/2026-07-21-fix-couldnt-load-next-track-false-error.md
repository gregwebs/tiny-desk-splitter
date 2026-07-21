# Fix "Couldn't load next track" shown on normal end-of-playback

## Status

Complete. Implementation, tests, adversarial review, and local/e2e verification
all passed.

## Root cause

Reaching the end of a set list or queue is normal, but the player's auto-advance
chain treated it as a fetch failure and showed an error banner every time playback
naturally ended.

The chain: a track's `ended` event → `EndedAudio` → `advanceOrCollapse` →
`DrainQueue({ plan: "next-or-collapse" })` → (queue empty) → `advanceToNextTrack`
→ `FetchNextTrackInfo`, which calls `GET /concerts/{id}/tracks/{idx}/next-media-info`.

The backend already returns **HTTP 404** for "no later playable track" — its
normal, documented signal for end-of-list (`handlers.rs`'s `next_track_media_info`).
But the client's `getJson` throws `ApiError(404)` on any non-2xx response, and
`FetchNextTrackInfo`'s `Effect.catch` collapsed every failure — the benign 404
included — into `FailedNextTrackInfo`. That message sets the error status
`"Couldn't load next track"` for every `AdvancePlan` except `next-or-stop`; the
natural end-of-track path uses `next-or-collapse`, so the banner fired on every
finished track with nothing left to play.

## State change

```text
Track ends
   |
   v
EndedAudio -> advanceOrCollapse -> DrainQueue(plan) -> (queue empty)
   |
   v
advanceToNextTrack -> FetchNextTrackInfo -> GET .../next-media-info
   |
   +-- 200 OK       --> SucceededMediaInfo         (unchanged: plays the next track)
   |
   +-- 404 (no next) --> NoNextTrack(plan)          (NEW: benign, no error status)
   |                        -> applyAdvanceFailure(model, plan)  [stop/collapse only]
   |
   +-- other failure --> FailedNextTrackInfo(plan)  (unchanged: real error)
                            -> status = Error("Couldn't load next track") for
                               every plan except next-or-stop
```

This mirrors an existing pattern in the same file: `FetchTrackInfo` already
distinguishes "track missing" (`getTrackMediaInfoOrNull` → `null` → `TrackMissing`,
benign) from a genuine fetch error. `FetchNextTrackInfo` now follows the same
shape for the "no next track" 404.

## Implementation

- Added `getJsonNullOn404<T>` to `concert-tracker/frontend/src/api/client.ts`: a
  generic GET helper that returns `null` on a 404 but still throws `ApiError` on
  any other non-2xx (unlike `getJsonOrNull`, which swallows all non-2xx into
  `null` and would hide a genuine 500 as if it were the benign case). Added
  `getNextTrackMediaInfoOrNull` as the typed wrapper over it, alongside the
  existing `getNextTrackMediaInfo` (still used by `ResolveFirstAvailableTrack`,
  unchanged).
- Added a new message `NoNextTrack({ plan })` in
  `concert-tracker/frontend/src/player/widget/message.ts`, documented as the
  normal "no later playable track" outcome, distinct from `FailedNextTrackInfo`.
- Changed `FetchNextTrackInfo` in
  `concert-tracker/frontend/src/player/widget/command.ts` to call
  `getNextTrackMediaInfoOrNull`; a `null` result now resolves to `NoNextTrack`, a
  real result still resolves to `SucceededMediaInfo`, and the `Effect.catch`
  still resolves to `FailedNextTrackInfo` for genuine errors (network failure,
  5xx, etc.).
- Added the `NoNextTrack` reducer case in
  `concert-tracker/frontend/src/player/widget/update.ts`:
  `applyAdvanceFailure(model, plan)` — the same plan-specific stop/collapse
  logic `FailedNextTrackInfo` already used, minus the error status.
  `applyAdvanceFailure` already sets `isPlaying: false` on every branch, so no
  extra `evo` wrap was needed.
- Rebuilt the committed `concert-tracker/static/player.js` bundle from the
  updated TypeScript sources (`just ts-build`); confirmed the rebuild is
  deterministic (identical SHA-256 across two consecutive builds).

### Behavior nuance (intended)

`NoNextTrack`'s branches do not clear `model.status`. On a natural end from
`Idle`, it stays `Idle` (no banner). `ErroredAudio` also routes through
`advanceOrCollapse`, so on an end-of-list path reached after a real media error,
this now *preserves* the accurate `"Failed to load media"` status instead of
overwriting it with `"Couldn't load next track"` as the old code did — a
secondary improvement, not just a side effect.

## Tests

Two seams, per the engineering-lead review that shaped this plan: the pure
reducer alone doesn't cover the actual fix, since Story/Scene tests resolve
commands with a hand-picked message and never exercise the client's status-code
mapping.

- **Reducer** (`update.story.test.ts`, new `describe("player update — end of
  playback (non-concert)")`):
  - `EndedAudio` with an empty queue and no next track resolves the full chain
    (`DrainQueue` → `DrainedQueue` → `FetchNextTrackInfo` → `NoNextTrack`) and
    ends with `status: Idle`, `isPlaying: false`.
  - `NoNextTrack({ plan: "next-or-collapse" })` also closes an open video panel
    with no error.
  - `NoNextTrack({ plan: "next-or-stop" })` stops playback cleanly with no
    error.
  - `FailedNextTrackInfo({ plan: "next-or-collapse" })` still surfaces
    `status: Error("Couldn't load next track")` — pins the genuine-failure path
    so it can't silently regress back to always-benign.
- **Client** (new `frontend/src/api/client.unit.test.ts`): `getJsonNullOn404`
  against a mocked `fetch` — returns `null` on a 404 `Response`, throws
  `ApiError` on a 500, returns the parsed body on 200. This is the test that
  would catch a future "simplify to `getJsonOrNull`" silently swallowing real
  server errors into the benign case.

All new tests were run and observed passing (256/256 vitest tests, including the
3 new client tests and 4 new reducer tests); the existing suite was unaffected.

## Verification to date

- `just ts-check` — passed (part of `just lint`).
- `just ts-lint` — passed (part of `just lint`).
- `just lint` — passed: `cargo fmt --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, shellcheck, ts-check, ts-lint.
- `just test-ts` — passed: 68 node:test tests, 256 Vitest tests (12 files).
- `just test-rs` — passed: 819 tests.
- `just ts-build` then `cargo build -p concert-tracker` — passed; the embedded
  `static/player.js` compiles.
- Rebuild determinism spot-check: rebuilding twice in a row produced a
  byte-identical `static/player.js` (same SHA-256), confirming the large diff
  against the pre-fix committed bundle is the real, intended source change —
  not build non-determinism.

- Added an e2e regression test in `e2e/player-queue.spec.js` ("track ending
  with nothing next stops cleanly without an error banner"): plays the last
  track of the `AUDIO` fixture concert, dispatches a synthetic `ended` event
  via the existing `simulateTrackEnd` helper, and asserts `#player-error`
  stays hidden and playback is paused. Ran against a real Chromium via
  `E2E_SANDBOX=1 npx playwright test e2e/player-queue.spec.js` — all 77 tests
  in the file passed, including this one. Also ran
  `e2e/concert-reconstruction.spec.js` (3 tests) since concert-reconstruction
  mode shares the `advanceOrCollapse` entry point — all passed, confirming
  that path (which never called `FetchNextTrackInfo` to begin with) is
  unaffected.

## Review

An adversarial engineering-lead review (Codex's app-server was unavailable in
this environment — `Operation not permitted (os error 1)` persisted even with
the sandbox disabled, despite `codex login status` confirming a valid login;
fell back to the project's documented Claude-subagent path) verdict:
**approve-with-changes**. The core fix was confirmed correct: exhaustiveness
checked by `tsc`, the 404-vs-other-non-2xx split verified at the one call site
that matters, the clean-end/`ErroredAudio`-preserved-status/genuine-failure-still-errors
behaviors all traced and confirmed, and the regenerated `static/player.js`
verified as a faithful, non-hand-edited rebuild. One required change: an
unrelated `CONTEXT.md` glossary edit (pre-existing in the working tree before
this branch was created, unrelated to this fix) was excluded from the commit.

### Out of scope (follow-up noted, not fixed here)

`ResolveFirstAvailableTrack` (`command.ts`) still uses the older
`getNextTrackMediaInfo` + `Effect.catch(() => null)`, which conflates a genuine
500 with the benign "no first track" 404 — the same bug class this change
fixes, one call site over. Blast radius there is smaller (fails to start
initial playback rather than showing a false error banner), so it's left alone
here, but a future pass could switch it to `getNextTrackMediaInfoOrNull` now
that helper exists, so the two call sites don't diverge in behavior.
