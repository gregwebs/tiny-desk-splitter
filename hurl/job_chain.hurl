# Job Driver-backed download/prepare/split chain and opener cases — slice 2
# of the remaining web integration Hurl migration described in
# docs/change/2026-07-15-job-driver-plan.md (issue #108). Migrates the
# job-chain and opener tests out of
# concert-tracker/tests/web_integration.rs; see that file for breadcrumbs
# back to each case below.
#
# The Job Driver's default plan is "succeed" for download/split/open and is
# never changed by this file — every case that needs different behavior uses
# a per-concert `test.job_set_plan` override instead, so cases stay
# independent of execution order within this shared-process file (see
# hurl/README.md's Job Driver section).

# ── download_endpoint_spawns_job_and_returns_row ────────────────────────────
# A concert with no set list: /download runs a plain download (no split
# chain) and returns 200 (an HTML card fragment, not JSON).

POST {{test_control_url}}/test/seed/scraped_concert
Content-Type: application/json
{
  "album": "Job Chain Plain Download",
  "set_list": []
}
HTTP 200
[Captures]
plain_download_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{plain_download_id}}/download
HTTP 200

# ── prepare_endpoint_runs_download_then_split_chain ─────────────────────────
# POST /prepare on a concert with no source file runs download -> split; a
# second POST is a no-op. Both return the current PrepareStatus JSON.

POST {{test_control_url}}/test/seed/scraped_concert
Content-Type: application/json
{
  "album": "Job Chain Prepare",
  "set_list": ["Song A", "Song B"]
}
HTTP 200
[Captures]
prepare_chain_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{prepare_chain_id}}/prepare
HTTP 200
[Asserts]
jsonpath "$.tracks_present[0]" == false
jsonpath "$.tracks_present[1]" == false

POST {{base_url}}/concerts/{{prepare_chain_id}}/prepare
HTTP 200

GET {{base_url}}/concerts/{{prepare_chain_id}}/prepare-status
[Options]
retry: 30
retry-interval: 50ms
HTTP 200
[Asserts]
jsonpath "$.download" == "downloaded"
jsonpath "$.split" == "split"
jsonpath "$.split_queued" == false
jsonpath "$.tracks_present[0]" == true
jsonpath "$.tracks_present[1]" == true

# ── download_auto_split_runs_full_chain ─────────────────────────────────────
# POST /download on a not-downloaded concert with a set list runs the full
# download -> split chain without any track click.

POST {{test_control_url}}/test/seed/scraped_concert
Content-Type: application/json
{
  "album": "Job Chain Auto Split",
  "set_list": ["Song A", "Song B"]
}
HTTP 200
[Captures]
auto_split_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{auto_split_id}}/download
HTTP 200

GET {{base_url}}/concerts/{{auto_split_id}}/prepare-status
[Options]
retry: 30
retry-interval: 50ms
HTTP 200
[Asserts]
jsonpath "$.download" == "downloaded"
jsonpath "$.split" == "split"
jsonpath "$.split_queued" == false
jsonpath "$.tracks_present[0]" == true
jsonpath "$.tracks_present[1]" == true

# ── download_auto_split_reconciles_source_present_downloaded_at_null ───────
# Source file exists on disk but downloaded_at is NULL (manual copy). POST
# /download must reconcile downloaded_at and start a split, not no-op.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Job Chain Reconcile",
  "set_list": ["Track 1"],
  "source_file": true,
  "downloaded": false
}
HTTP 200
[Captures]
reconcile_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{reconcile_id}}/download
HTTP 200

GET {{base_url}}/concerts/{{reconcile_id}}/prepare-status
[Options]
retry: 30
retry-interval: 50ms
HTTP 200
[Asserts]
jsonpath "$.split" == "split"
jsonpath "$.tracks_present[0]" == true

# ── download_auto_split_retries_on_split_error ──────────────────────────────
# A concert whose split failed (SplitError state) must have the chain
# re-triggered on a later Download click, just like a play-track click would.

