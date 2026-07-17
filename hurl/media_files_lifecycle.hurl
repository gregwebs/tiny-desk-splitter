# File-heavy product HTTP behavior migrated from
# concert-tracker/tests/web_integration.rs as part of the Scenario Seeds slice
# (issue #109) of the remaining web integration Hurl migration described in
# docs/change/2026-07-14-remaining-web-integration-hurl-migration-spec.md. See
# docs/change/2026-07-16-scenario-seeds-hurl-migration.md for the seed-param
# and public-proof design decisions this file relies on.
#
# Every case seeds its own concert with a unique album/source_url and uses
# per-concert Job Driver overrides only — this file shares one server
# process/DB with every other .hurl file, so no case may call
# `/test/reset` or mutate the Job Driver's default plan (see hurl/README.md).

# ── delete_download_removes_file_and_clears_state ───────────────────────────
# A downloaded (never-split) concert's source file is really removed from
# disk, not just marked deleted in the DB.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Media Files Delete Download",
  "downloaded": true,
  "source_file": true,
  "source_file_extension": "mp4",
  "set_list": []
}
HTTP 200
[Captures]
delete_dl_id: jsonpath "$.result.id"

# Before deleting: the source file is on disk, so playback plans to serve it
# whole ("source" mode) — the baseline this case's file-gone proof compares
# against.
GET {{base_url}}/concerts/{{delete_dl_id}}/concert-playback
HTTP 200
[Asserts]
jsonpath "$.mode" == "source"

POST {{base_url}}/concerts/{{delete_dl_id}}/delete-download
HTTP 200
[Asserts]
header "HX-Refresh" not exists
header "HX-Retarget" == "#concert-{{delete_dl_id}}"
header "HX-Reswap" == "outerHTML"
body contains "id=\"concert-{{delete_dl_id}}\""
body contains "/concerts/{{delete_dl_id}}/download\""

POST {{test_control_url}}/test/assert/concert_state
Content-Type: application/json
{
  "id": {{delete_dl_id}},
  "downloaded": false,
  "split": false
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

# File-gone public proof: with an empty set list and no source file left,
# there is nothing to play — the real product code (not just the DB column)
# now has to look at the filesystem and find the file gone.
GET {{base_url}}/concerts/{{delete_dl_id}}/concert-playback
HTTP 404

# ── delete_download_with_prior_split_error_restores_download_button ────────
# After a failed split, deleting the download must restore the plain
# "Download" action (not a stale Split/Listen action) on the re-rendered card.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Media Files Prior Split Error",
  "downloaded": true,
  "source_file": true,
  "source_file_extension": "mp4",
  "set_list": ["Song A", "Song B"]
}
HTTP 200
[Captures]
prior_split_err_id: jsonpath "$.result.id"

POST {{test_control_url}}/test/job/set_plan
Content-Type: application/json
{
  "concert_id": {{prior_split_err_id}},
  "split": "fail"
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

# Source file is already present, so /prepare goes straight to a split.
POST {{base_url}}/concerts/{{prior_split_err_id}}/prepare
HTTP 200

POST {{test_control_url}}/test/assert/job_observation
Content-Type: application/json
[Options]
retry: 30
retry-interval: 50ms
{
  "concert_id": {{prior_split_err_id}},
  "kind": "split",
  "failed": 1
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

POST {{base_url}}/concerts/{{prior_split_err_id}}/delete-download
HTTP 200

GET {{base_url}}/concerts/{{prior_split_err_id}}/status
HTTP 200
[Asserts]
body contains "/concerts/{{prior_split_err_id}}/download\""
body not contains "/concerts/{{prior_split_err_id}}/split\""
body not contains "/concerts/{{prior_split_err_id}}/listen\""

# ── play_button_visible_after_successful_split ──────────────────────────────
# A fully split concert's card shows "Play concert" plus track/tracks-row
# controls, and no split-in-progress or delete-split actions.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Media Files Play Button",
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
play_button_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{play_button_id}}/status
HTTP 200
[Asserts]
body contains "Player.playConcert({{play_button_id}})"
body contains "Play concert</button>"
body contains "Player.playTracks(this, {{play_button_id}})"
body contains "Player.playTrack(this, {{play_button_id}}, 0)"
body not contains "Player.watchDirect"
body not contains "Watch</button>"
body not contains "/concerts/{{play_button_id}}/split\""
body not contains "/concerts/{{play_button_id}}/delete-split"
# "Play concert" renders before the delete-download trash button in the same
# download slot — pins the button ordering the original test checked.
body matches "(?s)Player\\.playConcert\\({{play_button_id}}\\).*delete-download"

# ── ignore_deletes_preview_image ────────────────────────────────────────────
# Ignoring an unscraped-download listing also deletes its cached preview
# thumbnail from disk.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Media-Files-Ignore-Preview",
  "preview_image": true
}
HTTP 200
[Captures]
ignore_preview_id: jsonpath "$.result.id"

