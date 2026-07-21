# Concert Split interface

`live_set_splitter::concert_split` is the synchronous, library-owned interface
that owns the full transformation from source media + concert metadata to cut
tracks, interludes, and timestamps. It replaces the previous design where this
whole workflow lived only inside the `live-set-splitter` CLI binary; the CLI
(`live-set-song-splitter/src/main.rs`) is now a thin adapter over this library
function, and a future in-process caller (`concert-web`, see
[#141](https://github.com/gregwebs/tiny-desk-splitter/issues/141)) can call the
same `run` function directly, without shelling out to a subprocess or
round-tripping through temporary JSON transport files.

See [#138](https://github.com/gregwebs/tiny-desk-splitter/issues/138) for the
full Deep Concert Split specification this interface is the first slice of,
and [#140](https://github.com/gregwebs/tiny-desk-splitter/issues/140) for this
slice's ticket.

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
  - `Partial(ConcertSplitOutput)` — **reserved** for a later ticket
    (Recoverable Partial Split publication, #142+). This slice's workflow is
    binary: the missing-songs gate returns `NoOutput` before any cutting
    starts, so `run` never constructs `Partial` today. The variant exists now
    so the outcome seam doesn't need to change shape later.

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
   │                        Cleanup → Complete                      │
   └──────────────────────────────────────────────────────────────┘
                              │
             ┌────────────────┼──────────────────┬───────────────┐
             ▼                ▼                   ▼               ▼
        Complete       NoOutput{Analysis-   NoOutput{Nothing-  Err(anyhow)
     (all tracks)        Only}               Detected}         (infra fault:
                                                               ffprobe/ffmpeg/IO)
             │                │                   │               │
             ▼(CLI)           ▼                   ▼               ▼
          exit 0           exit 0        stderr msg + exit 1   Error: + exit 1

   Partial: reserved enum variant, NOT constructed by this slice.
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
is written by the **CLI adapter**, not the library — only the CLI has the
original file path to copy from; the library only produces its own computed
`timestamps.json` artifact.

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
| `Err(_)` (infrastructure fault) | 1 |

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
