# Concert Split Publication — Implementation Plan

Implements [#142](https://github.com/gregwebs/tiny-desk-splitter/issues/142),
the third slice of parent [#139](https://github.com/gregwebs/tiny-desk-splitter/issues/139),
on top of the completed #140 and #141 commits.

## Scope

This pull request makes a successful Concert Split write only to a per-run
staging directory until all expected output is complete and non-empty. It then
publishes canonical track, interlude, and timestamp filenames under a
per-concert publication lock, retaining at most one backup of the preceding
Published Concert Split and restoring that backup when ordinary publication
fails.

Recoverable Partial Split behavior and crash journals remain #143 and #144.
This slice handles synchronous I/O errors during publication, not process or
host crashes.

## Test seam

Most tests cross the public synchronous `concert_split::run` interface. They pass a
typed request with supplied timestamps and a tiny real media input, then assert
the structured outcome plus visible canonical, staging, and backup filesystem
state. Focused tests may cross the internal output-validator/publication seam
for states real FFmpeg cannot deterministically produce (empty/missing output,
copy failure after N files, and rollback failure). A test-only fault injection
is acceptable only at a narrow internal filesystem-operations adapter covering
copy, remove, rename, and atomic manifest installation, because an ordinary
filesystem cannot deterministically fail after a chosen operation.

The CLI and `concert-web` already call the same interface. Their existing
adapter tests will assert that requests still name the canonical concert
directory; no adapter may implement separate publication logic.

## State changes

```text
                  Concert Split starts
                           │
                           ▼
                 create per-run staging
                           │
                  analysis and cutting
                           │
             ┌─────────────┴──────────────┐
             │ workflow error             │ complete candidate
             ▼                            ▼
       remove staging             validate expected files
       canonical unchanged                 │
                                  ┌────────┴────────┐
                                  │ invalid         │ valid
                                  ▼                 ▼
                            remove staging   acquire concert lock
                            canonical        │
                            unchanged        ▼
                                       read exact prior manifest
                                             │
                                       prepare backup candidate
                                             │
                                  ┌──────────┴──────────┐
                                  │ backup failure      │ ready
                                  ▼                     ▼
                              canonical          install one backup
                              unchanged                 │
                                                        ▼
                                            copy replacement files
                                                        │
                                  ┌─────────────────────┴────────────┐
                                  │ copy/remove failure               │ success
                                  ▼                                   ▼
                         restore canonical from backup       remove obsolete files
                         (or clear copied files when none)             │
                                  │                                   ▼
                                  ▼                            release lock; Complete
                              return error
```

```text
Backup rotation

old retained backup ───────────────┐
current canonical ──copy──> candidate backup
                                   │ complete
                                   ▼
                         replace old retained backup

At no point does an incomplete candidate backup replace the retained backup.
```

## Detailed changes

### 1. Add a publication module inside `live-set-splitter`

- Add `live-set-song-splitter/src/publication.rs` as an internal deep module.
- Model publication inputs with named structs rather than parallel path lists:
  canonical output directory, exact staged replacement files, the source-media
  path to exclude, and the expected prior managed-output set.
- Use hidden per-concert directories with stable roles:
  `.concert-split-staging-*`, `.concert-split-backup`, and a temporary backup
  candidate. Staging is created beside the canonical directory so copies remain
  on the same filesystem without making an analysis-only output directory visible.
- Use an advisory filesystem lock with shared reader and exclusive publisher
  modes. Expose a small read-lock interface from `live-set-splitter` for
  `concert-tracker`'s Concert Media Inventory and filesystem reconciliation
  callers. Publisher contention and reader exclusion therefore work across the
  in-process and CLI adapters. Use named timeout/poll constants, monotonic
  elapsed time, typed timeout errors, and progress diagnostics. A guard releases
  the advisory lock on every ordinary return and panic unwind; process abort and
  host crash behavior remains #144's recovery scope.
- Add a durable `.concert-split-published.json` manifest containing the exact
  canonical relative filenames owned by the current Published Concert Split.
  The manifest is replaced only after all replacement copies and obsolete-file
  removals succeed. Never infer ownership from file extension alone.
- For a legacy directory with no manifest, conservatively adopt only exact
  filenames derived from the current set list, anchored interlude names, and
  `timestamps.json`; uncertain media is preserved rather than deleted. Once a
  split publishes a manifest, every subsequent obsolete-file decision uses its
  exact prior set. Preserve `concert.json`, preview/thumbnail files, the source
  media, and unrelated files in every path.
- Stage and validate the replacement manifest before canonical mutation. Treat
  manifest installation as part of publication: write/fsync its sibling
  temporary file and atomically rename it only after media changes succeed. A
  write/fsync/rename failure rolls canonical media back to the exact prior set
  before returning an error.
- Build a complete backup candidate before rotating the retained backup. Copy
  current managed output and reject empty source files. Only then replace the
  old retained backup.
- Publish by recording which replacement paths were successfully copied. Copy
  each staged file to a canonical-directory sibling temp file, fsync it, then
  rename it over the canonical filename; never truncate a canonical inode in
  place. After all replacements land, remove exact prior-manifest paths absent
  from the replacement set and atomically install the replacement manifest.
- On any copy or stale-removal error, remove only newly introduced successfully
  copied paths, restore every exact prior-manifest path from the retained backup,
  remove replacement-only paths, and preserve every unrelated path. If no prior
  manifest/output existed, restore the original empty managed state; partial
  salvage remains #143. Propagate both publication and restoration context if
  restoration itself fails.
- Remove per-run staging on all ordinary Complete/NoOutput/error returns.

### 2. Make `concert_split::run` stage, validate, and publish

- Extend `SplitPhase` with `ValidateOutput` and `Publish` so both adapters expose
  the new work through typed progress.
- Keep `ConcertSplitRequest.output_dir` as the canonical directory. Create a
  staging guard after request validation and route metadata/cutting output to
  its path.
- Move stale-interlude cleanup behind the staging seam; it may only inspect the
  fresh staging directory, never mutate canonical output before publication.
- Derive the exact expected replacement filenames from the set list,
  `OutputFormat`, derived interludes, and whether analysis writes
  `timestamps.json`. Validate that every expected file exists, is a regular
  file, and has non-zero length; verify analyzed timestamps count/title order
  against the set list without FFprobe.
- Before cutting, require supplied/embedded timestamps to have exactly the set
  list cardinality and title order. Derive a one-to-one expected filename set
  for every requested output format and reject duplicate titles or distinct
  titles that sanitize to the same canonical stem. Reject unexpected outcome
  mismatches such as a `ProducedTrack` set that does not cover the planned
  songs/interludes.
- Call the publication module only after validation. Return
  `ConcertSplitOutput.output_dir` as the canonical directory, not staging.
- Preserve analysis-only behavior: it may publish `timestamps.json` when the
  existing workflow writes it, but it remains `NoOutput::AnalysisOnly` and does
  not claim track completion.

### 3. Keep both adapters on the shared behavior

- Update `concert-tracker/src/jobs/split.rs` so pre-run stale-interlude deletion
  is removed; canonical cleanup now belongs exclusively to successful library
  publication shared by both adapters.
- Confirm the CLI continues passing the canonical directory into the same
  library interface and the in-process adapter does likewise.
- Adjust adapter tests only where typed phase/output expectations changed.
- Keep `concert.json` outside the Published Concert Split manifest: it is
  scraped metadata, not generated split output. Its existing adapter-owned copy
  remains after `run`; document that a `concert.json` failure can fail the Job
  Run after media publication and that #142's lock protects split media only.

### 4. Exclude application consumers during publication

- Add shared-lock acquisition to `concert-tracker/src/concert_media.rs` at the
  Concert Media Inventory seam so playback, reconstruction, track lookup, and
  web media reads cannot observe the exclusive publication interval.
- Audit direct filesystem reconciliation in `scan.rs`, split completion fact
  gathering, split-timestamp media lookup, prepare/recovery checks, and lifecycle
  deletion. Each multi-file observation/mutation holds one shared lock for the
  whole logical read; avoid separately locking individual free-function calls
  where that would permit a mixed snapshot between calls.
- Wrap `/concert-files` serving with shared-lock acquisition off the Tokio worker
  pool (`spawn_blocking`) and hold the guard until `ServeDir` has opened the
  requested file and constructed its response. Publication replaces files by
  sibling-temp rename rather than in-place truncation, so the opened file handle
  remains a stable old-or-new inode while the response streams after the guard
  is released. If the serving implementation cannot guarantee the handle is
  opened before response construction on a supported platform, retain the guard
  in a response-body wrapper through end-of-stream instead.
- Keep source-media-only reads outside the split-output lock when they do not
  inspect or mutate published split files.
- Add a concurrency test that blocks publication after the first canonical copy
  and proves the real `/concert-files` media-serving seam cannot open a mixed
  canonical file until publication completes. Also prove two different concert
  directories do not contend and that an already-opened handle streams stable
  bytes across a later rename.

### 5. Tests — red/green vertical slices

1. **First publication:** a supplied-timestamp split writes only staging during
   cutting (the progress callback asserts canonical absence on each
   `TrackCompleted`), then exposes the full non-empty canonical set and leaves no
   staging or backup containing fictitious prior output.
2. **Replacement and backup:** a second split backs up the first canonical set,
   publishes new canonical bytes, and retains exactly one complete backup.
3. **Obsolete cleanup:** replacement with fewer/different tracks/interludes
   removes old managed output but preserves source media, `concert.json`, and
   preview/unrelated files.
4. **Validation failure:** focused validator tests cover missing/empty output,
   extra/missing/reordered timestamps, duplicate titles, sanitized-name
   collisions, and output/manifest mismatches; interface tests prove validation
   prevents publication and leaves canonical output/backup unchanged.
5. **Publication failure:** deterministic failures during backup copy,
   replacement copy, stale removal, manifest write/fsync/rename, and rollback
   assert the exact captured prior set is restored, replacement-only paths are
   removed, unrelated files survive, and incomplete backup candidates never
   rotate into place.
6. **Concurrency:** two publishers for one concert serialize at the publication
   lock; a shared-lock consumer blocks through the exclusive copy interval; and
   different concert directories do not contend.
7. **Adapter parity:** CLI and library paths both exercise the shared staging
   and canonical publication behavior.

### 6. Documentation

- Update `docs/concert-split.md` with the staging/publication/backup state
  diagrams, exact manifest ownership, validation contract, shared/exclusive lock behavior, and
  the remaining crash-recovery limitation assigned to #144.
- Update `docs/data.md` only if the hidden on-disk working directories or backup
  need inclusion in the canonical filesystem description.
- Add `docs/change/2026-07-21-concert-split-publication.md` with implementation,
  tests, review findings, and manual verification. This plan remains the
  pre-implementation record and will link to the final Change Record.
- Check README links and avoid duplicating the canonical design outside
  `docs/concert-split.md`.

## Verification

Automated, run frequently from narrow to broad:

```sh
cargo test -p live-set-splitter concert_split
cargo test -p concert-tracker jobs::split
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
just test-rs
```

Manual verification uses a separate temporary database, workdir, and port:

1. Start `concert-web` in default library mode; perform a timestamp-driven
   first split and inspect that canonical media appears only as a complete set.
2. Resplit with changed timestamps; verify playback remains available, the new
   canonical output is complete, and exactly one prior backup exists.
3. Repeat in `--splitter cli` mode to confirm the same on-disk behavior.
4. Induce a normal validation/publication failure with a scratch fixture;
   verify the previous canonical split remains playable and no staging output
   is treated as canonical.
5. Verify source media, `concert.json`, preview images, and unrelated files are
   preserved through replacement.

Use Playwright only if visible/interaction behavior changes; #142 is expected
to retain the existing UI and HTTP contracts, so backend/API and filesystem
verification are the primary live surfaces.

## Checklist

- [x] Public-interface tests fail before implementation.
- [x] Staging guard and output validation implemented.
- [x] Publication lock, backup rotation, copy, stale cleanup, and rollback implemented.
- [x] Published-output manifest and conservative legacy adoption implemented.
- [x] Concert Media consumers take shared publication locks.
- [x] Concert Split workflow routes all generated output through staging.
- [x] Legacy pre-run canonical cleanup removed from the Job Run adapter.
- [x] Library and CLI adapter tests pass.
- [x] Technical documentation and final Change Record updated.
- [x] Adversarial code review completed and findings resolved.
- [x] Full Rust test/lint suite passes.
- [x] Manual live-server/API verification completed on scratch state; real-FFmpeg
      adapter coverage remains automated for deterministic assertions.
- [x] Follow-up review completed after verification changes.
- [ ] Commit references the final Change Record and #142.
- [ ] Pull request targets the parent/integration branch and resolves #142.
- [ ] GitHub Actions CI passes.