POST {{test_control_url}}/test/seed/scraped_concert
Content-Type: application/json
{
  "album": "Job Chain Split Retry",
  "set_list": ["Song X"]
}
HTTP 200
[Captures]
split_retry_id: jsonpath "$.result.id"

POST {{test_control_url}}/test/job/set_plan
Content-Type: application/json
{
  "concert_id": {{split_retry_id}},
  "split": "fail"
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

POST {{base_url}}/concerts/{{split_retry_id}}/download
HTTP 200

GET {{base_url}}/concerts/{{split_retry_id}}/prepare-status
[Options]
retry: 30
retry-interval: 50ms
HTTP 200
[Asserts]
jsonpath "$.split" == "split-error"

POST {{test_control_url}}/test/job/set_plan
Content-Type: application/json
{
  "concert_id": {{split_retry_id}},
  "split": "succeed"
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

POST {{base_url}}/concerts/{{split_retry_id}}/download
HTTP 200

GET {{base_url}}/concerts/{{split_retry_id}}/prepare-status
[Options]
retry: 30
retry-interval: 50ms
HTTP 200
[Asserts]
jsonpath "$.split" == "split"
jsonpath "$.tracks_present[0]" == true

# ── download_no_set_list_plain_download_no_split_queued ─────────────────────
# A concert with no set list still downloads (plain download, no split
# queued) — behavior unchanged from before this feature.

POST {{test_control_url}}/test/seed/scraped_concert
Content-Type: application/json
{
  "album": "Job Chain No Setlist",
  "set_list": []
}
HTTP 200
[Captures]
no_setlist_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{no_setlist_id}}/download
HTTP 200

GET {{base_url}}/concerts/{{no_setlist_id}}/prepare-status
[Options]
retry: 30
retry-interval: 50ms
HTTP 200
[Asserts]
jsonpath "$.download" == "downloaded"
jsonpath "$.split_queued" == false

# ── download_does_not_resplit_already_split_concert ──────────────────────────
# Re-downloading an already-split concert (e.g. source file deleted
# out-of-band while tracks exist) must not re-split. Stronger than the
# original Rust test's "track file bytes untouched" check: no split job runs
# at all — see docs/change/2026-07-15-job-driver-plan.md's migration table.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Job Chain Already Split",
  "set_list": ["Keep Track"],
  "split": true,
  "track_files": [0]
}
HTTP 200
[Captures]
already_split_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{already_split_id}}/download
HTTP 200

