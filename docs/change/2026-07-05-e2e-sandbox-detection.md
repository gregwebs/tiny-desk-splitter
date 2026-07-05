# Make Playwright e2e tests runnable outside the Claude Code sandbox

## Summary

`npx playwright test` failed on every test on a normal developer machine, at `chromium.launch()`
in `e2e/fixtures.js`, with `Error: browserType.launch: Target page, context or browser has been
closed`. `e2e/fixtures.js` and `playwright.config.js` unconditionally passed Claude-Code-sandbox-
only Chromium launch args (`--single-process`, `--no-proxy-server`). Those flags are required
inside the sandbox (it blocks the Mach-port IPC multi-process Chromium needs, so multi-process
Chromium dies instantly there) but `--single-process` is an unsupported Chromium mode that crashes
the browser at launch everywhere else — producing the exact same error message in both directions.
Confirmed directly in this session: `chromium.launch` with `--single-process` inside the sandbox
works (`e2e/sidebar.spec.js` 16/16); without it, inside the same sandbox, launch fails with the
identical error the reporting user saw on their own machine.

**Fix:** a new `e2e/sandbox.js` module is the single source of truth for whether sandbox-only
Chromium args and worker serialization are needed, auto-detected from the `SANDBOX_RUNTIME`
environment variable (set by the sandbox runtime itself) with an `E2E_SANDBOX=1`/`0` manual
override for reproducing either mode from anywhere. `e2e/fixtures.js` and `playwright.config.js`
now both consume this module instead of hardcoding the flags.

## Changes

- **New `e2e/sandbox.js`**: exports `isSandbox` and `browserArgs`. `E2E_SANDBOX` values other than
  `"1"`/`"0"` throw immediately rather than silently picking a mode.
- **`e2e/fixtures.js`**: `BROWSER_ARGS` now comes from `e2e/sandbox.js`. The per-test `_ownBrowser`
  fixture (isolates single-process crashes) is kept in both modes deliberately — a per-test launch
  is cheap outside the sandbox too, and one code path is simpler than branching back to
  Playwright's worker-scoped `browser` fixture.
- **`playwright.config.js`**: `workers: 1` only when `isSandbox` (parallel single-process Chromium
  instances crash under CPU contention there; outside the sandbox, default parallelism is safe
  since every test already has its own server + temp-dir DB copy). Removed the dead
  `use.launchOptions` — the built-in browser fixture that would consume it is never launched,
  since `context`/`page` are overridden to flow through `_ownBrowser`.
- **`docs/playwright.md`**: rewritten to document the two modes as conditional rather than fixed,
  with a troubleshooting entry for a forced-wrong `E2E_SANDBOX` (same launch error either
  direction — check the env var before assuming a regression).

## Detection design

```
E2E_SANDBOX unset  → isSandbox = SANDBOX_RUNTIME is set in env
E2E_SANDBOX="1"    → isSandbox = true   (forced)
E2E_SANDBOX="0"    → isSandbox = false  (forced)
E2E_SANDBOX=<other>→ throws
```

`SANDBOX_RUNTIME` was chosen over `CLAUDE_CODE_ENTRYPOINT` (also present in this environment)
because it reflects whether the *sandbox itself* is active, not merely whether the shell is a
Claude Code session — the latter would false-positive if a session ever runs with sandboxing
disabled.

## Out of scope

Three pre-existing failures in `e2e/openapi.spec.js` (relative-URL fetch from `about:blank`, and
two `.swagger-ui` strict-mode-locator violations) are test bugs unrelated to the launch problem —
they reproduce identically inside the sandbox where the rest of the suite passes, and appear to
have been committed without a green run. Filed as a separate GitHub issue rather than fixed here.

## Verification

- Inside the sandbox: `npx playwright test e2e/sidebar.spec.js` — 16/16 pass with auto-detection
  (unchanged from before this fix).
- `E2E_SANDBOX=0 npx playwright test e2e/sidebar.spec.js` from inside the sandbox — all 16 fail at
  launch with `Target page, context or browser has been closed`, confirming the override actually
  switches behavior (this is the same failure mode the reporting user saw on a normal machine with
  the old, unconditional flags).
- `E2E_SANDBOX=2 node -e "require('./e2e/sandbox.js')"` — throws immediately, as designed.
- User acceptance (environment this fix targets, not reproducible from inside the sandbox): plain
  `npx playwright test` in a normal terminal should now launch Chromium successfully.
