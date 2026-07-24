# `/health` self-identification endpoint (issue: local-api-request.sh
# misdirection guardrail). No Test Control seeding needed — the route has no
# `AppState` dependency and is always available. See
# `concert-tracker/src/web/handlers.rs`'s `health` doc comment for the
# negotiation rules this exercises, and
# `docs/adr/0008-health-endpoint-content-negotiation.md` for why both a
# text and a JSON form exist.

# ── Explicit text/plain ──────────────────────────────────────────────────────

GET {{base_url}}/health
Accept: text/plain
HTTP 200
[Asserts]
header "Content-Type" == "text/plain; charset=utf-8"
body matches "^concert-web\n$"

# ── Explicit application/json ────────────────────────────────────────────────

GET {{base_url}}/health
Accept: application/json
HTTP 200
[Asserts]
header "Content-Type" == "application/json"
jsonpath "$.service" == "concert-web"

# ── Absent Accept header defaults to text ───────────────────────────────────

GET {{base_url}}/health
HTTP 200
[Asserts]
header "Content-Type" == "text/plain; charset=utf-8"
body matches "^concert-web\n$"

# ── Accept: */* also defaults to text (text wins wildcards) ────────────────

GET {{base_url}}/health
Accept: */*
HTTP 200
[Asserts]
header "Content-Type" == "text/plain; charset=utf-8"
body matches "^concert-web\n$"

# ── Unsupported concrete type is rejected, not silently downgraded ─────────

GET {{base_url}}/health
Accept: application/xml
HTTP 406
