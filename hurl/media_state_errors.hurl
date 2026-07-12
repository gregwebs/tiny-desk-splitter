# State-only media error cases. The lifecycle seed never writes files, so the
# scratch workdir naturally represents missing source media.

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "source_url": "https://npr.org/c/delete-download-missing-hurl",
    "title": "Delete Download Missing Hurl",
    "concert_date": "2024-06-01",
    "artist": "Media Artist",
    "album": "Delete Download Missing Album",
    "downloaded": true
  }
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

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "source_url": "https://npr.org/c/watch-missing-hurl",
    "title": "Watch Missing Hurl",
    "concert_date": "2024-06-02",
    "artist": "Media Artist",
    "album": "Watch Missing Album",
    "downloaded": true
  }
}
HTTP 200
[Captures]
watch_missing_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{watch_missing_id}}/watch
HTTP 500

GET {{base_url}}/concerts/{{watch_missing_id}}/media-info
HTTP 500

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "test.seed_scraped_concert",
  "params": {
    "source_url": "https://npr.org/c/watch-not-downloaded-hurl",
    "title": "Watch Not Downloaded Hurl",
    "concert_date": "2024-06-03",
    "artist": "Media Artist",
    "album": "Watch Not Downloaded Album"
  }
}
HTTP 200
[Captures]
watch_not_downloaded_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{watch_not_downloaded_id}}/watch
HTTP 404

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "source_url": "https://npr.org/c/concert-playback-empty-hurl",
    "title": "Concert Playback Empty Hurl",
    "concert_date": "2024-06-04",
    "artist": "Media Artist",
    "album": "Concert Playback Empty Album",
    "set_list": ["Song A"],
    "downloaded": true,
    "split": true
  }
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
