# Split Timestamp Workflow Module

This refactor deepens `concert-tracker/src/split_timestamps.rs` from a pure
validation module into the backend home for split timestamp editing workflow.
HTTP handlers keep route annotations and status/body translation.

## Scope

- Moved split timestamp response DTOs into `split_timestamps.rs`.
- Added typed workflow outcomes and errors for read, user timestamp apply, and
  reset-to-auto operations.
- Kept DB, job registry, and job config dependencies explicit; the module does
  not depend on `web::AppState`.
- Preserved behavior of public routes and OpenAPI paths.

## State Changes

None. This is a behavior-preserving refactor.

```text
GET split-timestamps
  missing concert -> 404
  source exists + ffprobe ok -> fresh duration
  source exists + ffprobe fails -> stored media_duration
  source missing -> stored media_duration
  no stored duration -> null media_duration

POST user timestamps
  missing concert -> 404
  source missing -> 409
  count/title/time validation fails -> 422
  source exists + valid payload -> reconcile downloaded_at if missing -> start UserTimestamps split -> 202
  split already running -> 409

POST reset
  missing concert -> 404
  no auto timestamps -> 422
  user timestamps already null -> 200 already-auto
  auto timestamps stale vs set list -> 422
  valid reset -> reconcile downloaded_at if missing -> start ResetToAuto split -> 202
  split already running -> 409
```

`downloaded_at` reconciliation remains non-fatal: failures are logged and the
workflow continues.

## Verification

- `cargo check -p concert-tracker`
- `cargo test -p concert-tracker split_timestamps --lib`
- `cargo test -p concert-tracker --test web_integration split_timestamps`
- `just lint`
- engineering-lead Agent Review: approved with no blocking findings.
