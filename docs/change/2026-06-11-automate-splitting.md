# Automate Splitting

## Goal

Move from the user managing the splitting process to automating it for the user.

Original requirements:

- On the listing page, on mouseover of a concert card, hide the picture and
  show the tracks — even if they have not been split yet.
- On the detail page, both the picture and tracks should always be shown.
- The play button next to the tracks button is no longer needed.
- The tracks button no longer shows/hides the tracks; clicking it plays them.
- If the concert is not yet split, still show the tracks.
- Remove the split button.
- If the concert is not yet split, clicking play on any of the tracks splits
  the tracks; if not yet downloaded, it first downloads the concert.
- The downloading and splitting statuses still need to be displayed.
- No UI to explicitly delete the existing split; tracks are deleted one by one.
- Playing a deleted song triggers a re-split (and re-download if necessary);
  restoring all deleted songs when one is played is acceptable.
- Jobs gain dependencies: a job can have a `depends_on` (the job it waits
  for); a running job has `dependencies` it triggers when it completes.

## Implementation summary

### Job dependencies (in-memory)

`JobRegistry` (`src/jobs/mod.rs`) gained a `dependents` map:
`dependents[upstream] = Vec<JobKey>` — jobs to start when `upstream`
completes successfully. **`JobKey {concert_id, kind}` itself is the job id**
(it is already `Hash + Eq`; no separate hashing needed). The reverse view
(dependent → upstream) is the spec's `depends_on`; a queued dependent has no
spawned task until its upstream succeeds.

- `add_dependent` / `take_dependents` / `has_dependent` /
  `drop_dependency_edges` manage the edges.
- On success, `run_download` / `run_split` call `spawn_dependents`, which
  starts each queued job in its own tokio task.
- On failure or cancellation, the queued dependents are dropped — they never
  run, and the upstream's error badge tells the user why.
- Dependencies are **in-memory only**: a server restart mid-chain drops the
  queued split; the user just clicks play again.

### The `prepare` orchestrator

`src/jobs/prepare.rs` is the single entry point behind track playback, exposed
as `POST /concerts/:id/prepare` (idempotent) with a read-only poll at
`GET /concerts/:id/prepare-status`:

- All set-list track files on disk → `Ready` (nothing started).
- Source file on disk, tracks missing → start a split. Re-split works with
  `split_at` already set; the post-split rescan restores **all** deleted
  tracks. If `downloaded_at` was NULL while the file exists (manual copy),
  it is reconciled first.
- Source file missing → queue Split as a dependent of Download (with a
  re-check race guard), then start the download — even when `downloaded_at`
  is still set (file deleted out-of-band).
- The handler scrapes metadata first when needed (same path as the old
  Download button) and returns 422 when there is no set list.

`prepare-status` returns `{download, split, split_queued, tracks_present}`
where `tracks_present` is checked against the filesystem (the same source of
truth as media-info).

### Status derivation fix

`DownloadStatus`/`SplitStatus::from_concert` now rank the in-progress marker
**above** the completed marker: a re-split runs with `split_at` still set and
must surface as Splitting (card polls, buttons disabled); same for re-download
with a stale `downloaded_at`. If the re-run fails, the started marker is
cleared and the status falls back to Split/Downloaded — the surviving files
remain usable.

### UI

- Listing cards: track list visibility is pure CSS
  (`#concert-list .card:hover`); the list HTML is fetched once on first hover
  and kept in the DOM as a cache. Mouseleave restores the picture. Single-card
  htmx swaps embed the track list so an open list never blanks; only the bulk
  listing render leaves it empty.
- Detail page: the card embeds its track list, always visible alongside the
  picture (the separate bottom "Tracks" section was removed); per-track
  delete buttons live here too.
- The Split button, tracks-row Play button, and delete-split button are gone
  (the `/split` and `/delete-split` endpoints remain). The tracks button
  carries the split status (badge class + label, e.g. "not-split (0/12)" →
  "splitting (0/12)" → "tracks (12)") and plays the tracks.
