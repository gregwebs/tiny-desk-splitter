# Concert Lifecycle Module

This change adds `concert-tracker/src/lifecycle.rs` as the focused home for
destructive concert lifecycle transitions and stale-job recovery. Job execution
modules still own orchestration, command spawning, and download-to-split
chaining.

## Scope

- Moved durable delete/cancel/recovery policy out of web handlers.
- Added typed outcomes for download deletion, redundant source deletion, split
  deletion, track deletion, job cancellation, and stale in-progress recovery.
- Kept missing-file confirmation and HTMX rendering in the web layer.
- Kept `jobs::prepare`, `jobs::download`, `jobs::split`, and `jobs::archive`
  as concrete orchestration modules.

## State Changes

Download deletion:

```text
Downloaded + file removed       -> clear download state + download_delete
Downloaded + file missing       -> handler confirms unless force=true
Downloaded + redundant coverage -> clear download state + download_delete + source_redundant_delete
Not downloaded                  -> reject
```

Split deletion:

```text
Split or SplitError -> clear split_at, split_started_at, tracks_present, split_errors_json
NotSplit            -> reject
```

Track deletion:

```text
Valid track + some tracks remain -> mark track absent + track_delete
Valid track + no tracks remain   -> track_delete + clear split state
Invalid track                    -> NotFound
File removal failure             -> warn and continue
```

Cancellation:

```text
Running task aborted      -> mark job failed with "cancelled by user"
Queued dependent dropped  -> no event; task was never spawned
Stale DB in-progress flag -> mark job failed with "cancelled by user"
No active job             -> no event
```

Restart recovery:

```text
download_started_at IS NOT NULL -> download_error("server restarted")
split_started_at IS NOT NULL    -> split_error("server restarted")
archive_started_at IS NOT NULL  -> archive_error("server restarted")
```

## Verification

- `cargo check -p concert-tracker`
- `cargo test -p concert-tracker lifecycle -- --nocapture`
- `cargo test -p concert-tracker --test web_integration -- --nocapture`
  reached the lifecycle-related delete tests successfully, then failed in
  `detail_page_auto_scrape_failure_still_renders` with a
  `system-configuration`/`reqwest` runtime panic unrelated to this lifecycle
  change.
