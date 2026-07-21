# Extract the synchronous Concert Split interface

Implements [#140](https://github.com/gregwebs/tiny-desk-splitter/issues/140),
the first implementation slice of
[#139 — Deep Concert Split operation implementation](https://github.com/gregwebs/tiny-desk-splitter/issues/139)
(spec: [#138](https://github.com/gregwebs/tiny-desk-splitter/issues/138)).

## Purpose

The full splitting workflow — validation, ffprobe inspection, OCR text-overlay
detection, silence-based recovery, audio-analysis refinement, cutting, output
production, and cleanup — lived entirely inside the `live-set-splitter` CLI
binary (`live-set-song-splitter/src/main.rs`, ~2,815 lines). `concert-web`
exercises this workflow only by shelling out to the CLI binary as a
subprocess, writing concert metadata and timestamps to temporary JSON
transport files and inferring success/failure from the exit code and files on
disk. This made the workflow impossible to call in-process, gave it no typed
result, and required the splitter executable to be built separately from
`concert-web` in development.

This change extracts the workflow behind a synchronous, library-owned
`live_set_splitter::concert_split::run` interface, with the CLI reduced to a
thin adapter (argument translation, progress rendering, exit-code mapping).
Wiring `concert-web` to call this library directly (making it the web app's
default splitter adapter) is **out of scope** here — that is
[#141](https://github.com/gregwebs/tiny-desk-splitter/issues/141);
`concert-tracker` does not yet depend on this crate.

## Design

See [`docs/concert-split.md`](../concert-split.md) for the full interface,
state diagram, and CLI exit-code mapping. Summary:

- `ConcertSplitRequest` carries typed `ConcertInfo`, the resolved input file
  and output directory, optional explicit timestamps (mirrors
  `--timestamps-file`), and `ConcertSplitOptions` (the CLI's existing tuning
  flags, unchanged) — no temporary JSON transport files.
- `ConcertSplitProgress` events (`PhaseStarted`, `CutPlanned { total }`,
  `TrackCompleted`, `Warning`, `Diagnostic`) are emitted through a
  `&mut dyn FnMut(ConcertSplitProgress)` sink — the deliberate seam for #141
  to forward events over a channel from inside `spawn_blocking`, without
  imposing a channel type on this library.
- `ConcertSplitOutcome` distinguishes domain results (`Complete`, `NoOutput {
  AnalysisOnly | NothingDetected }`) from infrastructure errors
  (`Err(anyhow::Error)`). `Partial` is a **reserved, unconstructed** variant
  for #142+'s Recoverable Partial Split — today's workflow is binary (the
  missing-songs gate returns `NoOutput` before any cutting starts), so `run`
  never produces `Partial`; a dedicated test locks this classification
  boundary.
- The workflow moved out of `main.rs` into four new top-level library modules
  grouped by phase (`detect.rs`, `recover.rs`, `refine.rs`, `produce.rs`),
  plus the thin `concert_split.rs` interface module. `audio`, `video`, and
  `io` — previously binary-private — are now library modules alongside the
  existing `cut`, `ffmpeg`, `image`, `ocr`, `ocr_backend`. Migrated internals
  are `pub(crate)`; only the `ConcertSplit*` types and re-exported
  `OutputFormat`/`OcrChoice`/`VideoCutMode`/`SongTimestamp` are `pub`.
- **Output-writing parity**: `run` always computes the outcome timestamps
  (so a library caller gets them without reading a file back), but writes
  `timestamps.json` only under the CLI's original condition
  (`timestamps.is_none() || options.refine_timestamps`) — so user-timestamp
  and reset-to-auto runs still write no file, exactly as before. `concert.json`
  (a byte-for-byte copy of the input) is written by the **CLI adapter**, not
  the library, since only the CLI has the original file path — the library
  only produces its own computed `timestamps.json`. Neither concert-web's
  `split.rs` nor `normalize.rs` is affected: the former reads only
  `timestamps.json`, and `concert.json` is still produced exactly as before.
- **Validation moved into `run`'s Validate phase** (OCR-backend availability,
  non-empty set list, non-empty explicit timestamps, input-file existence) so
  a future in-process caller that bypasses the CLI's `build_request` is still
  self-protected. The CLI's `build_request` now only does transport work
  (parsing the concert/timestamps JSON, resolving paths).
- **Exit-code parity**: `Complete` and `NoOutput { AnalysisOnly }` map to exit
  0 (matching past success); `NoOutput { NothingDetected }` maps to exit 1
  with the missing-titles message on stderr via `NoOutputReason`'s `Display`
  impl, reproducing the CLI's former hard-error text and exit code exactly.
- **Typed progress reaches every phase, not just the top level.** An initial
  pass only emitted `ConcertSplitProgress` from `run` itself and from
  `produce::process_segments`'s track-completion events, leaving `detect.rs`,
  `recover.rs`, and `refine.rs` (and the rest of `produce.rs`) writing straight
  to `println!`/`eprintln!` — silently unavailable to any in-process caller,
  which contradicted this ticket's own "typed progress covers phase starts,
  completed tracks, warnings, and diagnostics" acceptance criterion. An
  adversarial two-axis review (Standards + Spec) independently caught this on
  the same diff, so `progress: &mut dyn FnMut(ConcertSplitProgress)` was
  threaded through every function that previously logged directly
  (`detect_song_boundaries_from_text`, `match_song_titles`,
  `refine_song_start_time`, `first_song_missing_fallback`,
  `recover_missing_songs`, `refine_segments_with_audio_analysis`,
  `refine_last_song_end_time`, `find_black_frame_end_time`,
  `remove_stale_interlude_files`, and the remaining diagnostics in
  `process_segments`), converting each call to `ConcertSplitProgress::Diagnostic`
  or `::Warning`. A new assertion in the interface test suite locks this in by
  asserting a deep-phase `Diagnostic` event (from `process_segments`) actually
  reaches the caller, not just the phase/track-level events. The same review
  pass also caught (and fixed) a pre-existing duplication of the adaptive
  silence-threshold formula between the old `recover_missing_songs` and
  `refine_segments_with_audio_analysis` (present in the monolith before this
  extraction) — `refine.rs` now calls `recover::adaptive_silence_threshold`
  instead of re-inlining it, so the two passes cannot drift apart.

## Tests

- All 38 pre-existing algorithm tests moved with their functions (frame-index
  math, silence recovery, overlay clustering, refinement) — no behavior
  change; verified the pre/post test counts match exactly.
- 8 new interface-level tests in `concert_split::tests`, covering both
  acceptance-criteria categories:
  - Validation (fast, no ffmpeg): empty set list, empty explicit timestamps,
    missing input file, an OCR backend not compiled into this build.
  - Successful outcomes, against a small real-media fixture generated via
    `ffmpeg -f lavfi` (a `testsrc` + sine-tone clip — the Inspect phase's
    ffprobe runs unconditionally even when timestamps are supplied, so these
    cannot avoid real ffmpeg/ffprobe): provided timestamps cut to `Complete`
    with the expected tracks, `CutPlanned`, and `TrackCompleted` events;
    `--no-save-songs` yields `NoOutput { AnalysisOnly }` and creates no output
    directory at all (refine and cut were both skipped, matching the original
    CLI); embedded (not `--timestamps-file`) timestamps still refine and write
    `timestamps.json` unconditionally, per the CLI's original condition.
  - Unsuccessful outcome: a head-position missing song (no anchor before it,
    so silence-based recovery cannot fill it) yields
    `NoOutput { NothingDetected }`, **not** `Partial` — the test that locks
    the classification boundary described above.
- Parallel-test note: each test uses a distinct concert album name, because
  `run`'s `temp_frames/<album>` scratch directory is a relative path shared
  by the test process's working directory — a repeated album name across
  concurrently-running tests raced (one test's cleanup deleted another's
  in-flight scratch directory) until this was fixed.

## Verification performed

Automated:

- `cargo build -p live-set-splitter` (lib and `live-set-splitter` bin) —
  passed.
- `cargo test -p live-set-splitter --lib` — 113 passed (105 pre-existing + 8
  new).
- `cargo clippy -p live-set-splitter --all-targets -- -D warnings` — clean.
- `cargo fmt -p live-set-splitter -- --check` — clean.
- `cargo build --workspace` — passed.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --
  -D warnings` — clean.
- `cargo nextest run --tests` across `live-set-splitter`, `concert-tracker`,
  `concert-types`, and `tiny-desk-scraper` — 708 passed, 0 failed (confirms
  `concert-tracker`'s existing subprocess-based integration with the
  `live-set-splitter` CLI is unaffected).

Manual: not performed as part of this slice beyond the automated fixture-based
interface tests above, since #140 does not change `concert-web`'s runtime
behavior (it still shells out to the unchanged CLI binary) — end-to-end manual
verification of the web app's split workflow belongs to #141, where
`concert-web` is wired to call this interface directly.

## Review record

An adversarial code review ran independently on two axes (Standards and Spec)
against the full uncommitted diff (`/codex:rescue` was unavailable in this
session due to a sandbox permission error on its own job-log path, so both
axes ran as general-purpose sub-agents instead). Both axes independently
converged on the same real finding: typed progress did not actually reach
`detect.rs`/`recover.rs`/`refine.rs` or the rest of `produce.rs`, which still
called `println!`/`eprintln!` directly — contradicting this slice's own
"typed progress covers phase starts, completed tracks, warnings, and
diagnostics" acceptance criterion and the `concert_split` module doc's claim
about the new architectural boundary. Fixed as described above (progress
threaded through every deep-phase function), with a new test assertion
locking in that a deep-phase `Diagnostic` event actually reaches the caller.

The Standards axis also flagged a pre-existing duplication of the adaptive
silence-threshold formula (present in the monolith before this extraction,
between `recover_missing_songs` and `refine_segments_with_audio_analysis`) —
fixed by having `refine.rs` call the one function `recover.rs` already
exposes for it.

The Spec axis additionally noted: (a) `run`'s new empty-set-list validation
has no precedent in the old CLI (which silently no-op'd on an empty set
list) — an intentional new library-level invariant per the plan's engineering
review ("the interface owns validation"), not a defect, so left as-is; (b) an
apparent scope-creep flag on `CONTEXT.md`'s `Published Concert Split`/
`Recoverable Partial Split` glossary entries — these predate this session's
work (already present, uncommitted, from earlier spec-alignment work on the
parent #139/#138) and are not part of this diff's own additions, so no
action taken. Both axes otherwise confirmed the CLI's historical
byte-for-byte parity claims (conditional `timestamps.json` writing,
`concert.json` copy semantics, exit codes, the missing-songs error path) by
tracing the pre-extraction `main.rs` against the new code line-by-line.

Verification after the fixes: `cargo test -p live-set-splitter --lib` (113
passed), `cargo clippy -p live-set-splitter --all-targets -- -D warnings`,
and `cargo fmt -p live-set-splitter -- --check` reran clean; the workspace-wide
`cargo build`, `cargo fmt --all -- --check`, and `cargo clippy --workspace
--all-targets -- -D warnings` reran clean.
