# Recoverable Partial Splits — Implementation Plan

Implements [#143](https://github.com/gregwebs/tiny-desk-splitter/issues/143),
the fourth slice of parent [#139](https://github.com/gregwebs/tiny-desk-splitter/issues/139),
on top of #142's staged Published Concert Split operation.

## Scope

When a Concert Split fails after completing one or more song tracks and no
Published Concert Split exists, preserve those non-empty staged song tracks at
canonical filenames as a Recoverable Partial Split. This includes cut failures,
post-cut completeness-validation failures, and a complete-publication failure
after its ordinary rollback has restored the pre-run state. Report the partial
result through the library and CLI adapters, then persist its exact track
availability in the same terminal database transaction that records the failed
Job Run.

A failed resplit must leave an existing Published Concert Split, its manifest,
backup, files, and persisted availability unchanged. Failures before any song
track completes remain ordinary errors or `NoOutput` and publish no new track
state. Crash recovery and publication journals remain #144.

## TDD seams

Tests cross three externally meaningful seams:

1. `live_set_splitter::concert_split::run` for structured `Partial`,
   `Complete`, and `NoOutput` behavior plus canonical filesystem state.
2. The `live-set-splitter` CLI process contract, using a machine-readable
   outcome file in addition to its existing human output and exit status.
3. `jobs::split::start_split` / the Job Run terminal database state for atomic
   failed-run persistence and the resulting playback/UI model facts.

Focused publication tests may exercise the public publication module directly
to deterministically inject copy failures and distinguish a partial set from a
Published Concert Split. Production media-command failures at the main seam use
a small deterministic executable fixture or the existing real-FFmpeg fixtures;
tests do not mock private call sequences.

## State changes

```text
No Published Concert Split
          │
          ▼
    cut in staging
          │
     ┌────┴───────────────┐
     │ no song completed  │ one or more songs completed
     ▼                    ▼
 ordinary failure      validate completed song files
 canonical unchanged      │
                       copy under exclusive lock
                           │
                           ▼
                  Recoverable Partial Split
                  canonical song files + partial manifest
                  split_at remains NULL
                  Job Run terminal state = failed
                  tracks_present = exact salvaged songs
```

```text
Published Concert Split A
          │
          ▼
      resplit stages B
          │ cut/validation failure
          ▼
 Published Concert Split A unchanged
 manifest A + backup + canonical files unchanged
 persisted tracks_present unchanged
 Job Run terminal state = failed
```

```text
Job Run execution result
       │
       ├─ Complete ───────────────> success terminal transaction
       │
       ├─ Recoverable Partial ────> failure terminal transaction
       │                              ├─ persist exact tracks_present
       │                              ├─ clear split_started_at / keep split_at NULL
       │                              ├─ append SplitError event
       │                              └─ insert Failed Job
       │
       ├─ NoOutput::AnalysisOnly ─> success terminal transaction
       │                            without track completion
       │
       └─ ordinary failure or
          NoOutput::NothingDetected > failure terminal transaction
                                       without new track availability
```

## Detailed changes

### 1. Preserve completed production facts on a cut error

- Change `live-set-song-splitter/src/produce.rs` so segment production returns
  a typed result that retains the ordered `Vec<ProducedTrack>` accumulated
  before the first media-command error. The failure carries the original
  `anyhow::Error`; pre-cut validation failures carry an empty completed set.
- A song is completed only after every requested output format for that song
  was written successfully and is non-empty. A half-written `Both` output is
  not a completed track and is not eligible for partial publication.
- In `live-set-song-splitter/src/concert_split.rs`, route failures after song
  production through one `salvage_or_error` decision, including cutting,
  completeness validation, and complete publication after it rolls back. If no
  completed song exists, return the original error and let the staging guard
  remove all work. Never translate analysis/detection `NoOutput` into `Partial`.
- Derive exact eligible files from completed song titles and `OutputFormat`;
  exclude timestamps and interludes because partial output cannot prove a full
  reconstruction timeline.

### 2. Add an explicit Recoverable Partial Split publication state

- Extend `live-set-song-splitter/src/publication.rs` with a typed partial
  publication request/result and a durable
  `.concert-split-partial.json` manifest containing each song's title,
  start/end times, and exact canonical filenames. Validate finite ordered
  timing, unique titles, every relative filename, and every staged file before
  canonical mutation. This manifest contains enough information to rebuild the
  complete typed `Partial(ConcertSplitOutput)` on a later partial retry.
- Acquire the same per-concert exclusive publication lock used by complete
  publication. Copy each eligible file through a synced sibling temporary file
  and rename it into its canonical filename; install the partial manifest last.
- Refuse partial publication when `.concert-split-published.json` exists. Also
  conservatively refuse when a legacy directory without either manifest already
  contains any expected canonical split output, treating it as a prior
  Published Concert Split rather than risking replacement.
- Model canonical input as one validated state: `Empty`, `Partial`, or
  `Published`. Both manifests at once are an explicit error. A missing, empty,
  path-invalid, duplicate, or timing-invalid entry in an existing partial
  manifest fails before mutation; it is never silently dropped or inferred.
  Validate every retained and current partial title maps exactly once into the
  current request's set list before mutation, so filesystem publication cannot
  succeed with facts the Job failure transaction would reject.
- If a valid prior partial manifest exists, retain it and merge newly completed
  tracks by title; current completed bytes/timing replace the same prior title,
  while prior-only valid tracks remain available. Never infer ownership from
  extensions.
- Before either partial mutation or complete-over-partial mutation, copy the
  exact prior partial files and manifest into a per-attempt ephemeral rollback
  directory beside canonical output. This is not the retained Published backup.
  On copy, obsolete-removal, or manifest-install failure, remove attempt-only
  canonical files and restore every prior partial byte plus its manifest. If
  restoration fails, return both contexts and leave the rollback directory for
  #144 recovery rather than claiming a domain outcome.
- Teach complete publication to treat the partial manifest's exact files as
  partial-owned obsolete inputs, not as a previous Published Concert Split:
  do not create the known-good backup from partial files; overwrite replacement
  names and remove partial-only names after all replacement copies are ready.
  Remove the partial marker before atomically installing the Published manifest
  last. Any failure before that final rename restores the prior partial snapshot.
  After the Published manifest rename no fallible canonical mutation remains;
  rollback-snapshot cleanup is best-effort with a warning and cannot turn a
  committed publication into an error.
- Return `ConcertSplitOutcome::Partial(ConcertSplitOutput)` only after partial
  publication succeeds. Its `tracks` contains every currently available
  partial song in set-list order reconstructed from the validated manifest,
  its timestamps remain the current run's typed analysis result, and its
  `output_dir` is canonical. Prior partial timing is authoritative for retained
  tracks; current timing replaces a title completed again by the current run.

### 3. Carry the typed outcome through the CLI adapter

- Derive a stable serializable representation for the outcome data needed by
  adapters in `concert_split.rs` (outcome kind, produced song titles, and
  `NoOutputReason`). Do not make stderr parsing or filename scanning part of
  the adapter contract.
- Add an optional `--outcome-file PATH` argument to
  `live-set-song-splitter/src/main.rs`. After `run` returns a domain outcome,
  atomically write the structured report before rendering human output and
  exiting. Infrastructure `Err` leaves no successful report.
- Keep CLI compatibility: `Partial` still prints an error and exits non-zero;
  `Complete` and analysis-only retain exit zero; nothing-detected retains exit
  one. Replace the reserved-variant wording with Recoverable Partial Split
  diagnostics and completed-track count.
- Add a temporary outcome-file guard to `concert-tracker::jobs::SplitJob` and
  pass it from `build_cli_split_command`. The command-backed `JobRunner` reads
  and validates the report after the child exits, translating `Partial` to the
  same typed failure used by the library adapter. Missing/malformed reports on
  non-zero exit remain ordinary infrastructure failures with stderr context.

### 4. Make partial failure a typed Job Run terminal outcome

- Replace the string-only `JobStepOutcome::Failed` payload in
  `concert-tracker/src/jobs/mod.rs` with a typed `JobStepFailure` enum:
  `Ordinary { message }` and `RecoverablePartialSplit { message, tracks }`.
  Constructors for Download, Archive, Test Control, panic, and command failures
  use `Ordinary`; both split adapters construct the partial variant from the
  same serialized/domain report.
- Add `JobRequest::record_step_failure(&Connection, &JobStepFailure)` with a
  default implementation that delegates the failure message exactly once to
  `JobCancellation::record_failure`. Add a distinct
  `commit_step_failure_tx<R: JobRequest>` used only when `execute` returned a
  `JobStepOutcome::Failed`; it calls that hook once and inserts the Failed Job
  in the same transaction. Setup failure, panic, cancellation, restart, and
  shutdown continue through the existing `commit_failure_tx<R:
  JobCancellation>` string path, so they cannot carry or double-commit partial
  facts. Both paths still claim the same terminal gate before writing.
- Override the hook in `jobs/split.rs`. For a Recoverable Partial Split, map
  its validated unique track titles to the concert set list, persist the exact
  `tracks_present` vector, and call `mark_split_failed` in the same transaction.
  Reject unknown/duplicate titles so invalid adapter data rolls the whole
  terminal transaction back rather than persisting disagreement.
- Ordinary failed resplits do not rewrite `tracks_present`; therefore the
  previous Published Concert Split availability remains unchanged. Partial
  failure never calls `mark_split_succeeded`, never sets `split_at`, and never
  spawns successful dependents.

### 5. Preserve playback and UI semantics

- Keep individual track lookup driven by canonical files and persisted
  `tracks_present`, so partial tracks render and play through the existing
  Concert Media Inventory and `/concert-files` lock.
- Add a `split_at.is_some()` Published-success fact to
  `ConcertMediaInventory`; gate `reconstruction_items` on it because the current
  implementation can reconstruct from track presence and timestamps alone.
  Individual track lookup remains ungated so Recoverable Partial Split tracks
  are playable.
- Update `docs/concert-split.md`, `docs/data.md`, and `docs/jobs.md` with the
  partial manifest, adapter report, failure transaction, and state diagrams.
  Link only canonical durable design from README-reachable docs; keep verbose
  implementation/test history in the Change Record.

## Red/green vertical slices

1. **First failed split:** force the second song cut to fail; `run` returns
   `Partial`, only the first non-empty song is canonical, the partial manifest
   owns it, and no Published manifest/backup exists.
2. **Nothing completed:** fail the first cut; `run` returns the original error,
   canonical output and both manifests remain absent.
3. **Post-cut salvage:** after all songs complete, inject completeness
   validation failure and assert the first split returns `Partial`. Separately
   inject complete-publication failure, assert its rollback, then assert
   `salvage_or_error` publishes the completed songs as `Partial`.
4. **Failed resplit:** publish A, fail while staging B, and assert every byte,
   manifest entry, backup entry, and reported track in A is unchanged.
5. **Partial retry:** begin with a valid partial marker, complete another song,
   and assert both old and new valid tracks remain available with one merged
   partial manifest. Reject an otherwise-valid prior partial title absent from
   the current set list before filesystem mutation.
6. **Complete after partial:** publish a complete split over a partial set;
   assert canonical completeness, removal of partial-only managed files and the
   partial marker, a Published manifest, and no backup fabricated from partial.
   Inject failures at overwrite, obsolete removal, partial-marker removal, and
   Published-manifest installation; each restores the exact prior partial set.
7. **Partial publication rollback:** inject copy-N and partial-manifest install
   failures for both first publication and merge; assert no unowned new file,
   exact prior-byte restoration, and retryability. Reject corrupt partial
   manifests with missing, empty, path-invalid, duplicate, or invalid-timing
   entries before mutation.
8. **CLI translation:** run the binary through failures before and after the
   first completed track; assert exit status, human diagnostic, and structured
   outcome report for Partial, `NoOutput::AnalysisOnly`, and
   `NoOutput::NothingDetected`.
9. **Library translation:** assert `outcome_to_step` produces typed partial
   failure with exact titles, analysis-only success, and ordinary
   nothing-detected/infrastructure failures.
10. **Atomic Job failure:** submit a deterministic partial Split Job and assert
   one transaction produces `tracks_present`, SplitError/event history, and a
   Failed Job while `split_at` remains null. Inject persistence failure and
   prove no subset commits.
11. **NoOutput Job matrix:** drive both library-backed and CLI-backed Job Runs
    through analysis-only and nothing-detected reports; analysis-only succeeds
    under its existing contract, nothing-detected fails, and neither publishes
    or persists new canonical track availability.
12. **Failed resplit persistence:** seed a Published Concert Split and complete
   availability, return ordinary failure, and assert availability unchanged.
13. **Playback/UI model:** a partial track is individually available/playable,
    missing tracks render unavailable, and whole-concert reconstruction is not
    offered.

## Documentation

- Add `docs/change/2026-07-22-recoverable-partial-splits.md` with implementation,
  red/green evidence, review findings, verification, and CI status.
- Update the lasting `docs/concert-split.md` state machine and adapter contract.
- Update `docs/data.md` for `.concert-split-partial.json` and `docs/jobs.md` for
  the partial failure transaction. Check their README link paths and avoid
  duplicating the canonical rules in the Change Record.

## Verification

Run narrow tests after every vertical slice, then:

```sh
cargo test -p live-set-splitter concert_split
cargo test -p live-set-splitter publication
cargo test -p concert-tracker jobs::split --no-default-features
cargo test -p concert-tracker jobs::run --no-default-features
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
just test-rs
npx playwright test
git diff --check
```

Manual verification uses a new `/private/tmp/tiny-desk-splitter.XXXXXX`
directory, separate database/workdir, and a separate port:

1. Start `concert-web` in default library mode and exercise the backend first.
2. Use the checked-in tiny media fixture with an intentionally invalid second
   timestamp end (the first track remains valid) through the normal timestamp
   API; verify the Job is failed, only that track is playable, and
   whole-concert reconstruction is absent. If FFmpeg validates all timestamps
   before writing, perform this failure assertion through the deterministic
   Job Driver/API seam and retain real-FFmpeg coverage for complete/NoOutput.
3. Complete a split, then force a resplit failure and verify old playback and
   availability remain unchanged.
4. Repeat first-attempt partial behavior in `--splitter cli` mode and compare
   the terminal database/filesystem state.
5. Use Playwright for the partial track availability and reconstruction UI
   assertions, since #143 changes visible behavior.

## Checklist

- [x] TDD seams confirmed.
- [x] Plan reviewed and findings resolved.
- [x] Production retains completed-track facts on cut failure.
- [x] Partial publication manifest and exclusive copy operation implemented.
- [x] Failed resplit preserves Published output and availability.
- [x] Complete publication supersedes partial state without backing it up.
- [x] Structured CLI outcome report implemented and consumed.
- [x] Typed partial Job failure implemented.
- [x] Partial availability and Job failure commit atomically.
- [x] Partial playback/UI works without whole-concert reconstruction.
- [x] Lasting technical documentation and Change Record updated.
- [x] Two-axis code review passes.
- [x] Automated and scratch live verification pass.
- [x] Commit points to the Change Record and #143.
- [x] PR targets the surviving parent branch `concert-split-interface` after
  #142 merged there and its source branch was deleted; it resolves #143.
- [ ] GitHub Actions CI passes.
