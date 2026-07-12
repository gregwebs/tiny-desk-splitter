# Detail, notes, and prepare endpoint state-only cases.

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "test.seed_listing",
  "params": {
    "source_url": "https://npr.org/c/notes-hurl",
    "title": "Notes Hurl Concert",
    "concert_date": "2024-05-01"
  }
}
HTTP 200
[Captures]
notes_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{notes_id}}/notes
[FormParams]
notes: great show
HTTP 200

GET {{base_url}}/concerts/{{notes_id}}
HTTP 200
[Asserts]
body contains "great show"

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "test.seed_scraped_concert",
  "params": {
    "source_url": "https://npr.org/c/detail-set-list-hurl",
    "title": "Detail Set List Hurl",
    "concert_date": "2024-05-02",
    "artist": "Detail Artist",
    "album": "Detail Album",
    "set_list": ["Song One", "Song Two"]
  }
}
HTTP 200
[Captures]
detail_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{detail_id}}
HTTP 200
[Asserts]
body contains "Song One"
body contains "Song Two"
body contains "Detail Artist"

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "test.seed_scraped_concert",
  "params": {
    "source_url": "https://npr.org/c/prepare-empty-set-list-hurl",
    "title": "Prepare Empty Set List Hurl",
    "concert_date": "2024-05-03",
    "artist": "Prepare Artist",
    "album": "Empty Album",
    "set_list": []
  }
}
HTTP 200
[Captures]
prepare_empty_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{prepare_empty_id}}/prepare
HTTP 422

POST {{base_url}}/concerts/999999/prepare
HTTP 404
