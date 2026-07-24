# `/health` identity endpoint + `local-api-request.sh` guardrail

## Purpose

`d4cbe51` ("broaden local-api helper script") widened
`scripts/local-api-request.sh` from GET-only `/api/...` paths to any absolute
path, any HTTP method, and a JSON body against any loopback port. Its stable
approval prefix `./scripts/local-api-request.sh` is allow-listed to run
without a permission prompt, so that broadening turned it into an allow-listed
HTTP client that could be repurposed to fire ad-hoc requests at *any* loopback
service — not just `concert-web`. Add a `/health` endpoint that self-identifies
as `concert-web`, and have the script confirm that identity before issuing a
caller's real request, confining the allow-listed script to concert-web
instances. See
[`docs/adr/0008-health-endpoint-content-negotiation.md`](../adr/0008-health-endpoint-content-negotiation.md)
for the endpoint's own text/JSON design, and
[`CONTEXT.md`](../../CONTEXT.md#language) for the Service Identity Handshake
term. This is a **misdirection guardrail, not authentication** — any process
on loopback could return the same identifier.

## Implementation plan

- [x] Add `GET /health` to `concert-tracker`, content-negotiated on `Accept`:
  `text/plain` (default) returns a line-oriented body whose first line is
  `concert-web`; `application/json` returns `{"service":"concert-web"}`; an
  unsupported concrete type gets `406`.
- [x] Document only the JSON response in the OpenAPI schema
  (`handlers::HealthIdentity`, tag `meta`) via the existing
  `#[utoipa::path]` + `.routes(routes!(...))` pattern; the text default is
  deliberately left undocumented there.
- [x] Update `local-api-request.sh` to probe `/health` before every real
  request and fail closed (dedicated exit code, distinct from curl's own)
  on a missing/mismatched identity.
- [x] Document the endpoint (ADR), the glossary term (`CONTEXT.md`), and the
  script's guardrail rationale (code comment + `CONTRIBUTING.md`).
- [x] Add black-box `hurl/health.hurl` coverage for the full negotiation
  matrix; minimal in-process Rust tests for the handler; extend the OpenAPI
  path/schema tests.
- [x] Run the full Rust, hurl, and TS suites plus lint/shellcheck; manually
  verify against a live isolated `concert-web` instance, including a
  genuinely wrong loopback service and a closed port.

## State changes

```text
local-api-request.sh invoked
       |
       v
arguments valid? -- no --> usage/error, exit 2
       |
      yes
       |
       v
GET http://127.0.0.1:<port>/health   (Accept: text/plain)
       |
       +-- unreachable / non-2xx / first line != "concert-web"
       |         |
       |         v
       |   error, exit 100 (identity mismatch) -- real request never sent
       |
       `-- first line == "concert-web"
                 |
                 v
       METHOD http://127.0.0.1:<port><path> [BODY_FILE]   (the caller's real request)
                 |
                 +-- HTTP/connection failure --> curl diagnostic, curl's own exit code
                 |
                 `-- success -----------------> response body, exit 0
```

`/health` itself:

```text
GET /health, Accept header read
       |
       v
Accept absent or contains "*/*"? -- yes --> 200 text/plain, line 1 "concert-web"
       |
       no
       |
       v
Accept accepts text/plain or text/*? -- yes --> 200 text/plain, line 1 "concert-web"
       |
       no
       |
       v
Accept accepts application/json or application/*? -- yes --> 200 {"service":"concert-web"}
       |
       no
       |
       v
406 Not Acceptable
```

## Verification plan

1. Against a live isolated `concert-web` (scratch `--db`/`--workdir`, separate
   port): confirm `GET /health` for `Accept: text/plain`, `application/json`,
   absent, `*/*`, and `application/xml` all match the negotiation table above.
2. `./scripts/local-api-request.sh <port> /api/playlists` succeeds against the
   real instance.
3. Point the script at a genuinely different loopback HTTP server (not just a
   closed port) and confirm it fails closed with exit `100` before any real
   request is sent; also confirm a closed port fails the same way.
4. Confirm `/health` and its `HealthIdentity` schema appear in
   `/api-docs/openapi.json` and render in Swagger UI at `/swagger-ui`.
5. Run `cargo nextest run -p concert-tracker --tests`, `just test-hurl`,
   `just test-ts`, `just lint`, `bash -n`/ShellCheck on the script, and the
   release-safety guard (`cargo build --release --bin concert-web --features
   test-control`, expected to fail on the `compile_error!` guard).

## Change record

Implemented `handlers::health`/`HealthIdentity` in `concert-tracker/src/web/`,
wired via `.routes(routes!(handlers::health))` in `api_router()` and
registered in `openapi.rs` (schema + `meta` tag). Extended
`openapi.rs`'s `EXPECTED_PATHS` and `documents_representative_schemas` tests,
and added three minimal handler unit tests (text default, JSON, 406) — full
negotiation-matrix coverage lives in the new `hurl/health.hurl` per this
repo's black-box-HTTP-in-hurl convention (an engineering-lead review flagged
that Accept-header/status-code behavior belongs there, not only in in-process
Rust tests).

Updated `scripts/local-api-request.sh` to probe `/health` (`Accept:
text/plain`) before every real request, comparing the trimmed first response
line against the literal `concert-web`. The probe captures curl's output into
a variable and slices the first line without a pipe (`curl | head -1` risked
a spurious `pipefail` trip if `head` closed the pipe early under `set -euo
pipefail`) — also an engineering-lead review finding. Identity failure exits
`100`, chosen because curl reserves exit `3` ("URL malformed") and the
script's final `exec curl` passes its own exit code straight through, so `3`
would have been ambiguous between "wrong service" and "curl's own error."

Wrote ADR 0008 for the endpoint's text+JSON negotiation design, a `CONTEXT.md`
glossary entry for "Service Identity Handshake" (explicitly distinguished from
authentication), and updated `CONTRIBUTING.md`'s manual-verification section
with the new probe-first behavior and its exit code. `AGENTS.md` was not
changed — it has no existing reference to this script to update, and
CONTRIBUTING.md remains the single canonical place for it.

Verified manually: started an isolated `concert-web` (scratch DB/workdir,
`--port 43199`, via the harness's own background-process mechanism — `nohup`
detachment and `ps`/`pkill` are blocked under this sandbox, a known
constraint unrelated to the change). Exercised all five `/health`
Accept-header cases matching the negotiation table, confirmed
`./scripts/local-api-request.sh 43199 /api/playlists` succeeds, confirmed the
script fails closed with exit `100` against both a real non-concert-web
`python3 -m http.server` (genuinely answers HTTP, just not with the right
identity) and a closed port, and confirmed `/health` / `HealthIdentity` appear
in the served OpenAPI doc and render in Swagger UI.

Automated: `cargo nextest run -p concert-tracker --tests` — 597 passed.
`just test-hurl` — 13 files / 250 requests, all succeeded, including the new
`hurl/health.hurl` (5 requests). `just test-ts` — 256 tests passed (unchanged
by this change; run for completeness per the standard verification step).
`just lint` (fmt, clippy `-D warnings`, shellcheck, ts-check, ts-lint) — clean.
`bash -n` and `shellcheck` on `local-api-request.sh` directly — clean. The
release-safety guard (`cargo build --release --bin concert-web --features
test-control`) failed exactly as designed, on the `test_control.rs`
`compile_error!`.

### Code review and follow-up fix

`/codex:rescue` (this repo's mandated review path per `CLAUDE.md`) failed
twice with a sandbox `EPERM` opening its own job-log file, and
`dangerouslyDisableSandbox` was policy-disabled for the session — an
environment conflict, not something fixable from within the change. Asked the
user how to proceed; they chose to run `/code-review` directly for this
session instead.

**Spec axis**: no findings — negotiation ladder order, exit code 100's
rationale, the pipefail-safe first-line extraction, the exact (not substring)
identity match, and the JSON-only OpenAPI documentation all matched the plan
verbatim.

**Standards axis** surfaced one real, worth-fixing issue: `health`'s original
`Accept` handling computed three overlapping booleans via
`accept.contains("application/json")`-style substring checks. Besides
violating `CODING_STANDARDS.md`'s "use enums and case analysis" guidance,
this was an actual precision bug — `application/jsonlines` or
`application/json-patch+json` would have spuriously matched. Fixed by parsing
`Accept` into exact, case-folded media ranges (splitting on `,`/`;`) and
resolving to a `HealthFormat` enum via `negotiate_health_format`, with a new
regression test (`health_rejects_type_that_merely_contains_a_supported_one`)
pinning the exact-match behavior. Also added `tracing::debug!` on each
negotiated outcome (text/JSON/406), per the repo's existing debug-logging
convention for handler code, which the review noted was absent.

Two other Standards findings were considered and deliberately left as-is: (1)
the `SERVICE_IDENTITY` string's duplication across Rust and the shell
script — this is the cross-language duplication already named and accepted
during design (see the ADR and both files' comments); no shared symbol can
cross that boundary without disproportionate machinery for one literal. (2)
The test helper `health_body`'s `(StatusCode, String, String)` return
tuple — a private, 3-call-site test helper already read via named
destructuring at each site; promoting it to a struct was judged not worth the
indirection per `CODING_STANDARDS.md`'s "favor readability over DRY" and
"wait for three concrete instances" guidance.

Re-ran the full verification suite after the fix: `cargo nextest run -p
concert-tracker --tests` (598 passed, +1 for the new regression test),
`cargo clippy --all-targets -- -D warnings` (clean), `just test-hurl` (13
files / 250 requests, all succeeded), and `just lint` (clean).
