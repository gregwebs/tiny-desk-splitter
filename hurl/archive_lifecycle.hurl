# Archive / unarchive lifecycle (#164, #167).
#
# These two routes had no black-box coverage. The manual pass for #168 found they
# answer their own 500 for a request the concert cannot satisfy (no archive
# location configured, nothing to archive, not archived) rather than the typed
# 409 the rest of the contracted surface returns. That is this endpoint's
# long-standing protocol and the adapter keeps it deliberately — see the comment
# on `handlers::archive` — so this file pins the behaviour instead of changing it.
#
# Archive moves the concert's directory into the configured location, so the
# location is a scratch path outside the workdir. `scripts/hurl-test.js`
# creates one per run and passes it in as `{{archive_location}}`: archive
# renames `<workdir>/<album>` onto `<archive location>/<album>`, so a fixed path
# would fail with ENOTEMPTY on any leftover from an aborted earlier run.
#
# A configured archive location makes the Archived chip appear. This file's
# `test.reset` clears it too (`reset_test_data` nulls the settings row), so the
# file always starts from no location. The clear at the end is hygiene for the
# rest of the suite: the settings row is shared with the later files, which
# mostly do not reset.

POST {{test_control_url}}/test/reset
HTTP 200

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Archive Lifecycle",
  "set_list": [],
  "downloaded": true,
  "source_file": true,
  "source_file_extension": "mp4"
}
HTTP 200
[Captures]
archive_id: jsonpath "$.result.id"

# ── archive_without_a_configured_location_is_the_endpoints_own_500 ─────────
POST {{base_url}}/concerts/{{archive_id}}/archive
HTTP 500
[Asserts]
body contains "archive location is not configured"

# Without an archive location the Archived chip is not offered at all.
GET {{base_url}}/
HTTP 200
[Asserts]
body not contains "/?filter=archived"

# ── configuring_a_location_turns_the_archived_filter_on ───────────────────
POST {{base_url}}/settings
[FormParams]
archive_location: {{archive_location}}
theme: system
HTTP 303

GET {{base_url}}/
HTTP 200
[Asserts]
body contains "/?filter=archived"
body contains "/concerts/{{archive_id}}/archive\""

# ── archive_moves_the_concert_into_the_archived_filter ────────────────────
POST {{base_url}}/concerts/{{archive_id}}/archive
HTTP 200
[Asserts]
header "HX-Refresh" == "true"

# Wait on the archive Job Run's own terminal event, not on the rendered page:
# the move is asynchronous, so retrying the listing for a fixed 1.5 s window
# loses under load. The window is long (20 s) because this is the only wait in
# the file that can be slow; Hurl retries a failed assertion, so a genuinely
# failed Job Run is reported at the end of the window rather than immediately.
#
# `$.error.message not exists`, not `$.result.ok == true`: on failure Hurl prints
# the value it found, so the server's own mismatch text says whether the job is
# still pending ("archived ... found none") or has failed (`archive_error`
# present).
POST {{test_control_url}}/test/assert/concert_events
Content-Type: application/json
[Options]
retry: 100
retry-interval: 200ms
{
  "concert_id": {{archive_id}},
  "present": ["archived"],
  "absent": ["archive_error"]
}
HTTP 200
[Asserts]
jsonpath "$.error.message" not exists

GET {{base_url}}/?filter=archived
HTTP 200
[Asserts]
body contains "id=\"concert-{{archive_id}}\""
body contains "badge-archived"
body contains "/concerts/{{archive_id}}/unarchive\""

# ── archiving_an_archived_concert_is_a_no_op_not_an_error ─────────────────
POST {{base_url}}/concerts/{{archive_id}}/archive
HTTP 200

# ── unarchive_brings_it_back_and_flips_the_card_back ──────────────────────
POST {{base_url}}/concerts/{{archive_id}}/unarchive
HTTP 200
[Asserts]
header "HX-Refresh" == "true"

# Same terminal-event wait as the archive case: unarchive moves the directory
# back on a blocking thread and records `archive_delete` when it commits.
POST {{test_control_url}}/test/assert/concert_events
Content-Type: application/json
[Options]
retry: 100
retry-interval: 200ms
{
  "concert_id": {{archive_id}},
  "present": ["archive_delete"],
  "absent": ["archive_error"]
}
HTTP 200
[Asserts]
jsonpath "$.error.message" not exists

GET {{base_url}}/?filter=archived
HTTP 200
[Asserts]
body not contains "id=\"concert-{{archive_id}}\""

GET {{base_url}}/?filter=available
HTTP 200
[Asserts]
body contains "/concerts/{{archive_id}}/archive\""
body not contains "badge-archived"

# ── unarchiving_what_is_not_archived_keeps_the_endpoints_own_500 ──────────
POST {{base_url}}/concerts/{{archive_id}}/unarchive
HTTP 500
[Asserts]
body contains "is not archived"

# ── a_concert_with_no_media_has_nothing_to_archive ────────────────────────
POST {{test_control_url}}/test/seed/listing
Content-Type: application/json
{
  "title": "Archive Lifecycle Nothing",
  "teaser": "nothing downloaded yet"
}
HTTP 200
[Captures]
empty_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{empty_id}}/archive
HTTP 200

GET {{base_url}}/?filter=archived
HTTP 200
[Asserts]
body not contains "id=\"concert-{{empty_id}}\""

# ── a_missing_concert_is_also_this_endpoints_500 ──────────────────────────
# Unlike the other contracted commands, which answer a missing concert with the
# page's own 404, archive never distinguished it: the legacy handler went
# straight into the job submission and let the lookup failure surface as a 500.
POST {{base_url}}/concerts/999999/archive
HTTP 500

# ── leave_the_shared_process_as_this_file_found_it ────────────────────────
# The settings row outlives this file and the later files mostly do not reset, so
# leave it as this file found it rather than relying on them not minding.
POST {{base_url}}/settings
[FormParams]
archive_location:
theme: system
HTTP 303

GET {{base_url}}/
HTTP 200
[Asserts]
body not contains "/?filter=archived"
