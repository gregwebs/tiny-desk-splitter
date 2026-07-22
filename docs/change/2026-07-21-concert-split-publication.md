# Publish complete Concert Splits through staging and backup

Implements [#142](https://github.com/gregwebs/tiny-desk-splitter/issues/142),
the third implementation slice of parent #139. The reviewed implementation
plan is [2026-07-21-concert-split-publication-plan.md](2026-07-21-concert-split-publication-plan.md).

## Change

The shared Concert Split workflow now writes generated timestamps, tracks, and
interludes to a hidden per-run staging directory. Only a validated non-empty
replacement set reaches canonical filenames. Both the default in-process
adapter and debugging CLI adapter inherit this behavior because publication
lives behind the shared `concert_split::run` interface.

Publication retains one previous exact output set in
`.concert-split-backup`, replaces files through sibling-temp rename, removes
obsolete files owned by the prior manifest, and installs
`.concert-split-published.json` last. Ordinary publication failure restores the
prior exact set. The pre-#142 Job Run cleanup that deleted canonical interludes
before execution was removed; stale cleanup now happens only after a complete
replacement is ready.

```text
Canonical state A
      │ copy exact manifest set
      ▼
Backup A      Staging B ──validate──> Replacement B
      │                              │
      └──── rollback on error ◀──────┤
                                     ▼ success
                               Canonical state B
                               manifest B installed last
```

## Domain constraints

- This change publishes only a complete Concert Split. Recoverable Partial
  Split behavior remains #143.
- The manifest owns only generated split output; source media, scraped metadata,
  previews, and unrelated files are never inferred from extensions and deleted.
- Process/host-crash recovery remains #144. This change handles errors returned
  during the live publication operation.
- `concert.json` remains adapter-owned scraped metadata outside the Published
  Concert Split manifest.

## Tests

Publication seam tests cover first publication, replacement, a single retained
backup, exact obsolete cleanup, unrelated-file preservation, rejection of
empty staged output without canonical mutation, injected publication failures,
rollback, and reader exclusion. Existing real-FFmpeg Concert Split tests cover
the full typed interface through staging and publication; tracker Job Run tests
cover adapter integration.

## Verification and review

The implementation followed red/green slices at the Concert Split/publication
interface. The initial publication tests failed against a no-op seam, then
passed after staging, validation, backup, manifest, and rollback behavior was
implemented.

Automated verification:

- `cargo test -p live-set-splitter publication::tests` — 8 passed, including
  legacy adoption, injected mid-publication copy failure/rollback, and shared
  reader exclusion plus failures during stale removal and manifest installation.
- `cargo test -p live-set-splitter concert_split::tests` — 8 passed, including
  real-FFmpeg supplied/embedded timestamp workflows through publication.
- `cargo test -p concert-tracker jobs::split --no-default-features` — passed.
- `cargo test -p concert-tracker concert_media::tests --no-default-features` —
  59 passed after adding shared read locking at the Concert Media Inventory.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `just test-rs` — 803 passed.
- `cargo fmt --all -- --check` and `git diff --check` — clean.

Manual verification started `concert-web` on port 43117 with an isolated
database and work directory under `/private/tmp`. The root page loaded and the
JSON `GET /api/playlists` endpoint returned an empty list from the scratch
database. The real-FFmpeg Job Run integration test exercised a complete split
through the default library adapter and filesystem publication. Playwright was
not run because this slice does not change visible UI or interaction behavior.

The initial adversarial review found unsafe legacy handling, backup rotation,
missing exact-output validation, non-participating readers, unvalidated manifest
paths, and insufficient failure/concurrency coverage. The implementation was
updated to conservatively adopt exact legacy names, rotate backups through an
old-backup path, validate exact song/interlude/timestamp coverage, lock the HTTP
media and Concert Media Inventory seams, reject non-filename manifest entries,
bound lock acquisition, and add injected rollback plus lock-exclusion tests.
The final Standards and Spec reviews found no remaining blockers after the lazy
timestamp backfill was also placed under one shared lock through DB persistence.