- Unavailable tracks (unsplit or deleted) render as clickable buttons styled
  unavailable; clicking one enters the prepare flow.
- While a split runs (or is queued behind a download) the tracks button and
  track buttons render `disabled` server-side; the client also disables them
  immediately on click.
- Player (`static/player.js`): clicking a missing track POSTs `/prepare`,
  stores a `pendingPlay {concertId, trackIdx}` in player state (outside
  `#content`, so it survives card swaps), marks the button `.preparing`,
  kicks the card into its 3s status polling via one htmx refresh, then polls
  `prepare-status` every 2s (30 min cap) and auto-plays — or enqueues, if
  something else is playing — when the track file appears. Download/split
  errors stop the polling and surface in the player bar (the card badge shows
  the job error via its own polling). One pending track at a time; a second
  click retargets it (the first chain keeps running server-side).

## State tables

State notation: **D0** not downloaded · **D~** downloading · **D1** downloaded
(source file on disk) · **DE** download error · **S0** unsplit · **S~**
splitting · **S1** split (all tracks present) · **S1-** split with some tracks
deleted · **SE** split error · **+q** split queued as dependent of download.

### Concert pipeline: triggers → state transitions

| Trigger | Current State | New State | Backend Effects | UI Effects |
|---|---|---|---|---|
| Play click (track file exists) | D1, S1/S1- | unchanged | media-info 200; listen event | Plays immediately (or enqueues) |
| Play click (track file missing) | D1, S0/S1-/S1 | D1, S~ | `/prepare` → `start_split`; SplitStarted event | Tracks button + tracks disabled; clicked track pending; player "Preparing…"; 2 s poll |
| Play click | D0, S0 (or source deleted, any split state) | D~ +q, S0 | `add_dependent(Download→Split)`; `start_download`; DownloadStarted event | "downloading" badge; tracks disabled; pending; poll |
| Play click | D~ +q or S~ | unchanged | idempotent — no new jobs | Stays disabled; pendingPlay retargets |
| Play click | DE, S0 | D~ +q, S0 | retry `start_download` | Error badge → downloading |
| Play click | D1, SE | D1, S~ | retry `start_split` | Error label → splitting |
| Download completes | D~ +q, S0 | D1, S~ | `mark_download_succeeded`; `spawn_dependents` → `start_split` | Badge → downloaded; tracks button → "splitting"; still disabled |
| Download completes (no dependent) | D~, S0 | D1, S0 | `mark_download_succeeded` | Tracks enabled (click triggers split) |
| Download fails | D~ +q, S0 | DE, S0 | `mark_download_failed`; **queued split dropped** | Error badge; player "Preparing failed"; poll stops; tracks re-enabled (retry) |
| Split completes | D1, S~ | D1, S1 | `mark_split_succeeded`; `tracks_present` rescanned — **all deleted tracks restored** | Tracks button → "tracks (N)"; tracks enabled; pending track auto-plays |
| Split fails | D1, S~ | D1, SE | `mark_split_failed` | "split-error" label; player error; poll stops; tracks re-enabled (retry) |
| Delete a track | D1, S1 | D1, S1- | files removed; TrackDelete event; `tracks_present[idx]=false` | Track styled unavailable but clickable (click → re-split) |
| Delete last track | D1, S1- | D1, S0 | `clear_split_state` | "not-split (0/N)"; all tracks unavailable-but-clickable |
| Job cancelled (jobs page) | D~ +q | D0 | task aborted; **dependents dropped** | Poll times out or user retries |
| Server restart | D~ +q | D~ (chain lost) | in-memory dependents lost | User clicks play again (accepted) |

### Listing-page hover (UI-only)

