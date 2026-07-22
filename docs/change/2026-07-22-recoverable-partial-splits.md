# Recoverable Partial Splits

Issue [#143](https://github.com/gregwebs/tiny-desk-splitter/issues/143)
adds the Recoverable Partial Split state to the staged publication workflow
introduced by #142.

## Outcome

When a first Concert Split fails after producing at least one complete,
non-empty song, those songs are copied from the per-run staging directory to
their ordinary canonical filenames. A `.concert-split-partial.json` manifest
owns the exact subset. The Job Run still fails, leaves `split_at` unset, and
commits exact track availability with its SplitError and Failed Job.

A failed resplit never replaces an existing Published Concert Split. A later
partial retry merges valid tracks by title; a later complete retry supersedes
partial state without treating it as the known-good backup.

```text
first attempt:  Empty → stage songs → failure → Partial + Failed Job
failed resplit: Published A → stage B → failure → Published A unchanged
retry success:  Partial → stage complete → Published (no partial backup)
```

## Implementation

- Segment production retains ordered completed-track facts with its error.
- Library orchestration uses one salvage path for cut, completeness-validation,
  and complete-publication failures.
- Partial publication validates staged and prior files before mutation, uses
  the existing exclusive publication lock, installs canonical copies before
  its manifest, and restores an exact ephemeral snapshot on ordinary failure.
- The CLI optionally writes a stable minimal `ConcertSplitReport` through
  `--outcome-file`; the subprocess adapter consumes that report instead of
  parsing stderr or coupling to internal paths and timestamps.
- `JobStepFailure` distinguishes ordinary failures from
  `RecoverablePartialSplit { message, tracks }`. Split's failure hook writes
  exact availability and failure history in one terminal transaction.
- Concert reconstruction and redundant-source decisions now require successful
  Published split state; individual partial tracks remain available.

## TDD and verification record

The agreed seams were `concert_split::run`, the CLI structured-report process
contract, and Job Run terminal state. Focused publication tests cover partial
merge, first-attempt and retry rollback, Published refusal, and complete-over-
partial behavior. Real FFmpeg tests cover first-attempt salvage and failed
resplit preservation. Adapter tests cover partial, analysis-only, and nothing-
detected translation. Job tests cover the atomic partial failure facts and the
media inventory test proves partial playback without reconstruction.

Commands and final results are recorded here after final verification:

- `cargo test -p live-set-splitter concert_split::tests --lib` — 10 passed.
- `cargo test -p live-set-splitter publication::tests --lib` — 18 passed.
- Focused command/library adapter and Job Run tests — passed.
- `cargo nextest run --tests --no-fail-fast` — 821 passed.
- `npx playwright test` — 173 passed.

## Review and follow-up

The implementation-plan review required exact rollback snapshots, typed Job
failure data, complete-over-partial semantics, and a reconstruction success
gate; those findings are incorporated. Crash-interrupted publication and its
durable recovery journal were subsequently implemented by #144; see
`2026-07-22-concert-split-recovery.md`.
