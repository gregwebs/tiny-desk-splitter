# Job Requests and scrape behind Concerts

Issues [#163](https://github.com/gregwebs/tiny-desk-splitter/issues/163) and
[#164](https://github.com/gregwebs/tiny-desk-splitter/issues/164) move download
and split Job Requests, cancellation, and scrape requests behind `Concerts`. They
continue [Split timestamps behind Concerts](2026-07-25-split-timestamp-interface.md).

## Changes

- `Concerts::request_download`, `request_split`, `prepare`, and `cancel_job` own
  Job Request admission. Each returns the Concert State observed after persistent
  admission, so an adapter re-renders without re-observing.
- The download handler's policy lives in the interface: scrape when metadata is
  absent, then decide between the download → split chain and a plain download,
  including the "tracks exist but the source is gone" and "set list vanished
  between the check and preparation" cases.
- `Concerts::cancel_job` returns `JobCancellation::{Cancelled, NothingActive}`
  with the refreshed state; the adapter only redirects.
- `Concerts::scrape` and `ensure_scraped` own metadata refresh. Automatic scrape
  keeps its tolerate-failure behavior; an explicit rescrape reports the failure.
- `PrepareStatus` is now a projection of Concert State: the download/split
  statuses, `active_work.split_queued_after_download`, and the observed
  per-track presence. The adapter no longer reads the media inventory or the Job
  Registry to build it.
- Migrated the `download`, `split`, `prepare`, `prepare-status`, `cancel` and
  `rescape` (`scrape`) HTTP handlers. `start_download`/`start_split` imports and
  the handler-side scrape helpers are gone from the adapter.

## Preserved behaviour

Routes, payloads, and status codes are unchanged: 200 with a card fragment for
download/split, 200 with `PrepareStatus` for prepare, 422 with the same message
for a concert with no set list, 200 with `HX-Redirect` for cancel (including when
there was nothing active to cancel), and 200 with the rendered card for rescrape.
Double-clicking download still keeps the queued split edge, a concert with no set
list still downloads plainly, and a split request for an undownloaded concert is
still accepted and does nothing.

## Tests

- `concerts.rs` covers: a download with a set list queues the split; a download
  without one queues nothing; a repeated download keeps the queued split; a split
  request without a source is accepted and does nothing; cancellation reports
  `Cancelled` then `NothingActive`; `prepare` admits the chain and rejects a
  concert with no set list as `Invalid`; every Job Request rejects an absent
  concert as `NotFound`; a one-shot process reports that it cannot admit work.
- The existing engine-level tests in `jobs::{download, split, prepare}` continue
  to cover dependency edges, failure recording, and retries.