| Trigger | Current UI | New UI | Effects |
|---|---|---|---|
| Mouse enters card, tracks box empty | Picture | Tracks shown, picture hidden | One fetch of `/tracks`, injected (DOM is the cache) |
| Mouse enters card, cached | Picture | Tracks shown | Pure CSS `:hover`, no fetch |
| Mouse leaves card | Tracks | Picture | CSS only; cache kept |
| 3 s HTMX status swap | any | refreshed | Card arrives with tracks embedded; player JS re-applies pending mark |
| Detail page | — | Picture + tracks always | Hover CSS scoped to `#concert-list` |

### Player pendingPlay (client state, survives card swaps)

| Trigger | pendingPlay | New | Effects |
|---|---|---|---|
| Click missing track | none | `{id, idx}` | POST `/prepare`; disable buttons; card refresh; start 2 s poll |
| Click different missing track | `{a}` | `{b}` | Retargets; concert *a*'s server chain keeps running |
| Poll: track present | `{id, idx}` | none | `playTrack` — plays or enqueues |
| Poll: error / 30 min timeout | `{id, idx}` | none | Stop poll; player error; card badge shows job error |

## Job dependency state machine (per JobKey)

```
                      add_dependent(upstream, dep)
  ┌──────────┐  (dedup; no task spawned for dep)   ┌─────────────────────┐
  │ (absent) │ ─────────────────────────────────►  │ QUEUED              │
  └──────────┘                                     │ (entry in upstream's│
       ▲                                           │  dependents vec)    │
       │                                           └─────┬──────┬────────┘
       │              upstream SUCCEEDED:                │      │
       │              take_dependents + start_<kind>()   │      │ upstream FAILED /
       │           ┌─────────────────────────────────────┘      │ CANCELLED:
       │           ▼                                            ▼
  ┌──────────────────────┐                            ┌──────────────────┐
  │ RUNNING              │                            │ DROPPED          │
  │ (JoinHandle in       │                            │ (never runs;     │
  │  registry.running;   │                            │  error badge on  │
  │  *_started_at set)   │                            │  upstream shows) │
  └─────┬─────────┬──────┘                            └──────────────────┘
        │success  │failure
        ▼         ▼
 mark_*_succeeded  mark_*_failed (+ its own dependents dropped)
 + spawn_dependents(key)
```

## Testing

- `src/jobs/mod.rs`: dependency-edge unit tests (dedup, take, cancel drops).
- `src/jobs/download.rs`: chain success/failure tests using real shell
  commands (the test "splitter" touches the per-song files).
- `src/jobs/prepare.rs`: Ready / Splitting / Downloading / re-split after
  delete / stale-`downloaded_at` re-download / idempotency.
- `tests/web_integration.rs`: prepare endpoints (full chain over HTTP,
  status JSON, 422/404), card rendering assertions.
- Playwright: `e2e/automate-splitting.spec.js` drives hover reveal,
  removed buttons, and the full split-on-play/auto-play flow against
  `e2e/stub-splitter.js` (a real executable passed via `--splitter-bin` that
  "splits" by copying the source file per song, with a 1 s work delay so
  in-flight states are observable). Fixture concert id=6 is downloaded but
  never split (`split: false` knob in `examples/make_test_fixture.rs`).

## Notes / accepted trade-offs

- `prepare`'s partial-file guard (`registry.is_running` before trusting the
  source file) only covers downloads started by this process; a yt-dlp run
  started out-of-band could still expose a partially-written file to the
  split. Accepted: the app's own download path is the only supported writer.
- A dependency edge queued under a download that never completes is tolerated
  rather than prevented: `prepare` is re-entrant and `add_dependent`
  deduplicates, so the next play click re-converges (self-healing).

- Touch devices have no hover: listing track lists are unreachable by touch,
  but the tracks button plays and the detail page shows everything.
- A re-split rewrites all of the concert's track files, including liked ones
  (likes live in `tracks_liked` and persist).
- When the source file is missing but `downloaded_at` is set, prepare chains
  a re-download without clearing download state (clearing would record a
  misleading DownloadDelete event). The status fix above makes the card show
  "downloading" during the re-run anyway.
- Split errors surface via the tracks button label/class and the detail-page
  error list (the dedicated split badge slot is gone).
