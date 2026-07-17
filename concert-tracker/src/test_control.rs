//! Test Control API — a feature-gated JSON-RPC surface that Hurl black-box
//! tests use to arrange fixture data and, when needed, assert internal-only
//! facts against a real running `concert-web` process. Product behavior is
//! still verified through the normal `concert-web` HTTP routes; this module
//! never adds test-only routes to the product axum router.
//!
//! See `docs/change/2026-07-11-hurl-web-integration-tests.md` and
//! `docs/adr/0001-jsonrpsee-for-test-control-api.md`.
//!
//! Defense in depth: reaching this API requires *all* of — the non-default
//! `test-control` Cargo feature, the explicit `--test-control-port` runtime
//! flag (wired in `bin/concert_web.rs`), loopback-only binding (enforced in
//! [`start`], which ignores the configured host), and the compile-time guard
//! below. No single one of these is sufficient on its own.

// `assert_job_observation`'s 8 arguments (self + concert_id + kind + 5
// optional counts) are required by the `#[rpc(param_kind = map)]` flat-args
// convention this module's other assert/job methods use (see
// docs/change/2026-07-15-job-driver-plan.md's adapter-parameter-shape
// correction) — grouping the counts into a struct would revert to the
// seed-style single-struct-param wrapping the adapter doesn't apply to this
// route. The `jsonrpsee::rpc` macro's generated code doesn't pick up a
// per-item `#[allow(...)]` here, so this is a module-level exception rather
// than a narrower one.
#![allow(clippy::too_many_arguments)]

#[cfg(all(feature = "test-control", not(debug_assertions)))]
compile_error!("test-control must not be compiled into release builds");

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, LazyLock};

use jsonrpsee::core::{async_trait, RpcResult};
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::server::{RpcModule, ServerBuilder, ServerHandle};
use jsonrpsee::types::ErrorObjectOwned;
use serde::Serialize;

use crate::db;
use crate::db::seeds::{
    FixtureIds, SeedAlbumNullConcert, SeedContext, SeedLifecycleConcert, SeedListing,
    SeedMediaConcert, SeedScrapedConcert,
};
use crate::events::Event;
use crate::web::AppState;

mod adapter;
pub mod job_driver;
pub mod scrape_driver;

use job_driver::{JobDriver, JobStepKind, StepOutcome};
use scrape_driver::{ScrapeDriver, ScrapeOutcome};

/// One fixture-number allocator for the process lifetime — per ADR 0003, not
/// reset by `test.reset`, and shared across every [`TestControlServer`] clone
/// (all of which wrap the same `AppState` and therefore the same database).
static FIXTURE_IDS: LazyLock<FixtureIds> = LazyLock::new(FixtureIds::default);

#[rpc(server, namespace = "test", namespace_separator = ".")]
pub trait TestControlApi {
    /// Clear concert, event, playlist, job, and settings test data, and
    /// remove generated concert files/thumbnails under the configured
    /// workdir. Leaves the SQLite schema and server configuration intact.
    #[method(name = "reset")]
    async fn reset(&self) -> RpcResult<OkResult>;

    #[method(name = "seed_listing", param_kind = map)]
    async fn seed_listing(&self, params: SeedListing) -> RpcResult<SeedListingResult>;

    #[method(name = "seed_scraped_concert", param_kind = map)]
    async fn seed_scraped_concert(
        &self,
        params: SeedScrapedConcert,
    ) -> RpcResult<SeedScrapedConcertResult>;

    #[method(name = "seed_lifecycle_concert", param_kind = map)]
    async fn seed_lifecycle_concert(
        &self,
        params: SeedLifecycleConcert,
    ) -> RpcResult<SeedLifecycleConcertResult>;

    #[method(name = "seed_media_concert", param_kind = map)]
    async fn seed_media_concert(
        &self,
        params: SeedMediaConcert,
    ) -> RpcResult<SeedLifecycleConcertResult>;

    /// Seed a concert with a NULL `album` column — a historical/defensive
    /// shape no current product write path produces, kept for the
    /// `track-details` handler test that exercises it. See
    /// `db::seeds::SeedAlbumNullConcert`.
    #[method(name = "seed_album_null_concert", param_kind = map)]
    async fn seed_album_null_concert(
        &self,
        params: SeedAlbumNullConcert,
    ) -> RpcResult<SeedListingResult>;

    /// Assert semantic domain conditions for a seeded concert without Hurl
    /// having to read raw DB rows. Only the expectations that are present are
    /// checked; omit a field to not assert on that dimension. At least one of
    /// `ignored`/`downloaded`/`split` must be present — a call that provides
    /// none (all omitted or explicit `null`) errors instead of vacuously
    /// succeeding without checking anything. Prefer a public HTTP assertion
    /// instead of this method whenever the behavior is already visible on a
    /// public route/fragment — see "Assertion Methods" in
    /// docs/change/2026-07-11-hurl-web-integration-tests.md.
    #[method(name = "assert_concert_state", param_kind = map)]
    async fn assert_concert_state(
        &self,
        id: i64,
        ignored: Option<bool>,
        downloaded: Option<bool>,
        split: Option<bool>,
    ) -> RpcResult<OkResult>;

    /// Configure the Job Driver's default plan (when `concert_id` is
    /// omitted/`null`) or a per-concert override plan (when present) for
    /// download/split/open outcomes. Only present fields are changed; a
    /// per-concert override materializes from the current default plan the
    /// first time it's set for that concert, then updates independently of
    /// later default changes. Reached via the adapter's `/test/job/set_plan`
    /// route. `open = "block"` is rejected — see `job_driver::JobDriver`'s
    /// docs for why.
    #[method(name = "job_set_plan", param_kind = map)]
    async fn job_set_plan(
        &self,
        concert_id: Option<i64>,
        download: Option<StepOutcome>,
        split: Option<StepOutcome>,
        open: Option<StepOutcome>,
    ) -> RpcResult<OkResult>;

    /// Release a step currently blocked at `(concert_id, kind)` with the
    /// given outcome (`"succeed"` or `"fail"` — `"block"` is invalid here).
    /// Errors if no step is blocked there; poll `test.assert_job_observation`
    /// for `blocked=1` first. Reached via the adapter's `/test/job/release`
    /// route.
    #[method(name = "job_release", param_kind = map)]
    async fn job_release(
        &self,
        concert_id: i64,
        kind: JobStepKind,
        outcome: StepOutcome,
    ) -> RpcResult<OkResult>;

    /// Assert Job Driver observation counts for `(concert_id, kind)`. Only
    /// present fields are checked, mismatches are collected and reported
    /// together, and a call with every count field omitted is rejected —
    /// same shape as [`TestControlApi::assert_concert_state`].
    #[method(name = "assert_job_observation", param_kind = map)]
    async fn assert_job_observation(
        &self,
        concert_id: i64,
        kind: JobStepKind,
        started: Option<u32>,
        completed: Option<u32>,
        failed: Option<u32>,
        blocked: Option<u32>,
        released: Option<u32>,
    ) -> RpcResult<OkResult>;

    /// Assert internal event-log facts with no public HTTP surface: `present`
    /// lists event names that must have at least one recorded row for the
    /// concert, `absent` lists names that must have none. At least one of
    /// `present`/`absent` must be non-empty — same "reject a vacuous call"
    /// shape as [`TestControlApi::assert_concert_state`]. Every listed name
    /// must be a real event (see `crate::events::Event::parse`) — an unknown
    /// name errors rather than vacuously passing in `absent`. First consumer:
    /// interlude deletion (`present: ["interlude_delete"], absent:
    /// ["track_delete"]`).
    #[method(name = "assert_concert_events", param_kind = map)]
    async fn assert_concert_events(
        &self,
        concert_id: i64,
        present: Option<Vec<String>>,
        absent: Option<Vec<String>>,
    ) -> RpcResult<OkResult>;

