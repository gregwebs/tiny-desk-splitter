# Concert Split interface

`live_set_splitter::concert_split` is the synchronous, library-owned interface
that owns the full transformation from source media + concert metadata to cut
tracks, interludes, and timestamps. It replaces the previous design where this
whole workflow lived only inside the `live-set-splitter` CLI binary; the CLI
(`live-set-song-splitter/src/main.rs`) is a thin adapter over this library
function, and `concert-web` (see
[#141](https://github.com/gregwebs/tiny-desk-splitter/issues/141) and
["concert-web adapter selection"](#concert-web-adapter-selection) below) calls
the same `run` function directly by default, without shelling out to a
subprocess or round-tripping through temporary JSON transport files.

See [#138](https://github.com/gregwebs/tiny-desk-splitter/issues/138) for the
full Deep Concert Split specification this interface is the first slice of,
[#140](https://github.com/gregwebs/tiny-desk-splitter/issues/140) for this
interface's own extraction ticket, and
[#141](https://github.com/gregwebs/tiny-desk-splitter/issues/141) for wiring
`concert-web` to call it in-process by default.

## The interface

```rust
pub fn run(
    request: ConcertSplitRequest,
    progress: &mut dyn FnMut(ConcertSplitProgress),
) -> anyhow::Result<ConcertSplitOutcome>;
```

- **`ConcertSplitRequest`** — typed concert metadata (`ConcertInfo`), the
  resolved input file and output directory, optional explicit timestamps
  (mirrors `--timestamps-file`), and `ConcertSplitOptions` (the CLI's tuning
  flags, unchanged). No temporary transport files: callers pass Rust values.
- **`ConcertSplitProgress`** — typed events emitted as the workflow advances:
  `PhaseStarted(SplitPhase)` for each major phase, `CutPlanned { total }` once
  the track plan is known (so a consumer can render "k of total" before any
  track is produced), `TrackCompleted` per song/interlude, and `Warning`/
  `Diagnostic` for everything else. The `&mut dyn FnMut` sink is the seam a
  future in-process caller uses to forward events over a channel from inside
  `spawn_blocking`, without imposing any channel type on this library.
- **`ConcertSplitOutcome`** — the domain result, distinct from infrastructure
  errors (`Err(anyhow::Error)`, e.g. an ffprobe/ffmpeg/IO failure):
  - `Complete(ConcertSplitOutput)` — every expected set-list track was
    produced.
  - `NoOutput { reason }` — `AnalysisOnly` (`--no-save-songs`: analysis (and,
    when applicable, `timestamps.json`) ran, but no tracks were cut) or
    `NothingDetected { missing }` (detection and silence-based recovery could
    not find all expected songs).
  - `Partial(ConcertSplitOutput)` — the complete split failed after one or more
    song tracks finished, and those non-empty tracks were safely published at
    canonical filenames. This remains a failed split, not reconstruction-ready
    Published output.

## State diagram

```
                 ConcertSplitRequest (typed; no transport files)
                              │
                              ▼
   ┌──────────────────────────────────────────────────────────────┐
   │  run()  — emits PhaseStarted(..) at each phase                │
   │                                                                │
   │  Validate(inputs+OCR) → Inspect(ffprobe) → [timestamps given?] │
   │        │                              │yes → skip Detect       │
   │        │no                            ▼                        │
   │     Detect(OCR) → RecoverSilence → RefineAudio → WriteMetadata │
   │                        │                                       │
   │                 all songs found? ──no──► NoOutput{Nothing-     │
   │                        │yes               Detected} (gate      │
   │                        │                  hard-stops before    │
   │                        │                  any cutting)         │
   │                        ▼                                       │
   │              [no_save_songs?] ──yes──► NoOutput{AnalysisOnly}  │
   │                        │no                                     │
   │                        ▼                                       │
   │          Cut (emit CutPlanned{total}) → TrackCompleted* →      │
   │                  ValidateOutput → Publish → Complete           │
   │                         │ failure after ≥1 song                │
   │                         ▼                                      │
   │                publish completed songs → Partial               │
   └──────────────────────────────────────────────────────────────┘
                              │
             ┌──────────┬─────┴────────────┬──────┴────────┬──────┐
             ▼          ▼                  ▼               ▼      ▼
        Complete       NoOutput{Analysis-   NoOutput{Nothing-  Err(anyhow)
     (all tracks)        Only}               Detected}         (infra fault:
                                                               ffprobe/ffmpeg/IO)
             │                │                   │               │
             ▼(CLI)           ▼                   ▼               ▼
          exit 0           exit 0        stderr msg + exit 1   Error: + exit 1

   Partial (canonical subset, failed split) ──CLI──► report + exit 1
```

`RefineAudio` and `WriteMetadata` are skipped when explicit timestamps were
supplied and `--refine-timestamps`/`options.refine_timestamps` was not
requested — this mirrors the CLI's historical `user-timestamp` and
`reset-to-auto` workflows, which pass already-correct timestamps and don't
want them silently rewritten. Timestamps embedded in the concert JSON itself
(as opposed to an explicit `--timestamps-file`) still refine unconditionally,
matching the CLI's original condition exactly.

## Output writing

`timestamps.json` is written by the library only when refinement ran
(`request.timestamps.is_none() || options.refine_timestamps`) — this exactly
mirrors the CLI's historical condition, so `user-timestamp`/`reset-to-auto`
runs still write no file. The `ConcertSplitOutput.timestamps` value returned
from a `Complete` outcome is **always** populated regardless of whether the
file was written, so an in-process caller never needs to read it back off
disk.

`concert.json` (a byte-for-byte copy of the original input concert metadata)
is written by the **adapter**, not the library — the library only produces its
own computed `timestamps.json` artifact. The CLI adapter's own `main()` copies
it from the concert JSON file path it parsed (`copy_concert_json`).
`concert-web`'s in-process library adapter has no such file path to copy from
directly, so it instead copies from the same typed `ConcertInfo` it also
serializes as the CLI-adapter subprocess's transport JSON — see
[`jobs::split_library`](#concert-web-adapter-selection) below.

## CLI adapter

The CLI (`live-set-song-splitter/src/main.rs`) translates `Cli` arguments into
a `ConcertSplitRequest` (`build_request`), renders `ConcertSplitProgress`
events to stdout/stderr, and maps the outcome to a process exit code
(`exit_code_for`) that reproduces the CLI's historical behavior:

| Outcome | Exit code |
|---|---|
| `Complete` | 0 |
| `NoOutput { AnalysisOnly }` | 0 |
| `NoOutput { NothingDetected }` | 1 (message on stderr, mirroring the CLI's former hard error) |
| `Partial` | 1 after atomically writing `--outcome-file` when requested |
| `Err(_)` (infrastructure fault) | 1 |

`--outcome-file PATH` is the subprocess adapter's machine-readable contract.
Its stable tagged report contains only outcome kind, partial track titles, or
`NoOutputReason`; internal paths and timestamps are not part of the process
contract. `concert-web` never infers a partial result from stderr or scans
filenames. Infrastructure errors do not write a report.

The three workflows the CLI has always supported map onto `ConcertSplitRequest`
fields without any new mode enum:

- **automated** — `timestamps: None` (detection runs).
- **user-timestamp** — `timestamps: Some(..)`, `emit_interludes: true`,
  `media_duration: Some(..)`, `refine_timestamps: false`.
- **reset-to-auto** — `timestamps: Some(..)`, `refine_timestamps: false`, no
  interludes.

## Module layout

The workflow that used to live entirely in `main.rs` is now split across
top-level library modules, grouped by the algorithm phase each owns:

- `concert_split.rs` — the public interface (`ConcertSplit*` types) and `run`'s
  orchestration. Kept thin; phase-specific types and logic live in their own
  modules below.
- `detect.rs` — text-overlay (OCR) song boundary detection.
- `recover.rs` — silence-based recovery of songs detection missed.
- `refine.rs` — audio-analysis refinement of detected/recovered boundaries.
- `produce.rs` — cutting song/interlude tracks and writing timestamps.

`audio`, `video`, `io`, `cut`, `ffmpeg`, `image`, `ocr`, and `ocr_backend`
remain the lower-level library modules these phase modules build on.

## Published and Recoverable Partial output

A Concert Split writes timestamps, song tracks, and interludes into a hidden
per-run staging directory beside the concert directory. The canonical concert
directory is not mutated during detection, refinement, or cutting. After the
workflow has produced its full expected set, the publication module verifies
that every expected output is a non-empty regular file and copies each file to
a sibling temporary path before renaming it to its canonical filename.

```text
analyze / cut ──> .concert-split-staging-* ──> validate
                                                    │
                                  invalid ──────────┴──── valid
                                     │                      │
                              discard staging       exclusive file lock
                                                            │
                                  current canonical ──copy──> backup candidate
                                                            │
                                                      rotate one backup
                                                            │
                                                    rename replacements
                                                            │
                                                   remove obsolete owned files
                                                            │
                                                   install exact manifest
```

`.concert-split-published.json` is the source of truth for which canonical
files belong to the Published Concert Split. Obsolete cleanup uses this exact
set rather than treating every media extension as splitter-owned, so source
media, `concert.json`, previews, and unrelated files are preserved. The
previous exact set is retained under `.concert-split-backup`; a later
successful publication replaces that one backup rather than accumulating
generations.

If an ordinary copy, removal, or manifest-install operation fails, publication
restores the preceding exact set from backup and returns an infrastructure
error. Process and host crashes are different: their journaled recovery is
owned by #144.

When no Published manifest exists and a later cut, validation, or complete
publication step fails, completed song files are copied out of staging under
the same lock. `.concert-split-partial.json` records exact titles, timing, and
filenames. Partial retries merge validated tracks by title. A complete retry
overwrites them, removes partial-only files and the partial marker, and installs
the Published manifest without treating partial bytes as a known-good backup.
If a Published manifest already exists, a failed resplit never publishes a
partial replacement and the previous files, manifest, backup, and database
availability remain unchanged.

```text
Empty ──failed after tracks──▶ Partial ──failed retry──▶ Partial (merged)
  │                              │
  └────complete split────────────┴────complete retry──▶ Published

Published ──failed resplit──▶ Published (unchanged)
Published ──complete resplit▶ Published (new; previous retained in backup)
```

The publication lock is advisory and shared by both the library and CLI
adapters because both call this same module. Canonical replacements use rename
rather than truncating an existing file in place, so a reader that already
opened a track continues reading a stable inode.

## concert-web adapter selection

`concert-web`'s `--splitter` flag (`concert-tracker/src/bin/concert_web.rs`)
picks how it executes a Concert Split, and `concert-db`'s `resplit` command
always uses the CLI adapter (it's a batch/offline tool, not a long-running
server, so the library adapter's dev-convenience default doesn't apply there):

- **`library`** (default) — calls `concert_split::run` in-process, on a
  `tokio::task::spawn_blocking` thread (`concert-tracker/src/jobs/split_library.rs`).
  No separate `cargo build --bin live-set-splitter` is needed for
  `cargo run --bin concert-web` to split.
- **`cli`** — shells out to the `live-set-splitter` binary as a subprocess
  (`build_cli_split_command` in `concert-tracker/src/jobs/mod.rs`), for
  process-level debugging and strict process-kill cancellation (see
  "Cancellation semantics" below). `--splitter-bin <path>` is accepted only in
  this mode (rejected at startup otherwise) and, together with automatic
  resolution, follows this priority order (`resolve_splitter_cli`):

  ```
  --splitter-bin override
          │ absent
          ▼
  sibling of the running executable (`<exe-dir>/live-set-splitter`)
          │ not found
          ▼
  `live-set-splitter` on PATH
          │ not found
          ▼
  debug build? ──yes──► `cargo run --bin live-set-splitter
  │no                     --manifest-path <workspace>/Cargo.toml --`
  ▼
  startup error (release builds never shell out to cargo)
  ```

Both adapters share the same `SplitJob`/`SplitMode` (Analyze,
UserTimestamps, ResetToAuto) built once in `jobs::split::setup` — the library
adapter translates it to a `ConcertSplitRequest` field-for-field the same way
the CLI adapter translates it to subprocess arguments (`jobs::split_library`'s
`request_for`/`options_for` mirror `build_cli_split_command`), including
setting `ConcertSplitOptions::output_format`/`video_cut_mode` explicitly to
`Both`/`Smart` — the values the CLI subprocess gets for free from clap's
defaults but the library adapter, with no clap layer of its own, must set
itself.

### Cancellation semantics

The two adapters diverge on what happens to in-flight work when a split is
cancelled. The CLI adapter's subprocess is spawned with `kill_on_drop`, so
cancelling the tokio task promptly `SIGKILL`s the splitter, and no more writes
into the concert's output directory happen after that. The library adapter's
`concert_split::run` executes on a `spawn_blocking` thread, which tokio cannot
cancel: the job is marked Failed immediately (no double-commit — this matches
the pre-existing archive-job cancellation residual documented in
`jobs/run.rs`), but the detached blocking thread keeps running ffmpeg/OCR and
writing output files to completion in the background.

Staging and locked publication ensure readers never observe a half-written
track. They do not make a detached blocking thread cancellable: after the Job
Run is marked failed, that thread may still finish and publish a complete or
partial filesystem result without a matching terminal DB transaction. The
guarantee “a Job Run never publishes a partially completed Concert Split”
therefore requires `--splitter cli` when cancellation is possible; killing the
child prevents further publication before the registry admits a retry.
