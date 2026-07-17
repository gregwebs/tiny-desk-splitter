# Fix rapid host pause toggles using live media state

Resolves #40.

## Status

Implementation, review, and local non-browser verification complete; CI browser verification pending.

## Root cause

The player host command `TogglePause` branches on `model.isPlaying`. That model field is a
projection of native `play` and `pause` events and intentionally is not updated optimistically.
Two host toggles can therefore be reduced before the first native event updates the model. Both
observe the same stale value and both issue `PauseAudio`, leaving playback paused instead of
performing pause then resume.

The keyboard shortcut does not have this defect because its subscription samples the live media
element's `paused` property. Host callers cannot safely sample that property before dispatch, so
the read and playback mutation belong together in a player Command effect.

## State change

```text
Host TogglePause
      |
      v
ToggleAudio effect reads #player-audio.paused
      |
      +-- paused=false --> audio.pause() --> native pause event --> isPlaying=false
      |
      +-- paused=true  --> audio.play()  --> native play event  --> isPlaying=true
                              |
                              +-- rejected --> RejectedAudioPlay --> status error
```

Each invocation reads live media state when its effect runs. A second rapid invocation therefore
observes the first invocation's media mutation even when `model.isPlaying` has not caught up.
This relies on each Foldkit Command effect synchronously reading and mutating the media element
before the next host-toggle effect samples it. The real-runtime Playwright regression verifies
that ordering. The resume branch awaits the `play()` promise for error handling, but correctness
must depend on the media element's synchronous state transition, not promise-resolution order.

## Implementation Plan

### Test seams

- Command effect: execute the real DOM-dependent effect against a media element and assert that
  two consecutive effects pause then play using one getter-backed live `paused` state, without
  injecting model/native events between them. happy-dom does not implement real playback, so the
  fixture controls the getter-backed state through its `pause()` and `play()` functions. Assert
  exact missing-media behavior (`Acked`) and rejected playback (`RejectedAudioPlay`).
- Host port acceptance: call `window.Player.togglePause()` twice rapidly against the live
  application and assert that the media finishes playing.
- Message/command boundary: assert that host `TogglePause` delegates to `ToggleAudio` for both
  cached `model.isPlaying` values.

### Required changes

- [x] Add red command-effect tests in
  `concert-tracker/frontend/src/player/widget/toggleAudio.command.test.ts`.
- [x] Add a red host-command Story test in
  `concert-tracker/frontend/src/player/widget/update.story.test.ts`.
- [x] Add a red rapid-toggle Playwright regression in `e2e/player-queue.spec.js`.
- [x] Add `ToggleAudio` to `concert-tracker/frontend/src/player/widget/command.ts`, reading
  `HTMLMediaElement.paused` inside one Effect and preserving rejected-play handling.
- [x] Change `TogglePause` in
  `concert-tracker/frontend/src/player/widget/update/handleHostCommand.ts` to emit `ToggleAudio`.
- [x] Build the generated `concert-tracker/static/player.js` bundle.
- [x] Add `docs/player.md` as the lasting canonical explanation of event-derived player state and
  live DOM toggle decisions, and link it from `README.md`.
- [x] Update `model.ts`'s stale `TogglePause` cross-reference; review `message.ts` and
  `docs/change/2026-07-03-player-keyboard-shortcut-targets.md` cross-references for accuracy.
- [x] Perform adversarial engineering-lead review and required follow-up review.
- [ ] Commit, push, open a PR, and monitor CI.

### Verification plan

- Run the new command-effect test alone during red/green development.
- Run the relevant Story test alone during red/green development.
- Run the rapid host-toggle Playwright test repeatedly. Start known playing media, enable looping,
  dispatch both host calls in one `page.evaluate` task, and assert the deterministic command test's
  pause-then-play proof plus final live `paused === false` in the real application.
- Manually exercise one toggle, two rapid toggles, missing media, and rejected `play()` behavior.
- `just test-ts`
- `just ts-check`
- `just ts-lint`
- `just ts-build` followed by `./scripts/ts-verify.sh`
- `E2E_SANDBOX=1 npx playwright test e2e/player-queue.spec.js -g "rapid host toggles" --repeat-each=5`
- `just lint`
- `just test-rs`
- Full `npx playwright test` plus manual live verification on an isolated server/database/workdir.

## Change Record

### Implementation

- Added `ToggleAudio`, a DOM Command effect that reads live `HTMLMediaElement.paused` and performs
  the corresponding pause or resume operation. Missing media preserves existing host-toggle
  semantics by returning `Acked`; rejected resume returns `RejectedAudioPlay`.
- Changed the `TogglePause` host command to emit `ToggleAudio` without consulting cached model
  state. Explicit `PauseAudio` and `ResumeAudio` callers are unchanged.
- Rebuilt the committed `concert-tracker/static/player.js` bundle from TypeScript sources.

### Tests

- The command-effect test was observed red before implementation because `ToggleAudio` did not
  exist, then green after the effect was added. Its getter-backed happy-dom media fixture proves
  two consecutive effects call pause then play without native/model events between them.
- Story tests were observed red with the old handler emitting `PauseAudio`/`ResumeAudio`, then
  green after both cached `isPlaying` values delegated to `ToggleAudio`.
- Added a Playwright regression that starts looped playing media, instruments the real element's
  pause/play calls, sends both host toggles in one browser task, and asserts pause→play plus a
  final live playing state.

### Documentation

- Added `docs/player.md` and linked it from `README.md` as the canonical playback-state boundary.
- Updated the model invariant and the #28 Change Record cross-reference.
- Corrected `scripts/ts-verify.sh`'s stale reference to a nonexistent `just ts-verify` recipe.

### Review

The initial adversarial engineering-lead, Standards, and Spec reviews found no functional defect.
They requested narrower wording for the `isPlaying` invariant, a current Change Record, and an
accurate generated-bundle verification description. Those documentation findings were corrected;
the non-adversarial engineering-lead follow-up approved the implementation for verification.

### Verification to date

- `just ts-check` — passed.
- `just test-ts` — passed: 68 Node tests and 249 Vitest tests.
- `just ts-build` — rebuilt all committed frontend bundles.
- `./scripts/ts-verify.sh` — passed after the intended source and generated bundle were staged,
  proving a clean rebuild matches the committed artifact candidate.
- `just lint` — passed, including Rust formatting/clippy, shellcheck, TypeScript type-checking,
  oxlint, and the generated-player drift guard.
- `just test-rs` — passed: 792 tests.
- Isolated live server using a separate database, workdir, and port served the rebuilt
  `static/player.js`; the served bundle contains `ToggleAudio`.
- Local Playwright execution could not reach the test because Chromium crashed at launch with
  `SIGTRAP`, both normally and with the repository's `E2E_SANDBOX=1` single-process mode. The test
  remains scheduled for CI; visual/interaction verification is therefore delegated to the PR's
  Playwright job rather than claimed as locally complete.
