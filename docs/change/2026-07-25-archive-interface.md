# Archive and unarchive behind Concerts

Issue [#167](https://github.com/gregwebs/tiny-desk-splitter/issues/167) moves
archive admission, archive cancellation (already covered by
`Concerts::cancel_job`), and the whole unarchive workflow behind `Concerts`. It
continues [Job Requests and scrape behind Concerts](2026-07-25-job-request-interface.md).

## Changes

- `Concerts::request_archive` owns archive admission: it reads the configured
  archive location, refuses with `Conflict` when there is none, admits the Job
  Run through the Job Run engine, and reports `ArchiveOutcome::{Accepted,
  NothingToArchive}` with the refreshed Concert State.
- `Concerts::unarchive` owns the entire unarchive workflow: eligibility (archived,
  no archive Job Run in flight, an album to restore to), the blocking filesystem
  work, `clear_archive_state` on success, and `mark_archive_failed` plus a Failed
  Job entry on failure.
- The `archive` and `unarchive` HTTP handlers are now transport translation only:
  they call one interface method and answer `HX-Refresh`.

## Deliberate non-changes

- Unarchive stays outside the Job Run protocol. It registers no Job Run, so it
  also gains no recovery, cancellation, or admission racing semantics. That is
  the ADR-0006 distinction preserved: the Job Run engine owns Job Runs, not every
  long operation.
- Both endpoints still answer **500** for requests the concert cannot satisfy
  (archive location not configured, concert not archived, archive in flight, no
  album). Those answers are arguably wrong status codes, but they are this
  endpoint's existing behavior; the adapter maps the interface's rejections onto
  the same status so nothing user-visible changes. The message text now names the
  concert the way every other rejection does.

## Tests

- `concerts.rs` covers: an archive request without a configured location is a
  rejection; a concert with no media has nothing to archive; unarchive moves the
  media back, replaces the symlink, and clears the lifecycle; unarchiving a
  concert that is not archived is a rejection; a missing symlink records a Failed
  Job and repeats cleanly on retry.
- Engine-level coverage of archive admission, cancellation, and failure stays in
  `jobs::archive`.
