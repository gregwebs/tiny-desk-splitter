# E2E fixture and test stabilization

Issue [#44](https://github.com/gregwebs/tiny-desk-splitter/issues/44) tracked
16 failures in an older 155/16 Playwright run. The failures combined stale
test contracts, nondeterministic external requests, and fixture errors that
did not identify a failed `concert-web` child.

## Changes

- `e2e/fixtures.js` now requires a successful readiness response and reports
  startup failures or unexpected exits with separate stdout/stderr and the
  child exit code or signal.
- Intentional `killServer` shutdowns and ordinary teardown are distinguished
  from unexpected exits.
- Add-to-playlist tests use `body.showing-add`, the application's sole add-mode
  owner.
- Track-button locators target the canonical `.card-tracks-row` control while
  preserving the second splitting-status control used during card hover.
- OpenAPI tests use the browser context's request client and tag-scoped Swagger
  selectors.
- The sync filter test derives its mocked `HX-Location` from the request's
  `HX-Current-URL`, eliminating live NPR access and route teardown races.

## State transitions

```text
starting -> listening -> readiness
   |                      |
   +-- exit/timeout ------+-- non-2xx exhaustion -> diagnostic failure
                          +-- 2xx -> running

running -> killServer/teardown -> expected-stop -> cleaned
running -> child exit/error    -> unexpected failure -> diagnostic failure -> cleaned
```

```text
closed -> body.showing-add + sidebar open -> target loading -> target loaded
   ^                                                        |
   +---------------- close/external close ------------------+
```

```text
stable card -> operation -> full updated card response
            -> one outerHTML replacement -> htmx processes fresh controls
```

## Verification

- `npx playwright test --list` — 171 tests discovered.
- `cargo check` — passed.
- `just lint` — passed.
- Manual isolated server using a copied fixture DB/workdir and port 43117:
  `GET /` returned 200, OpenAPI reported 3.1.0 with `/api/playlists`, the
  liked filter rendered a Sync control, and concert cards rendered canonical
  `.card-tracks-row` controls.
- Required Codex follow-up review — no remaining material findings.

Targeted and full Playwright execution reached global setup and started the
per-test server, but both Playwright's headless shell and its full Chromium
binary exited with `SIGTRAP` before creating a page. No browser assertion ran.
This is the documented host regression in `docs/playwright.md`, so a healthy
browser host or CI must supply the final Playwright acceptance result.
