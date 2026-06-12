# Download Auto-split

## Goal

After the automated-splitting change (2026-06-11) made track playback trigger
a download → split chain automatically, the Download button itself was left as
a plain download: the user still had to click a track afterward to trigger the
split. This change closes that gap: clicking Download on an unsplit concert now
queues a split to run as soon as the download finishes.

## Queue / skip conditions

A split is queued behind the download when **all** of:

- The concert has an album and a non-empty set list (needed by the splitter).
- `SplitStatus` is `NotSplit` **or** `SplitError` — i.e. `split_at` and
  `split_started_at` are both NULL.

**Not** queued (plain download, unchanged behavior):

- No album or empty set list — the concert needs metadata scraping first; the
  download still proceeds so the file is ready when metadata arrives.
- `SplitStatus::Split` — source file deleted out-of-band while tracks exist.
  Re-downloading restores the source video but intentionally does **not**
  rewrite surviving (possibly liked) track files. Playing a deleted track
  remains the deliberate re-split gesture.
- `SplitStatus::Splitting` — a split is already running; don't stack another.

`SplitError` auto-retry is intentional: a previously failed split is retried on
Download click, mirroring the play-track retry rows in the 2026-06-11 table.

## Implementation

`handlers::download` (`concert-tracker/src/web/handlers.rs`) is the only
changed file. After `ensure_scraped_blocking` (which may have just populated
metadata), the handler reloads the concert and branches:

- **Queue branch** — calls `crate::jobs::prepare::prepare(...)` directly. This
  reuses the existing idempotent orchestrator that chains Download → Split via
  the in-memory `JobRegistry` dependency edge, with a race guard ensuring the
  split starts exactly once.

  `prepare()` return values are handled:
  - `Ready` (all track files exist but source missing — contrived manual-copy
    state) → force `start_download` so the button always delivers the source
    video.
  - `Ready` (source also present) → `downloaded_at` was reconciled by
    `prepare`; the Download button will disappear. No job started.
  - `Splitting` / `Downloading` → chain is running; nothing more to do.
  - `Err(NoSetList)` TOCTOU (metadata cleared between the condition check and
    `prepare`'s re-read) → fall back to `start_download`, not a 500.

- **Skip branch** → `start_download` only, as before.

No new jobs or prepare code was added. The card render already picks up the
queued split via `split_queued` / `tracks_busy` (which check
`registry.has_dependent`), so the returned card immediately reflects the
pending state. No template or `player.js` changes.

## State table

Same notation as the 2026-06-11 doc:
**D0** not downloaded · **D~** downloading · **D1** downloaded (source on
disk) · **DE** download error · **S0** unsplit · **S~** splitting ·
**S1** split (all tracks present) · **S1-** split, some tracks deleted ·
**SE** split error · **+q** split queued as dependent of download.

| Trigger | Current State | New State | Backend Effects | UI Effects |
|---|---|---|---|---|
| Download click | D0/DE, S0 or SE (has set list) | D~ +q | `prepare()`: `add_dependent(D→S)`; `start_download` | badge "downloading"; tracks button pending/disabled |
| Download click | D0/DE, source file on disk, S0/SE | D1, S~ | `prepare()`: reconcile `downloaded_at`; `start_split` | tracks button "splitting" |
| Download click | D0/DE, no set list | D~ | `start_download` only (unchanged) | badge "downloading" |
| Download click | D0/DE, S1/S1- (source deleted out-of-band) | D~ | `start_download` only — surviving tracks untouched | badge "downloading" |
| Download click | all tracks on disk, source missing, S0 | D~ | `prepare()`=Ready, then forced `start_download` (no split queued) | badge "downloading" |
| Download click | all tracks + source on disk, `downloaded_at` NULL | reconciled | `prepare()`=Ready; `set_downloaded_at_if_missing`; no job | Download button disappears |
| Download completes | D~ +q | D1, S~ | `spawn_dependents` → split (existing machinery) | "splitting", then "tracks (N)" |
| Download fails | D~ +q | DE | queued split dropped (existing) | error badge; tracks re-enabled |

## Trade-offs

- Re-downloading a split concert's deleted source does **not** re-split. The
  asymmetry is intentional: surviving track files (potentially liked) are not
  touched. Play a deleted track to trigger a re-split.
- `SplitError` is auto-retried. Consistent with the play-track retry behavior.
- Ready-edge (all tracks present, source missing) forces a download even though
  playback works fine. The user presumably clicked Download to get the source
  video back; not downloading would be surprising.
- The Download button is only rendered for `NotDownloaded` / `DownloadError`
  states, so it is never reachable from a concert that is currently `Splitting`
  or has `split_at` set and a source file present.

## Tests

Seven integration tests added in `concert-tracker/tests/web_integration.rs`:

1. `download_auto_split_runs_full_chain` — full download → split chain via
   POST /download without any track click.
2. `download_auto_split_reconciles_source_present_downloaded_at_null` —
   source file exists but `downloaded_at` NULL; verifies split starts, not a
   silent no-op (regression guard for the rejected extraction design).
3. `download_auto_split_retries_on_split_error` — prior split failure; chain
   re-triggered.
4. `download_no_set_list_plain_download_no_split_queued` — no set list;
   split never queued.
5. `download_does_not_resplit_already_split_concert` — surviving track file
   contents unchanged after re-download.
6. `download_double_click_does_not_drop_split_edge` — idempotent; one chain.
7. `download_force_starts_when_tracks_present_but_source_missing` — Ready
   edge; download runs and source file appears.
