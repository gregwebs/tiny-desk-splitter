# Split timestamp API cases that need only DB lifecycle/timestamp state.

GET {{base_url}}/concerts/999999/split-timestamps
HTTP 404

POST {{base_url}}/concerts/999999/split-timestamps
Content-Type: application/json
{"songs":[]}
HTTP 404

POST {{base_url}}/concerts/999999/split-timestamps/reset
HTTP 404

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "source_url": "https://npr.org/c/split-ts-null-hurl",
    "title": "Split TS Null Hurl",
    "concert_date": "2024-07-01",
    "artist": "Timestamp Artist",
    "album": "Null Album",
    "set_list": ["Song A", "Song B"]
  }
}
HTTP 200
[Captures]
split_ts_null_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{split_ts_null_id}}/split-timestamps
HTTP 200
[Asserts]
jsonpath "$.auto" == null
jsonpath "$.user" == null
jsonpath "$.set_list[0]" == "Song A"
jsonpath "$.set_list[1]" == "Song B"

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "source_url": "https://npr.org/c/split-ts-auto-hurl",
    "title": "Split TS Auto Hurl",
    "concert_date": "2024-07-02",
    "artist": "Timestamp Artist",
    "album": "Auto Album",
    "set_list": ["Track One", "Track Two"],
    "auto_timestamps": [
      {"title": "Track One", "start_time": 0.0, "end_time": 55.0, "duration": 55.0},
      {"title": "Track Two", "start_time": 60.0, "end_time": 115.0, "duration": 55.0}
    ]
  }
}
HTTP 200
[Captures]
split_ts_auto_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{split_ts_auto_id}}/split-timestamps
HTTP 200
[Asserts]
jsonpath "$.auto[0].title" == "Track One"
jsonpath "$.auto[1].title" == "Track Two"
jsonpath "$.user" == null

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "source_url": "https://npr.org/c/split-ts-both-hurl",
    "title": "Split TS Both Hurl",
    "concert_date": "2024-07-03",
    "artist": "Timestamp Artist",
    "album": "Both Album",
    "set_list": ["Alpha", "Beta"],
    "auto_timestamps": [
      {"title": "Alpha", "start_time": 0.0, "end_time": 55.0, "duration": 55.0},
      {"title": "Beta", "start_time": 60.0, "end_time": 115.0, "duration": 55.0}
    ],
    "user_timestamps": [
      {"title": "Alpha", "start_time": 2.0, "end_time": 57.0, "duration": 55.0},
      {"title": "Beta", "start_time": 62.0, "end_time": 117.0, "duration": 55.0}
    ]
  }
}
HTTP 200
[Captures]
split_ts_both_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{split_ts_both_id}}/split-timestamps
HTTP 200
[Asserts]
jsonpath "$.auto[0].start_time" == 0.0
jsonpath "$.auto[1].title" == "Beta"
jsonpath "$.user[0].start_time" == 2.0
jsonpath "$.user[1].title" == "Beta"

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "source_url": "https://npr.org/c/split-ts-duration-hurl",
    "title": "Split TS Duration Hurl",
    "concert_date": "2024-07-04",
    "artist": "Timestamp Artist",
    "album": "Stored Duration Album",
    "set_list": ["Song A", "Song B"],
    "media_duration": 321.5
  }
}
HTTP 200
[Captures]
split_ts_duration_id: jsonpath "$.result.id"

GET {{base_url}}/concerts/{{split_ts_duration_id}}/split-timestamps
HTTP 200
[Asserts]
jsonpath "$.media_duration" == 321.5

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "source_url": "https://npr.org/c/split-ts-source-missing-hurl",
    "title": "Split TS Source Missing Hurl",
    "concert_date": "2024-07-05",
    "artist": "Timestamp Artist",
    "album": "No Source Album",
    "set_list": ["A", "B"]
  }
}
HTTP 200
[Captures]
split_ts_source_missing_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{split_ts_source_missing_id}}/split-timestamps
Content-Type: application/json
{
  "songs": [
    {"title": "A", "start_time": 0.0, "end_time": 55.0},
    {"title": "B", "start_time": 60.0, "end_time": 115.0}
  ]
}
HTTP 409
[Asserts]
body contains "Source file not found"

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 6,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "source_url": "https://npr.org/c/split-ts-reset-no-auto-hurl",
    "title": "Split TS Reset No Auto Hurl",
    "concert_date": "2024-07-06",
    "artist": "Timestamp Artist",
    "album": "No Auto Album",
    "set_list": ["A", "B"]
  }
}
HTTP 200
[Captures]
split_ts_reset_no_auto_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{split_ts_reset_no_auto_id}}/split-timestamps/reset
HTTP 422
[Asserts]
body contains "No automated split timestamps available"

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 7,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "source_url": "https://npr.org/c/split-ts-reset-auto-only-hurl",
    "title": "Split TS Reset Auto Only Hurl",
    "concert_date": "2024-07-07",
    "artist": "Timestamp Artist",
    "album": "Already Auto Album",
    "set_list": ["A", "B"],
    "auto_timestamps": [
      {"title": "A", "start_time": 0.0, "end_time": 55.0, "duration": 55.0},
      {"title": "B", "start_time": 60.0, "end_time": 115.0, "duration": 55.0}
    ]
  }
}
HTTP 200
[Captures]
split_ts_reset_auto_only_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{split_ts_reset_auto_only_id}}/split-timestamps/reset
HTTP 200
[Asserts]
jsonpath "$.status" == "already-auto"

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 8,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "source_url": "https://npr.org/c/delete-split-preserve-ts-hurl",
    "title": "Delete Split Preserve TS Hurl",
    "concert_date": "2024-07-08",
    "artist": "Timestamp Artist",
    "album": "Preserve TS Album",
    "set_list": ["Keep A", "Keep B"],
    "downloaded": true,
    "split": true,
    "auto_timestamps": [
      {"title": "Keep A", "start_time": 0.0, "end_time": 55.0, "duration": 55.0},
      {"title": "Keep B", "start_time": 60.0, "end_time": 115.0, "duration": 55.0}
    ],
    "user_timestamps": [
      {"title": "Keep A", "start_time": 0.0, "end_time": 55.0, "duration": 55.0},
      {"title": "Keep B", "start_time": 60.0, "end_time": 115.0, "duration": 55.0}
    ]
  }
}
HTTP 200
[Captures]
delete_split_preserve_ts_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{delete_split_preserve_ts_id}}/delete-split
HTTP 200

GET {{base_url}}/concerts/{{delete_split_preserve_ts_id}}/split-timestamps
HTTP 200
[Asserts]
jsonpath "$.auto[0].title" == "Keep A"
jsonpath "$.user[0].title" == "Keep A"

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 9,
  "method": "test.seed_listing",
  "params": {
    "source_url": "https://npr.org/c/delete-split-not-split-hurl",
    "title": "Delete Split Not Split Hurl",
    "concert_date": "2024-07-09"
  }
}
HTTP 200
[Captures]
delete_split_not_split_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{delete_split_not_split_id}}/delete-split
HTTP 400

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 10,
  "method": "test.seed_scraped_concert",
  "params": {
    "source_url": "https://npr.org/c/like-out-of-range-hurl",
    "title": "Like Out Of Range Hurl",
    "concert_date": "2024-07-10",
    "artist": "Timestamp Artist",
    "album": "Like Out Of Range Album",
    "set_list": ["Song A"]
  }
}
HTTP 200
[Captures]
like_out_of_range_id: jsonpath "$.result.id"

POST {{base_url}}/concerts/{{like_out_of_range_id}}/tracks/5/like
HTTP 404