    /// Configure the Scrape Driver's plan for `concert_id` (`"succeed"` or
    /// `"block"`). Unlike the Job Driver there is no default plan — an
    /// unconfigured concert always scrape-succeeds deterministically.
    /// Reached via the adapter's `/test/scrape/set_plan` route.
    #[method(name = "scrape_set_plan", param_kind = map)]
    async fn scrape_set_plan(&self, concert_id: i64, scrape: ScrapeOutcome) -> RpcResult<OkResult>;

    /// Enqueue `concert_id` for a background scrape through the app's real
    /// [`crate::jobs::scrape_queue::ScrapeQueue`] (the same queue
    /// `/sync/:year/:month` uses), looking up its `source_url` from the
    /// database. Returns `enqueued: false` if the concert was already
    /// queued/in-flight (normal dedupe, not an error). Reached via the
    /// adapter's `/test/scrape/enqueue` route.
    #[method(name = "scrape_enqueue", param_kind = map)]
    async fn scrape_enqueue(&self, concert_id: i64) -> RpcResult<ScrapeEnqueueResult>;

    /// Release a scrape currently blocked for `concert_id`. Errors if none is
    /// blocked there; poll `test.assert_scrape_observation` for `blocked=1`
    /// first. Reached via the adapter's `/test/scrape/release` route.
    #[method(name = "scrape_release", param_kind = map)]
    async fn scrape_release(&self, concert_id: i64) -> RpcResult<OkResult>;

    /// Assert Scrape Driver observation counts for `concert_id`. Only present
    /// fields are checked, mismatches are collected and reported together,
    /// and a call with every count field omitted is rejected — same shape as
    /// [`TestControlApi::assert_job_observation`].
    #[method(name = "assert_scrape_observation", param_kind = map)]
    async fn assert_scrape_observation(
        &self,
        concert_id: i64,
        started: Option<u32>,
        completed: Option<u32>,
        blocked: Option<u32>,
        released: Option<u32>,
    ) -> RpcResult<OkResult>;
}

#[derive(Clone, Debug, Serialize)]
pub struct OkResult {
    pub ok: bool,
}

/// Seed Result for `test.seed_listing`. Deliberately only the id plus the
/// fields already meaningful to the public list/detail HTML — no full DB row
/// (see "Seed Methods" in docs/change/2026-07-11-hurl-web-integration-tests.md).
#[derive(Clone, Debug, Serialize)]
pub struct SeedListingResult {
    pub id: i64,
    pub source_url: String,
    pub title: String,
    pub concert_date: Option<String>,
}

/// Seed Result for `test.seed_scraped_concert`. Same "public fields only"
/// rule as [`SeedListingResult`], plus `album` since it is the field the
/// scraped-status Hurl cases assert on and the public detail page displays.
#[derive(Clone, Debug, Serialize)]
pub struct SeedScrapedConcertResult {
    pub id: i64,
    pub source_url: String,
    pub title: String,
    pub album: String,
}

/// Seed Result for `test.seed_lifecycle_concert`. The booleans echo the
/// requested state so Hurl can capture one fixture call and then assert the
/// corresponding public endpoint behavior.
#[derive(Clone, Debug, Serialize)]
pub struct SeedLifecycleConcertResult {
    pub id: i64,
    pub source_url: String,
    pub title: String,
    pub album: String,
    pub downloaded: bool,
    pub split: bool,
}

/// Result for `test.scrape_enqueue`. `enqueued` distinguishes "this call
/// queued a new scrape" from "already queued/in-flight" (a normal dedupe
/// no-op) so Hurl cases can assert either shape.
#[derive(Clone, Debug, Serialize)]
pub struct ScrapeEnqueueResult {
    pub ok: bool,
    pub enqueued: bool,
}

/// Clone is cheap (an `AppState` clone) and lets [`start`] clone the built
/// `RpcModule` — one copy is converted to [`jsonrpsee::server::Methods`] for
/// the [`adapter`] to dispatch through in-process, the other is handed to
/// jsonrpsee's own server unchanged.
#[derive(Clone)]
pub struct TestControlServer {
    state: AppState,
    job_driver: Arc<JobDriver>,
    scrape_driver: Arc<ScrapeDriver>,
}

impl TestControlServer {
    pub fn new(
        state: AppState,
        job_driver: Arc<JobDriver>,
        scrape_driver: Arc<ScrapeDriver>,
    ) -> Self {
        Self {
            state,
            job_driver,
            scrape_driver,
        }
    }

    fn rpc_module(self) -> RpcModule<Self> {
        TestControlApiServer::into_rpc(self)
    }
}

#[async_trait]
impl TestControlApiServer for TestControlServer {
    async fn reset(&self) -> RpcResult<OkResult> {
        reset_test_data(&self.state)
            .map(|()| {
                self.job_driver.reset();
                self.scrape_driver.reset();
                OkResult { ok: true }
            })
            .map_err(internal_error)
    }

    async fn seed_listing(&self, params: SeedListing) -> RpcResult<SeedListingResult> {
        seed_listing(&self.state, params).map_err(internal_error)
    }

    async fn seed_scraped_concert(
        &self,
        params: SeedScrapedConcert,
    ) -> RpcResult<SeedScrapedConcertResult> {
        seed_scraped_concert(&self.state, params).map_err(internal_error)
    }

    async fn seed_lifecycle_concert(
        &self,
        params: SeedLifecycleConcert,
    ) -> RpcResult<SeedLifecycleConcertResult> {
        seed_lifecycle_concert(&self.state, params).map_err(internal_error)
    }

    async fn seed_media_concert(
        &self,
        params: SeedMediaConcert,
    ) -> RpcResult<SeedLifecycleConcertResult> {
        seed_media_concert(&self.state, params).map_err(internal_error)
    }

    async fn seed_album_null_concert(
        &self,
        params: SeedAlbumNullConcert,
    ) -> RpcResult<SeedListingResult> {
        seed_album_null_concert(&self.state, params).map_err(internal_error)
    }

    async fn assert_concert_state(
        &self,
        id: i64,
        ignored: Option<bool>,
        downloaded: Option<bool>,
        split: Option<bool>,
    ) -> RpcResult<OkResult> {
        assert_concert_state(&self.state, id, ignored, downloaded, split)
            .map(|()| OkResult { ok: true })
            .map_err(assertion_error)
    }

    async fn job_set_plan(
        &self,
        concert_id: Option<i64>,
        download: Option<StepOutcome>,
        split: Option<StepOutcome>,
        open: Option<StepOutcome>,
    ) -> RpcResult<OkResult> {
        let result = match concert_id {
            Some(id) => self.job_driver.set_concert_plan(id, download, split, open),
            None => self.job_driver.set_default_plan(download, split, open),
        };
        result
            .map(|()| OkResult { ok: true })
            .map_err(internal_error)
    }

    async fn job_release(
        &self,
        concert_id: i64,
        kind: JobStepKind,
        outcome: StepOutcome,
    ) -> RpcResult<OkResult> {
        self.job_driver
            .release(concert_id, kind, outcome)
            .map(|()| OkResult { ok: true })
            .map_err(internal_error)
    }

