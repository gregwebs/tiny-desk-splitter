# Detail, notes, and prepare endpoint state-only cases.

POST {{test_control_url}}/test/seed/scraped_concert
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

# Notes and Concert Status are independent desired state: ignoring a concert
# keeps the notes it already had, and the card the notes endpoint swaps in still
# shows the Ignored badge and its "clear ignored" action.
POST {{base_url}}/concerts/{{notes_id}}/ignore
HTTP 200
[Asserts]
body contains "badge-ignored"

GET {{base_url}}/concerts/{{notes_id}}
HTTP 200
[Asserts]
body contains "badge-ignored"
body contains "great show"

POST {{base_url}}/concerts/{{notes_id}}/notes
[FormParams]
notes: rewatched, still great
HTTP 200
[Asserts]
body contains "badge-ignored"
body contains "title=\"Clear ignored\""

GET {{base_url}}/concerts/{{notes_id}}
HTTP 200
[Asserts]
body contains "badge-ignored"
body contains "rewatched, still great"
body not contains "great show"

# A concert that does not exist is an expected rejection, so the status and
# notes endpoints answer 404 rather than reporting a server failure.
POST {{base_url}}/concerts/999999/notes
[FormParams]
notes: nowhere to store this
HTTP 404

POST {{base_url}}/concerts/999999/ignore
HTTP 404

POST {{base_url}}/concerts/999999/want
HTTP 404

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
