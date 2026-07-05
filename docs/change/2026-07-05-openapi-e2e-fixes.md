# Fix OpenAPI Playwright failures

Issue [#45](https://github.com/gregwebs/tiny-desk-splitter/issues/45)
tracked three failures in `e2e/openapi.spec.js`. The earlier issue #44 work
already replaced the unresolved browser-relative OpenAPI fetch and narrowed
the Swagger tag and operation selectors. This change completes the repair:

- The OpenAPI document test uses Playwright's request fixture directly. It
  resolves relative URLs against the isolated test server without loading a
  browser page.
- The Swagger render assertion targets the unique outer
  `section.swagger-ui.swagger-container` instead of the two-element
  `.swagger-ui` class.
- The exact `/api/playlists` operation selector from issue #44 remains the
  scope for the interactive request, avoiding similarly prefixed operations.
- The interactive response assertion selects the response table body rather
  than its `Code` header before checking for status `200`.
- `scripts/check-playwright-job.sh` reports or waits for the current commit's
  Playwright check, making CI browser acceptance directly observable when the
  local host cannot launch Chromium.

## State transitions

```text
isolated server ready -> request fixture GET -> HTTP 200
                      -> parse and validate OpenAPI 3.1 document
```

```text
Swagger page loading -> unique outer container visible
                     -> GET /api/playlists expanded
                     -> Try it out enabled -> Execute -> response 200
```

## Verification

- `npx playwright test e2e/openapi.spec.js --grep 'openapi.json'
  --reporter=line` — passed.
- `npx playwright test e2e/openapi.spec.js --list` — all three tests
  discovered.
- `cargo check --workspace --all-targets` — passed.
- `just lint` — passed.
- Pull request CI confirmed that the OpenAPI document and Swagger render tests
  pass on Linux. Its first run also exposed the response-header ambiguity in
  the third test, which is corrected by the final response-body selector.

The complete browser run could not reach its UI assertions on the local host:
Chromium exits with the documented `SIGTRAP` regression in normal mode, while
forced sandbox mode prevents the isolated server from binding. GitHub Actions
therefore supplies the final browser acceptance run.