    async fn assert_job_observation(
        &self,
        concert_id: i64,
        kind: JobStepKind,
        started: Option<u32>,
        completed: Option<u32>,
        failed: Option<u32>,
        blocked: Option<u32>,
        released: Option<u32>,
    ) -> RpcResult<OkResult> {
        assert_job_observation(
            &self.job_driver,
            concert_id,
            kind,
            started,
            completed,
            failed,
            blocked,
            released,
        )
        .map(|()| OkResult { ok: true })
        .map_err(assertion_error)
    }

    async fn assert_concert_events(
        &self,
        concert_id: i64,
        present: Option<Vec<String>>,
        absent: Option<Vec<String>>,
    ) -> RpcResult<OkResult> {
        assert_concert_events(&self.state, concert_id, present, absent)
            .map(|()| OkResult { ok: true })
            .map_err(assertion_error)
    }

    async fn scrape_set_plan(&self, concert_id: i64, scrape: ScrapeOutcome) -> RpcResult<OkResult> {
        self.scrape_driver.set_plan(concert_id, scrape);
        Ok(OkResult { ok: true })
    }

    async fn scrape_enqueue(&self, concert_id: i64) -> RpcResult<ScrapeEnqueueResult> {
        scrape_enqueue(&self.state, concert_id)
            .map(|enqueued| ScrapeEnqueueResult { ok: true, enqueued })
            .map_err(internal_error)
    }

    async fn scrape_release(&self, concert_id: i64) -> RpcResult<OkResult> {
        self.scrape_driver
            .release(concert_id)
            .map(|()| OkResult { ok: true })
            .map_err(internal_error)
    }

    async fn assert_scrape_observation(
        &self,
        concert_id: i64,
        started: Option<u32>,
        completed: Option<u32>,
        blocked: Option<u32>,
        released: Option<u32>,
    ) -> RpcResult<OkResult> {
        assert_scrape_observation(
            &self.scrape_driver,
            concert_id,
            started,
            completed,
            blocked,
            released,
        )
        .map(|()| OkResult { ok: true })
        .map_err(assertion_error)
    }
}

/// Checks each present expectation against the Job Driver's observed counts
/// for `(concert_id, kind)`, returning every mismatch in one message — same
/// "check only present fields, report everything, reject a vacuous call"
/// shape as [`assert_concert_state`].
#[allow(clippy::too_many_arguments)]
fn assert_job_observation(
    job_driver: &JobDriver,
    concert_id: i64,
    kind: JobStepKind,
    started: Option<u32>,
    completed: Option<u32>,
    failed: Option<u32>,
    blocked: Option<u32>,
    released: Option<u32>,
) -> anyhow::Result<()> {
    if started.is_none()
        && completed.is_none()
        && failed.is_none()
        && blocked.is_none()
        && released.is_none()
    {
        anyhow::bail!(
            "assert_job_observation called for concert {concert_id} {kind:?} with no \
             expectations (started/completed/failed/blocked/released all omitted or null) \
             — this would silently pass without checking anything; assert at least one count"
        );
    }

    let actual = job_driver.observation(concert_id, kind);
    let mut mismatches = Vec::new();
    if let Some(expected) = started {
        if actual.started != expected {
            mismatches.push(format!(
                "started: expected {expected}, got {}",
                actual.started
            ));
        }
    }
    if let Some(expected) = completed {
        if actual.completed != expected {
            mismatches.push(format!(
                "completed: expected {expected}, got {}",
                actual.completed
            ));
        }
    }
    if let Some(expected) = failed {
        if actual.failed != expected {
            mismatches.push(format!(
                "failed: expected {expected}, got {}",
                actual.failed
            ));
        }
    }
    if let Some(expected) = blocked {
        if actual.blocked != expected {
            mismatches.push(format!(
                "blocked: expected {expected}, got {}",
                actual.blocked
            ));
        }
    }
    if let Some(expected) = released {
        if actual.released != expected {
            mismatches.push(format!(
                "released: expected {expected}, got {}",
                actual.released
            ));
        }
    }

    if mismatches.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "concert {concert_id} {kind:?} observation mismatch: {}",
            mismatches.join("; ")
        );
    }
}

/// Checks each expected event name against the concert's recorded event log,
/// returning every mismatch in one message — same "check only present
/// fields, report everything, reject a vacuous call" shape as
/// [`assert_concert_state`]/[`assert_job_observation`]. Every name in
/// `present`/`absent` must be a real event name (`Event::parse`); an unknown
/// name errors immediately rather than silently never matching in `absent`.
fn assert_concert_events(
    state: &AppState,
    concert_id: i64,
    present: Option<Vec<String>>,
    absent: Option<Vec<String>>,
) -> anyhow::Result<()> {
    let present = present.unwrap_or_default();
    let absent = absent.unwrap_or_default();
    if present.is_empty() && absent.is_empty() {
        anyhow::bail!(
            "assert_concert_events called for concert {concert_id} with no expectations \
             (present/absent both omitted, null, or empty) — this would silently pass \
             without checking anything; assert at least one event name"
        );
    }
    for name in present.iter().chain(absent.iter()) {
        if Event::parse(name).is_none() {
            anyhow::bail!(
                "assert_concert_events: unknown event name {name:?} — see \
                 crate::events::Event for the known names"
            );
        }
    }

    let conn = state.db.lock().unwrap();
    db::concerts::get_concert(&conn, concert_id)
        .map_err(|e| anyhow::anyhow!("no concert with id {concert_id}: {e}"))?;
    // The fallible `try_list_for_concert`, not `list_for_concert` — a query
    // failure here must surface as an error, not be misread as "no events"
    // and let an `absent` assertion silently pass without truly checking
    // the event log.
    let events = crate::events::try_list_for_concert(&conn, concert_id)
        .map_err(|e| anyhow::anyhow!("failed to list events for concert {concert_id}: {e}"))?;

    let mut mismatches = Vec::new();
    for name in &present {
        if !events.iter().any(|e| &e.event == name) {
            mismatches.push(format!("expected event {name:?} to be present, found none"));
        }
    }
    for name in &absent {
        let count = events.iter().filter(|e| &e.event == name).count();
        if count > 0 {
            mismatches.push(format!(
                "expected event {name:?} to be absent, found {count}"
            ));
        }
    }

    if mismatches.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "concert {concert_id} event mismatch: {}",
            mismatches.join("; ")
        );
    }
}

/// Looks up `concert_id`'s `source_url` and enqueues it on the app's real
/// scrape queue, matching what `/sync/:year/:month` does for a newly-listed
/// concert. Drops the DB lock before calling `enqueue` — `ScrapeQueue` takes
/// no DB lock itself, but keeping the critical section to the lookup only
/// stays deadlock-proof by construction.
fn scrape_enqueue(state: &AppState, concert_id: i64) -> anyhow::Result<bool> {
    let source_url = {
        let conn = state.db.lock().unwrap();
        db::concerts::get_concert(&conn, concert_id)
            .map_err(|e| anyhow::anyhow!("no concert with id {concert_id}: {e}"))?
            .source_url
    };
    Ok(state.scrape_queue.enqueue(concert_id, source_url))
}

