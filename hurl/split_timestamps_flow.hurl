# File-backed split-timestamps behavior migrated from
# concert-tracker/tests/web_integration.rs as part of the Scenario Seeds slice
# (issue #109). State-only split-timestamps cases (no files involved) stay in
# hurl/split_timestamps_state.hurl — this file is only for cases that need a
# legacy on-disk timestamps.json, a sentinel/real source file, or a completed
# split job. See docs/change/2026-07-16-scenario-seeds-hurl-migration.md.
#
# Shares one server process/DB with every other .hurl file — unique
# albums/source_urls per case, per-concert Job Driver overrides only.

# ── get_split_timestamps_lazy_backfill_from_timestamps_json ─────────────────
# A legacy on-disk timestamps.json (predating the DB's auto column) is
# lazily backfilled into the auto response field.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Split TS Flow Legacy Backfill",
  "set_list": ["Old Song A", "Old Song B"],
  "legacy_timestamps_json": true
}
HTTP 200
[Captures]
legacy_backfill_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{legacy_backfill_id}}/split-timestamps
HTTP 200
[Asserts]
jsonpath "$.auto" count == 2
jsonpath "$.auto[0].title" == "Old Song A"

# ── set_split_timestamps_returns_422_on_count_mismatch ──────────────────────
# The source file exists (so the request passes the earlier "source
# present" check) but the submitted timestamp count doesn't match the set
# list — a user-correctable 422, checked before any ffprobe call.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Split TS Flow Count Mismatch",
  "set_list": ["A", "B"],
  "source_file": true,
  "source_file_extension": "mp4"
}
HTTP 200
[Captures]
count_mismatch_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{count_mismatch_id}}/split-timestamps
Content-Type: application/json
{
  "songs": [
    {"title": "A", "start_time": 0.0, "end_time": 55.0}
  ]
}
HTTP 422
[Asserts]
body == "Expected 2 timestamps (one per set-list song), got 1"

# ── set_split_timestamps_happy_path_returns_202_and_stores_user_column ──────
# The only real-media case in the whole Hurl suite: the handler runs real
# ffprobe against the source to bounds-check proposed end times, so the seed
# generates a genuine ~5s sine-wave m4a rather than a sentinel file.
# `source_file: true` is required here — `source_file_kind: real_audio`
# alone does not create a file.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Split TS Flow Real Media Happy Path",
  "set_list": ["Song One", "Song Two"],
  "source_file": true,
  "source_file_kind": "real_audio"
}
HTTP 200
[Captures]
real_media_happy_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{real_media_happy_id}}/split-timestamps
Content-Type: application/json
{
  "songs": [
    {"title": "Song One", "start_time": 0.0, "end_time": 2.5},
    {"title": "Song Two", "start_time": 2.5, "end_time": 5.0}
  ]
}
HTTP 202
[Asserts]
jsonpath "$.status" == "splitting"

# The split job itself runs under the Job Driver's default succeed plan; the
# user column is written by the shared lifecycle code and is publicly
# readable once the (fast, in-process) job completes.
GET {{base_url}}/concerts/{{real_media_happy_id}}/split-timestamps
[Options]
retry: 30
retry-interval: 50ms
HTTP 200
[Asserts]
jsonpath "$.user" count == 2
jsonpath "$.user[0].title" == "Song One"

# ── reset_split_timestamps_happy_path_returns_202_and_clears_user_column ────
# Resetting never probes media (the auto timestamps already came from the
# same source file), so a sentinel source — checked only for existence —
# is enough here.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Split TS Flow Reset Happy Path",
  "set_list": ["Reset A", "Reset B"],
  "source_file": true,
  "source_file_extension": "mp4",
  "auto_timestamps": [
    {"title": "Reset A", "start_time": 0.0, "end_time": 55.0, "duration": 55.0},
    {"title": "Reset B", "start_time": 60.0, "end_time": 115.0, "duration": 55.0}
  ],
  "user_timestamps": [
    {"title": "Reset A", "start_time": 2.0, "end_time": 57.0, "duration": 55.0},
    {"title": "Reset B", "start_time": 62.0, "end_time": 117.0, "duration": 55.0}
  ]
}
HTTP 200
[Captures]
reset_happy_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{reset_happy_id}}/split-timestamps/reset
HTTP 202
[Asserts]
jsonpath "$.status" == "splitting"

GET {{base_url}}/concerts/{{reset_happy_id}}/split-timestamps
[Options]
retry: 30
retry-interval: 50ms
HTTP 200
[Asserts]
jsonpath "$.user" == null
jsonpath "$.auto" count == 2
