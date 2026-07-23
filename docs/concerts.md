# Concert application interface

`concert-tracker::concerts::Concerts` is the presentation-neutral application
interface for one concert. It is long-lived and shared by adapters. The HTMX
detail page is its first caller; later slices will move individual commands and
other adapters behind the same interface.

`ConcertState` is the canonical point-in-time description of a concert. It
combines:

- persisted metadata and download, split, and archive lifecycle;
- persisted per-track presence and like state;
- independently observed source and Published Concert Split media;
- persisted and in-memory active work;
- whether archiving is configured;
- the actions currently permitted by those facts.

Event history and Failed Job history are intentionally excluded. They are
unbounded audit data and are loaded explicitly through
`Concerts::event_history` and `Concerts::failed_job_history`.

## Observation semantics

Filesystem facts use `Observation<T>`:

- `Known(present/absent)` means the relevant directory was successfully
  observed.
- `Unknown(reason)` means the application could not safely distinguish
  presence from absence.

Source media and Published Concert Split media are independent observations.
For example, a publication-lock failure makes split media Unknown while a
successful source-directory scan can still prove the source is present.
Actions that require an Unknown fact are unavailable. Operational persistence
errors are not domain observations: they propagate as
`ConcertQueryError::Operational`.

```text
Concerts::get(id)
  |
  +--> DB lock: concert + timestamps + settings
  |       `--> absent row -------------------------------> NotFound
  |
  +--> fallible source scan
  |       +--> readable --------------------------------> Known
  |       `--> I/O failure ------------------------------> Unknown
  |
  +--> shared Published Concert Split lock + scan
  |       +--> readable --------------------------------> Known
  |       `--> lock/I/O failure -------------------------> Unknown
  |
  +--> ScrapeQueue + JobRegistry ------------------------> ActiveWork
  |
  `--> pure policy --------------------------------------> PermittedActions
```

The result is internally consistent at observation time, not a global atomic
snapshot across SQLite, the filesystem, and in-memory registries. Future
commands must re-read their own preconditions.

## Active-work consistency

For each Download, Split, and Archive Job Run, `JobActivity` retains both the
persisted `*_started_at` fact and `JobRegistry::is_running`. Either one counts
as active. This fails closed during admission, completion, recovery, or
inconsistency windows rather than permitting a conflicting operation.

A queued download-to-split dependency counts as split activity and makes tracks
busy. Scrape activity remains separate, as required by
[ADR 0006](adr/0006-scrape-queue-separate-from-job-registry.md).

## Adapter boundary

Adapters may translate canonical state into labels, URLs, HTML, or a future
transport representation. They must not rescan media, inspect the Job Registry
or Scrape Queue, or derive action policy. The migrated detail path only performs
presentation transformations before rendering the existing Askama template.

## Test seam

`Concerts` is the primary test seam. Tests use a fresh in-memory SQLite
database, `db::seeds::SeedContext`, a temporary media directory, and real
`JobRegistry` and `ScrapeQueue` state. Tests assert public state and history
queries; they do not mock internal collaborators or inspect private call
structure. See
[Backend persistence: testing persistence-backed modules](backend-persistence.md#testing-persistence-backed-modules).