POST {{test_control_url}}/test/assert/job_observation
Content-Type: application/json
{
  "concert_id": {{already_split_id}},
  "kind": "split",
  "started": 0
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

# ── download_double_click_does_not_drop_split_edge ──────────────────────────
# A second POST /download while one is already running must not drop the
# queued split edge; after the download finishes, exactly one split runs.

POST {{test_control_url}}/test/seed/scraped_concert
Content-Type: application/json
{
  "album": "Job Chain Double Click",
  "set_list": ["Song 1"]
}
HTTP 200
[Captures]
double_click_id: jsonpath "$.result.id"

POST {{test_control_url}}/test/job/set_plan
Content-Type: application/json
{
  "concert_id": {{double_click_id}},
  "download": "block"
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

POST {{base_url}}/concerts/{{double_click_id}}/download
HTTP 200

POST {{base_url}}/concerts/{{double_click_id}}/download
HTTP 200

POST {{test_control_url}}/test/assert/job_observation
Content-Type: application/json
[Options]
retry: 30
retry-interval: 50ms
{
  "concert_id": {{double_click_id}},
  "kind": "download",
  "blocked": 1
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

POST {{test_control_url}}/test/job/release
Content-Type: application/json
{
  "concert_id": {{double_click_id}},
  "kind": "download",
  "outcome": "succeed"
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

GET {{base_url}}/concerts/{{double_click_id}}/prepare-status
[Options]
retry: 30
retry-interval: 50ms
HTTP 200
[Asserts]
jsonpath "$.split" == "split"
jsonpath "$.split_queued" == false
jsonpath "$.tracks_present[0]" == true

POST {{test_control_url}}/test/assert/job_observation
Content-Type: application/json
{
  "concert_id": {{double_click_id}},
  "kind": "download",
  "started": 1
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

POST {{test_control_url}}/test/assert/job_observation
Content-Type: application/json
{
  "concert_id": {{double_click_id}},
  "kind": "split",
  "completed": 1
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

# ── cancel_running_download_creates_failed_job_history ──────────────────────
# #125: cancelling an accepted download Job Run through the Job Run engine
# must produce exactly one terminal outcome (a Failed Job, "cancelled by
# user") and clear the concert back to download-error — not silently leave
# it stuck, and not also let the blocked download itself go on to succeed.

POST {{test_control_url}}/test/seed/scraped_concert
Content-Type: application/json
{
  "title": "Job Chain Cancel Download",
  "album": "Job Chain Cancel Download",
  "set_list": ["Song 1"]
}
HTTP 200
[Captures]
cancel_dl_id: jsonpath "$.result.id"

POST {{test_control_url}}/test/job/set_plan
Content-Type: application/json
{
  "concert_id": {{cancel_dl_id}},
  "download": "block"
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

POST {{base_url}}/concerts/{{cancel_dl_id}}/download
HTTP 200

POST {{test_control_url}}/test/assert/job_observation
Content-Type: application/json
[Options]
retry: 30
retry-interval: 50ms
{
  "concert_id": {{cancel_dl_id}},
  "kind": "download",
  "blocked": 1
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

POST {{base_url}}/jobs/{{cancel_dl_id}}/cancel/download
HTTP 200

GET {{base_url}}/concerts/{{cancel_dl_id}}/prepare-status
[Options]
retry: 30
retry-interval: 50ms
HTTP 200
[Asserts]
jsonpath "$.download" == "download-error"

GET {{base_url}}/jobs
HTTP 200
[Asserts]
body contains "cancelled by user"
body contains "Job Chain Cancel Download"

# Releasing the (now-cancelled) blocked download must not resurrect a
# success: the Job Run's terminal gate was already claimed by cancel.
POST {{test_control_url}}/test/job/release
Content-Type: application/json
{
  "concert_id": {{cancel_dl_id}},
  "kind": "download",
  "outcome": "succeed"
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

GET {{base_url}}/concerts/{{cancel_dl_id}}/prepare-status
HTTP 200
[Asserts]
jsonpath "$.download" == "download-error"

POST {{test_control_url}}/test/assert/job_observation
Content-Type: application/json
{
  "concert_id": {{cancel_dl_id}},
  "kind": "split",
  "started": 0
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

# ── download_force_starts_when_tracks_present_but_source_missing ────────────
# All track files exist on disk but the source video is missing and
# downloaded_at is NULL. The Download button must still start a download so
# the user gets the source video.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Job Chain Tracks Present",
  "set_list": ["Existing Track"],
  "track_files": [0]
}
HTTP 200
[Captures]
tracks_present_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{tracks_present_id}}/download
HTTP 200

GET {{base_url}}/concerts/{{tracks_present_id}}/prepare-status
[Options]
retry: 30
retry-interval: 50ms
HTTP 200
[Asserts]
jsonpath "$.download" == "downloaded"

# ── watch_uses_injected_opener_and_succeeds ─────────────────────────────────
# The default Job Driver plan opens successfully, so /watch returns 200
# without launching a real media player.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Job Chain Opener Ok",
  "downloaded": true,
  "source_file": true
}
HTTP 200
[Captures]
opener_ok_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{opener_ok_id}}/watch
HTTP 200

# ── watch_returns_500_when_opener_fails ─────────────────────────────────────
# A per-concert open=fail plan escalates to 500 even though the file exists —
# proving the handler runs the Job Driver's opener outcome and checks it.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Job Chain Opener Fail",
  "downloaded": true,
  "source_file": true
}
HTTP 200
[Captures]
opener_fail_id: jsonpath "$.result.id"

POST {{test_control_url}}/test/job/set_plan
Content-Type: application/json
{
  "concert_id": {{opener_fail_id}},
  "open": "fail"
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

POST {{base_url}}/concerts/{{opener_fail_id}}/watch
HTTP 500
