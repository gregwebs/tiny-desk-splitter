# Playwright tests

## Running normally

```sh
npx playwright test
```

Chromium launch args and worker count are environment-dependent — see
`e2e/sandbox.js`, the single source of truth for this — so this works
unchanged both on a normal machine and inside the Claude Code sandbox;
detection is automatic. If Chromium isn't installed yet, see the install step
below.

`e2e/sandbox.js` decides which mode to use:
- **Auto-detect**: sandbox mode is on iff `SANDBOX_RUNTIME` is set in the
  environment (that's how the Claude Code sandbox marks itself; it's absent
  in a normal terminal).
- **Override**: set `E2E_SANDBOX=1` (force sandbox mode) or `E2E_SANDBOX=0`
  (force normal mode) to bypass auto-detection — for example to reproduce a
  sandbox-only failure locally, or to sanity-check the detection itself. Any
  other value throws immediately rather than silently picking a mode.

## Running in a restricted / proxied sandbox

In a sandboxed environment (e.g. the Claude Code sandbox) `npx playwright
test` fails out of the box with **Chromium failing to launch**, unless the
fixes below are applied. This is a host/egress-proxy problem, not a test bug,
and it requires fixes at two independent layers: the Chromium *browser*
process, and (if a test makes outbound HTTP) the `concert-web` *server*
process.

**1. One-time install — override `no_proxy` for the download:**

```sh
# The default no_proxy excludes *.googleapis.com, which breaks the
# cdn.playwright.dev → storage.googleapis.com redirect the installer follows.
# Lowercase no_proxy specifically — undici/proxy-from-env prefer it over
# NO_PROXY, and an empty string is falsy in JS and falls through to the
# unhelpful default, so "localhost" is the minimal value that works.
no_proxy="localhost" npx playwright install chromium
```

**2. Already wired in — Chromium launch args, conditional on sandbox
detection.** `e2e/sandbox.js` computes the args below and both
`playwright.config.js` and `e2e/fixtures.js` consume them, so
`npx playwright test` works once the browser is installed — the
sandbox-only flags are added automatically when `e2e/sandbox.js` detects the
sandbox, and skipped otherwise (they crash Chromium at launch on a normal
machine, since `--single-process` is an unsupported mode there):

| Flag | When | Why |
| --- | --- | --- |
| `--single-process` | sandbox only | The sandbox blocks the Mach-port IPC Chromium normally uses between its processes; without this every launch dies immediately with `bootstrap_check_in org.chromium.Chromium.MachPortRendezvousServer: Permission denied (1100)`. Runs browser + renderer + GPU in one process instead. Outside the sandbox this same flag crashes Chromium at launch, so it must not be passed there. |
| `--no-proxy-server` | sandbox only | Chromium connects directly to the test's `127.0.0.1` server instead of routing through the egress proxy. |
| `--autoplay-policy=no-user-gesture-required` | always | Lets the player start tracks programmatically (auto-advance, back/next) without a real user gesture. |

`--single-process` has two structural consequences elsewhere in the suite
when sandbox mode is active, so they aren't mistaken for bugs:
- `playwright.config.js` sets `workers: 1` only in sandbox mode — parallel
  single-process Chromium instances crash under CPU contention, so the suite
  is serialized there. Outside the sandbox, `workers` is left at Playwright's
  default (parallel), since each test already has its own server and DB copy
  (see `e2e/fixtures.js`). If you see cross-test flakes under parallelism
  outside the sandbox, `npx playwright test --workers=1` is a useful triage
  step before assuming a real bug.
- `e2e/fixtures.js` always launches a **per-test** browser via a private
  `_ownBrowser` fixture instead of Playwright's worker-scoped `browser`
  fixture (in both modes, for one code path), because in sandbox mode
  `--single-process` Chromium can crash during `browserContext` cleanup —
  isolating each test to its own browser keeps one crash from failing every
  subsequent test in the worker. Outside the sandbox this per-test launch is
  simply unnecessary overhead (~0.2s/test), not a correctness requirement.

