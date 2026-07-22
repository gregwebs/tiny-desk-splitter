# Make the Concert Split library adapter the web default

Implements [#141](https://github.com/gregwebs/tiny-desk-splitter/issues/141),
the second implementation slice of
[#139 — Deep Concert Split operation implementation](https://github.com/gregwebs/tiny-desk-splitter/issues/139)
(spec: [#138](https://github.com/gregwebs/tiny-desk-splitter/issues/138)),
building on [#140](https://github.com/gregwebs/tiny-desk-splitter/issues/140)'s
extraction of `live_set_splitter::concert_split::run`.

## Purpose

`concert-web` split concerts only by shelling out to the `live-set-splitter`
binary as a subprocess (`JobConfig::production`'s `split_cmd` closure), writing
concert metadata and timestamps to temporary JSON transport files and inferring
success/failure from the exit code and files on disk. This required the
splitter executable to be built separately (`cargo build --bin
live-set-splitter`) before `cargo run --bin concert-web` could split anything
in development.

This change makes the in-process library adapter — calling `concert_split::run`
directly — the **default** for `concert-web`, so `cargo run --bin concert-web`
splits with no separate build step. An explicit CLI (subprocess) adapter
remains available (`--splitter cli`) for process-level debugging and strict
process-kill cancellation; `concert-db`'s `resplit` command always uses it (a
batch/offline tool, not a long-running server, so the library adapter's
dev-convenience default doesn't apply there).

## Design

See [`docs/concert-split.md`](../concert-split.md)'s new "concert-web adapter
selection" section for the full picture (resolution-order diagram, the shared
`SplitJob` translation, and the accepted cancellation-semantics divergence).
Summary:

- `concert-tracker` now depends on `live-set-splitter` as a path dependency
  (default features → `paddle-ocr`, needed so the library adapter's Analyze
  mode has a working OCR backend in-process — `default-features = false` would
  make Analyze fail at runtime). `concert-web` itself now links the OCR (MNN)
  backend; the `Containerfile`'s stale "OCR inference runs inside the spawned
  subprocess, not concert-web" comment is corrected — `$PADDLE_OCR_MODEL_DIR`
  is a container-wide env var, so model resolution is unaffected either way.
- `--splitter {library,cli}` (default `library`) on `concert-web`.
  `--splitter-bin` is rejected (clear startup error, before any DB work) unless
  `--splitter cli`. CLI-mode resolution (`resolve_splitter_cli`, pure/testable
  core in `resolve_splitter_cli_with`): override → sibling-of-current-exe →
  PATH → (debug builds only) `cargo run --bin live-set-splitter
  --manifest-path <workspace>/Cargo.toml --` → a clear release-build error.
  `default_splitter_bin`'s old silent PATH fallback is gone.
- **Single runner, backend enum** — `CommandJobRunner` is renamed
  `ProductionJobRunner`; its `split_cmd: SplitCommandFn` field becomes
  `split: SplitBackend` (`Command(SplitCommandFn) | Library`). `download_cmd`/
  `open_cmd` are unchanged and shared by both split backends. `JobRunner`,
  `TestControlJobRunner`, and the generic `run.rs` Job Run engine are
  untouched. `JobConfig::from_commands` (the existing test seam used by ~15
  tests) keeps its exact signature, now wrapping `SplitBackend::Command`
  internally; a new `JobConfig::with_split_backend` exposes the full
  `SplitBackend` choice for the library backend's own tests.
- **Typed `SplitJob.concert: ConcertInfo`, no adapter branch in `setup`** —
  `jobs::split::setup` now builds a typed `ConcertInfo` (`build_concert_info`,
  replacing the old `SplitterInput`/`SplitterSong`/`SplitterMusician` transport
  DTOs — `ConcertInfo` is a serde superset, so the CLI subprocess still parses
  it) and always serializes it to the temp file `SplitJob.json_path` points at,
  regardless of adapter. The library backend consumes `SplitJob.concert`
  directly (no transport file read); the CLI backend's subprocess still reads
  the file. `setup` itself stays adapter-agnostic — the choice lives in
  `ProductionJobRunner::run_split`, not in job preparation.
- **Library backend** (`jobs::split_library`, new module): `request_for`/
  `options_for` translate `SplitJob`+`SplitMode` into a `ConcertSplitRequest`
  field-for-field, mirroring `build_cli_split_command`'s argument translation
  — critically, `ConcertSplitOptions::output_format`/`video_cut_mode` are set
  explicitly to `Both`/`Smart` (the CLI subprocess gets these for free from
  clap's `default_value_t`, but the library adapter has no clap layer, so
  omitting them would have silently shipped audio-only/copy-cut splits).
  `run` hands the translated request to `spawn_blocking` (the library's `run`
  is synchronous and can take minutes of ffmpeg/OCR work — calling it inline
  would starve the tokio runtime), forwarding `ConcertSplitProgress` to
  `tracing` and the per-job log file exactly like the subprocess path's
  stdout/stderr streaming does. `outcome_to_step` maps `ConcertSplitOutcome`
  onto `JobStepOutcome`, mirroring the CLI's `exit_code_for`: `Complete`/
  `NoOutput::AnalysisOnly` succeed; `NoOutput::NothingDetected`/`Partial`/an
  infrastructure `Err` fail. `gather_success_facts` (disk-based: reads
  `tracks_present`/`timestamps.json`) is unchanged and works identically for
  both adapters — the structured `ConcertSplitOutput` the library adapter
  already holds in-process is not yet threaded through further; that's
  deferred to #142+, which will need it for Recoverable Partial Split
  publication (narrow seam now, no premature widening of `JobStepOutcome`).
- **`concert.json` parity** — the CLI adapter gets `concert.json` written for
  free: the spawned `live-set-splitter` binary's own `main()` copies it
  (`copy_concert_json`). The library adapter runs no subprocess, so
  `write_concert_json_if_analyze` replicates that copy (from
  `SplitJob.json_path`, gated identically: only Analyze mode, only when not
  already present) — a copy failure here fails the job step too, mirroring the
  CLI binary's own `?`-propagated `copy_concert_json` error.
- `check_dependencies` now takes `Option<&SplitterCli>`: `None` (library
  adapter) skips the splitter-binary check entirely; shared checks (`ffmpeg`,
  `yt-dlp`) still run for both adapters (the library's Inspect phase ffprobes
  unconditionally, so `ffmpeg` is required either way).
- `SplitExecution::Run(SplitJob)` is now `Run(Box<SplitJob>)` — the new
  `concert: ConcertInfo` field pushed `SplitJob` well past clippy's
  `large_enum_variant` threshold against `ExistingTracksRecovery`.
- **e2e fixture fix**: `e2e/fixtures.js` spawned `concert-web` with
  `--splitter-bin <stub-splitter.js>` but no `--splitter cli` — under the new
  default this is now rejected at startup ("--splitter-bin requires --splitter
  cli"), which would have broken the entire Playwright suite (every spec uses
  this shared fixture). Fixed by adding `--splitter cli` alongside
  `--splitter-bin`.

## Tests

- Pure/deterministic (`concert-tracker/src/bin/concert_web.rs`):
  `build_split_target` — the `--splitter`/`--splitter-bin` combining logic
  extracted from `main` specifically so the "`--splitter-bin` requires
  `--splitter cli`" rejection has unit coverage, not just the live check
  described under Verification (4 tests, `resolve_splitter_cli` injected so
  the CLI-mode delegation case doesn't touch the real filesystem/PATH either).
- Pure/deterministic (`concert-tracker/src/jobs/mod.rs`):
  `resolve_splitter_cli_with`'s override→sibling→PATH→cargo-debug→release-error
  priority order (5 tests, `exists`/`on_path` injected — no real
  filesystem/PATH); `build_cli_split_command`
  for both `SplitterCli::Executable` and `::CargoRun` (asserts `cargo run --bin
  live-set-splitter --manifest-path ... --` followed by the same job args, and
  `--emit-interludes` for `UserTimestamps`); `check_dependencies` for all four
  cases (missing/present splitter, library `None`, `CargoRun`).
- Pure (`concert-tracker/src/jobs/split_library.rs`): `options_for`/
  `request_for` for all three `SplitMode`s (asserts `Both`/`Smart` and every
  other option field, not just the mode-varying ones); `outcome_to_step` for
  `Complete`/`NoOutput::NothingDetected`/`Partial`/an infra `Err`, plus
  `concert.json` written-for-Analyze / not-written-for-UserTimestamps.
- Integration (`concert-tracker/src/jobs/split.rs`,
  `library_backend_splits_user_timestamps_end_to_end`): a real `ffmpeg -f
  lavfi` fixture driven through `start_split` → the real `ProductionJobRunner`
  with `SplitBackend::Library` → `gather_success_facts`/DB commit, asserting
  `split_at`, `tracks_present`, no errors, and the actual `.m4a`/`.mp4` files
  on disk. Uses `UserTimestamps` (not `Analyze`) so no OCR backend/models are
  needed in this test — only the always-unconditional Inspect-phase ffprobe
  and the Cut phase touch real ffmpeg, matching the library's own `#140` test
  fixture pattern.
- `concert-tracker/src/test_control/job_driver.rs`'s existing `SplitJob`
  fixtures got a placeholder `concert: ConcertInfo` field (unused by
  `TestControlJobRunner`, which reads `job.json_path`/`job.mode`, not
  `job.concert`) so they keep compiling; no behavior change there.

## Verification performed

Automated:

- `cargo build --workspace`, `cargo build -p concert-tracker --features
  test-control` — passed (confirms the new `live-set-splitter` path dependency,
  including its native MNN OCR backend, links cleanly into `concert-tracker`).
- `cargo clippy --workspace --all-targets -- -D warnings` and the same with
  `--features test-control` — clean.
- `cargo fmt --all -- --check` — clean.
- `cargo nextest run --workspace --tests` — 795 passed, 0 failed (up from
  #140's 708 baseline: +23 pure tests in `jobs::mod`/`jobs::split_library`, +4
  pure tests in `bin/concert_web.rs` (added during review, see "Review record"
  below), +1 library-backend integration test, plus incidental growth
  elsewhere). `cargo nextest run -p concert-tracker --features test-control` —
  676 passed, 0 failed.
- `npx playwright test` (full suite) — **173 passed**, including
  `splitter.spec.js`, `automate-splitting.spec.js`, and
  `interlude-tracks.spec.js` — confirms the e2e fixture fix above and that the
  split UI flow is unaffected end to end.

Manual (scratch `--db`/`--workdir`, `target/debug/live-set-splitter` moved
aside to genuinely simulate "not built"):

- **Library default, no splitter binary present**: started `concert-web`
  with default flags — startup logged no "splitter binary not found" warning.
  POSTed `/concerts/:id/split` (Analyze mode) against a real `ffmpeg -f lavfi`
  fixture with no matching title-overlay text: OCR detection ran in-process
  (tracing showed `Attempting to detect song boundaries...` through
  `No song titles detected... falling back to audio analysis`, proving the
  paddle-ocr/MNN backend resolved and ran inside `concert-web`, not a
  subprocess) and correctly failed with `NoOutput::NothingDetected`'s exact
  message, recorded as a split error. POSTed
  `/concerts/:id/split-timestamps` (UserTimestamps mode) against the same
  concert: split succeeded end to end — `Song.m4a`/`Song.mp4` and a tail
  `interlude_01.*` (from `--emit-interludes`) appeared on disk, `split_at` was
  set, `tracks_present = [true]`, no errors. `concert.json` was correctly
  *not* written (UserTimestamps mode).
- **`--splitter cli`, no splitter binary present**: same scratch setup with
  `--splitter cli`. `/concerts/:id/split-timestamps` on a second concert:
  log showed `Compiling live-set-splitter...` then `Running \`target/debug/
  live-set-splitter ... --emit-interludes --media-duration 6\`` — the debug
  `cargo run` fallback genuinely built and ran the real splitter binary as a
  subprocess (not a coincidental sibling/PATH resolution) — split succeeded,
  same track/interlude files appeared on disk.
- **`--splitter-bin` rejected under library mode**: `--splitter library
  --splitter-bin /fake/path` exits 1 with `Error: --splitter-bin requires
  --splitter cli`, before any DB/backfill work runs (moved the validation
  ahead of `db::connection::open` during verification, after an initial pass
  had it after DB backfill).
- Release-build CLI-mode-with-nothing-resolved (the "clear startup error" for
  a real release binary) is covered by the automated
  `resolve_splitter_cli_errors_in_release_when_nothing_resolves` unit test
  rather than a live release build — `lto = true`/`codegen-units = 1` make a
  full release build slow, and the unit test exercises the exact same
  `resolve_splitter_cli_with(debug_build: false)` code path.

## Review record

`/codex:rescue` was unavailable in this session (the same sandbox permission
error on its own job-log path noted in #140's own change record), so both
axes ran as parallel general-purpose sub-agents against the uncommitted diff,
as #140 did.

**Standards axis** found the code disciplined overall (invalid-state-proof
adapter types, a pure/injectable `resolve_splitter_cli_with` core, `Result`-
propagated errors, WHY-focused comments) and one real finding: cancelling a
library-adapter split frees the job registry slot and DB state immediately,
permitting an **immediate retry** while the orphaned `spawn_blocking` thread
from the cancelled run may still be writing into the *same* output directory
— a concrete file-interleaving/corruption risk, not just wasted background
work, that the first documentation pass under-disclosed (it only mentioned
"no double DB-commit" and wasted work). Fixed by rewriting
`docs/concert-split.md`'s "Cancellation semantics" section to state the
retry-collision risk explicitly and point at #142 (staged/validated
publication under a lock) as the ticket that closes it for every adapter and
retry path — not a new mitigation in this ticket's scope, since a structural
fix here would mean either the cooperative-cancellation seam already ruled
out (would change #140's fixed `run` signature) or duplicating #142's own
staging work early.

**Spec axis** confirmed every acceptance criterion is met against the actual
code (not just this document's claims) and found one minor gap: the
"`--splitter-bin` requires `--splitter cli`" rejection had only a live check,
not a unit test, for the "adapter selection ... have automated coverage"
criterion. Fixed by extracting the combining logic out of `main` into
`build_split_target` (`concert-tracker/src/bin/concert_web.rs`), with
`resolve_splitter_cli` injected as a parameter so the rejection path (and the
CLI-mode delegation path) are both unit-tested without touching the real
filesystem/PATH — 4 new tests.

Verification after the fixes: `cargo build -p concert-tracker --bins`,
`cargo nextest run -p concert-tracker --bin concert-web` (4/4 passed), and the
full `cargo build --workspace`/`clippy --workspace --all-targets -D
warnings`/`fmt --all -- --check`/`cargo nextest run --workspace --tests`
(791/791) reran clean.
