# Canonical Concert State — Implementation Plan

Implements [#159](https://github.com/gregwebs/tiny-desk-splitter/issues/159),
the first unblocked slice of parent
[#158](https://github.com/gregwebs/tiny-desk-splitter/issues/158). This pull
request is based on the parent `concert-interface` branch and will target that
branch.

## Goal and scope

Add the long-lived, presentation-neutral `Concerts` application module and
make the existing concert detail page obtain its complete card state through
that module. The state combines persisted concert/lifecycle data, observed
media, current scrape and Job Run activity, and authoritative permitted
actions. The route, HTML markup, and user-visible behavior remain unchanged.

This is an expand-only slice. It does not add commands, `ConcertCatalog`, a
JSON adapter, subscriptions, search, filters, pagination, or migrate the list
page and single-card mutation responses. History remains an explicit query and
is not embedded in `ConcertState`.

## TDD seams

The pre-agreed primary seam is the public `Concerts` interface:

```rust
let concerts = Concerts::new(db, workdir, registry, scrape_queue);
let state = concerts.get(concert_id)?;
let events = concerts.event_history(concert_id)?;
let failed_jobs = concerts.failed_job_history(concert_id)?;
```

Co-located `concerts` tests use a fresh in-memory SQLite database,
`db::seeds::SeedContext`, a unique temporary work directory, and real
`JobRegistry`/`ScrapeQueue` state. They assert only public results, never
private call structure or raw SQL. Existing handler and black-box tests remain
the compatibility seam for the migrated `GET /concerts/:id` adapter.

## State model

```rust
pub enum Observation<T> {
    Known(T),
    Unknown(ObservationFailure),
}

pub struct ConcertState {
    pub concert: Concert,
    pub tracks: Vec<ConcertTrackState>,
    pub media: MediaAvailability,
    pub active_work: ActiveWork,
    pub archive_configured: bool,
    pub permitted_actions: PermittedActions,
}

pub struct ConcertTrackState {
    pub index: usize,
    pub title: String,
    pub persisted_available: bool,
    pub liked: bool,
}

pub struct MediaAvailability {
    pub source: Observation<SourceMediaState>,
    pub published_split: Observation<PublishedSplitState>,
}

pub struct SourceMediaState {
    pub path: Option<PathBuf>,
}

pub struct PublishedSplitState {
    pub tracks_present_on_disk: Vec<bool>,
    pub reconstruction_items: Vec<PlaybackItem>,
    pub source_redundant: bool,
}

pub struct ActiveWork {
    pub scrape_pending: bool,
    pub download: JobActivity,
    pub split: JobActivity,
    pub archive: JobActivity,
    pub split_queued_after_download: bool,
}

pub struct JobActivity {
    pub persisted_started: bool,
    pub registry_active: bool,
}

pub struct PermittedActions {
    pub can_download: bool,
    pub can_delete_download: bool,
    pub can_archive: bool,
    pub can_unarchive: bool,
    pub can_play_concert: bool,
    pub can_delete_redundant_source: bool,
    pub tracks_busy: bool,
}
```

`ObservationFailure` is a presentation-neutral, typed reason suitable for
diagnostics. It does not expose an `anyhow::Error` as domain state.
`MediaAvailability` preserves independently knowable source-media facts when
the Published Concert Split cannot be observed. `ActiveWork` combines
persistent lifecycle facts with in-memory scrape, running, and queued facts.
`PermittedActions` contains the exact action booleans required by the migrated
card (`download`, `delete_download`, `archive`, `unarchive`, play,
delete-redundant-source, and track-busy gating). Safety-dependent actions are
false when their prerequisite observation is `Unknown`.

`tracks` is the persisted per-track state currently rendered by the detail
card; filesystem-backed track and reconstruction facts live only under the
fallible `published_split` observation. The handler may format labels, choose
the detail preview URL, and trim the title, but it must not rescan media,
inspect the registry/queue, or re-derive action policy.

Concert Status and desired-state commands remain #161. Media commands remain
#162/#165. Job Request commands remain #163. This ticket only names query
facts and policies already rendered by the detail card.

## Observation and state transitions

The query is point-in-time consistent by construction, not a cross-resource
transaction:

```text
get(id)
  |
  +--> DB lock: Concert + user split timestamps + archive setting
  |       |
  |       `--> missing concert ----------------------------> NotFound
  |
  +--> fallible source-directory scan
  |       |
  |       +--> directory absent ---------------------------> Known(absent)
  |       +--> readable media entry -----------------------> Known(present)
  |       `--> read/entry/type failure --------------------> Unknown(reason)
  |
  +--> shared Published Concert Split lock + fallible scan
  |       |
  |       +--> lock/read succeeds -------------------------> Known
  |       `--> lock/read fails ----------------------------> Unknown(reason)
  |
  +--> ScrapeQueue + JobRegistry snapshots ----------------> ActiveWork
  |
  `--> derive permitted actions from all facts
          |
          `--> unknown safety prerequisite ----------------> action unavailable
```

No state is mutated by `get`. Commands added by later tickets will re-read
their own preconditions rather than treating this point-in-time state as an
optimistic-lock token.

History follows a separate path:

```text
Concerts::event_history(id)
Concerts::failed_job_history(id)
  |
  +--> verify concert exists
  `--> fallible ordered history query ---------------------> Vec<HistoryRow>

ConcertState ------------------------------------------------X no event history
ConcertState ------------------------------------------------X no Failed Job log
```

## Detailed changes

### 1. Add the deep query module

Create `concert-tracker/src/concerts.rs` and export it from
`concert-tracker/src/lib.rs`.

- Add cloneable `Concerts` dependencies: `Arc<Mutex<Connection>>`,
  `PathBuf`, `Arc<JobRegistry>`, and `ScrapeQueue`.
- Add an explicit `get(i64) -> Result<ConcertState, ConcertQueryError>`. Keep
  the existing validated database identifier type for this expand slice; a
  `ConcertId` newtype would force unrelated persistence and adapter churn and
  is not needed to make negative/absent identifiers safe.
- Add `event_history` as a fallible explicit query using
  `events::try_list_for_concert`. Add a precise, fallible
  `failed_job_history` query, backed by a new ordered
  `db::failed_jobs::list_for_concert`. Keep both histories out of
  `ConcertState`.
- Reuse `ConcertMediaInventory` for canonical path/publication knowledge.
  Add fallible source lookup and a fallible Published Concert Split snapshot
  that acquires the shared lock once. The snapshot must use fallible directory
  entry/file-type probes throughout; it must not call helpers that use
  `.ok()`, `flatten()`, `Path::exists`, or conservative fallback values that
  erase I/O failure. A missing directory or file is `Known(absent)`; failure to
  read the directory, an entry, its type, or the publication lock is
  `Unknown(FilesystemObservationFailure { operation })`. A malformed
  publication journal/manifest remains governed by the publication module's
  existing recovery/validation contract; this ticket does not invent a second
  manifest validator. Preserve existing convenience methods for unmigrated
  callers.
- Read DB-owned facts under one mutex hold. Probe the filesystem outside the
  DB lock so a slow work directory does not block unrelated handlers.
- Add debug tracing for state observation and warning tracing for degraded
  observations.

`ConcertQueryError` has two top-level outcomes:

```rust
pub enum ConcertQueryError {
    NotFound { concert_id: i64 },
    Operational(anyhow::Error),
}
```

Only `rusqlite::Error::QueryReturnedNoRows` from the primary concert lookup
maps to `NotFound`. Split-timestamp, settings, event-history, Failed Job,
mutex-poisoning, and all other database/decode failures remain operational and
propagate. `Concerts` must not use `Mutex::lock().unwrap()`. Poisoning is
converted to an operational error with context.

### 2. Centralize active-work and permitted-action policy

In `concerts.rs`, define presentation-neutral types for:

- download, split, and archive lifecycle (reusing existing typed status enums);
- scrape pending, running Job Runs, and the queued download-to-split edge;
- source availability and Published Concert Split availability;
- the action decisions currently calculated by `render_row_inner` and
  `tracks_busy`.

Use one pure policy function:

```rust
fn permitted_actions(
    concert: &Concert,
    media: &MediaAvailability,
    work: &ActiveWork,
    archive_configured: bool,
) -> PermittedActions
```

The policy must preserve current detail-card behavior. If Published Concert
Split observation is unknown, reconstruction-dependent playback and
source-redundancy deletion are unavailable. Source playback can remain
available when its independent source observation is known present. A failed
archive-setting database read remains an operational query error, not
`Archive unavailable`.

### 3. Migrate the detail adapter without changing presentation

Add `Concerts` to `web::AppState` and construct it once in
`src/bin/concert_web.rs`; test helpers construct it from the same test
dependencies.

In `web/handlers.rs`:

- keep the existing first-view auto-scrape orchestration unchanged;
- after auto-scrape, call `state.concerts.get(id)` for the canonical state;
- render the detail card from `ConcertState`, translating its typed fields
  into the existing `RowTemplate` fields;
- load events through `state.concerts.event_history(id)` separately;
- preserve `DetailTemplate` and `concert_card.html` output, route, status,
  polling, image URL, notes, track list, and auto-scrape behavior;
- remove detail-only duplicate reads/policy after the migrated path is green,
  but leave catalog and mutation-card paths for their assigned tickets.

Map `ConcertQueryError::NotFound` to the existing HTTP 404 and operational
errors to the existing top-level 500 path.

The action policy uses this explicit consistency matrix:

Registry activity means `JobRegistry::is_running`, including a Reserved
admission slot or unfinished accepted run. Persisted activity means the
corresponding `*_started_at` is set. The policy treats either source as active
so disagreement is visible and fails closed:

| Action/fact | Download active | Split active/queued | Archive active |
|---|---:|---:|---:|
| `can_download` | false | false | false |
| `can_delete_download` | false | false | false |
| `can_archive` | false | false | false |
| `can_unarchive` | false | false | false |
| `tracks_busy` | unchanged | true | unchanged |
| source playback | allowed if source is Known present | allowed if already present | allowed if already present |
| reconstruction playback | unchanged | false while split active/queued | unchanged |
| redundant-source deletion | false | false | false |

Outside active/disagreement windows, existing lifecycle predicates remain
unchanged: download is allowed only for NotDownloaded/DownloadError; download
deletion only for Downloaded; archive only when configured, media has persisted
download/split state, and archive is NotArchived/ArchiveError; unarchive only
for Archived. The queued download-to-split edge counts as split activity. A
pending scrape is reported but does not change existing card actions.

### 4. Tests: red → green vertical slices

1. Seed a scraped lifecycle concert with source and track media. Assert
   `Concerts::get` returns metadata/lifecycle, known media, active-work facts,
   and the currently permitted detail-card actions.
2. Create a normal concert directory and make
   `.concert-split-publication.lock` a directory. Opening the lock then fails
   promptly with a typed operation error while the independent source scan
   remains Known. Assert reconstruction/source-deletion actions are
   unavailable. Separately replace the expected concert directory with a
   regular file; assert both source and Published Split observations are
   Unknown rather than absent.
3. For Download, Split, and Archive, independently arrange a Reserved registry
   slot with no persisted started timestamp, then a persisted started timestamp
   without a registry slot. Assert `ActiveWork` exposes each disagreement and
   every conflicting action follows the matrix. Also arrange a queued split
   edge and pending scrape request and assert their exact facts.
4. Insert event and Failed Job history. Assert `get` is unchanged by history,
   `event_history` and `failed_job_history` return their ordered rows
   explicitly, and no history fields exist on `ConcertState`. Corrupt a
   history row and assert the explicit query returns an operational error.
5. Run focused handler tests for a detail page with media and for auto-scrape
   failure. Assert status and stable markup/action fragments are unchanged.
   Add a structural review assertion that the detail path contains no calls to
   `ConcertMediaInventory`, `has_archive_location`, `split_queued`,
   `tracks_busy`, registry/queue inspection, or equivalent policy helpers.
6. Query a missing id and assert `NotFound`. Induce a split-timestamp/settings
   decode failure in an isolated database and assert `Operational`, proving
   these failures are no longer swallowed as missing facts.

Each slice starts with one failing public-seam test, adds only enough
implementation to pass, then proceeds to the next slice.

## Lasting documentation

- Add `docs/concerts.md` as the canonical description of `Concerts`, Concert
  State, observation semantics, point-in-time consistency, test seam, and the
  diagrams above.
- Link `docs/concerts.md` from `README.md`.
- Update `docs/backend-persistence.md` only to clarify that the application
  module composes persistence; keep persistence ownership canonical there.
- Keep the verbose implementation evidence in
  `docs/change/2026-07-23-canonical-concert-state.md`, begun in this ticket and
  updated by later epic tickets where appropriate.
- Verify links and avoid duplicating rules between lasting docs.

## Checklist

- [ ] Public long-lived `Concerts` module and explicit `get`.
- [ ] Canonical presentation-neutral `ConcertState`.
- [ ] Typed Known/Unknown observation semantics.
- [ ] Active scrape, Job Run, and queued-work facts.
- [ ] Central authoritative permitted-action policy.
- [ ] Explicit event-history query; no unbounded history in state.
- [ ] Explicit Failed Job history query; no Failed Job log in state.
- [ ] Detail page renders through `ConcertState` with compatibility preserved.
- [ ] Interface-level seeded SQLite/temp-media tests.
- [ ] Focused detail adapter regression tests.
- [ ] Lasting docs, README link, state diagrams, and Change Record.
- [ ] Formatting, lint, focused tests, full Rust suite, and live verification.
- [ ] Adversarial code review and follow-up review after any fixes.
- [ ] Commit, push, PR to `concert-interface`, and CI monitoring.

## Verification

Automated checks:

```sh
cargo test -p concert-tracker concerts
cargo test -p concert-tracker web::handlers
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
just test-rs
git diff --check
```

Manual verification uses a new `mktemp` work directory, separate SQLite
database, and unused port. Start `concert-web` according to
`CONTRIBUTING.md`, then:

1. Seed or sync a listing and open `/concerts/:id`; confirm the same title,
   metadata, notes, card actions, tracks, and event table render.
2. Verify a concert with source media, split media, archived state, and active
   work exposes the same enabled/disabled actions.
3. Verify an absent concert remains 404 and an auto-scrape failure still
   renders listing data.
4. Create an observation failure using an isolated fixture and verify the page
   remains safe: no reconstruction or redundant-source deletion action is
   offered.
5. Query the local API first if any backend API is incidentally affected; this
   ticket intentionally adds no API behavior.

No real user database or work directory is read or mutated.
