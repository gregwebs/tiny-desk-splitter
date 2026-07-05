# Run Playwright in GitHub Actions

## Summary

The browser-driven end-to-end suite previously ran only on developer machines,
so regressions covered only by Playwright could merge without a CI signal. The
existing CI workflow now runs all 171 Playwright tests on every pull request and
every push to `main`.

This supersedes the earlier follow-up preference in
`2026-06-27-foldkit-player-behavior-fixes.md` for a smoke subset on pull
requests. The suite is self-contained and parallel-safe outside the Claude Code
sandbox, so CI runs the same complete command for both events.

## Architecture

The new `playwright` job is independent of the Rust, frontend, and shellcheck
jobs. A clean Ubuntu runner installs the root Node dependencies, system
`ffmpeg`, and only the Chromium browser. Rust dependency caching reduces the
cost of the global setup, which builds `concert-web` and generates the
deterministic fixture before the tests start.

The existing test architecture is unchanged: each test launches its own
Chromium process and `concert-web` process, copies the fixture into a unique
temporary work directory, uses an ephemeral loopback port, and removes the
temporary data after the child exits.

```text
PR or push to main
        |
        v
Node + Rust cache + ffmpeg + Chromium
        |
        v
global setup: concert-web -> pristine fixture
        |
        v
test: copy fixture -> start server -> browser assertions
        |
   +----+----+
   |         |
 pass      fail
   |         |
   +----+----+
        |
        v
HTML report artifact (14 days)
```

## Implementation details

- `.github/workflows/ci.yml` adds a 60-minute `playwright` job under the
  workflow's existing triggers, concurrency cancellation, and read-only
  permissions.
- `playwright.config.js` emits line and HTML reports in CI. Retries remain
  disabled and normal-host parallelism remains unchanged.
- `e2e/global-setup.js` passes `--locked` to both Cargo commands so the job
  cannot resolve dependencies by changing `Cargo.lock`.
- `.gitignore` excludes the generated local HTML report.
- Reports are uploaded after every non-cancelled run, including failures during
  test execution, with missing reports ignored when provisioning fails before
  Playwright starts.

## Verification

- `npx playwright test --list` discovers 171 tests.
- `npx playwright test` reaches fixture generation locally; on the current
  macOS host Chromium exits with the previously documented `SIGTRAP` before
  browser assertions, so the GitHub-hosted run supplies the browser result.
- `just lint` validates Rust, shell, and TypeScript sources.
- The GitHub-hosted `playwright` job must pass all 171 tests and publish its
  HTML report before this change is complete.
