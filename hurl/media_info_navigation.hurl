# Media-info track navigation cases backed by dummy track files. The files are
# not valid audio; these routes only need path existence and extension.

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Hurl Next Track Album",
  "set_list": ["Song A", "Song B", "Song C"],
  "track_files": [0, 1, 2]
}
HTTP 200
[Captures]
next_track_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{next_track_id}}/tracks/0/next-media-info
HTTP 200
[Asserts]
jsonpath "$.title" == "Song B"
jsonpath "$.track_index" == 1
jsonpath "$.playable" == true

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Hurl Skip Album",
  "set_list": ["Song A", "Song B", "Song C"],
  "track_files": [0, 2]
}
HTTP 200
[Captures]
next_skip_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{next_skip_id}}/tracks/0/next-media-info
HTTP 200
[Asserts]
jsonpath "$.title" == "Song C"
jsonpath "$.track_index" == 2

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Hurl Last Track Album",
  "set_list": ["Song A", "Song B"],
  "track_files": [0, 1]
}
HTTP 200
[Captures]
next_last_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{next_last_id}}/tracks/1/next-media-info
HTTP 404

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Hurl Prev Track Album",
  "set_list": ["Song A", "Song B", "Song C"],
  "track_files": [0, 1, 2]
}
HTTP 200
[Captures]
prev_track_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{prev_track_id}}/tracks/2/prev-media-info
HTTP 200
[Asserts]
jsonpath "$.title" == "Song B"
jsonpath "$.track_index" == 1
jsonpath "$.playable" == true
jsonpath "$.has_prev" == true
jsonpath "$.has_next" == true

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Hurl Prev Skip Album",
  "set_list": ["Song A", "Song B", "Song C"],
  "track_files": [0, 2]
}
HTTP 200
[Captures]
prev_skip_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{prev_skip_id}}/tracks/2/prev-media-info
HTTP 200
[Asserts]
jsonpath "$.title" == "Song A"
jsonpath "$.track_index" == 0
jsonpath "$.has_prev" == false

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Hurl First Track Album",
  "set_list": ["Song A", "Song B"],
  "track_files": [0, 1]
}
HTTP 200
[Captures]
prev_first_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{prev_first_id}}/tracks/0/prev-media-info
HTTP 404

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Hurl Has Prev Album",
  "set_list": ["Song A", "Song B"],
  "track_files": [0, 1]
}
HTTP 200
[Captures]
has_prev_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{has_prev_id}}/tracks/0/media-info
HTTP 200
[Asserts]
jsonpath "$.has_prev" == false

GET {{base_url}}/concerts/{{has_prev_id}}/tracks/1/media-info
HTTP 200
[Asserts]
jsonpath "$.has_prev" == true

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Hurl Liked Album",
  "set_list": ["Song A", "Song B"],
  "track_files": [0, 1],
  "tracks_liked": [false, true]
}
HTTP 200
[Captures]
liked_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{liked_id}}/tracks/1/media-info
HTTP 200
[Asserts]
jsonpath "$.liked" == true

GET {{base_url}}/concerts/{{liked_id}}/tracks/0/media-info
HTTP 200
[Asserts]
jsonpath "$.liked" == false

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Hurl No Likes Album",
  "set_list": ["Song A", "Song B"],
  "track_files": [0, 1]
}
HTTP 200
[Captures]
no_likes_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{no_likes_id}}/tracks/1/media-info
HTTP 200
[Asserts]
jsonpath "$.liked" == false

POST {{test_control_url}}/test/seed/media_concert
Content-Type: application/json
{
  "album": "Hurl Next Liked Album",
  "set_list": ["Song A", "Song B", "Song C"],
  "track_files": [0, 1, 2],
  "tracks_liked": [false, true, false]
}
HTTP 200
[Captures]
next_liked_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{next_liked_id}}/tracks/0/next-media-info
HTTP 200
[Asserts]
jsonpath "$.track_index" == 1
jsonpath "$.liked" == true
