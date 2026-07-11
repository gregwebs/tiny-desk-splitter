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
