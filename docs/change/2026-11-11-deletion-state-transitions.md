# Deletion state transitions, publication locking, and manifest truth

Issue [#165](https://github.com/gregwebs/tiny-desk-splitter/issues/165) asks that
concert media deletions be explicit `Concerts` methods backed by Concert Media
Inventory, safe to retry, respectful of publication locking and manifest
ownership, and returning the resulting Concert State. It continues
[Idempotent, interface-owned concert media deletions](2026-07-25-idempotent-concert-deletions.md),
whose implementation landed with the #177 contraction.

## The audit

Auditing the ticket's acceptance criteria against `main` found the interface
itself complete — the five `Concerts::delete_*` methods exist, are idempotent for
a valid absent target, reject invalid identities, and the handlers are thin
adapters — and three gaps in what it promised:

- **Concert Media Inventory did not own interlude mutation.** `remove_interlude_files`
  existed but nothing called it: `lifecycle::delete_interlude` still rebuilt the
  concert directory, the `interlude_NN` stem and the extension list itself, the
  duplicate of the discovery logic the change record of #177 claimed to have
  removed.
- **No mutation held the publication lock.** Canonical split output is owned by
  the Concert Split publication journal and manifest (ADR-0007), and every read
  of it already took the shared lock. Deletion took none, so it could remove
  files while a publication was rewriting them.
- **Deletion left the manifest claiming files that no longer exist.** The
  published manifest names exactly the files that belong to a Published Concert
  Split, and `Publication::publish` refuses to back up a prior generation whose
  named file is missing (`prior Published Concert Split file is missing or
  empty`). Deleting a track or interlude therefore made every later re-split of
  that concert fail — and a re-split is exactly how the UI restores a deleted
  track. The Recoverable Partial Split marker has the same shape of problem: a
  retry validates it, and a deleted salvaged file fails that validation.

The e2e fixture never writes a manifest, so `automate-splitting.spec.js`'s
"clicking a deleted track re-splits and restores it" passed while the production
path was broken.

## Changes

The mutation seam and its tests changed; no production route, payload or markup
did, and no stored field changed meaning. One response is new: a track or
interlude deletion that a Concert Split publication is holding answers 409
instead of deleting files out from under it, which used to be possible.

| Area | Change |
| --- | --- |
| `publication.rs` | `SharedPublicationLock` carries the canonical directory it guards and gains `try_acquire`, which reports a publication in progress instead of waiting for it. `ExclusivePublicationLock` becomes the one type `publish`, `publish_partial` and `recover_publication` use, replacing three hand-rolled lock acquisitions. New `forget_removed_split_files(&guard, removed)` reconciles the Published manifest and the Recoverable Partial Split marker with what is on disk: the files this deletion removed are dropped, a partial track whose output is gone leaves the marker, and the marker itself goes when no partial output is left. It also drops an entry whose file is absent for any other reason, so a deletion that removed the files but could not rewrite the manifest is repaired by the next request rather than reporting success over a stale manifest. The rewrite uses a unique temporary name, because — unlike a publication's own installs — two of these can be in flight for one directory. It takes the guard so it cannot run during a publication, and it never invents a manifest. |
| `concert_media.rs` | `remove_track_files` and `remove_interlude_files` become the one place application code removes canonical split media: they take the shared publication guard without waiting, remove the files canonical discovery names, and keep both manifests true — including when a removal fails part-way, so a file that is gone stops being claimed either way. A new `MediaRemovalError` separates `PublicationInProgress` from `Failed`. |
| `lifecycle.rs` | `delete_interlude` delegates to `remove_interlude_files` instead of rebuilding the path, stem and extension list. `delete_track` and `delete_interlude` report `MediaRemovalError`, so "a publication holds this concert" stays distinct from a fault. |
| `concerts.rs` | `media_removal_error` maps that distinction onto the interface's vocabulary: a publication in progress is a `CommandRejection::Conflict` (retry when it ends), a removal failure is `Operational`. |
| `web/handlers.rs` | No handler change: the existing mapping already turns `Conflict` into the 409 the coverage gate uses. |

## Deletion state transitions

| Command | Recorded effect |
| --- | --- |
| `delete_split` | clears the recorded split; media untouched, so the concert can be split again |
| `delete_track` | clears the recorded split only when no recorded track is left, otherwise stays Split with that track unavailable |
| `delete_interlude` | records `interlude_delete`; never changes the lifecycle |
| `delete_download`, `delete_redundant_source` | clear the recorded download; the split record survives |

A repeated request for a valid absent target records nothing further. A track or
interlude removal that fails part-way records nothing at all — no event, no
lifecycle change — so the audit trail never claims a deletion that did not
finish.

## Preserved behaviour

- Routes, card swaps, `HX-Retarget` headers, the interlude sidebar fragment, and
  the 409 coverage-gate body are unchanged.
- `delete-split` still answers with the card for a concert with no split state
  (the approved retry semantics), never a 400.
- An absent concert is still a 404 on every deletion route, and a concert with no
  album still answers an interlude deletion with the historical 404
  (`MissingMedia`), not a new status.
- Source deletion is unchanged: the source file is not part of the published
  manifest, so it needs no guard. Excluding work in flight for the same concert
  is issue #178, not this ticket.
- The publication lock remains fail-closed for reads (30s shared acquisition); a
  deletion's own acquisition is non-blocking, so a request cannot hang the
  handler behind a publication.
- The deletion guard is shared, so a stream in progress never blocks a deletion.
  That leaves two deletions unserialized by the lock. In the supported
  configuration they are serialized by the database lock instead, which
  `Concerts` holds across the file removal and the manifest rewrite, and
  `Concerts` is the only path that deletes canonical media (no CLI command does).
  Two `concert-web` processes sharing one workdir are already unsupported —
  SQLite plus the in-memory Job Registry and Scrape Queue do not tolerate it —
  and the manifest rewrite's unique temporary name means such a race could at
  worst lose one update, which the next deletion's reconciliation repairs,
  rather than install a half-written manifest.

## Tests

Interface level (`concerts.rs`):

- `every_deletion_rejects_an_absent_concert` — all five methods reject an invalid
  concert identity with `NotFound` rather than reporting a successful retry.
- `deleting_an_interlude_from_a_concert_without_an_album_is_a_rejection` — the
  preserved rejection behind the interlude route's historical 404.
- `deleting_a_track_during_a_publication_is_a_conflict_and_a_retry_succeeds` —
  the locked outcome, and that the media is untouched until the retry.
- `a_failed_track_removal_is_a_fault_and_records_no_deletion` — a removal that
  deleted the first canonical file and then failed is `Operational`, records no
  `track_delete` event, and leaves the track recorded as present.
- `assert_is_current_state` now pins that every deletion's returned state *is* the
  fresh observation, for the delete, its retry, and the confirmed repair.

Mutation seam (`concert_media.rs`):

- `removing_a_track_keeps_the_published_manifest_true` — publish, delete a track,
  and publish again; the manifest no longer names the removed file and the
  re-split succeeds.
- `removing_media_during_a_publication_is_refused` — the guard, not a timeout.
- `a_partial_removal_is_an_error_rather_than_a_silent_success`.
- `removing_an_absent_track_is_a_successful_no_op`,
  `removing_an_interlude_byte_extension_matches_discovery`.

Publication (`publication.rs`):

- `try_acquire_reports_a_publication_in_progress`,
  `forgetting_a_removed_file_keeps_a_resplit_possible`,
  `forgetting_a_removed_file_keeps_the_partial_manifest_true`,
  `a_retry_repairs_a_manifest_a_failed_deletion_left_stale`,
  `forgetting_files_without_a_published_split_creates_no_manifest`.

Mutation checks, each reverted afterwards:

- dropping the `forget_removed_split_files` call fails
  `removing_a_track_keeps_the_published_manifest_true`;
- falling back to a blocking acquisition when the publication is in progress
  fails both locked tests (after the 30s lock timeout, which is exactly why the
  non-blocking acquisition exists);
- reporting `PublicationInProgress` as an operational failure instead of a
  `Conflict` fails
  `deleting_a_track_during_a_publication_is_a_conflict_and_a_retry_succeeds`;
- returning the state observed before the deletion fails
  `deleting_split_state_is_idempotent`.

Adapter level (`web/handlers.rs`):

- `deletion_routes_answer_404_for_an_absent_concert` — all five routes.
- `delete_split_on_a_concert_without_a_split_swaps_the_card` — safe retry
  semantics over HTTP.
- `the_source_coverage_gate_answers_409`.
- `delete_track_answers_with_the_refreshed_card` — the card, and the split controls
  it stops offering once the last track is gone.

## Documentation

- `docs/concerts.md` — a deletion state diagram, and what a deletion may refuse
  (including the publication-lock conflict, and a part-way removal being a fault).
- `docs/concert-split.md` — application-side deletion holds the publication lock
  shared and keeps both manifests naming only files that exist.

## Review follow-ups

An adversarial review of this change raised four things that were fixed and two
that were not:

- A failed manifest rewrite left a state no retry could repair, because the files
  were already gone. Fixed by making the rewrite a reconciliation against what is
  on disk (`a_retry_repairs_a_manifest_a_failed_deletion_left_stale`), and by
  syncing the directory after removing the partial marker.
- The manifest rewrite used a fixed `.next` temporary path, so two writers could
  interleave into it. Fixed with a unique temporary file plus an atomic rename.
- The new transition diagram claimed `delete_redundant_source` moves
  `Downloaded → Scraped`, which the coverage gate makes impossible.
- The change record claimed no status code changed, while a locked deletion now
  answers 409.
- Not changed: deletions do not take an exclusive lock. Readers hold the same lock
  shared while they stream, so an exclusive deletion would 409 whenever the player
  is streaming that concert; the reasoning and the single-process constraint are
  documented in `remove_canonical_media`, `docs/concerts.md`, and above.
- Not changed: the canonical media name stays a `&str`, so that discovery and
  mutation keep one vocabulary.

## Verification

Artifacts were built in a scratch target dir under `/tmp`
(`CARGO_TARGET_DIR=/tmp/tds-target`): the checkout volume holds 24 G of `videos/`
out of 27 G, so an in-repo debug link fails. `Dockerfile.dev` documents the recipe.

- `cargo fmt --all -- --check` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — exit 0
- `cargo nextest run --tests` — **957 passed, 0 skipped** (939 before this change)
- `cargo nextest run --tests -p concert-tracker --features test-control` —
  **798 passed, 0 skipped**
- `node scripts/hurl-test.js` — 14 files, 298 requests, 100%
- `npx playwright test` — 173 passed
- `./scripts/shellcheck.sh`, `./scripts/ts-check.sh`, `./scripts/ts-lint.sh` —
  exit 0

One hurl run failed `archive_lifecycle.hurl` while the Playwright suite was
running concurrently: the archive Job Run did not finish inside that file's
1.5 s retry window, and because the file leaves a fixed
`/tmp/tds-hurl-archive` scratch directory behind when it aborts, every later run
of that file failed until the directory was cleared. Both reproduce without this
change (the file is not hermetic and its scratch path is shared), and the suite
passes 14/14 on its own.
