# State-only media error cases. The lifecycle seed never writes files, so the
# scratch workdir naturally represents missing source media.

POST {{test_control_url}}/test/seed/lifecycle_concert
Content-Type: application/json
{
  "downloaded": true
}
HTTP 200
[Captures]
delete_download_missing_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{delete_download_missing_id}}/delete-download
HTTP 200
[Asserts]
header "HX-Refresh" not exists
body contains "delete-confirm"
body contains "/concerts/{{delete_download_missing_id}}/delete-download?force=true"

POST {{test_control_url}}/test/seed/lifecycle_concert
Content-Type: application/json
{
  "downloaded": true
}
HTTP 200
[Captures]
watch_missing_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{watch_missing_id}}/watch
HTTP 500

GET {{base_url}}/concerts/{{watch_missing_id}}/media-info
HTTP 500

POST {{test_control_url}}/test/seed/scraped_concert
Content-Type: application/json
{}
HTTP 200
[Captures]
watch_not_downloaded_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{watch_not_downloaded_id}}/watch
HTTP 404

POST {{test_control_url}}/test/seed/lifecycle_concert
Content-Type: application/json
{
  "split": true
}
HTTP 200
[Captures]
concert_playback_empty_id: jsonpath "$.result.id"

# No source or track files exist in the scratch workdir, and the state-only
# seed deliberately leaves tracks_present unset. The production playback path
# treats absent track-presence data as unavailable for each set-list slot, so
# this is the public-HTTP equivalent of the old split/no-playable-tracks case.
GET {{base_url}}/concerts/{{concert_playback_empty_id}}/concert-playback
HTTP 404
