# Test Control HTTP Adapter Spec

## Problem Statement

Hurl tests currently call the Test Control API by sending full JSON-RPC
requests to `{{test_control_url}}`. Even after seed defaults, each setup step
still has to spell out `jsonrpc`, `id`, `method`, and `params`. That noise makes
the `.hurl` files harder to scan and keeps JSON-RPC protocol details in tests
whose real purpose is arranging fixture state before exercising product HTTP
routes.

The server-side Test Control implementation also has a competing pressure. The
maintainable long-term shape is for real Test Control methods to be generated
by jsonrpsee's `#[rpc]` macro, but single request-object methods with
`param_kind = map` use a nested raw JSON-RPC shape such as
`params: { "params": { ... } }`. Hurl should not have to know that shape.

## Solution

Add a Test Control HTTP Adapter on the same test-control listener as the
existing raw JSON-RPC endpoint. Hurl authors use concise adapter routes:

```hurl
POST {{test_control_url}}/test/seed/listing
Content-Type: application/json
{
  "title": "Example"
}
```

The adapter translates each request into a single in-process JSON-RPC call using
`"id": "default"` and returns the JSON-RPC response envelope unchanged:

```json
{
  "jsonrpc": "2.0",
  "result": {
    "id": 1,
    "source_url": "https://example.test/tiny-desk/test-control-1",
    "title": "Example",
    "concert_date": "2026-01-01"
  },
  "id": "default"
}
```

Raw JSON-RPC remains available at the root test-control endpoint for debugging
adapter issues and validating the underlying JSON-RPC layer. The adapter is the
preferred Hurl authoring interface.

This is test-control-only. It must not change product routes, public JSON APIs,
OpenAPI output, database schema, or Hurl runner process isolation.

## Route Contract

All adapter routes are `POST` only. They use the existing
`{{test_control_url}}`; no separate adapter variable is introduced.

| Adapter route | JSON-RPC method |
|---|---|
| `/test/reset` | `test.reset` |
| `/test/seed/{name}` | `test.seed_{name}` |
| `/test/assert/{name}` | `test.assert_{name}` |

The `{name}` path segment is forwarded directly. No extra token validation is
required beyond using one path segment: `/test/seed/foo` maps to
`test.seed_foo`, while `/test/seed/foo/bar` does not match the route and should
return HTTP 404.

Unknown adapter paths return ordinary HTTP 404, not JSON-RPC
`method not found`. Paths that match the adapter pattern but name an unknown
JSON-RPC method should be translated and let JSON-RPC return its normal method
error.

The adapter does not support JSON-RPC batching. If batching is ever needed,
revisit it as a separate design.

## Request Translation

The adapter reads the raw HTTP body. `Content-Type` is not enforced; missing
content type is assumed to be JSON.

| HTTP body | Adapter behavior |
|---|---|
| empty body | use JSON `{}` |
| valid JSON value | preserve that value |
| invalid JSON | return HTTP 400 with a JSON-RPC parse-error response |

Literal JSON `null` is preserved as `null`; only an actually empty body becomes
`{}`.

For slice 1, the adapter targets the current manually registered flat seed
methods:

```text
POST /test/seed/listing
{"title":"Example"}

=> method: test.seed_listing
=> params: {"title":"Example"}
```

For slice 3, after seed methods move back to generated request-object
`#[rpc]` methods, only seed routes gain the nested wrapper needed by
jsonrpsee:

```text
POST /test/seed/listing
{"title":"Example"}

=> method: test.seed_listing
=> params: {"params":{"title":"Example"}}
```

Assertion routes do not use that seed wrapper. Keep
`test.assert_concert_state` in its current multi-argument generated shape:

```text
POST /test/assert/concert_state
{"id":1,"downloaded":true}

=> method: test.assert_concert_state
=> params: {"id":1,"downloaded":true}
```

`/test/reset` has no request object. It should translate empty body or `{}` to
a no-argument-compatible JSON-RPC request using whichever representation is
least awkward for jsonrpsee.

All adapter-generated requests use JSON-RPC id `"default"`.

## Response And Error Contract

