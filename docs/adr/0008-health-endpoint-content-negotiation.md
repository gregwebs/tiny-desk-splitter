# `/health` self-identification via strict Accept negotiation, text and JSON both served

Status: Accepted

`GET /health` lets a caller confirm it is actually talking to `concert-web`
before doing anything else with the connection. Its first (and currently
only) consumer is `scripts/local-api-request.sh`'s identity guardrail — see
that script's header comment and
`concert-tracker/src/web/handlers.rs`'s `SERVICE_IDENTITY`/`health` doc
comment for that side of the design. This ADR is about the endpoint's own
shape: why it serves two response formats under real `Accept` negotiation
instead of one fixed format.

## Decision

`/health` negotiates on `Accept`:

- **`text/plain`** — a line-oriented body whose first line is the identity
  token alone (`concert-web`). Lines 2+ are reserved for a future version and
  health-detail, not implemented yet.
- **`application/json`** — `{"service": "concert-web"}`.
- An `Accept` that is absent or contains `*/*` defaults to **text/plain** (no
  q-value parsing — text simply wins ties and wildcards). A concrete type
  this endpoint does not serve (e.g. `application/xml`) gets **406 Not
  Acceptable** rather than a silent downgrade to whichever format the server
  felt like sending.

Both formats exist because the endpoint has two real audiences with different
needs: a human or shell script running `curl`/`local-api-request.sh` wants a
bare, greppable line with no JSON parsing required; a machine client (or a
future Swagger "Try it out" call) wants a typed, schema-checkable object. One
audience should not have to pay the other's parsing tax, and negotiation
already exists in HTTP for exactly this.

Only the JSON response is documented in the OpenAPI schema
(`handlers::HealthIdentity`, tag `meta`) — deliberately. The text form is not
modeled as an OpenAPI response even though it's the *default* response for an
absent/`*/*` Accept header. Machine clients that actually need to parse the
body are expected to send `Accept: application/json` and get the documented
shape; the text form stays outside the schema because it's for
humans/scripts, and modeling a line-oriented body with reserved-but-unused
future lines in OpenAPI would document a contract the endpoint doesn't
actually commit to yet.

## Alternatives considered

- **JSON only.** Simpler (one format, fully documented), but forces
  `scripts/local-api-request.sh` — a `set -euo pipefail` Bash script with no
  `jq` dependency today — to either add a JSON-parsing dependency or grep the
  raw body, which is more fragile than an exact string match on line 1.
- **Text only.** Keeps the shell script simple but gives a future
  machine/Swagger client an unstructured body with no schema, and no clean
  place to add versioned/structured health detail later without a breaking
  change.
- **Substring match on the whole body instead of first-line-exact.** Rejected
  independently of format choice: a substring match would let an unrelated
  service that merely *mentions* "concert-web" somewhere in its response pass
  the identity check, weakening the one guarantee this endpoint exists to
  provide.

## Consequences

- Adding version/health-detail lines to the text body later is additive and
  doesn't change the identity guardrail's contract (still "line 1 equals
  `concert-web`"), but every such addition is invisible to the OpenAPI doc by
  design — this endpoint's `text/plain` response will never appear there.
- `/health`'s identity string is a **self-declared identity handshake, not
  authentication** — any process on loopback could return `concert-web`. This
  endpoint only makes the *legitimate* case (a real concert-web instance)
  identifiable; it does not defend against a hostile impersonator. See the
  guardrail's own documentation for that threat-model boundary.
- No q-value/media-range parsing means a client that actually prefers JSON
  but sends `Accept: application/json, */*` gets text (the wildcard wins).
  Acceptable here since the only current consumer sends a bare
  `Accept: text/plain` and any future JSON client can do the same by omitting
  the wildcard.
