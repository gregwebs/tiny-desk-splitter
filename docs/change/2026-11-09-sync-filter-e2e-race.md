# Fix a synchronization race in the sync-filter e2e test

CI's `playwright` job failed twice on PR
[#184](https://github.com/gregwebs/tiny-desk-splitter/pull/184) at
`e2e/sync-player-persistence.spec.js:149`
("Sync preserves active filter in the HX-Location path"):

```text
Error: expect(received).toBe(expected) // Object.is equality
Expected: "/?filter=liked"
Received: null
```

`capturedLocationPath` is set by the test's `page.route("**/sync/**")` handler,
in the Node process, from the request's `HX-Current-URL` header. The only
synchronization between the click and the assertion was

```js
await page.waitForFunction(() => !document.querySelector("[hx-request]"));
```

which is a *negative* condition. `hx-request` is set only while htmx has a
request in flight, so before htmx's click handler has issued anything there is
no such element and the wait resolves immediately — the assertion then runs
before the click has produced a request at all. Even when the request is issued,
the capture happens in the route handler on the Node side, and the assertion runs
in Node too, so nothing orders the two.

The test therefore failed intermittently under load (CI's `retries: 0` means one
occurrence is a red job). It is test-only: no product code changed.

## Fix

- Wait for the request itself — `page.waitForRequest((request) =>
  request.url().includes("/sync/"), { timeout: 10_000 })` — before awaiting the
  htmx-settled condition. A click that issues no sync request is a real
  regression (the `HX-Current-URL` wiring is what this test guards), so it now
  fails at that wait with a clear message instead of as a `null` mismatch.
- Poll the captured path with `expect.poll(...)`, because the capture and the
  assertion are not ordered by the page-side wait.

## Verification

- `e2e/sync-player-persistence.spec.js --repeat-each=12 --workers=4` — 24
  passed.
- Full suite, CI configuration (`CI=1`, `E2E_SANDBOX=0`, four workers,
  `retries: 0`) — 173 passed, twice.
- The assertion still bites: expecting `/?filter=wrong` fails with
  `Received: "/?filter=liked"`.
- No product code, route, header, or markup changed; `cargo` gates are untouched
  by this file.

## Follow-on

The same full-suite runs surfaced a second, unrelated intermittent failure:
`e2e/player-queue.spec.js:667` "Space on focused Next button pauses without
skipping again" (expected `#player-title` "Celular", received "Limbo" — Space on
the focused Next button skipped a second time). It is a player keyboard-handling
race in the family recorded by
[Fix player keyboard shortcuts](2026-07-03-player-keyboard-shortcut-targets.md),
passes 8/8 in isolation, and needs its own investigation rather than a change
here.
