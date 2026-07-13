# Detail, notes, and prepare endpoint state-only cases.

POST {{test_control_url}}/test/seed/listing
Content-Type: application/json
{}
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

POST {{test_control_url}}/test/seed/scraped_concert
Content-Type: application/json
{
  "artist": "Detail Artist",
  "set_list": ["Song One", "Song Two"]
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

POST {{test_control_url}}/test/seed/scraped_concert
Content-Type: application/json
{
  "set_list": []
}
HTTP 200
[Captures]
prepare_empty_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{prepare_empty_id}}/prepare
HTTP 422

POST {{base_url}}/concerts/999999/prepare
HTTP 404
