# Canonical Concert State

Issue [#159](https://github.com/gregwebs/tiny-desk-splitter/issues/159)
introduces the first tracer bullet of the deep concert application interface
specified by parent [#158](https://github.com/gregwebs/tiny-desk-splitter/issues/158).
The approved implementation plan is
[canonical-concert-state-plan.md](2026-07-23-canonical-concert-state-plan.md).

## Changes

- Added the long-lived `Concerts` application module and canonical
  `ConcertState`.
- Combined persisted concert/track state, independently observed source and
  Published Concert Split media, active scrape/Job Run facts, archive
  configuration, and permitted actions.
- Added fallible Concert Media Inventory observations so I/O and publication
  lock failures become typed Unknown facts rather than false absence.
- Added explicit event and per-concert Failed Job history queries outside
  Concert State.
- Migrated `GET /concerts/:id` card rendering to Concert State while retaining
  first-view auto-scrape, routes, templates, markup, and visible behavior.
- Added `Concerts` to application state so adapters share one long-lived
  interface.

## State and safety

```text
media read succeeds                 media read fails
        |                                  |
        v                                  v
 Known(present/absent)              Unknown(operation)
        |                                  |
        +---------------+------------------+
                        v
             authoritative action policy
                        |
            unsafe prerequisite Unknown
                        v
               action is unavailable
```

Registry Reserved/running state and persisted `*_started_at` are retained
separately. Either prevents conflicting download, delete, split, archive, or
unarchive actions. This addresses short disagreement windows without claiming
cross-resource transactional consistency.

## TDD evidence

The red/green slices cover:

- complete state for seeded SQLite plus temporary source/track media;
- Published Concert Split lock failure while source remains Known;
- invalid concert-directory shape producing Unknown instead of absent;
- registry slots, queued split work, and pending scrape activity;
- explicit event and Failed Job histories plus typed missing-concert errors;
- focused detail renderer and auto-scrape-failure compatibility.

## Review and verification

The implementation-plan review first identified missing Failed Job history,
incomplete filesystem Unknown semantics, and an underspecified active-work
matrix. The plan was revised and approved on follow-up.

The adversarial code review then found two blocking defects:

- the first Published Split snapshot validated the directory fallibly but
  called legacy lossy probes afterward;
- the detail route's pre-scrape read bypassed typed query errors.

The snapshot now derives every split fact from one fallibly captured file set
under the shared publication lock, and both detail reads go through
`Concerts`. Follow-up review also drove independent registry-only and
persisted-only action-matrix tests, including source versus reconstruction
playback and meaningful unarchive behavior. The final follow-up review
reported no actionable findings.

Verification on 2026-07-23:

- `just test-rs`: 844 passed, 0 skipped.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `just ts-check`: passed.
- `just ts-lint`: passed.
- `/opt/homebrew/bin/bash ./scripts/shellcheck.sh`: passed. The aggregate
  `just lint` recipe reached this same step but macOS system Bash failed on
  `mapfile`; running the repository helper with installed modern Bash verifies
  the scripts themselves.
- `npx playwright test e2e/concert-reconstruction.spec.js
  e2e/delete-track.spec.js`: 8 passed.
- `git diff --check`: passed.

Live verification used
`/private/tmp/tiny-desk-splitter.4DnSVO/verify.db`, its own work directory,
ports 43172/43173, and the deterministic Test Control seed. The real
`GET /concerts/1` detail route rendered the expected title, preview, source
playback, downloaded/split badges, `1/2` track availability, explicit event
history, notes, and unchanged HTMX actions. `GET /concerts/999999` returned
404. The isolated server shut down cleanly; no user database or media was read
or changed.

GitHub Actions run
[30018565944](https://github.com/gregwebs/tiny-desk-splitter/actions/runs/30018565944)
passed its frontend, Playwright, Rust, and shellcheck jobs for implementation
commit `24a31422`.
