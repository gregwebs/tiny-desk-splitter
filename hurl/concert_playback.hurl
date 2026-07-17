# Whole-concert playback plan and interlude-deletion behavior migrated from
# concert-tracker/tests/web_integration.rs as part of the Scenario Seeds slice
# (issue #109). See docs/change/2026-07-16-scenario-seeds-hurl-migration.md
# for the seed-param design this file relies on (`interlude_files` derives
# sentinel interlude tracks from a seeded timestamp gap).
#
# `concert_playback_returns_404_when_nothing_playable` already lives in
# hurl/media_state_errors.hurl. Shares one server process/DB with every
# other .hurl file — unique albums/source_urls per case.

# ── concert_playback_source_mode_when_file_present ──────────────────────────
# A split concert whose source file is still present plays as one whole
# source, not a reconstruction from tracks.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Concert Playback Source Mode",
  "split": true,
  "set_list": ["Song A", "Song B"],
  "tracks_present": [true, true],
  "track_files": [0, 1],
  "track_file_extension": "m4a",
  "source_file": true,
  "source_file_extension": "mp4"
}
HTTP 200
[Captures]
playback_source_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{playback_source_id}}/concert-playback
HTTP 200
[Asserts]
jsonpath "$.mode" == "source"
jsonpath "$.source" isCollection

# ── concert_playback_reconstruction_mode_when_source_gone ───────────────────
# The same shape without a source file reconstructs playback from the
# individual song tracks.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Concert Playback Reconstruction Mode",
  "split": true,
  "set_list": ["Song A", "Song B"],
  "tracks_present": [true, true],
  "track_files": [0, 1],
  "track_file_extension": "m4a"
}
HTTP 200
[Captures]
playback_recon_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{playback_recon_id}}/concert-playback
HTTP 200
[Asserts]
jsonpath "$.mode" == "reconstruction"
jsonpath "$.items" count == 2
jsonpath "$.items[0].kind" == "song"
jsonpath "$.items[1].kind" == "song"

# ── concert_playback_reconstruction_includes_interlude ──────────────────────
# A reconstruction with a real timestamp gap between two songs includes the
# interlude item at its correct position.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Concert Playback Reconstruction Interlude",
  "split": true,
  "set_list": ["Song A", "Song B"],
  "tracks_present": [true, true],
  "track_files": [0, 1],
  "track_file_extension": "m4a",
  "user_timestamps": [
    {"title": "Song A", "start_time": 0.0, "end_time": 55.0, "duration": 55.0},
    {"title": "Song B", "start_time": 60.0, "end_time": 115.0, "duration": 55.0}
  ],
  "media_duration": 115.0,
  "interlude_files": true
}
HTTP 200
[Captures]
playback_interlude_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{playback_interlude_id}}/concert-playback
HTTP 200
[Asserts]
jsonpath "$.mode" == "reconstruction"
jsonpath "$.items" count == 3
jsonpath "$.items[0].kind" == "song"
jsonpath "$.items[1].kind" == "interlude"
jsonpath "$.items[2].kind" == "song"

# ── delete_interlude_removes_file_records_event_returns_fragment ───────────
# Deleting an interlude removes its file, records an interlude_delete event
# (and no track_delete event, since deleting an interlude is not deleting a
# song track), and returns the refreshed track-list sidebar fragment.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Concert Playback Interlude Delete",
  "split": true,
  "set_list": ["Song A", "Song B"],
  "tracks_present": [true, true],
  "track_files": [0, 1],
  "track_file_extension": "m4a",
  "user_timestamps": [
    {"title": "Song A", "start_time": 0.0, "end_time": 55.0, "duration": 55.0},
    {"title": "Song B", "start_time": 60.0, "end_time": 115.0, "duration": 55.0}
  ],
  "media_duration": 115.0,
  "interlude_files": true
}
HTTP 200
[Captures]
interlude_delete_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{interlude_delete_id}}/concert-playback
HTTP 200
[Asserts]
jsonpath "$.items" count == 3

POST {{base_url}}/concerts/{{interlude_delete_id}}/interludes/1/delete
HTTP 200
[Asserts]
body contains "track-list"

# File-gone public proof: the interlude item drops out of the public
# playback plan once its file is really removed.
GET {{base_url}}/concerts/{{interlude_delete_id}}/concert-playback
HTTP 200
[Asserts]
jsonpath "$.items" count == 2

POST {{test_control_url}}/test/assert/concert_events
Content-Type: application/json
{
  "concert_id": {{interlude_delete_id}},
  "present": ["interlude_delete"],
  "absent": ["track_delete"]
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true
