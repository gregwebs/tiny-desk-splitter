# Concert Catalog browsing and Concert Status commands

Issues
[#160](https://github.com/gregwebs/tiny-desk-splitter/issues/160) (serve existing
browsing through Concert Catalog) and
[#161](https://github.com/gregwebs/tiny-desk-splitter/issues/161) (move Concert
Status and notes commands behind `Concerts`) are the second and third slices of
the deep concert application interface specified by parent
[#158](https://github.com/gregwebs/tiny-desk-splitter/issues/158). They follow
[Canonical Concert State](2026-07-23-canonical-concert-state.md).

## Changes

- Added `ConcertCatalog` (`concerts/catalog.rs`), the collection-browsing
  interface. `browse(ConcertFilter)` returns canonical `ConcertState` values;
  the filters keep their existing meanings (All, Wanted, Available, Ignored,
  Downloaded, Split, Archived) and Archived still degrades to All when no
  archive location is configured.
- Moved the HTMX listing (`GET /`) and `concert-db list` onto the catalog. Both
  previously carried their own copy of the filter predicates, and the HTMX page
  derived card actions from a listing-only projection; the CLI's copy only knew
  three of the six filters.
- Unified card rendering: listing cards, cards swapped in place after a command,
  and the detail page card are now one template over Concert State
  (`CardVariant` chooses image size, embedded track list, and whether the
  background-scrape placeholder can appear). Removed `render_row`,
  `render_row_inner`, `render_row_with_actions`, and the action policy they
  re-derived from a `Concert`, plus the `tracks_busy`/`split_queued`/
  `scrape_pending`/`has_archive_location` helpers that duplicated Concert State
  facts.
- Added `Concerts::set_status` and `Concerts::set_notes`. Both are desired-state
  commands that return the resulting Concert State; `set_status` writes both
  stored flags from one `ConcertStatus`, so Wanted and Ignored cannot coexist,
  and repeating a request is a no-op that records no event.
- Added `ConcertCommandError`/`CommandRejection`, so an expected rejection is
  distinct from an operational failure. The HTMX adapters map a rejection to a
  client error and a failure to a server error.
- Moved the cached preview/thumbnail cleanup for an ignored concert from the
  `ignore` handler into `Concerts::set_status`, behind
  `scrape::remove_cached_images`.
- Replaced `db::concerts::toggle_ignored`/`toggle_wanted` with
  `set_concert_status`, which is idempotent and records one event per
  transition.
- Added `Concerts::without_workers` and `ScrapeQueue::detached` for one-shot
  CLI processes: no Job Registry and no scrape worker exist there, so active
  work is only what persistence records.
- Added `db::split_timestamps::list_user_split_timestamps` so catalog browsing
  reads the whole collection's timestamps in one query.

## Preserved behaviour

- HTMX routes and markup are unchanged. `POST /concerts/:id/ignore` and
  `POST /concerts/:id/want` still toggle, because the markup posts the same
  route from the badge's ✕; the adapter translates the click into the desired
  status and `Concerts` stores it. `concert-db ignore`/`want` keep their
  toggle behaviour for the same reason.
- Event streams for a status change are unchanged: entering a status records it,
  leaving one records its delete, and a direct Ignored/Wanted swap records only
  the status entered.
- Filters, month grouping, ordering, and page structure are unchanged.

## Deliberate behaviour change

The listing card's actions now come from Concert State rather than a listing-only
projection. Two visible consequences, both of which remove a disagreement between
the listing and the detail page:

- "Play concert" appears when the concert can actually be played — source file
  present, or a Published Concert Split available for reconstruction — instead of
  whenever `downloaded_at` is set. A concert whose source was deleted as
  redundant keeps its Play button; one marked downloaded whose file vanished
  loses it.
- Actions that conflict with in-flight work (download, delete, archive) are
  suppressed on the listing while a Job Run is active, exactly as on the detail
  page.

The destructive "source is redundant" button stays off listing cards, as before.

## Tests

- `concerts/catalog.rs` covers every filter meaning, the Archived degradation,
  slug parsing, collection-wide observation, and operational failures.
- `concerts.rs` gained status-command and notes tests: mutual exclusivity,
  idempotent retries, typed NotFound, refusals, and operational errors.
- `db/concerts.rs` covers `set_concert_status` flag writing, idempotency, and
  one-event-per-transition.
- `web/handlers.rs` card tests were rewritten against Concert State; the
  handler-level `matches_filter` tests were deleted because filter policy is
  asserted once, against the catalog.
