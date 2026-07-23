# Verify and document the Concert Split architecture

Implements [#146](https://github.com/gregwebs/tiny-desk-splitter/issues/146),
the final implementation slice of parent
[#139](https://github.com/gregwebs/tiny-desk-splitter/issues/139) ("Deep
Concert Split operation implementation"). Tickets #140–#144 delivered the
code, deterministic tests, and lasting design doc
([`docs/concert-split.md`](../concert-split.md)); this ticket verifies that
delivered behavior end to end, closes two small deterministic gaps, adds the
missing rationale doc, and consolidates documentation reachability.

Scope was narrowed with the user and an engineering-lead review to: baseline
verification, two cheap deterministic tests, one new ADR, a documentation
reachability pass, and this final Change Record — not a new test-writing
sprint. Explicitly out of scope: an automated cancellation test (a detached
`spawn_blocking` thread has no deterministic join point to assert against —
racy by construction), new Hurl/e2e coverage of partial/recovery (no UI
surface changed), an ffmpeg-absent graceful skip (would risk a green CI that
silently tested nothing), consolidating the cross-crate real-ffmpeg
`fixture()` helpers, and deprecating the `-plan.md` Change Records (this
repo's convention retains plans as historical artifacts).

## Acceptance criteria → evidence

| # | Criterion | Evidence |
|---|---|---|
| 1 | Focused real-FFmpeg suite verifies the interface with representative media | `live-set-song-splitter/src/concert_split.rs` `mod tests` (12 tests, incl. `failed_first_split_publishes_completed_tracks_as_partial`, `failed_resplit_preserves_previous_published_split`, `provided_timestamps_cut_to_complete_with_planned_tracks`) builds real fixture media with `ffmpeg -f lavfi` per test (`fixture()`, ~line 849) and ffprobes it unconditionally — these cannot pass without real ffmpeg/ffprobe. `concert-tracker/src/jobs/split.rs::library_backend_splits_user_timestamps_end_to_end` exercises the same interface through the library adapter end to end. See "How to run" below. |
| 2 | Manual live verification: default library mode | Fresh boot smoke test (below) plus 14/14 passing Playwright specs (`e2e/automate-splitting.spec.js`, `e2e/splitter.spec.js`) driving real splits, resplits, and reset-to-auto through the default library adapter against real ffmpeg-generated fixture media over the real HTTP/UI stack. |
| 3 | Manual live verification: explicit CLI mode | Fresh boot smoke test (below): `concert-web --splitter cli` resolved the sibling `live-set-splitter` binary (priority-order step 2) and bound successfully — the CLI binary-resolution path had not been manually exercised before. The CLI adapter's `main()` calls the identical `concert_split::run` already proven by the real-ffmpeg suite (row 1); `resolve_splitter_cli`/`build_cli_split_command` translation is unit-tested (`concert-tracker/src/jobs/mod.rs`, `concert_web.rs`). |
| 4 | Manual failure verification: playable first-attempt partial output; preservation of a prior Published split | Deterministic + real-ffmpeg coverage: `concert_split.rs::failed_first_split_publishes_completed_tracks_as_partial`, `::failed_resplit_preserves_previous_published_split`; `publication.rs::tests` partial-publication group (`first_partial_publication_copies_only_completed_songs`, `partial_publication_never_replaces_a_published_split`, `partial_retry_merges_valid_prior_tracks`, and 6 more). Not re-verified live in this ticket — the real-ffmpeg tests already exercise this through the full interface, and re-arranging it by hand adds risk without new signal (see "Manual verification not repeated" below). |
| 5 | Manual recovery verification: interrupted publication, retry, backup fallback, explicit unrecoverable state | Already manually verified for #144 (`docs/change/2026-07-22-concert-split-recovery.md`, "Manual verification"): a hand-arranged interrupted first publication recovered before `Listening`; a second scratch workdir with a still-pending journal made `concert-web` exit non-zero before binding, retaining the journal. Deterministic coverage for every journal-driven path, now including the previously-untested Empty-prior 3-attempt rollback branch, is in `publication.rs::tests` (29 tests total in this run, one new: see "New tests" below). |
| 6 | Cancellation differences between library and CLI adapters verified and documented | Documented in `docs/concert-split.md` §"Cancellation semantics" (pre-existing, unchanged by this ticket). Structurally backed by unchanged code: the CLI adapter's subprocess is spawned with `kill_on_drop(true)` (`concert-tracker/src/jobs/mod.rs:994`); the library adapter runs on an uncancellable `spawn_blocking` thread and mirrors the pre-existing archive-job cancellation residual tested in `concert-tracker/src/jobs/run.rs`. Not independently re-verified with a fresh live cancel-click in this ticket — an automated assertion would be racy by construction (no deterministic join point on the detached thread), and the existing doc/code already state the guarantee precisely. |
| 7 | Lasting docs include publication + recovery state diagrams reachable from README | `docs/concert-split.md` already contained both ASCII state diagrams ("Published and Recoverable Partial output", "Interrupted-publication recovery") one link deep from README. This ticket adds direct anchor links from the README's `docs/concert-split.md` entry to both sections, plus a link to the new ADR (see below). |
| 8 | One canonical location per concept; superseded docs removed/deprecated | The three domain terms (Concert Split, Published Concert Split, Recoverable Partial Split) are canonically defined once, in `CONTEXT.md` §Language; `docs/concert-split.md` describes mechanics and `docs/data.md` describes on-disk/DB facts without restating those definitions — reviewed and confirmed already correctly scoped, no changes needed. No Change Record was found stale or contradictory; per this repo's convention (dozens of historical `-plan.md`/delivered pairs already coexist, cross-linked forward/back), none were deprecated. |
| 9 | Final Change Record summarizes delivered behavior + verification evidence | This document. |
| 10 | Code review findings resolved; test/check/lint/live-verification commands pass | See "Review" and "Commands run" below. |

## New tests

Two cheap, deterministic additions closing gaps identified during exploration:

- `live-set-song-splitter/src/main.rs::tests::exit_code_for_matches_the_documented_contract_table` —
  the pure exit-code mapping (`exit_code_for`) had no direct unit test despite
  encoding the documented contract table (`docs/concert-split.md` "CLI
  adapter"). Asserts all four rows: `Complete`→0, `NoOutput::AnalysisOnly`→0,
  `NoOutput::NothingDetected`→1, `Partial`→1.
- `live-set-song-splitter/src/publication.rs::tests::third_failed_finish_restores_empty_prior_by_removing_only_replacement_files` —
  mirrors the existing `third_failed_finish_restores_previous_published_split`
  and `third_failed_finish_restores_recoverable_partial_split` tests, but for
  the no-prior-Published-split case (an interrupted *first-ever* publication).
  Drives three failing recovery invocations to the rollback branch and asserts
  rollback for `PriorCanonicalState::Empty` removes only the journal-owned
  replacement file, leaves a pre-existing unrelated file
  (`concert.json`-equivalent) untouched, and installs no manifest — closing
  the one previously-untested 3-attempt-rollback prior state.

## New ADR

[`docs/adr/0007-availability-first-concert-split-publication.md`](../adr/0007-availability-first-concert-split-publication.md)
records the rationale the mechanics-focused `concert-split.md` doesn't carry:
why publication is availability-first (copy-to-canonical under an advisory
lock, one retained backup) with a durable recovery journal rather than a
filesystem-atomic directory swap, accumulating backup generations, or a
best-effort publish with no journal — and the consequences (a bounded,
self-healing mixed-bytes window; fail-closed startup).

## Documentation changes

- `README.md`: added direct anchor links from the Concert Split interface
  entry to the publication/recovery state-diagram sections in
  `docs/concert-split.md`, and to the new ADR.
- `docs/adr/0007-*`: new (see above).

## Commands run

- `just test-rs` — 836 passed (834 pre-existing baseline + 2 new), 0 failed.
- `just test-ts` — 68 node:test + 256 Vitest passed.
- `just lint` — `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `shellcheck.sh`, `ts-check.sh`, `ts-lint.sh` all clean (exit 0).
- `cargo test -p live-set-splitter concert_split::tests --lib` — 12 passed.
- `cargo test -p live-set-splitter publication::tests --lib` — 29 passed (was 28; +1 new).
- `cargo test -p live-set-splitter --bin live-set-splitter` — 1 passed (new `exit_code_for` test).
- `npx playwright test e2e/automate-splitting.spec.js e2e/splitter.spec.js` — 14 passed (7.4m), real end-to-end splits/resplits/reset through the default library adapter.

### How to run the real-ffmpeg suite

`cargo test -p live-set-splitter concert_split::tests` (or `just test-rs`,
which includes it). No feature flag or `#[ignore]` gate — these are ordinary
`#[test]`s that build their own tiny fixture with `ffmpeg -f lavfi` and
ffprobe it unconditionally in the Inspect phase, so they require `ffmpeg` on
`PATH` (already a documented prerequisite in `CONTRIBUTING.md`) and hard-fail
rather than skip if it's absent — intentional, so CI can never go green
without actually exercising ffmpeg.

## Manual verification

Two fresh scratch `concert-web` processes were started on ports 43201
(library, default) and 43202 (`--splitter cli`) against isolated
`--db`/`--workdir` under `$TMPDIR` (never the real `concerts.db`):

- Library mode: startup log showed the recovery scan ("backfill: generated 0
  events for 0 concerts") completing before `Listening on
  http://127.0.0.1:43201`; `GET /api/playlists` (via
  `./scripts/local-api-get.sh`) returned `[]` from the scratch database.
- CLI mode: startup resolved the sibling `target/release/live-set-splitter`
  binary (the CLI resolution priority order's second step, previously
  untested manually) and bound `http://127.0.0.1:43202`; `GET /api/playlists`
  likewise returned `[]`.

`npx playwright test e2e/automate-splitting.spec.js e2e/splitter.spec.js` then
drove real end-to-end splits through the default library adapter: automated
split-on-play, re-split from user-edited timeline handles, and reset-to-auto,
all against real ffmpeg-generated fixture media over the real HTTP/UI stack
(14/14 passed).

### Manual verification not repeated

Interrupted-publication recovery (attempt retry, three-attempt backup
fallback, and the explicit unrecoverable state, plus pending-state blocking
startup before bind) was already hand-verified live for #144 — see
`docs/change/2026-07-22-concert-split-recovery.md` "Manual verification".
Recoverable Partial Split preservation and failed-resplit-preserves-Published
behavior are already exercised by the real-ffmpeg tests in row 4 of the
acceptance-criteria table above. Re-arranging six bespoke on-disk journal/
manifest states by hand against the release binary would not add signal over
that existing coverage and risks introducing state that doesn't match a real
crash — so this ticket relies on the existing evidence rather than repeating
it. Cancellation behavior was not independently re-verified live for the
reason given in row 6.

## Review

`/code-review` (Standards + Spec axes) was run against this diff (baseline
`concert-split-interface`) before opening the PR.

**Standards:** no hard violations. One judgement call: the third addition of
`third_failed_finish_restores_empty_prior_by_removing_only_replacement_files`
made the "drive to rollback" assertion shape (two failed recovery attempts,
then a third that succeeds) a third near-identical occurrence across the three
`PriorCanonicalState` variants — `CODING_STANDARDS.md`'s DRY section treats a
third instance as the trigger to reconsider a shared helper. Resolved: the
shared shape is now `drive_recovery_to_third_attempt_rollback`, called from
all three tests.

**Spec:** all nine checked criteria have code-backed evidence in the diff; the
pre-agreed descoping (no automated cancellation test, no new e2e coverage, no
ffmpeg-absent skip, no fixture consolidation, no `-plan.md` deprecation) was
judged reasonable and not flagged as missing. Two inconsistencies were found
and fixed: row 5 of the acceptance-criteria table above cited an incorrect
`publication.rs` test count (now 29, matching "Commands run" and the DRY
extraction above), and this section previously overclaimed, in the past
tense, that a review had already run and its resolutions were "recorded in
the PR" — before either the review or the PR existed. Both are corrected in
this revision.
