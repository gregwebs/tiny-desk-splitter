# Listing and status basics — first Hurl slice for the black-box migration
# described in docs/change/2026-07-11-hurl-web-integration-tests.md.
#
# Run via `just test-hurl` (scripts/hurl-test.js starts a scratch concert-web
# with the Test Control API enabled and supplies {{base_url}} /
# {{test_control_url}}).

# Seed a plain listing through the Test Control API and capture its id from
# the Seed Result instead of assuming id = 1.
POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "test.seed_listing",
  "params": {
    "source_url": "https://npr.org/c/listing-status-a",
    "title": "Listing Status A",
    "concert_date": "2024-01-15"
  }
}
HTTP 200
[Captures]
concert_a_id: jsonpath "$.result.id"
[Asserts]
jsonpath "$.result.title" == "Listing Status A"
jsonpath "$.result.source_url" == "https://npr.org/c/listing-status-a"
jsonpath "$.result.concert_date" == "2024-01-15"

# The seeded listing must appear on the real public list page.
GET {{base_url}}/
HTTP 200
[Asserts]
body contains "Listing Status A"

# Seed a concert with scraped metadata (no download yet) and capture its id
# from the Seed Result.
POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "test.seed_scraped_concert",
  "params": {
    "source_url": "https://npr.org/c/listing-status-scraped",
    "title": "Listing Status Scraped",
    "concert_date": "2024-02-01",
    "artist": "Listing Status Artist",
    "album": "Listing Status Album"
  }
}
HTTP 200
[Captures]
scraped_id: jsonpath "$.result.id"
[Asserts]
jsonpath "$.result.album" == "Listing Status Album"

# The public status fragment for a scraped-but-not-downloaded concert must
# show the Download action and must not show the old "not-downloaded" badge
# it replaced.
GET {{base_url}}/concerts/{{scraped_id}}/status
HTTP 200
[Asserts]
body contains "/concerts/{{scraped_id}}/download\""
body not contains "badge-not-downloaded"

# Seed two more listings, capturing both ids from their Seed Results.
POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "test.seed_listing",
  "params": {
    "source_url": "https://npr.org/c/listing-filter-a",
    "title": "Listing Filter A",
    "concert_date": "2024-03-01"
  }
}
HTTP 200
[Captures]
filter_a_id: jsonpath "$.result.id"

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "test.seed_listing",
  "params": {
    "source_url": "https://npr.org/c/listing-filter-b",
    "title": "Listing Filter B",
    "concert_date": "2024-03-02"
  }
}
HTTP 200
[Captures]
filter_b_id: jsonpath "$.result.id"

# Ignore the first through the real public app endpoint (not a Test Control
# method — ignoring is product behavior, not fixture setup) and verify the
# ignored badge/action markup it returns.
POST {{base_url}}/concerts/{{filter_a_id}}/ignore
HTTP 200
[Asserts]
body contains "badge-ignored"
body contains "title=\"Clear ignored\""

# The ignored filter must include the ignored concert and exclude the other
# seeded-but-not-ignored one.
GET {{base_url}}/?filter=ignored
HTTP 200
[Asserts]
body contains "Listing Filter A"
body not contains "Listing Filter B"