GET {{base_url}}/concert-files/Media-Files-Ignore-Preview/preview.jpg
HTTP 200

POST {{base_url}}/concerts/{{ignore_preview_id}}/ignore
HTTP 200

GET {{base_url}}/concert-files/Media-Files-Ignore-Preview/preview.jpg
HTTP 404

# ── prepare_status_reports_filesystem_track_state ───────────────────────────
# `tracks_present` in the prepare-status JSON reflects real files on disk, not
# a DB column — one song file present, the other genuinely missing.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Media Files Prepare Status",
  "set_list": ["Song A", "Song B"],
  "track_files": [1],
  "track_file_extension": "m4a"
}
HTTP 200
[Captures]
prepare_status_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{prepare_status_id}}/prepare-status
HTTP 200
[Asserts]
jsonpath "$.tracks_present[0]" == false
jsonpath "$.tracks_present[1]" == true
jsonpath "$.download" == "not-downloaded"
jsonpath "$.split" == "not-split"
jsonpath "$.split_queued" == false

# ── track_details_returns_200_without_album ─────────────────────────────────
# A concert with a NULL album (a historical/defensive shape no current
# product write path produces) still renders track details correctly.

POST {{test_control_url}}/test/seed/album_null_concert
Content-Type: application/json
{
  "set_list": ["Song A"],
  "tracks_present": [true],
  "tracks_liked": [true]
}
HTTP 200
[Captures]
album_null_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{album_null_id}}/track-details
HTTP 200
[Asserts]
jsonpath "$.tracks_busy" == false
jsonpath "$.tracks[0].title" == "Song A"
jsonpath "$.tracks[0].available" == true
jsonpath "$.tracks[0].is_video" == false
jsonpath "$.tracks[0].liked" == true

# ── track_details_reports_busy_from_handler_state ───────────────────────────
# `tracks_busy` reflects a split genuinely in flight (Job Driver block),
# arranged through the real /prepare route rather than a hand-set DB column.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Media Files Busy Track Details",
  "downloaded": true,
  "source_file": true,
  "source_file_extension": "mp4",
  "set_list": ["Song A"]
}
HTTP 200
[Captures]
busy_details_id: jsonpath "$.result.id"

POST {{test_control_url}}/test/job/set_plan
Content-Type: application/json
{
  "concert_id": {{busy_details_id}},
  "split": "block"
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

POST {{base_url}}/concerts/{{busy_details_id}}/prepare
HTTP 200

POST {{test_control_url}}/test/assert/job_observation
Content-Type: application/json
[Options]
retry: 30
retry-interval: 50ms
{
  "concert_id": {{busy_details_id}},
  "kind": "split",
  "blocked": 1
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

GET {{base_url}}/concerts/{{busy_details_id}}/track-details
HTTP 200
[Asserts]
jsonpath "$.tracks_busy" == true

POST {{test_control_url}}/test/job/release
Content-Type: application/json
{
  "concert_id": {{busy_details_id}},
  "kind": "split",
  "outcome": "succeed"
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

# Let the released split settle before this file's next case runs — this
# process is shared across every .hurl file in the run.
GET {{base_url}}/concerts/{{busy_details_id}}/prepare-status
[Options]
retry: 30
retry-interval: 50ms
HTTP 200
[Asserts]
jsonpath "$.split" == "split"
