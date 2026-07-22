# Recover interrupted Concert Split publication

Implements #144, the fifth implementation slice of parent #139. The approved
implementation plan is
[`2026-07-22-concert-split-recovery-plan.md`](2026-07-22-concert-split-recovery-plan.md).

## Change

Complete Concert Split publication now installs a durable, versioned journal
before its first canonical mutation. The journal records staging, the exact
replacement manifest, obsolete output, the prior Empty/Published/Recoverable
Partial state, durable backup or partial snapshot, and recovery attempts.

Publication and recovery share one idempotent finish operation. The initial
publication does not consume a recovery attempt. Later invocations increment
the persisted count before retrying. Attempts one and two retain the journal
and stop the caller; a third failed finish restores the exact prior state. If
that restoration also fails, the journal and filesystem evidence remain and an
explicit unrecoverable error identifies the inconsistent directory.
The journal also records a durable finish/rollback resolution before backup
rotation and cleanup, making a crash in that final window replay-safe.

`concert_split::run` resolves its own output directory before validating media,
so library and CLI adapters share the gate. `concert-web` scans all direct
concert directories before it opens the database or constructs job/server
state.

```text
Validated staging
      │ durable journal (attempts=0)
      ▼
Canonical mutation ──complete──> Published + retained backup; journal removed
      │ interrupted
      ▼
Recovery 1 ─fail─> Recovery 2 ─fail─> Recovery 3
      │ success         │ success         ├─ finish success → Published
      └─────────────────┴─────────────────└─ finish failure → exact rollback
                                                               │ failure
                                                               ▼
                                                        Unrecoverable;
                                                        evidence retained
```

## Design details

- Journal filenames are exact, validated single components. Replacement and
  obsolete sets cannot overlap, attempts cannot exceed three, and referenced
  staging/snapshot directories must be library-owned siblings.
- File data and parent directories are synced around journal, backup,
  canonical, manifest, and cleanup renames/removals.
- First-publication rollback removes only journal-owned replacement paths.
- Published rollback restores the retained known-good backup and exact
  manifest without deleting that backup.
- Recoverable Partial rollback restores its durable snapshot and Partial
  manifest rather than discarding playable work.
- Startup and new splits fail closed on malformed, pending, or unrecoverable
  state.

## Tests and verification record

Deterministic publication tests cover interrupted first publication,
interrupted replacement with obsolete cleanup, a failed retry followed by
success, three-attempt Published and Recoverable Partial fallback, an
already-at-three journal, third-generation backup rotation cleanup, strict
staging validation, and failed rollback retaining its journal. Concert Split
tests prove invalid pending state is checked before media validation and that
the all-directory scan fails closed. A `concert-web` startup helper test covers
an empty workdir.

Commands run during implementation:

- `cargo test -p live-set-splitter publication::tests`
- focused Concert Split recovery-gate tests
- `cargo check --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `just test-rs`
- `just test-ts` (68 Node tests and 256 Vitest tests passed)

`just lint` passed formatting and Clippy, then stopped in the existing
`scripts/shellcheck.sh` wrapper because the host Bash does not provide
`mapfile`. The remaining TypeScript typecheck and lint components were run
directly and passed.

## Review

The adversarial Standards and Spec reviews found a journal read-before-lock
race, incomplete backup/obsolete validation, incomplete unrecoverable context,
test-only path-validation gaps, missing Partial/attempt-limit coverage, and
stale backup-rotation artifacts. Follow-up changes acquire the lock before
reading, validate exact obsolete ownership and every backup file, report all
recovery paths/count/state, exercise production path validation, add the
missing recovery cases, share the journal rollback path, and resolve old/candidate
backup directories according to finish versus rollback. A final review found a
rollback-finalization crash window; the durable resolution phase and
pre-existing-backup bit now make that step idempotent, with a focused replay
test that leaves the journal between rotation and cleanup.

## Manual verification

A live `concert-web` was run on port 43144 with
`/private/tmp/tiny-desk-splitter.8jEgaG/concerts.db` and an isolated workdir.
Its first concert directory contained a hand-arranged interrupted first
publication. Startup logged recovery before `Listening`, installed the exact
track and Published manifest, removed the journal/staging directory, and the
real `GET /api/playlists` route returned `[]`.

A second scratch workdir contained a valid journal whose staged replacement was
missing. `concert-web` exited non-zero before binding port 43145, reported
recovery attempt 1 with canonical/staging paths, retained the journal, and did
not create/open the requested database. No UI behavior changed, so browser
interaction verification was not applicable to this backend/startup slice.