The adapter rewrites requests only. It returns the JSON-RPC response envelope
unchanged for successful calls and method-level errors.

| Condition | HTTP status | Body |
|---|---:|---|
| translated call succeeds | 200 | JSON-RPC success response |
| translated call reaches JSON-RPC and fails parsing/execution | 200 | JSON-RPC error response |
| adapter request body is invalid JSON | 400 | JSON-RPC parse error with `id: null` |
| route does not match adapter or raw JSON-RPC endpoint | 404 | ordinary HTTP 404 |

Adapter JSON responses should use `Content-Type: application/json`. Ordinary
404 responses do not need a JSON body.

## Translation State Diagram

```text
Incoming HTTP request
  |
  |-- POST /test/reset --------------------> Build test.reset call
  |
  |-- POST /test/seed/{name} --------------> Build test.seed_{name} call
  |                                           Slice 1: params = body
  |                                           Slice 3: params = {"params": body}
  |
  |-- POST /test/assert/{name} ------------> Build test.assert_{name} call
  |                                           params = body
  |
  |-- POST / (raw JSON-RPC root) ----------> Existing JSON-RPC handling
  |
  `-- anything else -----------------------> HTTP 404

Built adapter call
  |
  |-- body parse failed -------------------> HTTP 400 JSON-RPC parse error
  |
  `-- body parsed -------------------------> Dispatch in-process through JSON-RPC
                                                |
                                                `--> Return JSON-RPC response
```

## Delivery Slices

### Slice 1: Add the adapter

- Add adapter routing on the same test-control listener and port as raw
  JSON-RPC.
- Keep raw JSON-RPC at the listener root.
- Dispatch adapter calls in-process through the existing JSON-RPC machinery,
  not through HTTP loopback.
- Target current seed behavior: seed adapter params stay flat in this slice.
- Add unit tests for exact translation:
  - `/test/reset` maps to `test.reset`
  - `/test/seed/listing` maps to `test.seed_listing`
  - `/test/assert/concert_state` maps to `test.assert_concert_state`
  - empty body becomes `{}`
  - literal `null` is preserved
  - invalid JSON produces parse-error `id: null`
- Add integration coverage that adapter seed/reset/assert calls reach the real
  Test Control methods.
- Add compatibility coverage that raw JSON-RPC still works at the root.
- Add `hurl/test_control_adapter.hurl` with a success smoke path and one
  invalid-JSON parse-error check.
- Update `hurl/README.md` to introduce adapter routes as the preferred Hurl
  interface while documenting raw JSON-RPC as a debug fallback.
- Perform an adversarial Agent Review before implementation.

### Slice 2: Migrate Hurl files

- Mechanically rewrite existing `.hurl` files from raw JSON-RPC requests to
  adapter routes.
- Preserve explicit fixture values that are asserted or improve scenario
  readability.
- Keep using `{{test_control_url}}`, now with adapter paths appended.
- Keep response captures under the JSON-RPC `result` envelope because responses
  are not flattened.

### Slice 3: Restore generated seed methods

- Move seed methods back into the generated `#[rpc]` trait as request-object
  methods where that improves server-side authoring.
- Accept the raw JSON-RPC nested shape for seed request-object methods:
  `params: { "params": { ... } }`.
- Change only seed adapter translation to wrap body under `"params"` before
  dispatching to JSON-RPC.
- Keep assertion translation flat and keep `test.assert_concert_state` in its
  current generated multi-argument shape.
- Add tests proving Hurl-facing seed adapter bodies remain flat after the
  internal generated-method refactor.

## Verification

Run these after slice 1 and repeat the relevant subset after later slices:

```sh
cargo check -p concert-tracker --features test-control
cargo check -p concert-tracker
cargo build --bin concert-web --features test-control
node scripts/hurl-test.js --glob 'hurl/test_control_adapter.hurl'
just test-hurl
cargo nextest run -p concert-tracker --test web_integration
just lint
```

Also keep the release guard expected-failure check from `hurl/README.md`:

```sh
cargo build --release --bin concert-web --features test-control
```

That command should fail because `test-control` must not compile into release
builds.
