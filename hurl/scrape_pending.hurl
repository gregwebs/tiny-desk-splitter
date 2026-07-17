# Scrape Driver-backed pending-card case — slice 4 of the remaining web
# integration Hurl migration described in
# docs/change/2026-07-17-scrape-driver-hurl-migration.md (issue #110, parent
# #106). Migrates `pending_card_shows_loading_then_thumbnail` out of
# concert-tracker/tests/web_integration.rs; see that file for the breadcrumb.
#
# The Scrape Driver has no default plan to restore between cases: a concert
# without a per-concert `test.scrape_set_plan` override always scrape-succeeds
# deterministically (see hurl/README.md's Scrape Driver section), so this file
# does not need the "never change the default" discipline `hurl/job_chain.hurl`
# documents for the Job Driver.

# ── pending_card_shows_loading_then_thumbnail ───────────────────────────────
# A queued (not-yet-scraped) concert's card shows the "loading…" placeholder
# and polls; once the Scrape Driver's release writes deterministic metadata +
# thumbnail, the card shows the thumbnail and stops polling.

POST {{test_control_url}}/test/seed/listing
Content-Type: application/json
{
  "title": "Pending Card Concert"
}
HTTP 200
[Captures]
pending_id: jsonpath "$.result.id"

POST {{test_control_url}}/test/scrape/set_plan
Content-Type: application/json
{
  "concert_id": {{pending_id}},
  "scrape": "block"
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

POST {{test_control_url}}/test/scrape/enqueue
Content-Type: application/json
{
  "concert_id": {{pending_id}}
}
HTTP 200
[Asserts]
jsonpath "$.result.enqueued" == true

# Enqueue marks the queue's `pending` set synchronously (before the worker
# ever picks the item up), so no retry is needed here: the card already polls
# and shows the loading placeholder, with no thumbnail yet.
GET {{base_url}}/concerts/{{pending_id}}/status
HTTP 200
[Asserts]
body contains "hx-trigger=\"every 3s\""
body contains "card-thumb-loading"
body not contains "/thumbnails/"

# Release protocol: poll for blocked=1 before releasing — the worker may not
# have picked the item up yet (same idiom as the Job Driver's blocked-step
# release protocol; see hurl/README.md).
POST {{test_control_url}}/test/assert/scrape_observation
Content-Type: application/json
[Options]
retry: 30
retry-interval: 50ms
{
  "concert_id": {{pending_id}},
  "blocked": 1
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

POST {{test_control_url}}/test/scrape/release
Content-Type: application/json
{
  "concert_id": {{pending_id}}
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

# After release: retry until the worker clears `pending` (the thumbnail write
# can land one poll before the queue actually clears), then the card shows
# the deterministic thumbnail URL and has stopped polling — both on the same
# response.
GET {{base_url}}/concerts/{{pending_id}}/status
[Options]
retry: 30
retry-interval: 50ms
HTTP 200
[Asserts]
body contains "/thumbnails/Scrape Driver Album {{pending_id}}.jpg"
body not contains "hx-trigger=\"every 3s\""
body not contains "card-thumb-loading"

# The thumbnail file itself was created and is a real, decodable JPEG — not
# just a servable file (see the Scrape Driver's module docs on why a text
# sentinel would be wrong here: the card's `onerror` handler hides a broken
# image, so a `200` alone would not prove anything visible).
GET {{base_url}}/thumbnails/Scrape%20Driver%20Album%20{{pending_id}}.jpg
HTTP 200
[Asserts]
bytes startsWith hex,ffd8ff;

# Deterministic completion is observable end to end: exactly one scrape ran,
# was blocked once, and was released once.
POST {{test_control_url}}/test/assert/scrape_observation
Content-Type: application/json
{
  "concert_id": {{pending_id}},
  "started": 1,
  "completed": 1,
  "blocked": 1,
  "released": 1
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true