/// Checks each present expectation against the Scrape Driver's observed
/// counts for `concert_id`, returning every mismatch in one message — same
/// "check only present fields, report everything, reject a vacuous call"
/// shape as [`assert_job_observation`].
fn assert_scrape_observation(
    scrape_driver: &ScrapeDriver,
    concert_id: i64,
    started: Option<u32>,
    completed: Option<u32>,
    blocked: Option<u32>,
    released: Option<u32>,
) -> anyhow::Result<()> {
    if started.is_none() && completed.is_none() && blocked.is_none() && released.is_none() {
        anyhow::bail!(
            "assert_scrape_observation called for concert {concert_id} with no expectations \
             (started/completed/blocked/released all omitted or null) — this would silently \
             pass without checking anything; assert at least one count"
        );
    }

    let actual = scrape_driver.observation(concert_id);
    let mut mismatches = Vec::new();
    if let Some(expected) = started {
        if actual.started != expected {
            mismatches.push(format!(
                "started: expected {expected}, got {}",
                actual.started
            ));
        }
    }
    if let Some(expected) = completed {
        if actual.completed != expected {
            mismatches.push(format!(
                "completed: expected {expected}, got {}",
                actual.completed
            ));
        }
    }
    if let Some(expected) = blocked {
        if actual.blocked != expected {
            mismatches.push(format!(
                "blocked: expected {expected}, got {}",
                actual.blocked
            ));
        }
    }
    if let Some(expected) = released {
        if actual.released != expected {
            mismatches.push(format!(
                "released: expected {expected}, got {}",
                actual.released
            ));
        }
    }

    if mismatches.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "concert {concert_id} scrape observation mismatch: {}",
            mismatches.join("; ")
        );
    }
}

fn internal_error(err: anyhow::Error) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(
        jsonrpsee::types::ErrorCode::InternalError.code(),
        err.to_string(),
        None::<()>,
    )
}

/// Distinct from [`internal_error`]: this is for an assertion that ran
/// successfully but found the domain condition doesn't hold (or the
/// concert doesn't exist) — an expected test-failure outcome, not a server
/// malfunction. Uses jsonrpsee's `CALL_EXECUTION_FAILED_CODE`, its
/// general-purpose "the call executed but did not succeed" code.
fn assertion_error(err: anyhow::Error) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(
        jsonrpsee::types::error::CALL_EXECUTION_FAILED_CODE,
        err.to_string(),
        None::<()>,
    )
}

/// Locks the app's connection, seeds a listing via the Database Seed API
/// (`db::seeds`), and adapts the resulting [`crate::model::Concert`] into
/// this method's Seed Result JSON shape. All fixture defaulting, `source_url`
/// upsert semantics, and null-vs-omitted handling live in `db::seeds` —
/// see its module docs.
fn seed_listing(state: &AppState, seed: SeedListing) -> anyhow::Result<SeedListingResult> {
    let conn = state.db.lock().unwrap();
    let concert = SeedContext::with_ids(&conn, FIXTURE_IDS.clone()).seed_listing(seed)?;
    Ok(SeedListingResult {
        id: concert.id,
        source_url: concert.source_url,
        title: concert.title,
        concert_date: concert.concert_date,
    })
}

/// See [`seed_listing`] — same shape, delegating to
/// `db::seeds::SeedContext::seed_scraped_concert`.
fn seed_scraped_concert(
    state: &AppState,
    seed: SeedScrapedConcert,
) -> anyhow::Result<SeedScrapedConcertResult> {
    let conn = state.db.lock().unwrap();
    let concert = SeedContext::with_ids(&conn, FIXTURE_IDS.clone()).seed_scraped_concert(seed)?;
    Ok(SeedScrapedConcertResult {
        id: concert.id,
        source_url: concert.source_url,
        title: concert.title,
        album: concert.album.unwrap_or_default(),
    })
}

/// See [`seed_listing`] — same shape, delegating to
/// `db::seeds::SeedContext::seed_lifecycle_concert`. `downloaded`/`split` in
/// the result are re-derived from the persisted domain state (not echoed from
/// the request) so the Seed Result reflects what was actually written.
fn seed_lifecycle_concert(
    state: &AppState,
    seed: SeedLifecycleConcert,
) -> anyhow::Result<SeedLifecycleConcertResult> {
    let conn = state.db.lock().unwrap();
    let concert = SeedContext::with_ids(&conn, FIXTURE_IDS.clone()).seed_lifecycle_concert(seed)?;
    Ok(seed_lifecycle_concert_result(concert))
}

/// See [`seed_lifecycle_concert`] — same Seed Result shape, plus dummy media
/// files written under the app's scratch workdir for filesystem-backed routes
/// that only check existence/extension.
fn seed_media_concert(
    state: &AppState,
    seed: SeedMediaConcert,
) -> anyhow::Result<SeedLifecycleConcertResult> {
    let conn = state.db.lock().unwrap();
    let concert = SeedContext::with_ids(&conn, FIXTURE_IDS.clone())
        .seed_media_concert(&state.jobs.working_dir, seed)?;
    Ok(seed_lifecycle_concert_result(concert))
}

/// See [`seed_listing`] — same shape, delegating to
/// `db::seeds::SeedContext::seed_album_null_concert`.
fn seed_album_null_concert(
    state: &AppState,
    seed: SeedAlbumNullConcert,
) -> anyhow::Result<SeedListingResult> {
    let conn = state.db.lock().unwrap();
    let concert =
        SeedContext::with_ids(&conn, FIXTURE_IDS.clone()).seed_album_null_concert(seed)?;
    Ok(SeedListingResult {
        id: concert.id,
        source_url: concert.source_url,
        title: concert.title,
        concert_date: concert.concert_date,
    })
}

fn seed_lifecycle_concert_result(concert: crate::model::Concert) -> SeedLifecycleConcertResult {
    let downloaded = concert.download_status() == crate::model::DownloadStatus::Downloaded;
    let split = concert.split_status() == crate::model::SplitStatus::Split;
    SeedLifecycleConcertResult {
        id: concert.id,
        source_url: concert.source_url,
        title: concert.title,
        album: concert.album.unwrap_or_default(),
        downloaded,
        split,
    }
}

/// Checks each present expectation against the concert's actual domain
/// state, returning every mismatch (not just the first) so a Hurl case gets
/// the full picture in one round trip. `downloaded`/`split` are checked
/// against [`crate::model::DownloadStatus`]/[`crate::model::SplitStatus`]
/// being exactly `Downloaded`/`Split` — a concert `Downloading` or in
/// `DownloadError` counts as "not downloaded" here, matching what a human
/// reading "downloaded: true/false" would expect.
fn assert_concert_state(
    state: &AppState,
    id: i64,
    ignored: Option<bool>,
    downloaded: Option<bool>,
    split: Option<bool>,
) -> anyhow::Result<()> {
    if ignored.is_none() && downloaded.is_none() && split.is_none() {
        anyhow::bail!(
            "assert_concert_state called for concert {id} with no expectations \
             (ignored/downloaded/split all omitted or null) — this would silently \
             pass without checking anything; assert at least one condition"
        );
    }

    let conn = state.db.lock().unwrap();
    let concert = db::concerts::get_concert(&conn, id)
        .map_err(|e| anyhow::anyhow!("no concert with id {id}: {e}"))?;

    let mut mismatches = Vec::new();
    if let Some(expected) = ignored {
        if concert.ignored != expected {
            mismatches.push(format!(
                "ignored: expected {expected}, got {}",
                concert.ignored
            ));
        }
    }
    if let Some(expected) = downloaded {
        let actual = concert.download_status() == crate::model::DownloadStatus::Downloaded;
        if actual != expected {
            mismatches.push(format!("downloaded: expected {expected}, got {actual}"));
        }
    }
    if let Some(expected) = split {
        let actual = concert.split_status() == crate::model::SplitStatus::Split;
        if actual != expected {
            mismatches.push(format!("split: expected {expected}, got {actual}"));
        }
    }

    if mismatches.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("concert {id} state mismatch: {}", mismatches.join("; "));
    }
}