Each test also owns one `concert-web` child process and does not begin until
the child prints its ephemeral `127.0.0.1` listening URL and `GET /` returns a
successful response. Startup timeout, readiness exhaustion, and unexpected
mid-test exits fail with the child exit code/signal plus separately captured
stdout and stderr. Tests using the `killServer` fixture deliberately stop the
child to exercise network-error behavior; that shutdown and normal fixture
teardown are marked expected before the signal is sent.

```text
starting -> listening -> readiness
   |                      |
   +-- exit/timeout ------+-- non-2xx exhaustion -> diagnostic failure
                          +-- 2xx -> running

running -> killServer/teardown -> expected-stop -> cleaned
running -> child exit/error    -> unexpected failure -> diagnostic failure -> cleaned
```

**3. Server-side proxy flags (only needed for manual/outbound runs).** The
`concert-web` binary (`concert-tracker/src/bin/concert_web.rs`) has its own
proxy flags, separate from Chromium's:

- `--no-proxy` — build HTTP clients with no proxy (direct egress). Skips
  reqwest's macOS SystemConfiguration proxy lookup, which is blocked (and
  panics) in some sandboxes.
- `--proxy-from-env` — build HTTP clients using `HTTPS_PROXY`/`HTTP_PROXY`/
  `ALL_PROXY` from the environment while still skipping the SystemConfiguration
  lookup. Mutually exclusive with `--no-proxy`.

The e2e fixtures do **not** pass either flag — the test server never makes
outbound HTTP calls, so there's nothing to proxy. They matter when running
`concert-web` by hand in a sandbox (e.g. for manual verification), or for any
future test that exercises scraping:

```sh
target/debug/concert-web --db test.db --workdir /tmp/tds --port 0 --no-proxy
```

**4. Troubleshooting, if it still fails:**

- **Every launch fails immediately with `Target page, context or browser has
  been closed`, and this is a normal (non-sandbox) machine** — check that
  `E2E_SANDBOX` isn't forced to `1` in the environment; that forces the
  sandbox-only `--single-process` flag, which crashes Chromium outside the
  sandbox. Conversely, forcing `E2E_SANDBOX=0` *inside* the sandbox produces
  the identical error in the other direction (multi-process Chromium can't
  start there). The error message is the same for both mismatches — check
  `E2E_SANDBOX` and `SANDBOX_RUNTIME` before assuming a code regression.
- **`bootstrap_check_in … MachPortRendezvousServer: Permission denied
  (1100)`** — `--single-process` is missing or was stripped from the launch
  args (or `E2E_SANDBOX=0` is forced while actually running in the sandbox).
- **Install fails with `EAI_AGAIN` / DNS error** — use the
  `no_proxy="localhost"` override above; an empty or unset `no_proxy` isn't
  enough (see the note in step 1).
- **First test in a worker passes, the next fails almost instantly with
  `Target page, context or browser has been closed`** — the shared
  worker-scoped browser died; check that `e2e/fixtures.js`'s `_ownBrowser` /
  `context` / `page` fixtures (step 2) haven't been reverted to the
  Playwright defaults.
- **A failure reports `concert-web failed unexpectedly during the test`** —
  use the attached stdout/stderr and reported exit code or signal. This is a
  server-process failure, not a generic browser connection error. Intentional
  `killServer` tests must not produce this diagnostic.
- **Flaky failures that pass solo but fail in a full run** — usually one of:
  media ending mid-test and auto-advance reacting (set
  `document.getElementById("player-audio").loop = true` for tests that need
  playback to persist, or don't assert on real video decode); or a real
  pointer move crossing a hover-reactive card crashing single-process
  Chromium (use `locator.evaluate(el => el.click())` / `dispatchEvent`
  instead of pointer movement for tests whose subject is event *logic*).
- **Every launch dies within seconds with `SIGTRAP` (exit code 133), even
  running `chrome-headless-shell` directly with no Playwright involved** —
  this is a host/container-level regression below all the workarounds above,
  not something fixable from inside a test run. Confirm with a direct-binary
  repro (run `chrome-headless-shell --headless --no-sandbox --single-process
  --no-proxy-server --disable-gpu about:blank` outside Playwright) before
  spending time on fixture changes; if confirmed, fall back to API-level
  (`curl`) verification for anything that doesn't strictly require
  pixel/interaction checks.
