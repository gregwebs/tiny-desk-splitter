# Playwright tests

#### Running in a restricted / proxied sandbox

In a sandboxed environment (e.g. the Claude Code sandbox) `npx playwright
test` fails out of the box with **Chromium failing to launch**. This is a
host/egress-proxy problem, not a test bug, and it requires fixes at two
independent layers: the Chromium *browser* process, and (if a test makes
outbound HTTP) the `concert-web` *server* process.

**1. One-time install — override `no_proxy` for the download:**

```sh
# The default no_proxy excludes *.googleapis.com, which breaks the
# cdn.playwright.dev → storage.googleapis.com redirect the installer follows.
# Lowercase no_proxy specifically — undici/proxy-from-env prefer it over
# NO_PROXY, and an empty string is falsy in JS and falls through to the
# unhelpful default, so "localhost" is the minimal value that works.
no_proxy="localhost" npx playwright install chromium
```

**2. Already wired in — Chromium launch args.** `playwright.config.js` and
`e2e/fixtures.js` already pass the args below to every browser launch, so
`npx playwright test` should work once the browser is installed. They're
documented here so the reasons are visible if they ever need to be touched:

| Flag | Why |
| --- | --- |
| `--single-process` | The sandbox blocks the Mach-port IPC Chromium normally uses between its processes; without this every launch dies immediately with `bootstrap_check_in org.chromium.Chromium.MachPortRendezvousServer: Permission denied (1100)`. Runs browser + renderer + GPU in one process instead. |
| `--no-proxy-server` | Chromium connects directly to the test's `127.0.0.1` server instead of routing through the egress proxy. |
| `--autoplay-policy=no-user-gesture-required` | Lets the player start tracks programmatically (auto-advance, back/next) without a real user gesture. |

`--single-process` has two structural consequences elsewhere in the suite,
so they aren't mistaken for bugs:
- `playwright.config.js` sets `workers: 1` — parallel single-process Chromium
  instances crash under CPU contention, so the suite is serialized.
- `e2e/fixtures.js` launches a **per-test** browser via a private
  `_ownBrowser` fixture instead of Playwright's worker-scoped `browser`
  fixture, because `--single-process` Chromium can crash during
  `browserContext` cleanup — isolating each test to its own browser keeps
  one crash from failing every subsequent test in the worker.

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

- **`bootstrap_check_in … MachPortRendezvousServer: Permission denied
  (1100)`** — `--single-process` is missing or was stripped from the launch
  args.
- **Install fails with `EAI_AGAIN` / DNS error** — use the
  `no_proxy="localhost"` override above; an empty or unset `no_proxy` isn't
  enough (see the note in step 1).
- **First test in a worker passes, the next fails almost instantly with
  `Target page, context or browser has been closed`** — the shared
  worker-scoped browser died; check that `e2e/fixtures.js`'s `_ownBrowser` /
  `context` / `page` fixtures (step 2) haven't been reverted to the
  Playwright defaults.
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
