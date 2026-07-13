# Test Control HTTP Adapter — first Hurl slice for the adapter described in
# docs/change/2026-07-13-test-control-http-adapter-spec.md and
# docs/adr/0004-test-control-http-adapter.md. Covers the adapter's success
# path (seed + assert through concise `/test/...` routes), that raw JSON-RPC
# still works unchanged at the listener root, and the adapter's invalid-JSON
# HTTP 400 parse-error shape.
#
# Run via `just test-hurl` (scripts/hurl-test.js starts a scratch concert-web
# with the Test Control API enabled and supplies {{base_url}} /
# {{test_control_url}}). Like every other file here, this one shares one
# concert-web process/DB with the rest of the suite for the whole
# `just test-hurl` run (see hurl/README.md) — it deliberately never calls the
# adapter's `/test/reset` route, since resetting mid-run would wipe fixtures
# another file still depends on.

# Seed a listing through the adapter's concise route instead of a raw
# JSON-RPC envelope, and capture its id from the Seed Result.
POST {{test_control_url}}/test/seed/listing
Content-Type: application/json
{
  "source_url": "https://npr.org/c/test-control-adapter-a",
  "title": "Test Control Adapter Smoke",
  "concert_date": "2024-02-01"
}
HTTP 200
[Captures]
adapter_concert_id: jsonpath "$.result.id"
[Asserts]
jsonpath "$.jsonrpc" == "2.0"
jsonpath "$.id" == "default"
jsonpath "$.result.title" == "Test Control Adapter Smoke"
jsonpath "$.result.source_url" == "https://npr.org/c/test-control-adapter-a"

# The seeded listing must appear on the real public list page — the adapter
# only changes the Test Control request shape, never product routes.
GET {{base_url}}/
HTTP 200
[Asserts]
body contains "Test Control Adapter Smoke"

# The Assertion API is reachable through its adapter route too.
POST {{test_control_url}}/test/assert/concert_state
Content-Type: application/json
{
  "id": {{adapter_concert_id}},
  "downloaded": false
}
HTTP 200
[Asserts]
jsonpath "$.result.ok" == true

# Raw JSON-RPC remains available at the listener root, unchanged, for
# debugging adapter issues — spec: "Raw JSON-RPC remains available at the
# root test-control endpoint for debugging adapter issues and validating the
# underlying JSON-RPC layer." Seed methods are generated request-object RPC
# methods, so raw JSON-RPC wraps the fixture body under params.params; the
# adapter route above is the Hurl-facing flat shape.
POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "test.seed_listing",
  "params": {
    "params": {
      "source_url": "https://npr.org/c/test-control-adapter-raw",
      "title": "Test Control Adapter Raw Compat"
    }
  }
}
HTTP 200
[Asserts]
jsonpath "$.result.title" == "Test Control Adapter Raw Compat"

# Invalid JSON on an adapter route is a JSON-RPC parse error at HTTP 400 with
# id: null — not the raw-JSON-RPC 200 error envelope.
POST {{test_control_url}}/test/seed/listing
Content-Type: application/json
`{not valid json`
HTTP 400
[Asserts]
jsonpath "$.error.code" == -32700
jsonpath "$.id" == null