/// Removes generated files under `<workdir>/concerts` and
/// `<workdir>/thumbnails`, then deletes concert/event/playlist/job/
/// synced-month rows (`playlist_items` cascades off `playlists`) and resets
/// the singleton `settings` row back to its migration defaults. Uses the
/// same connection/workdir as the app server so Test Control and product
/// HTTP requests observe the same state.
///
/// `synced_months` is cleared too even though the spec's contract only names
/// "concert, event, playlist, job, and settings" data: the real
/// `/sync/:year/:month` route (see `sync.rs`) persists rows there, and a
/// stale row would leave a later Hurl case observing a month as already
/// synced — the exact kind of cross-test pollution `reset` exists to
/// prevent.
///
/// Filesystem cleanup runs *before* the DB reset on purpose: `concert_dir`
/// (see `model.rs`) keys a concert's directory by its sanitized *album name*,
/// not its numeric id, so a same-named concert seeded after a failed reset
/// would otherwise silently inherit a stale directory's leftover files —
/// pollution a Hurl test has no way to detect. Doing the filesystem step
/// first means a failure here aborts before any DB row is touched, leaving
/// the previous concerts (and their now-still-matching directories) intact
/// and the error visible, instead of an empty concert list paired with
/// orphaned media.
///
/// `settings` is reset in place (never deleted): migration 0002 inserts its
/// `id = 1` row exactly once at first connection-open, so a bare `DELETE`
/// would leave every later request against that singleton 404ing on a
/// "Query returned no rows" error for the lifetime of the process.
///
/// Deliberately out of scope even after the Job Driver (slice 2) and Scrape
/// Driver (slice 4) migrations (see "Out Of Scope For First Slice" in
/// docs/change/2026-07-11-hurl-web-integration-tests.md): this is not a
/// quiescence boundary for either driver. `reset()` (the caller, below) does
/// drop both drivers' blocked senders so a *currently blocked* download/
/// split/scrape step is woken and resolves without writing further fixtures
/// — but a step already queued and not yet started, or one that already
/// released and is mid-write, is unaffected and can still write after this
/// function returns. Hurl's `--jobs 1` shared-process convention (see
/// hurl/README.md) means no file actually calls `/test/reset` mid-run, so
/// this caveat has not needed a stronger guarantee in practice.
fn reset_test_data(state: &AppState) -> anyhow::Result<()> {
    for dir_name in ["concerts", "thumbnails"] {
        let dir = state.jobs.working_dir.join(dir_name);
        if !dir.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
        }
    }
    let conn = state.db.lock().unwrap();
    conn.execute_batch(
        "DELETE FROM playlist_items;
         DELETE FROM playlists;
         DELETE FROM jobs;
         DELETE FROM events;
         DELETE FROM concerts;
         DELETE FROM synced_months;
         UPDATE settings SET archive_location = NULL, theme = 'system' WHERE id = 1;",
    )?;
    Ok(())
}

/// Start the Test Control API. Always binds loopback-only, regardless of the
/// app server's configured `--host` — the API never becomes reachable off-box.
/// Returns a handle (keep it alive for the process lifetime; dropping it stops
/// the server) and the bound address for the startup banner.
///
/// Mounts the [`adapter`] as HTTP middleware in front of jsonrpsee's own
/// server so `POST /test/...` adapter routes and the raw JSON-RPC root
/// endpoint share one listener and port (see
/// docs/adr/0004-test-control-http-adapter.md).
pub async fn start(
    state: AppState,
    job_driver: Arc<JobDriver>,
    scrape_driver: Arc<ScrapeDriver>,
    port: u16,
) -> anyhow::Result<(ServerHandle, SocketAddr)> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let module = TestControlServer::new(state, job_driver, scrape_driver).rpc_module();
    let methods: jsonrpsee::server::Methods = module.clone().into();
    let server = ServerBuilder::new()
        .set_http_middleware(
            tower::ServiceBuilder::new().layer(adapter::TestControlAdapterLayer::new(methods)),
        )
        .build(addr)
        .await?;
    let bound = server.local_addr()?;
    let handle = server.start(module);
    Ok((handle, bound))
}

/// Shared by this module's own tests and by [`adapter`]'s tests (a
/// descendant module can reach a private ancestor item directly).
#[cfg(test)]
fn test_state(conn: rusqlite::Connection, workdir: std::path::PathBuf) -> AppState {
    use crate::jobs::scrape_queue::ScrapeQueue;
    use crate::jobs::{JobConfig, JobRegistry};
    use std::sync::{Arc, Mutex};

    tiny_desk_scraper::set_proxy_mode(tiny_desk_scraper::ProxyMode::None);
    AppState {
        db: Arc::new(Mutex::new(conn)),
        registry: Arc::new(JobRegistry::new()),
        scrape_queue: ScrapeQueue::start(
            Arc::new(Mutex::new(db::connection::open_in_memory().unwrap())),
            workdir.clone(),
        ),
        jobs: JobConfig::test(workdir),
    }
}

/// Shared by this module's own tests and by [`adapter`]'s tests, same
/// visibility rationale as [`test_state`]. A fresh, never-configured driver —
/// tests that need specific plans/observations set them up explicitly.
#[cfg(test)]
fn test_job_driver() -> Arc<JobDriver> {
    Arc::new(JobDriver::new())
}

