# Contracting the legacy concert paths

Issue [#168](https://github.com/gregwebs/tiny-desk-splitter/issues/168) is the
last slice of the Concert application interface
([#158](https://github.com/gregwebs/tiny-desk-splitter/issues/158)). It removes
what the earlier slices superseded, verifies that the adapters now interact with
concerts only through the interface, and records the result. This Change Record
summarizes the whole sequence.

## The slices

| Issue | Change Record | What moved |
| --- | --- | --- |
| #159 | [Canonical Concert State](2026-07-23-canonical-concert-state.md) | `Concerts`, `ConcertState`, observation semantics, `PermittedActions` |
| #160, #161 | [Concert Catalog and status](2026-07-25-concert-catalog-and-status.md) | `ConcertCatalog`, one card over Concert State, desired-status commands |
| #162 | [Playback, listen, like](2026-07-25-playback-listen-and-like.md) | presentation-neutral media resolution, explicit listen recording |
| #165 | [Idempotent deletions](2026-07-25-idempotent-concert-deletions.md) | deletion commands and confirmation, canonical media mutation |
| #166 | [Split timestamps](2026-07-25-split-timestamp-interface.md) | timestamp read/desired/reset, typed rejections |
| #163, #164 | [Job Requests and scrape](2026-07-25-job-request-interface.md) | Job Request admission, cancellation, scrape admission |
| #167 | [Archive and unarchive](2026-07-25-archive-interface.md) | archive admission, whole unarchive workflow |
| #168 | this record | removal, contract, verification |

## Superseded paths removed

- Listing-only card projection and its field plumbing; `render_row*`,
  `has_archive_location`, `matches_filter`, `matches_split_filter`, and the
  handler-level filter tests.
- `tracks_busy`, `split_queued`, and `scrape_pending` derived in adapters from
  `Concert` plus the Job Registry.
- `locate_full_concert_file` and the handler-side reconstruction of canonical
  media paths; the unreachable `tracks_from_events` fallback.
- `toggle_ignored` / `toggle_wanted` / `toggle_track_liked`: toggles replaced by
  desired-state commands (`set_concert_status`, `set_track_liked`).
- `remove_file_if_present`'s fixed-extension deletion, replaced by
  `concert_media::{remove_track_files, remove_interlude_files}`.
- `concat_thumbnails` / `divide_thumbnails` are still not exposed as commands, and
  no JSON HTTP adapter, subscription, compound filter, search, or pagination was
  introduced.

## The contract

`docs/concerts.md` is the interface contract. It now covers observation
semantics, active-work consistency, every command group (status, notes, playback,
listen, like, deletion, Job Requests, archive, scrape, split timestamps), the
Catalog, the adapter boundary, and the test seam, with a
[state-change diagram](../concerts.md#state-changes) and the two retry invariants
behind it (desired state instead of toggles; idempotent media commands). The
README links the diagram, the Catalog rationale, and the boundary.

The boundary is explicit about what stays outside: settings, Failed Job history,
and the one-shot CLI's table-wide batch row selection. Everything that describes
or commands a single concert goes through `Concerts` or `ConcertCatalog` —
including `concert-db resplit`, which now admits its split requests through the
interface and reports `JobAdmission`.

## Behaviour

User-visible behaviour is unchanged, including:

- every route, form, and HTMX swap target, and every status code — 202 for an
  accepted split, 409 for a source conflict, 422 for validation, 404 for absent
  media, 500 for a failed opener or an unusable archive request;
- auto-split after download, double-click safety, and the queued split edge;
- cancellation producing exactly one terminal outcome and Failed Job history;
- the background scrape queue's placeholder-and-poll behaviour.

Two deliberate changes were made earlier in the sequence, both approved by their
issues: retried commands are idempotent rather than toggles (so a retried request
cannot reverse a user's choice), and deleting a split that no longer exists is a
no-op instead of a 400.

## Review findings folded in

An adversarial review of the whole branch (read-only, over the diff since
`origin/main`) found six things worth fixing, all in this Change Record's scope:

- the hurl contract still expected the pre-#165 400 for deleting a split that is
  not there;
- `listen`/`watch` on an absent concert lost its 404 to a 200 button state;
- `delete_redundant_source` trusted `downloaded_at` instead of the file on disk,
  so a hand-restored source could be reported deleted while staying there (now a
  regression test);
- `prepare-status` changed its JSON cardinality when the published split could
  not be observed;
- Test Control still read concert tables and the Scrape Queue directly;
- `JobAdmission` mislabelled an already-active chain and an unsatisfiable split.

A second, black-box pass ran every `hurl/*.hurl` suite against a real
`concert-web` process and found one more:

- `POST /concerts/{id}/split-timestamps/reset` had been routed through the
  "can the source file be observed" guard, so resetting timestamps on a concert
  with no downloaded source answered `409` where it used to report
  `422` ("no automated timestamps") or `200` ("already auto"). Reset never reads
  the source — it drops the desired timestamps and replays the automatic ones —
  so the guard belongs only on the set path, where the workflow's own
  "Source file not found" conflict already answers for an absent file.

One finding was recorded rather than fixed: cross-kind exclusion while work is
active (a delete during a Job Run, an archive during a split) is a pre-existing
gap that needs a persistent guard, not a change to this interface. See
[docs/concerts.md](../../concerts.md#state-changes).

## Verification gates

These are the gates for this sequence; CI runs the automated ones on the pull
request.

- `just fmt-check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test` for the workspace.
- Hurl suites: `archive_lifecycle`, `listing_status`, `detail_prepare_notes`,
  `job_chain`, `media_info_navigation`, `media_state_errors`,
  `media_files_lifecycle`, `concert_playback`, `scrape_pending`,
  `split_timestamps_flow`, `split_timestamps_state`, `playlists`,
  `test_control_adapter`.
- Playwright e2e, TypeScript type-check, and the JS test suites.
- A manual pass against a separate database, work directory, and port covering
  listing filters, metadata scrape, desired-status retries, download and split
  chains, Job Request cancellation, playback, deletions, split timestamps,
  archive and unarchive — the interactions whose feel no automated suite captures.

### Manual pass

Run on a scratch database, work directory, and port, driving the routes as a
person would and reading the rendered cards rather than only the status codes.
Everything behaved as this document describes:

- The listing filter chips keep their meanings, Ignored concerts stay out of All,
  and the Archived chip degrades to All until an archive location is configured.
- The card's Want/Ignore buttons keep the toggle shape the ticket asked for and
  still leave the interface receiving a desired status: ignoring a Wanted
  concert clears Wanted, and the pair can never coexist.
- A repeated *interface* command is the no-op the contract promises, while a
  repeated *click* flips the card back, which is what the ✕/Clear labels say it
  will do.
- Notes persist to the detail page; deletions report the same state on a second
  call; a blocked Job Request cancels to `cancelled by user` and a second cancel
  is a no-op; track like/unlike, listen/watch, and media navigation all answer.
- Split timestamps round-trip against a real media file, reject a wrong song
  count with 422, and answer `already-auto` when there is nothing to reset.

Two things the pass could not exercise, both environmental rather than
behavioural: a metadata scrape needs egress to the concert's source URL (it fails
with a 500 and leaves the row unchanged), and the seeded media files are
sentinels, so the real splitter needs one generated with `ffmpeg` first.

It found one gap, since closed: archive and unarchive had no automated coverage
at all. They deliberately keep the endpoint's own protocol — a 500 for a request
the concert cannot satisfy, as the legacy handler did — so `hurl/archive_lifecycle.hurl`
now pins that protocol, along with the lifecycle itself, the Archived filter,
`NothingToArchive`, and the settings round-trip that turns the chip on and off.
