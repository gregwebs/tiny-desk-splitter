# Hurl Web Integration Tests

## Progress

Tracks the stacked PR series implementing this spec (parent issue #84):

- [x] #78 Feature-Gated Test Control Server — `test.reset` JSON-RPC method, `--test-control-port` flag, release-build guard.
- [x] #79 Seed Listing Through Hurl — `test.seed_listing`, `hurl/listing_status.hurl`, `scripts/hurl-test.js`, `just test-hurl`.
- [ ] #80 Seed Scraped Concert Status Cases
- [ ] #82 Migrate Listing Filter And Ignore Cases
- [ ] #81 Add Semantic Assertion API When Needed
- [ ] #83 Document And Stabilize The Hurl Workflow

## Problem

`concert-tracker/tests/web_integration.rs` exercises HTTP behavior by linking directly to the Rust web implementation. The tests construct `AppState`, call the axum router with `tower::ServiceExt::oneshot`, seed SQLite directly, inject job command closures, and sometimes inspect DB or filesystem state after requests.

That makes useful assertions cheap, but it also means the tests are not black-box checks of the running `concert-web` binary. The goal is to move web integration coverage toward Hurl tests that exercise a real HTTP server and remain independent of axum, router internals, and Rust-only test seams.

## Architecture

Add a test-only **Test Control API** to `concert-web`. Hurl tests use it to arrange fixture data and, when needed, assert internal-only facts. Product behavior is still verified through the normal `concert-web` HTTP routes.

```text
Hurl test
   |
   +-- JSON-RPC setup/assertion --> Test Control API
   |                               127.0.0.1:<test-control-port>
   |                               same concert-web process
   |
   +-- product HTTP assertions ---> concert-web app
                                   127.0.0.1:<app-port>
                                   same DB/workdir/AppState
```

The Test Control API runs in the same `concert-web` process as the app server, but on a separate loopback port. This lets it share the same database, workdir, and process-level configuration without adding test-only routes to the product axum router.

## Decisions

- Use Hurl for black-box HTTP integration tests.
- Keep the Test Control API inside the `concert-web` process, not as a separate seed proxy process.
- Run the Test Control API on a separate loopback port, enabled by a flag such as `--test-control-port 0`.
- Use `jsonrpsee` with its trait-first `#[rpc(server)]` macro model.
- Use `test.<snake_case>` RPC method names, for example `test.seed_listing`.
- Use named/map params for Hurl readability.
- Do not use bearer-token authentication.
- Gate the API with all of:
  - non-default Cargo feature, for example `test-control`
  - explicit runtime flag
  - loopback binding
  - compile-time error if compiled with `not(debug_assertions)`
- Return compact **Seed Results** from seed methods. Prefer existing public API JSON shapes when practical, with test-only handles added only when needed.
- Prefer server-side **Assertion API** methods for internal-only facts instead of exposing raw DB rows to Hurl.
- Migrate tests in slices and delete Rust duplicates as each Hurl equivalent lands.

See [../adr/0001-jsonrpsee-for-test-control-api.md](../adr/0001-jsonrpsee-for-test-control-api.md) for the JSON-RPC dependency decision.

## First Slice

Start with a “listing and status basics” Hurl suite. This proves the new harness without designing job command stubbing, scrape queue controls, or complex filesystem controls.

Initial Hurl coverage:

- seed a listing
- `GET /` contains the seeded title
- `POST /concerts/:id/ignore` returns ignored badge markup
- `GET /?filter=ignored` includes the ignored concert and excludes another seeded concert
- seed scraped metadata
- `GET /concerts/:id/status` shows the Download button and omits the old not-downloaded badge

Initial Test Control methods:

- `test.reset`
- `test.seed_listing`
- `test.seed_scraped_concert`
- `test.assert_concert_state`, only if a first-slice assertion needs internal state

Hurl tests should capture IDs and other stable handles from Seed Results instead of assuming `id = 1`.

```hurl
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
concert_id: jsonpath "$.result.id"
[Asserts]
jsonpath "$.result.title" == "Listing Status A"

POST {{base_url}}/concerts/{{concert_id}}/ignore
HTTP 200
[Asserts]
body contains "badge-ignored"
```

## Out Of Scope For First Slice

- Job command stubbing for download/split chains.
- Scrape queue controls for `pending_card_shows_loading_then_thumbnail`.
- Opener command success/failure migration.
- Broad filesystem assertion helpers beyond what the first suite needs.
- Full deletion of `web_integration.rs`.
- CI enforcement of Hurl tests.

These should be designed in later migration slices when the target Rust tests make the needed control surface concrete.

## Test Control Contracts

### `test.reset`

Purpose: make a Hurl file independently repeatable. Tests should still prefer targeting Seed Results over relying on global cleanup.

Contract:

- clear concert, event, playlist, job, and settings test data
- remove generated concert files and thumbnails under the configured workdir
- leave the SQLite schema intact
- leave server configuration intact
- return `{ "ok": true }`

### Seed Methods

Seed methods arrange domain state through application/domain persistence helpers rather than ad hoc SQL where possible.

Seed Results should include only stable test handles:

- created object IDs
- source URL/title/album fields already meaningful to public APIs
- paths only when the method creates files and later test steps need the path

Avoid returning full DB rows.

### Assertion Methods

Assertion methods should express semantic expectations and fail with useful JSON-RPC errors.

Example:

```json
{
  "jsonrpc": "2.0",
  "id": 12,
  "method": "test.assert_concert_state",
  "params": {
    "id": 17,
    "ignored": true,
    "downloaded": false,
    "split": false
  }
}
```

Success:

```json
{ "jsonrpc": "2.0", "id": 12, "result": { "ok": true } }
```

Failure should be a JSON-RPC error with the mismatched domain condition in the message.

## Implementation Plan

1. Add a non-default `test-control` Cargo feature to `concert-tracker`.
2. Add a release-build guard:

   ```rust
   #[cfg(all(feature = "test-control", not(debug_assertions)))]
   compile_error!("test-control must not be compiled into release builds");
   ```

3. Add optional `jsonrpsee` dependencies behind the feature, using server and macro support only.
4. Add a `test_control` module behind `#[cfg(feature = "test-control")]`.
5. Define a `#[rpc(server, namespace = "test", namespace_separator = ".")]` trait for the initial methods.
6. Implement the generated server trait for a type holding the shared app state needed by seed/assertion methods.
7. Add `--test-control-port <PORT>` to `concert-web` only when the feature is enabled.
8. When the flag is present, start a `jsonrpsee` server bound to `127.0.0.1:<PORT>` and print:

   ```text
   Test control listening on http://127.0.0.1:<bound-port>
   ```

9. Keep existing `Listening on http://127.0.0.1:<port>` output unchanged for current Playwright fixtures.
10. Add `hurl/listing_status.hurl`.
11. Add `scripts/hurl-test.js` to build/start the test-control binary, parse both URLs, run Hurl with variables, and clean up.
12. Add `just test-hurl` as an optional local target that verifies `hurl` is installed before running the Node runner.
13. Delete the Rust tests migrated by `listing_status.hurl` once the Hurl suite passes.
14. Document local Hurl setup and the first slice in `hurl/README.md` and link it from `CONTRIBUTING.md`.

## Verification

For the first implementation slice:

- `cargo check -p concert-tracker --features test-control`
- `cargo check -p concert-tracker`
- `cargo build --bin concert-web --features test-control`
- `just test-hurl`
- targeted Rust integration tests that remain in `web_integration.rs`
- `just lint` before PR

Manual verification:

1. Start `concert-web` with a scratch DB/workdir, `--port 0`, and `--test-control-port 0`.
2. Confirm both listening URLs are printed.
3. Call `test.seed_listing` with Hurl or curl.
4. Open the app URL and confirm the seeded concert appears.
5. Stop the process and confirm cleanup removes only scratch data.

Release guard verification:

```sh
cargo build --release --bin concert-web --features test-control
```

Expected result: compile failure from the explicit guard.
