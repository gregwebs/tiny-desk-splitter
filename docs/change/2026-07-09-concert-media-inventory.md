# Concert Media Inventory Module

This refactor extracts filesystem-backed concert media facts out of
`model.rs`/`web/handlers.rs` into a new `concert-tracker/src/concert_media.rs`
module. `ConcertMediaInventory` is the primary interface and test seam:
a borrowed, per-request snapshot of the facts needed to answer media
questions for one concert. This is a v1, behavior-preserving refactor — no
schema, route, response-shape, or frontend changes.

## Scope

`concert_media` now owns:

- Downloaded source lookup (`find_downloaded_file`) and its known-extension
  list.
- Split-track lookup (`find_track_file`) and track-extension priority.
- Interlude lookup (`find_interlude_file` / `find_interlude_track_file`).
- All-tracks-present checks (`tracks_present_on_disk`,
  `all_tracks_present_on_disk`), replacing five call sites that previously
  duplicated the same per-title scan loop inline (`jobs/prepare.rs`,
  `scan.rs` ×2, `jobs/split.rs` ×2).
- Reconstruction-item construction (`build_reconstruction`) — moved from
  `model.rs`. It still returns `model::PlaybackItem`, since that type is
  shared with `playback`; concert_media now depends on that domain type
  rather than owning it. This is a deliberate, slightly wider boundary than
  "pure filesystem facts," documented here rather than "fixed" in v1.
- Source redundancy / destructive-deletion gating (`source_redundant`).
- `can_play_concert` — moved from `web/handlers.rs::compute_can_play_concert`
  onto `ConcertMediaInventory::can_play_concert`.
- Track detail media facts (`list_all_track_details`, availability + video
  facts for the track-details sidebar) — moved from `model.rs`, along with
  its private `track_file_extension` helper and `list_tracks`/`list_all_tracks`
  (which also depend on that helper).

`playback.rs` keeps playback plan selection (source vs. reconstruction),
playback-facing response structs/errors (`PlaybackPlan`, `SourceMedia`,
`TrackMedia`, `PlaybackLookupError`), and next/previous playable-track
policy. It now imports the facts it needs from `concert_media` directly.

`model.rs` keeps shared domain data (`Concert`, `TrackInfo`,
`TrackDetailItem`, `PlaybackItem`/`PlaybackItemKind`, status enums) and
generic path helpers used by unrelated concerns — scraping, archiving,
downloading — that aren't part of the media-availability question:
`concert_dir`, `sanitize_album`, `sanitize_filename`, `is_browser_playable`.
`model.rs` re-exports the moved functions (`pub use crate::concert_media::...`)
for compatibility; touched call sites import from `concert_media` directly.
`jobs::find_downloaded_file` is likewise re-exported for any remaining
indirect callers.

Deliberately out of scope for v1 (unchanged):

- `scan::has_split_tracks` stays in `scan.rs` — its full-source-stem exclusion
  invariant (used by `jobs/split.rs` source-missing auto-recovery) is a
  distinct fact from "does every set-list title have a file," and a broader
  inventory API must not silently replace it.
- The pre-existing mismatch between `find_downloaded_file` (accepts many
  extensions) and `scan::scan`'s reconciliation (only checks the `.mp4` path)
  is a known, pre-existing gap — not touched here.
- `downloaded_at` (DB state) vs. on-disk source presence: `playback.rs` keeps
  owning the `PlaybackLookupError::MarkedDownloadedButMissing` policy that
  distinguishes "never downloaded" from "marked downloaded but the file is
  gone." `ConcertMediaInventory` carries `downloaded_at` through only for
  diagnostic logging in `can_play_concert`, not as a decision input.
- ffprobe, media-duration backfill, split-timestamp source-duration probing,
  and preview/thumbnail asset policy are untouched.

## Module Boundary

```text
┌─────────────────────────────────────────────────────────────┐
│ model.rs                                                     │
│  Concert, TrackInfo, TrackDetailItem, PlaybackItem, status    │
│  enums; concert_dir/sanitize_*/is_browser_playable            │
│  (used by scrape/archive/download, unrelated to availability) │
└───────────────────────────▲───────────────────────────────────┘
                             │ shared domain types
┌───────────────────────────┴───────────────────────────────────┐
│ concert_media.rs                                              │
│  ConcertMediaInventory (primary test seam), built via         │
│  for_concert(working_dir, &Concert, user_split_timestamps)    │
│                                                                │
│  find_downloaded_file / find_track_file / find_interlude_*    │
│  tracks_present_on_disk / all_tracks_present_on_disk          │
│  build_reconstruction / source_redundant                      │
│  can_play_concert / list_all_track_details                    │
└───────────────────────────▲───────────────────────────────────┘
                             │ facts
┌───────────────────────────┴───────────────────────────────────┐
│ playback.rs                                                    │
│  concert_playback_plan (Source | Reconstruction)               │
│  track_media / next_track_media / prev_track_media             │
│  track_details, PlaybackLookupError                            │
└─────────────────────────────────────────────────────────────┘
```

`web/handlers.rs` builds one `ConcertMediaInventory` per card render
(`render_card`, `render_detail_card`) and per `prepare_status_payload` call,
replacing separate `source_redundant(...)` / `compute_can_play_concert(...)`
calls that each re-derived `working_dir` + `album` + `Concert` fields.

## State Changes

None — refactor only. Existing lifecycle/playback state diagrams in
`docs/data.md` are unchanged; only the module that computes the underlying
filesystem facts moved.

## Verification

- `cargo check -p concert-tracker`
- `cargo test -p concert-tracker --lib concert_media` (48 moved tests + 11 new
  `ConcertMediaInventory` tests)
- `cargo test -p concert-tracker --lib` (full crate suite)
- `cargo test -p concert-tracker --test web_integration --no-run`
- `just lint`
- Codex plan review (before implementation) and engineering-lead/Codex code
  review (before verification) per `CLAUDE.md`.
