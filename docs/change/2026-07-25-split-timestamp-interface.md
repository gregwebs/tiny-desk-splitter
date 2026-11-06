# Split timestamps behind Concerts

Issue [#166](https://github.com/gregwebs/tiny-desk-splitter/issues/166) moves
split-timestamp reads, desired updates, and reset behind `Concerts`. It continues
[Idempotent, interface-owned concert media deletions](2026-07-25-idempotent-concert-deletions.md).

## Changes

- Added `Concerts::split_timestamps`, `set_split_timestamps`, and
  `reset_split_timestamps`. The change methods return `SplitStart` (the immediate
  outcome plus the refreshed Concert State), so an adapter needs no second query.
- `Concerts` now holds the `JobConfig` it needs to admit Split Job Runs.
  `Concerts::without_workers` (the one-shot CLI) has none and reports an
  operational error instead of pretending to accept work.
- The split-timestamp workflow's outcomes are now interface rejections:
  `CommandRejection::Conflict` (409) and `CommandRejection::Invalid` (422),
  alongside `NotFound` (404); operational failures stay server errors.
- Timestamp changes refuse an *unobserved* source file rather than treating it as
  absent: `Concerts` checks the observed source media before asking the workflow
  to start a split.
- Migrated the three split-timestamp HTTP handlers to the interface; the
  adapter's workflow-error mappers were replaced by mappings from
  `ConcertCommandError`.

## Preserved behaviour

Routes, request payloads, response bodies, and status codes are unchanged,
including 202 for an accepted split, 409 for a source that is not downloaded or a
split already running, and 422 for validation failures. The editor's routes and
markup are untouched.

## Also fixed in this slice

- `db::concerts::set_concert_status` now compares the *stored flags* rather than
  the derived status, so a row whose flags were both set (a manual edit, or data
  from an older schema) is repaired when Ignored is stored. Previously the
  idempotent path returned early and left the stale flag set.
- `concert-db list --filter all` keeps its meaning: every concert, ignored ones
  included. The catalog gained `ConcertFilter::Everything` for it, because the
  web listing's default (`All`) deliberately hides ignored concerts.
- Command responses render the Concert State the command returned, instead of
  re-observing it: the ignore/want/notes and deletion handlers no longer perform
  a second `Concerts::get`.

## Tests

- `concerts.rs` covers timestamp reads, absent-concert rejections for all three
  methods, refusal of an unobservable source, validation failures as `Invalid`,
  and the one-shot process reporting it cannot admit work.
- `db/concerts.rs` gained a malformed-flag regression test for
  `set_concert_status`.
- `concerts/catalog.rs` covers `ConcertFilter::Everything`.