/// Shared by this module's own tests and by [`adapter`]'s tests, same
/// visibility rationale as [`test_job_driver`]. A fresh, never-configured
/// driver — tests that need specific plans/observations set them up
/// explicitly.
#[cfg(test)]
fn test_scrape_driver() -> Arc<ScrapeDriver> {
    Arc::new(ScrapeDriver::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::concerts::NewListing;
    use crate::db::settings;
    use jsonrpsee::types::Params;

    fn params(json: &'static str) -> Params<'static> {
        Params::new(Some(json))
    }

    fn request<T: serde::de::DeserializeOwned>(json: &'static str) -> T {
        params(json).parse().unwrap()
    }

    fn listing(json: &'static str) -> SeedListing {
        request(json)
    }

    fn scraped(json: &'static str) -> SeedScrapedConcert {
        request(json)
    }

    fn lifecycle(json: &'static str) -> SeedLifecycleConcert {
        request(json)
    }

    fn media(json: &'static str) -> SeedMediaConcert {
        request(json)
    }

    fn album_null(json: &'static str) -> SeedAlbumNullConcert {
        request(json)
    }

    fn fixture_number_from_source_url(source_url: &str) -> u64 {
        source_url
            .strip_prefix("https://example.test/tiny-desk/test-control-")
            .expect("generated source_url must use the example.test fixture prefix")
            .parse()
            .expect("generated source_url must end with a fixture number")
    }

    async fn raw_json_call(
        methods: &jsonrpsee::server::Methods,
        request: &str,
    ) -> serde_json::Value {
        let (response, _subscriptions) = methods.raw_json_request(request, 1).await.unwrap();
        serde_json::from_str(response.get()).unwrap()
    }

    // The tests below cover the Test Control-specific surface: RPC envelope
    // parsing (`param_kind = map`, the adapter's nested-params contract), the
    // shared FIXTURE_IDS allocator, and Concert -> Seed Result JSON mapping.
    // Fixture defaulting/null semantics and DB write behavior are unit-tested
    // directly against `db::seeds::SeedContext` in `db/seeds.rs`.

    #[tokio::test]
    async fn seed_methods_persist_through_the_rpc_dispatch_path() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let server = TestControlServer::new(state.clone(), test_job_driver(), test_scrape_driver());

        let result = server.seed_listing(listing("{}")).await.unwrap();
        assert!(result.id > 0);
        let conn = state.db.lock().unwrap();
        assert!(db::concerts::get_concert(&conn, result.id).is_ok());
    }

    #[tokio::test]
    async fn seed_lifecycle_concert_via_rpc_maps_persisted_state_into_result_booleans() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let server = TestControlServer::new(state, test_job_driver(), test_scrape_driver());

        let inert = server
            .seed_lifecycle_concert(lifecycle("{}"))
            .await
            .unwrap();
        assert!(!inert.downloaded);
        assert!(!inert.split);

        let split = server
            .seed_lifecycle_concert(lifecycle(r#"{"split": true}"#))
            .await
            .unwrap();
        assert!(
            split.downloaded,
            "split implies downloaded in the mapped result"
        );
        assert!(split.split);
    }

    /// `tracks_present` is deliberately not echoed on `SeedLifecycleConcertResult`
    /// (no Hurl case reads it yet, and echoing the request rather than the
    /// persisted row would let a broken seed write pass unnoticed) — so this
    /// asserts the DB write directly, the same way `db/seeds.rs`'s unit tests do.
    #[tokio::test]
    async fn seed_lifecycle_concert_tracks_present_persists_through_the_rpc_dispatch_path() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let server = TestControlServer::new(state.clone(), test_job_driver(), test_scrape_driver());

        let result = server
            .seed_lifecycle_concert(lifecycle(r#"{"tracks_present": [true, false]}"#))
            .await
            .unwrap();

        let conn = state.db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, result.id).unwrap();
        assert_eq!(concert.tracks_present, vec![true, false]);
    }

    #[tokio::test]
    async fn seed_media_concert_writes_files_and_liked_state_through_rpc_dispatch_path() {
        let conn = db::connection::open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let state = test_state(conn, workdir.path().to_path_buf());
        let server = TestControlServer::new(state.clone(), test_job_driver(), test_scrape_driver());

        let result = server
            .seed_media_concert(media(
                r#"{
                    "album": "RPC Media Fixture",
                    "set_list": ["Song A", "Song B"],
                    "track_files": [1],
                    "tracks_liked": [false, true]
                }"#,
            ))
            .await
            .unwrap();

        let conn = state.db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, result.id).unwrap();
        assert_eq!(concert.tracks_liked, vec![false, true]);
        let dir = crate::model::concert_dir(workdir.path(), "RPC Media Fixture");
        assert!(!dir.join("Song A.mp3").exists());
        assert!(dir.join("Song B.mp3").exists());
    }

    #[tokio::test]
    async fn seed_album_null_concert_persists_null_album_through_rpc_dispatch_path() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let server = TestControlServer::new(state.clone(), test_job_driver(), test_scrape_driver());

        let result = server
            .seed_album_null_concert(album_null(
                r#"{"set_list": ["Song A"], "tracks_present": [true]}"#,
            ))
            .await
            .unwrap();

        let conn = state.db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, result.id).unwrap();
        assert_eq!(concert.album, None);
        assert_eq!(concert.set_list, vec!["Song A".to_string()]);
        assert_eq!(concert.tracks_present, vec![true]);
    }

    #[tokio::test]
    async fn seed_defaults_generate_unique_urls_across_methods_and_reset() {
        // Pins that FIXTURE_IDS is one process-lifetime allocator (ADR 0003):
        // shared across every seed method and every TestControlServer clone,
        // and not reset by `test.reset`.
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let server = TestControlServer::new(state, test_job_driver(), test_scrape_driver());

        let listing_result = server.seed_listing(listing("{}")).await.unwrap();
        reset_test_data(&server.state).unwrap();
        let scraped_result = server.seed_scraped_concert(scraped("{}")).await.unwrap();
        let lifecycle_result = server
            .seed_lifecycle_concert(lifecycle("{}"))
            .await
            .unwrap();

        let urls = [
            &listing_result.source_url,
            &scraped_result.source_url,
            &lifecycle_result.source_url,
        ];
        assert_eq!(
            urls.iter().collect::<std::collections::HashSet<_>>().len(),
            3
        );
        for url in urls {
            fixture_number_from_source_url(url);
        }
    }

    #[tokio::test]
    async fn explicit_flat_map_seed_params_override_generated_defaults() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let server = TestControlServer::new(state, test_job_driver(), test_scrape_driver());

        let listing_result = server
            .seed_listing(listing(
                r#"{
                    "source_url": "https://npr.org/c/explicit-listing",
                    "title": "Explicit Listing",
                    "concert_date": "2024-01-15",
                    "teaser": "Visible teaser"
                }"#,
            ))
            .await
            .unwrap();
        let scraped_result = server
            .seed_scraped_concert(scraped(
                r#"{
                    "source_url": "https://npr.org/c/explicit-scraped",
                    "title": "Explicit Scraped",
                    "concert_date": "2024-02-15",
                    "artist": "Explicit Artist",
                    "album": "Explicit Album",
                    "set_list": ["One"]
                }"#,
            ))
            .await
            .unwrap();
        let lifecycle_result = server
            .seed_lifecycle_concert(lifecycle(
                r#"{
                    "source_url": "https://npr.org/c/explicit-lifecycle",
                    "title": "Explicit Lifecycle",
                    "concert_date": "2024-03-15",
                    "artist": "Explicit Lifecycle Artist",
                    "album": "Explicit Lifecycle Album",
                    "set_list": ["One"],
                    "downloaded": true,
                    "split": true
                }"#,
            ))
            .await
            .unwrap();

        assert_eq!(listing_result.title, "Explicit Listing");
        assert_eq!(
            listing_result.source_url,
            "https://npr.org/c/explicit-listing"
        );
        assert_eq!(scraped_result.album, "Explicit Album");
        assert_eq!(lifecycle_result.album, "Explicit Lifecycle Album");
        assert!(lifecycle_result.downloaded);
        assert!(lifecycle_result.split);
    }

    #[tokio::test]
    async fn raw_jsonrpc_generated_seed_method_accepts_nested_request_object_params() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let methods: jsonrpsee::server::Methods =
            TestControlServer::new(state, test_job_driver(), test_scrape_driver())
                .rpc_module()
                .into();

        let body = raw_json_call(
            &methods,
            r#"{
                "jsonrpc": "2.0",
                "id": "raw",
                "method": "test.seed_listing",
                "params": {
                    "params": {
                        "source_url": "https://npr.org/c/raw-nested-seed",
                        "title": "Raw Nested Seed"
                    }
                }
            }"#,
        )
        .await;

        assert_eq!(body["id"], "raw");
        assert_eq!(body["result"]["title"], "Raw Nested Seed");
        assert_eq!(
            body["result"]["source_url"],
            "https://npr.org/c/raw-nested-seed"
        );
    }

    #[tokio::test]
    async fn raw_jsonrpc_generated_seed_method_rejects_old_flat_params_shape() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let methods: jsonrpsee::server::Methods =
            TestControlServer::new(state, test_job_driver(), test_scrape_driver())
                .rpc_module()
                .into();

        let body = raw_json_call(
            &methods,
            r#"{
                "jsonrpc": "2.0",
                "id": "raw",
                "method": "test.seed_listing",
                "params": {
                    "title": "Raw Flat Seed"
                }
            }"#,
        )
        .await;

        assert_eq!(body["id"], "raw");
        assert_eq!(body["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn explicit_null_preserves_nullable_domain_absence_through_rpc() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let server = TestControlServer::new(state.clone(), test_job_driver(), test_scrape_driver());

        let listing_result = server
            .seed_listing(listing(r#"{"concert_date": null, "teaser": null}"#))
            .await
            .unwrap();
        let lifecycle_result = server
            .seed_lifecycle_concert(lifecycle(
                r#"{
                    "concert_date": null,
                    "set_list": null,
                    "auto_timestamps": null,
                    "user_timestamps": null,
                    "media_duration": null
                }"#,
            ))
            .await
            .unwrap();

        assert!(listing_result.concert_date.is_none());
        let conn = state.db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, lifecycle_result.id).unwrap();
        assert!(concert.concert_date.is_none());
        assert!(concert.media_duration.is_none());
    }

    /// Pins the ADR 0003 behavior change: explicit `null` for identity fields
    /// (`source_url`/`title`/`artist`/`album`) used to be rejected by the
    /// removed `OmittedOr` wrapper; now it deserializes fine and means "use
    /// the generated default", the same as omitting the field.
    #[tokio::test]
    async fn explicit_null_for_identity_fields_is_accepted_via_rpc() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let server = TestControlServer::new(state, test_job_driver(), test_scrape_driver());

        let parsed = params(r#"{"source_url": null, "artist": null}"#)
            .parse::<SeedLifecycleConcert>()
            .expect("explicit null for identity fields must deserialize, not error");
        let result = server.seed_lifecycle_concert(parsed).await.unwrap();
        assert!(result.id > 0);
    }

    #[tokio::test]
    async fn seed_lifecycle_concert_marks_downloaded_and_split_without_files() {
        let conn = db::connection::open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let state = test_state(conn, workdir.path().to_path_buf());

        let result = super::seed_lifecycle_concert(
            &state,
            SeedLifecycleConcert {
                source_url: Some("https://npr.org/c/lifecycle-split".to_string()),
                split: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert!(
            result.downloaded,
            "split implies downloaded lifecycle state"
        );
        assert!(result.split);
        assert!(
            std::fs::read_dir(workdir.path()).unwrap().next().is_none(),
            "state-only seed must not create files under the workdir"
        );
    }

    #[tokio::test]
    async fn seed_listing_result_maps_concert_fields() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());

        let result = super::seed_listing(
            &state,
            SeedListing {
                source_url: Some("https://npr.org/c/seed-test".to_string()),
                title: Some("Seed Test Concert".to_string()),
                concert_date: Some("2024-01-15".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(result.source_url, "https://npr.org/c/seed-test");
        assert_eq!(result.title, "Seed Test Concert");
        assert_eq!(result.concert_date.as_deref(), Some("2024-01-15"));
    }

    #[tokio::test]
    async fn seed_scraped_concert_result_maps_album_from_concert() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());

        let result = super::seed_scraped_concert(
            &state,
            SeedScrapedConcert {
                source_url: Some("https://npr.org/c/scraped-test".to_string()),
                title: Some("Scraped Test Concert".to_string()),
                album: Some("Test Album".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(result.source_url, "https://npr.org/c/scraped-test");
        assert_eq!(result.title, "Scraped Test Concert");
        assert_eq!(result.album, "Test Album");
    }

    #[tokio::test]
    async fn assert_concert_state_passes_when_expectations_match() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let seeded = super::seed_listing(
            &state,
            SeedListing {
                source_url: Some("https://npr.org/c/assert-pass".to_string()),
                title: Some("Assert Pass".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        super::assert_concert_state(&state, seeded.id, Some(false), Some(false), Some(false))
            .unwrap();
    }

    #[tokio::test]
    async fn assert_concert_state_reports_every_mismatch_in_one_message() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let seeded = super::seed_listing(
            &state,
            SeedListing {
                source_url: Some("https://npr.org/c/assert-fail".to_string()),
                title: Some("Assert Fail".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        let err = super::assert_concert_state(&state, seeded.id, Some(true), Some(true), None)
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("ignored: expected true, got false"),
            "{message}"
        );
        assert!(
            message.contains("downloaded: expected true, got false"),
            "{message}"
        );
        // split wasn't asserted (None), so it must not appear in the message.
        assert!(!message.contains("split:"), "{message}");
    }

    #[tokio::test]
    async fn assert_concert_state_only_checks_present_expectations() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let seeded = super::seed_listing(
            &state,
            SeedListing {
                source_url: Some("https://npr.org/c/assert-partial".to_string()),
                title: Some("Assert Partial".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        // Only asserting `ignored`; downloaded/split are omitted (None) and
        // must not be checked even though this concert can't be Downloaded.
        super::assert_concert_state(&state, seeded.id, Some(false), None, None).unwrap();
    }

    #[tokio::test]
    async fn assert_concert_state_reports_an_unknown_id_as_an_error() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());

        let err = super::assert_concert_state(&state, 999, Some(true), None, None).unwrap_err();
        assert!(err.to_string().contains("999"), "{err}");
    }

    #[tokio::test]
    async fn assert_concert_state_rejects_a_call_with_no_expectations() {
        // ignored/downloaded/split all None must error, not vacuously
        // succeed — a caller that forgets to fill in any expectation would
        // otherwise get a false "assertion passed" for any existing id.
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let seeded = super::seed_listing(
            &state,
            SeedListing {
                source_url: Some("https://npr.org/c/assert-empty".to_string()),
                title: Some("Assert Empty".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        let err = super::assert_concert_state(&state, seeded.id, None, None, None).unwrap_err();
        assert!(err.to_string().contains("no expectations"), "{err}");
    }

    // ---------- assert_concert_events ----------

    #[tokio::test]
    async fn assert_concert_events_passes_when_present_and_absent_both_hold() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let seeded = super::seed_listing(&state, SeedListing::default()).unwrap();
        {
            let conn = state.db.lock().unwrap();
            crate::events::record_now(
                &conn,
                seeded.id,
                crate::events::Event::InterludeDelete,
                None,
            );
        }

        super::assert_concert_events(
            &state,
            seeded.id,
            Some(vec!["interlude_delete".to_string()]),
            Some(vec!["track_delete".to_string()]),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn assert_concert_events_reports_every_mismatch_in_one_message() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let seeded = super::seed_listing(&state, SeedListing::default()).unwrap();
        {
            let conn = state.db.lock().unwrap();
            crate::events::record_now(&conn, seeded.id, crate::events::Event::TrackDelete, None);
        }

        let err = super::assert_concert_events(
            &state,
            seeded.id,
            Some(vec!["interlude_delete".to_string()]),
            Some(vec!["track_delete".to_string()]),
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("expected event \"interlude_delete\" to be present, found none"),
            "{message}"
        );
        assert!(
            message.contains("expected event \"track_delete\" to be absent, found 1"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn assert_concert_events_rejects_a_call_with_no_expectations() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let seeded = super::seed_listing(&state, SeedListing::default()).unwrap();

        let err = super::assert_concert_events(&state, seeded.id, None, None).unwrap_err();
        assert!(err.to_string().contains("no expectations"), "{err}");

        let err = super::assert_concert_events(&state, seeded.id, Some(vec![]), Some(vec![]))
            .unwrap_err();
        assert!(err.to_string().contains("no expectations"), "{err}");
    }

    #[tokio::test]
    async fn assert_concert_events_rejects_an_unknown_event_name() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let seeded = super::seed_listing(&state, SeedListing::default()).unwrap();

        let err = super::assert_concert_events(
            &state,
            seeded.id,
            None,
            Some(vec!["not_a_real_event".to_string()]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown event name"), "{err}");
    }

    /// A query failure while listing events must surface as an error, not be
    /// misread as "no events" and let an `absent` assertion vacuously pass —
    /// the exact gap `list_for_concert`'s error-swallowing would otherwise
    /// create here (see `crate::events::try_list_for_concert`'s doc comment).
    #[tokio::test]
    async fn assert_concert_events_propagates_a_query_failure_instead_of_passing_absent() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let seeded = super::seed_listing(&state, SeedListing::default()).unwrap();
        {
            let conn = state.db.lock().unwrap();
            conn.execute("DROP TABLE events", []).unwrap();
        }

        let err = super::assert_concert_events(
            &state,
            seeded.id,
            None,
            Some(vec!["track_delete".to_string()]),
        )
        .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("no such table"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn assert_concert_events_reports_an_unknown_id_as_an_error() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());

        let err = super::assert_concert_events(
            &state,
            999,
            Some(vec!["interlude_delete".to_string()]),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("999"), "{err}");
    }

    #[tokio::test]
    async fn reset_clears_concerts_and_settings_but_leaves_the_settings_row() {
        let conn = db::connection::open_in_memory().unwrap();
        db::concerts::upsert_listing(
            &conn,
            &NewListing {
                source_url: "https://npr.org/c/reset-test".to_string(),
                title: "Reset Test Concert".to_string(),
                concert_date: Some("2024-01-15".to_string()),
                teaser: None,
            },
        )
        .unwrap();
        settings::update_archive_location(&conn, "/nas/media").unwrap();
        settings::update_theme(&conn, settings::Theme::Dark).unwrap();
        db::sync::mark_month_synced(&conn, 2024, 1).unwrap();

        let workdir = tempfile::tempdir().unwrap();
        let concerts_dir = workdir.path().join("concerts");
        let thumbnails_dir = workdir.path().join("thumbnails");
        std::fs::create_dir_all(&concerts_dir).unwrap();
        std::fs::create_dir_all(&thumbnails_dir).unwrap();
        std::fs::write(concerts_dir.join("leftover.mp4"), b"x").unwrap();
        std::fs::write(thumbnails_dir.join("leftover.jpg"), b"x").unwrap();
        assert_eq!(
            db::sync::list_fully_synced_months(&conn).unwrap(),
            vec![(2024, 1)],
            "precondition: the synced-month row must be visible before reset"
        );

        let state = test_state(conn, workdir.path().to_path_buf());
        reset_test_data(&state).unwrap();

        let conn = state.db.lock().unwrap();
        assert!(db::concerts::list_concerts(&conn).unwrap().is_empty());
        // The settings singleton row survives reset (only its values are
        // cleared) — see reset_test_data's doc comment for why a bare DELETE
        // would break every subsequent request against it.
        let s = settings::get_settings(&conn).unwrap();
        assert!(s.archive_location.is_none());
        assert_eq!(s.theme, settings::Theme::System);
        assert!(db::sync::list_fully_synced_months(&conn)
            .unwrap()
            .is_empty());

        assert!(std::fs::read_dir(&concerts_dir).unwrap().next().is_none());
        assert!(std::fs::read_dir(&thumbnails_dir).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reset_leaves_db_rows_intact_when_filesystem_cleanup_fails() {
        use std::os::unix::fs::PermissionsExt;

        let conn = db::connection::open_in_memory().unwrap();
        db::concerts::upsert_listing(
            &conn,
            &NewListing {
                source_url: "https://npr.org/c/reset-fs-fail".to_string(),
                title: "Reset FS Fail Concert".to_string(),
                concert_date: Some("2024-01-15".to_string()),
                teaser: None,
            },
        )
        .unwrap();

        let workdir = tempfile::tempdir().unwrap();
        let blocked = workdir.path().join("concerts").join("blocked");
        std::fs::create_dir_all(&blocked).unwrap();
        std::fs::write(blocked.join("file.mp4"), b"x").unwrap();
        // Strip all permissions from the subdirectory so removing its
        // contents fails partway through, simulating a filesystem cleanup
        // failure (e.g. a permissions or transient I/O error).
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let state = test_state(conn, workdir.path().to_path_buf());
        let result = reset_test_data(&state);

        // Restore permissions so the tempdir's own Drop cleanup can succeed.
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            result.is_err(),
            "a filesystem cleanup failure must surface as an error, not be swallowed"
        );
        let conn = state.db.lock().unwrap();
        // Filesystem cleanup runs before the DB reset specifically so a
        // failure here leaves prior concert rows (and their now-still-valid
        // directories) intact rather than deleting the DB rows first and
        // leaving orphaned files a later same-named seed could inherit.
        assert_eq!(
            db::concerts::list_concerts(&conn).unwrap().len(),
            1,
            "DB must be untouched when filesystem cleanup fails first"
        );
    }

    #[tokio::test]
    async fn reset_on_a_fresh_db_with_no_workdir_succeeds() {
        let conn = db::connection::open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        // Deliberately don't create concerts/thumbnails — reset must tolerate
        // a workdir that has never produced any generated files yet.
        let state = test_state(conn, workdir.path().to_path_buf());
        reset_test_data(&state).unwrap();
    }

    // ---------- Scrape Driver: scrape_enqueue / assert_scrape_observation / reset ----------

    #[tokio::test]
    async fn scrape_enqueue_marks_pending_and_dedupes_a_second_call() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let seeded = super::seed_listing(&state, SeedListing::default()).unwrap();

        let first = super::scrape_enqueue(&state, seeded.id).unwrap();
        assert!(first, "first enqueue call must queue the scrape");
        assert!(state.scrape_queue.is_pending(seeded.id));

        let second = super::scrape_enqueue(&state, seeded.id).unwrap();
        assert!(
            !second,
            "a concert already queued/in-flight is a normal dedupe no-op, not an error"
        );
    }

    #[tokio::test]
    async fn scrape_enqueue_reports_an_unknown_id_as_an_error() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());

        let err = super::scrape_enqueue(&state, 999).unwrap_err();
        assert!(err.to_string().contains("999"), "{err}");
    }

    #[test]
    fn assert_scrape_observation_rejects_a_call_with_no_expectations() {
        let driver = ScrapeDriver::new();
        let err = super::assert_scrape_observation(&driver, 1, None, None, None, None).unwrap_err();
        assert!(err.to_string().contains("no expectations"), "{err}");
    }

    #[test]
    fn assert_scrape_observation_reports_every_mismatch_in_one_message() {
        let driver = ScrapeDriver::new();
        driver.set_plan(1, ScrapeOutcome::Block);
        // Never released — `started`/`blocked` will be 0 since nothing ran
        // `run_item` yet, so every non-zero expectation mismatches.
        let err = super::assert_scrape_observation(&driver, 1, Some(1), Some(1), Some(1), Some(1))
            .unwrap_err();
        for field in ["started", "completed", "blocked", "released"] {
            assert!(err.to_string().contains(field), "{err}");
        }
    }

    #[tokio::test]
    async fn reset_clears_scrape_driver_plans_and_observations() {
        let conn = db::connection::open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let state = test_state(conn, workdir.path().to_path_buf());
        let seeded = super::seed_listing(&state, SeedListing::default()).unwrap();
        let server = TestControlServer::new(state.clone(), test_job_driver(), test_scrape_driver());

        // Bump an observation through the public `ScrapeItemFn` seam (default
        // plan succeeds immediately, no blocking/threading needed) so there
        // is state for `reset` to actually clear.
        let item = scrape_driver::scrape_item_fn(server.scrape_driver.clone());
        let req = crate::jobs::scrape_queue::ScrapeRequest {
            concert_id: seeded.id,
            source_url: seeded.source_url.clone(),
        };
        item(&state.db, workdir.path(), &req);
        assert_eq!(server.scrape_driver.observation(seeded.id).completed, 1);

        TestControlApiServer::reset(&server).await.unwrap();

        assert_eq!(
            server.scrape_driver.observation(seeded.id),
            scrape_driver::ScrapeObservation::default(),
            "reset must clear scrape observations"
        );
    }
}
