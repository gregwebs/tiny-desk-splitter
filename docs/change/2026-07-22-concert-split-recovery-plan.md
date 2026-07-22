# Concert Split Publication Recovery — Implementation Plan

Implements [#144](https://github.com/gregwebs/tiny-desk-splitter/issues/144),
the fifth slice of parent [#139](https://github.com/gregwebs/tiny-desk-splitter/issues/139),
from the complete specification in [#138](https://github.com/gregwebs/tiny-desk-splitter/issues/138).

## Scope

Make complete Concert Split publication crash-recoverable. Before canonical
files can change, publication durably records enough information to finish the
replacement or restore the preceding Published Concert Split. Recovery retries
the intended publication on three invocations, then restores the retained
backup. `concert-web` must recover every concert directory before it creates a
Job Registry or serves routes, and every individual Concert Split must resolve
its own pending journal before analysis or cutting begins.

This slice does not make filesystem replacement atomic across a host crash,
add generation-qualified canonical filenames, or journal Recoverable Partial
Split publication. Partial publication retains its existing synchronous
rollback behavior; complete publication is the operation whose durable backup
and replacement state is defined by #138 and #144.

## Confirmed test seams

- `live_set_splitter::concert_split::recover_publication(canonical_dir)` is the
  public, deterministic recovery seam. Tests arrange a durable journal and
  filesystem state through publication's test harness, invoke recovery, and
  assert only the returned status/error plus canonical files, manifests,
  backup, staging, and journal state.
- `live_set_splitter::concert_split::run` remains the operation seam proving a
  new split resolves pending publication before doing media work.
- A small startup helper used by `concert-web` is the server seam. Tests give it
  a scratch `workdir/concerts`, assert all nested pending publications are
  recovered, and assert an unrecoverable journal prevents startup continuation.

Tests use the existing narrow filesystem fault injection only for failures an
ordinary filesystem cannot schedule deterministically. Each slice follows
red → green before the next state is added.

## Durable model

Add `.concert-split-publication.json`, atomically installed and synced before
the first canonical mutation. Its versioned record uses validated types so an
unsupported version, an attempt outside `0..=3`, a multi-component filename,
or a contradictory prior state cannot enter the mutation path:

```rust
struct PublicationJournal {
    version: JournalVersion,
    staging_dir: ValidatedStagingDirectory,
    intended: IntendedPublication,
    prior: PriorCanonicalState,
    recovery_attempts: RecoveryAttempt,
    resolution: Option<RecoveryResolution>,
}

struct IntendedPublication {
    replacement_manifest: NonEmptySet<ValidatedFilename>,
    obsolete_files: BTreeSet<ValidatedFilename>,
}

enum PriorCanonicalState {
    Empty,
    Published {
        manifest: NonEmptySet<ValidatedFilename>,
        backup: ValidatedBackupDirectory,
        retained_backup_before: bool,
    },
    RecoverablePartial {
        manifest: PartialManifest,
        files: NonEmptySet<ValidatedFilename>,
        snapshot: ValidatedPartialSnapshotDirectory,
    },
}

enum RecoveryResolution { Finished, RolledBack }
```

The constructor validates disjoint/containment invariants among replacement,
obsolete, and prior files and verifies every referenced snapshot file before
serialization. The staging/snapshot directories are stored as sibling paths
with their expected hidden prefixes. The canonical directory is identified by
the journal's own location rather than duplicated as an untrusted path.

The initial synchronous publication attempt does **not** increment
`recovery_attempts`; a journal left by that attempt therefore still permits
three later recovery invocations. Recovery increments and durably replaces the
counter before each finish attempt, so a crash during recovery consumes that
invocation. A journal already at three attempts skips finish and immediately
tries rollback.

Finish/rollback records `resolution` before backup rotation and journal cleanup.
Together with `retained_backup_before`, that makes finalization replay-safe if
another crash occurs between backup rotation and journal removal.

Durable mutation order is explicit:

```text
sync every staged/backup/snapshot file
→ sync staging/backup/snapshot directories
→ rename completed backup/snapshot candidates into place
→ sync their parent directory
→ write + sync journal.next
→ rename journal.next to journal
→ sync canonical directory                    # recovery point now committed
→ copy+sync and rename each canonical replacement; sync canonical directory
→ remove exact obsolete files; sync canonical directory
→ write+sync and rename Published manifest; sync canonical directory
→ remove journal; sync canonical directory    # publication commit
→ remove staging/partial snapshot; sync parent directory
```

Rollback follows the same copy/rename/file-sync/directory-sync discipline and
removes the journal only after the preceding exact state and manifest have
been durably restored. Cleanup after the commit is idempotent. Tests interrupt
the internal operation at each namespace boundary; test-only fault injection
returns control at those boundaries rather than attempting to simulate an
actual process crash.

## State changes

```text
No journal
    │ complete candidate validated; lock acquired; backup ready
    ▼
Journal(attempts=0) durably installed
    │
    ├─ ordinary publish succeeds ──> Published; remove obsolete + staging + journal
    │
    └─ process/host stops ─────────> Pending journal
                                      │ recovery invocation
                                      ▼
                              persist attempts = attempts + 1
                                      │
                    ┌─────────────────┴─────────────────┐
                    │ finish succeeds                  │ finish fails
                    ▼                                  ▼
             Published; clean journal        attempts < 3: keep journal
                                               return Pending error;
                                               server/new split stops
                                                       │ next invocation
                                                       └─ attempts == 3
                                                              │
                                              ┌───────────────┴──────────────┐
                                              │ backup restore succeeds      │ fails
                                              ▼                              ▼
                                      Previous Published;             Unrecoverable;
                                      clean staging+journal           retain journal
```

For an interrupted first publication, `Empty` rollback removes only replacement
filenames named by the journal and clears any incomplete manifest;
it never scans or deletes unrelated concert files. For replacement,
`Published` rollback copies the exact backup manifest/files over canonical,
removes only replacement-only files, and reinstalls the preceding Published
manifest. Successful finish retains the known-good backup. Successful rollback
also retains that same known-good backup. `RecoverablePartial` rollback restores
its exact durable snapshot and Partial manifest, so a failed complete retry
cannot destroy playable partial work.

## Implementation checklist

- [ ] In `live-set-song-splitter/src/publication.rs`, add the versioned journal,
  validated filename/attempt/prior-state types, strict cross-field validation,
  atomic file-and-parent-directory sync helpers, and explicit
  resolved/pending/unrecoverable recovery results.
- [ ] Refactor complete publication into idempotent `finish_publication` and
  `restore_from_journal` operations shared by normal publication and recovery;
  preserve the exclusive per-concert lock for all reads and mutations.
- [ ] Install the journal after staging/replacement/prior/obsolete/backup state
  is complete and durably synced, and before copying any canonical replacement.
- [ ] Make finish idempotent: recopy every staged replacement, remove the exact
  obsolete set, install the exact intended Published manifest, remove a prior
  Partial marker only for `RecoverablePartial`, then clean temporary files,
  staging, partial snapshot, and journal in durable commit order.
- [ ] Implement three cross-invocation finish attempts, followed on the third
  failed attempt by exact backup restoration (or exact first-publication
  cleanup); retain the journal and return an error containing canonical,
  staging, backup, and attempt information if rollback also fails.
- [ ] Re-export `recover_publication` and `recover_publications` from
  `live-set-song-splitter/src/concert_split.rs`; the plural operation scans only
  direct concert directories below `workdir/concerts` and fails closed on an
  unreadable or invalid journal.
- [ ] Call single-directory recovery at the beginning of
  `concert_split::run`, before creating new staging or probing media, so both
  library and CLI adapters enforce the same precondition.
- [ ] In `concert-tracker/src/bin/concert_web.rs`, recover
  `workdir/concerts` after CLI validation and before database/job recovery,
  scrape queue creation, Job Registry construction, route construction, or
  listener bind. Add contextual startup logging and propagate unrecoverable
  **or still-pending** errors from `main`; the first or second failed finish
  attempt must never permit serving or Job Run acceptance.
- [ ] Update `docs/concert-split.md` with journal schema/invariants, recovery
  commands/API, startup ordering, and the final state diagrams. Link any new
  lasting documentation from `README.md` only if it is not already reachable
  through the existing Concert Split link.
- [ ] Add `docs/change/2026-07-22-concert-split-recovery.md` as the change
  record; review links and mark superseded crash-recovery limitations in older
  change records as resolved by #144 without rewriting their historical scope.

## TDD slices

1. Interrupted first publication finishes on recovery and removes its journal
   and staging without deleting unrelated files.
2. Interrupted replacement finishes, removes exact obsolete output, installs
   the new manifest, and retains the preceding backup.
3. A scheduled finish failure increments and retains attempts; a later
   invocation succeeds and cleans recovery state. The original synchronous
   publish attempt leaves the count at zero.
4. Three failed finish invocations restore the prior Published Concert Split;
   the no-prior case removes only journal-owned replacement files.
5. Failed finish plus failed rollback returns an explicit unrecoverable error
   and retains the journal/staging/backup evidence.
6. Replacement of a Recoverable Partial Split can finish or restore its exact
   Partial manifest/files after the three-attempt fallback.
7. Crash-boundary tests cover journal installation, attempt persistence, each
   canonical rename, obsolete removal, manifest installation, journal removal,
   and an already-three-attempt journal. Every retained journal is idempotently
   recoverable on the next invocation.
8. `concert_split::run` resolves an existing journal before request media
   validation, proving every new split is gated.
9. Startup recovery visits multiple concert directories before any server/job
   construction and propagates pending (attempts one/two), invalid, and
   unrecoverable directory errors.

## Verification

Automated:

- Run the focused publication tests after every red/green slice:
  `cargo test -p live-set-splitter publication::tests`.
- Run the focused `concert_split` and `concert_web` tests after their wiring.
- Frequently run `cargo check --workspace` and `just fmt`; finish with
  `just lint`, `just test-rs`, and `just test-ts`.

Manual, using a scratch database, scratch workdir, and a separate port as
required by `CONTRIBUTING.md`:

1. Seed an interrupted first-publication journal and start `concert-web`; verify
   recovery finishes before the listening message and normal API routes work.
2. Seed an interrupted replacement after one canonical file changed; verify
   startup completes the exact replacement, removes obsolete output, preserves
   the backup, and exposes the new tracks through the API/UI.
3. Seed a journal at two attempts with deterministic unreadable staging and a
   valid backup; verify the third invocation restores the prior manifest/files.
4. Seed a journal whose staging and backup cannot complete; verify startup exits
   non-zero before binding the port and reports the inconsistent paths/state.
5. Start a new split against a pending recoverable journal through the API and
   verify recovery completes before the Job Run is accepted. Use Playwright for
   the visible track list/player state after recovery where host browser launch
   is available; otherwise use the repository's Linux Playwright CI surface.

## Pull request

Branch `concert-split-recovery` is based on parent branch
`concert-split-interface`, because #144 is a sibling implementation slice in
the #139 stack and depends on the already merged #142/#143 work. The pull
request should target `concert-split-interface`, cite the final change record,
and use `Resolves #144`.
