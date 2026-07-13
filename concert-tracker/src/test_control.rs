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

#[cfg(all(feature = "test-control", not(debug_assertions)))]
compile_error!("test-control must not be compiled into release builds");

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};

use jsonrpsee::core::{async_trait, RpcResult};
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::server::{RpcModule, ServerBuilder, ServerHandle};
use jsonrpsee::types::{ErrorObjectOwned, Params};
use serde::{Deserialize, Deserializer, Serialize};

use crate::db;
use crate::web::AppState;

const DEFAULT_CONCERT_DATE: &str = "2026-01-01";
static NEXT_TEST_CONTROL_FIXTURE: AtomicU64 = AtomicU64::new(1);

#[rpc(server, namespace = "test", namespace_separator = ".")]
pub trait TestControlApi {
    /// Clear concert, event, playlist, job, and settings test data, and
    /// remove generated concert files/thumbnails under the configured
    /// workdir. Leaves the SQLite schema and server configuration intact.
    #[method(name = "reset")]
    async fn reset(&self) -> RpcResult<OkResult>;

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

pub struct TestControlServer {
    state: AppState,
}

impl TestControlServer {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    fn rpc_module(self) -> RpcModule<Self> {
        let mut rpc = TestControlApiServer::into_rpc(self);
        rpc.register_async_method("test.seed_listing", |params, server, _| async move {
            let params = SeedListingRequest::parse(params)?;
            server.seed_listing_rpc(params).await
        })
        .expect("test.seed_listing must register once");
        rpc.register_async_method(
            "test.seed_scraped_concert",
            |params, server, _| async move {
                let params = SeedScrapedConcertRequest::parse(params)?;
                server.seed_scraped_concert_rpc(params).await
            },
        )
        .expect("test.seed_scraped_concert must register once");
        rpc.register_async_method(
            "test.seed_lifecycle_concert",
            |params, server, _| async move {
                let params = SeedLifecycleConcertRequest::parse(params)?;
                server.seed_lifecycle_concert_rpc(params).await
            },
        )
        .expect("test.seed_lifecycle_concert must register once");
        rpc
    }

    async fn seed_listing_rpc(&self, params: SeedListingRequest) -> RpcResult<SeedListingResult> {
        let params = params.with_defaults();
        seed_listing(
            &self.state,
            params.source_url,
            params.title,
            params.concert_date,
            params.teaser,
        )
        .map_err(internal_error)
    }

    async fn seed_scraped_concert_rpc(
        &self,
        params: SeedScrapedConcertRequest,
    ) -> RpcResult<SeedScrapedConcertResult> {
        let params = params.with_defaults();
        seed_scraped_concert(
            &self.state,
            params.source_url,
            params.title,
            params.concert_date,
            params.artist,
            params.album,
            params.set_list,
        )
        .map_err(internal_error)
    }

    async fn seed_lifecycle_concert_rpc(
        &self,
        params: SeedLifecycleConcertRequest,
    ) -> RpcResult<SeedLifecycleConcertResult> {
        let params = params.with_defaults();
        seed_lifecycle_concert(
            &self.state,
            SeedLifecycleConcertParams {
                source_url: params.source_url,
                title: params.title,
                concert_date: params.concert_date,
                artist: params.artist,
                album: params.album,
                set_list: params.set_list,
                downloaded: params.downloaded,
                split: params.split,
                auto_timestamps: params.auto_timestamps,
                user_timestamps: params.user_timestamps,
                media_duration: params.media_duration,
            },
        )
        .map_err(internal_error)
    }
}

#[async_trait]
impl TestControlApiServer for TestControlServer {
    async fn reset(&self) -> RpcResult<OkResult> {
        reset_test_data(&self.state)
            .map(|()| OkResult { ok: true })
            .map_err(internal_error)
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

fn allocate_fixture_number() -> u64 {
    NEXT_TEST_CONTROL_FIXTURE.fetch_add(1, Ordering::Relaxed)
}

fn fixture_source_url(n: u64) -> String {
    format!("https://example.test/tiny-desk/test-control-{n}")
}

fn fixture_set_list(n: u64) -> Vec<String> {
    vec![
        format!("Test Control Track {n}.1"),
        format!("Test Control Track {n}.2"),
        format!("Test Control Track {n}.3"),
    ]
}

#[derive(Debug)]
enum OmittedOr<T> {
    Omitted,
    Present(T),
}

impl<T> Default for OmittedOr<T> {
    fn default() -> Self {
        Self::Omitted
    }
}

impl<'de, T> Deserialize<'de> for OmittedOr<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Self::Present)
    }
}

impl<T> OmittedOr<T> {
    fn unwrap_or_else(self, default: impl FnOnce() -> T) -> T {
        match self {
            Self::Omitted => default(),
            Self::Present(value) => value,
        }
    }

    fn unwrap_or(self, default: T) -> T {
        self.unwrap_or_else(|| default)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeedListingRequest {
    #[serde(default)]
    source_url: OmittedOr<String>,
    #[serde(default)]
    title: OmittedOr<String>,
    #[serde(default)]
    concert_date: OmittedOr<Option<String>>,
    #[serde(default)]
    teaser: OmittedOr<Option<String>>,
}

struct SeedListingDefaults {
    source_url: String,
    title: String,
    concert_date: Option<String>,
    teaser: Option<String>,
}

impl SeedListingRequest {
    fn parse(params: Params<'static>) -> RpcResult<Self> {
        params.parse()
    }

    fn with_defaults(self) -> SeedListingDefaults {
        let n = allocate_fixture_number();
        SeedListingDefaults {
            source_url: self.source_url.unwrap_or_else(|| fixture_source_url(n)),
            title: self.title.unwrap_or_else(|| format!("Test Listing {n}")),
            concert_date: self
                .concert_date
                .unwrap_or_else(|| Some(DEFAULT_CONCERT_DATE.to_string())),
            teaser: self
                .teaser
                .unwrap_or_else(|| Some(format!("Test listing teaser {n}"))),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeedScrapedConcertRequest {
    #[serde(default)]
    source_url: OmittedOr<String>,
    #[serde(default)]
    title: OmittedOr<String>,
    #[serde(default)]
    concert_date: OmittedOr<Option<String>>,
    #[serde(default)]
    artist: OmittedOr<String>,
    #[serde(default)]
    album: OmittedOr<String>,
    #[serde(default)]
    set_list: OmittedOr<Option<Vec<String>>>,
}

struct SeedScrapedConcertDefaults {
    source_url: String,
    title: String,
    concert_date: Option<String>,
    artist: String,
    album: String,
    set_list: Vec<String>,
}

impl SeedScrapedConcertRequest {
    fn parse(params: Params<'static>) -> RpcResult<Self> {
        params.parse()
    }

    fn with_defaults(self) -> SeedScrapedConcertDefaults {
        let n = allocate_fixture_number();
        SeedScrapedConcertDefaults {
            source_url: self.source_url.unwrap_or_else(|| fixture_source_url(n)),
            title: self
                .title
                .unwrap_or_else(|| format!("Test Scraped Concert {n}")),
            concert_date: self
                .concert_date
                .unwrap_or_else(|| Some(DEFAULT_CONCERT_DATE.to_string())),
            artist: self
                .artist
                .unwrap_or_else(|| format!("Test Scraped Artist {n}")),
            album: self
                .album
                .unwrap_or_else(|| format!("Test Scraped Album {n}")),
            set_list: self
                .set_list
                .unwrap_or_else(|| Some(fixture_set_list(n)))
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeedLifecycleConcertRequest {
    #[serde(default)]
    source_url: OmittedOr<String>,
    #[serde(default)]
    title: OmittedOr<String>,
    #[serde(default)]
    concert_date: OmittedOr<Option<String>>,
    #[serde(default)]
    artist: OmittedOr<String>,
    #[serde(default)]
    album: OmittedOr<String>,
    #[serde(default)]
    set_list: OmittedOr<Option<Vec<String>>>,
    #[serde(default)]
    downloaded: OmittedOr<bool>,
    #[serde(default)]
    split: OmittedOr<bool>,
    #[serde(default)]
    auto_timestamps: OmittedOr<Option<Vec<concert_types::SongTimestamp>>>,
    #[serde(default)]
    user_timestamps: OmittedOr<Option<Vec<concert_types::SongTimestamp>>>,
    #[serde(default)]
    media_duration: OmittedOr<Option<f64>>,
}

impl SeedLifecycleConcertRequest {
    fn parse(params: Params<'static>) -> RpcResult<Self> {
        params.parse()
    }

    fn with_defaults(self) -> SeedLifecycleConcertParams {
        let n = allocate_fixture_number();
        SeedLifecycleConcertParams {
            source_url: self.source_url.unwrap_or_else(|| fixture_source_url(n)),
            title: self
                .title
                .unwrap_or_else(|| format!("Test Lifecycle Concert {n}")),
            concert_date: self
                .concert_date
                .unwrap_or_else(|| Some(DEFAULT_CONCERT_DATE.to_string())),
            artist: self
                .artist
                .unwrap_or_else(|| format!("Test Lifecycle Artist {n}")),
            album: self
                .album
                .unwrap_or_else(|| format!("Test Lifecycle Album {n}")),
            set_list: self
                .set_list
                .unwrap_or_else(|| Some(fixture_set_list(n)))
                .unwrap_or_default(),
            downloaded: self.downloaded.unwrap_or(false),
            split: self.split.unwrap_or(false),
            auto_timestamps: self.auto_timestamps.unwrap_or(None),
            user_timestamps: self.user_timestamps.unwrap_or(None),
            media_duration: self.media_duration.unwrap_or(None),
        }
    }
}

#[derive(Deserialize)]
struct SeedLifecycleConcertParams {
    source_url: String,
    title: String,
    concert_date: Option<String>,
    artist: String,
    album: String,
    set_list: Vec<String>,
    downloaded: bool,
    split: bool,
    auto_timestamps: Option<Vec<concert_types::SongTimestamp>>,
    user_timestamps: Option<Vec<concert_types::SongTimestamp>>,
    media_duration: Option<f64>,
}

/// Inserts (or updates, on a `source_url` collision) a listing through the
/// same `db::concerts::upsert_listing` path the real scraper uses. Looks the
/// row back up by `source_url` rather than trusting
/// `Connection::last_insert_rowid` — `upsert_listing` is an `INSERT ... ON
/// CONFLICT DO UPDATE`, and SQLite only advances `last_insert_rowid` for the
/// `INSERT` branch, so it would silently return a stale id whenever a Hurl
/// case reseeds an already-used `source_url`.
fn seed_listing(
    state: &AppState,
    source_url: String,
    title: String,
    concert_date: Option<String>,
    teaser: Option<String>,
) -> anyhow::Result<SeedListingResult> {
    let conn = state.db.lock().unwrap();
    db::concerts::upsert_listing(
        &conn,
        &db::concerts::NewListing {
            source_url: source_url.clone(),
            title,
            concert_date,
            teaser,
        },
    )?;
    let concert = db::concerts::get_concert_by_url(&conn, &source_url)?.ok_or_else(|| {
        anyhow::anyhow!(
            "upsert_listing succeeded but the row is not readable back by source_url: {source_url}"
        )
    })?;
    Ok(SeedListingResult {
        id: concert.id,
        source_url: concert.source_url,
        title: concert.title,
        concert_date: concert.concert_date,
    })
}

/// Seeds a listing, then applies scraped metadata through the same
/// `db::concerts::update_metadata` path the real scraper uses — setting
/// `metadata_scraped_at`, which is what moves a concert out of the
/// "Available" state into "NotDownloaded" for the status fragment. Musicians
/// and description aren't exposed as params: no first-slice Hurl case needs
/// them yet, and adding unused surface here would be speculative.
///
/// Always produces a `NotDownloaded`/`NotSplit`/not-archived concert, even on
/// a `source_url` reused from a prior seed: `upsert_listing` and
/// `update_metadata` only touch listing/metadata columns, so without an
/// explicit reset here a URL that was previously downloaded (e.g. by an
/// earlier Hurl case, or a retried run) would keep rendering as
/// Downloaded/Downloading/DownloadError — silently breaking this method's
/// documented contract instead of producing the state its name promises.
/// Uses a direct `UPDATE`, not `lifecycle::clear_download_state` /
/// `clear_split_state` (which are for genuine user-initiated deletes): those
/// each record a `DownloadDelete`/`SplitDelete` event, which would be a false
/// audit trail here since nothing was ever actually downloaded or split.
#[allow(clippy::too_many_arguments)]
fn seed_scraped_concert(
    state: &AppState,
    source_url: String,
    title: String,
    concert_date: Option<String>,
    artist: String,
    album: String,
    set_list: Vec<String>,
) -> anyhow::Result<SeedScrapedConcertResult> {
    let conn = state.db.lock().unwrap();
    db::concerts::upsert_listing(
        &conn,
        &db::concerts::NewListing {
            source_url: source_url.clone(),
            title,
            concert_date,
            teaser: None,
        },
    )?;
    let concert = db::concerts::get_concert_by_url(&conn, &source_url)?.ok_or_else(|| {
        anyhow::anyhow!(
            "upsert_listing succeeded but the row is not readable back by source_url: {source_url}"
        )
    })?;
    conn.execute(
        "UPDATE concerts SET
             download_started_at = NULL, downloaded_at = NULL,
             downloaded_extension = NULL, download_errors_json = '[]',
             split_started_at = NULL, split_at = NULL, split_errors_json = '[]',
             tracks_present = NULL,
             archive_started_at = NULL, archived_at = NULL, archive_errors_json = '[]'
         WHERE id = ?1",
        rusqlite::params![concert.id],
    )?;
    db::concerts::update_metadata(
        &conn,
        concert.id,
        &db::concerts::MetadataUpdate {
            artist,
            album,
            description: None,
            set_list,
            musicians: vec![],
        },
    )?;
    let concert = db::concerts::get_concert(&conn, concert.id)?;
    Ok(SeedScrapedConcertResult {
        id: concert.id,
        source_url: concert.source_url,
        title: concert.title,
        album: concert.album.unwrap_or_default(),
    })
}

fn seed_lifecycle_concert(
    state: &AppState,
    params: SeedLifecycleConcertParams,
) -> anyhow::Result<SeedLifecycleConcertResult> {
    let conn = state.db.lock().unwrap();
    db::concerts::upsert_listing(
        &conn,
        &db::concerts::NewListing {
            source_url: params.source_url.clone(),
            title: params.title,
            concert_date: params.concert_date,
            teaser: None,
        },
    )?;
    let concert =
        db::concerts::get_concert_by_url(&conn, &params.source_url)?.ok_or_else(|| {
            anyhow::anyhow!(
                "upsert_listing succeeded but the row is not readable back by source_url: {}",
                params.source_url
            )
        })?;
    db::concerts::update_metadata(
        &conn,
        concert.id,
        &db::concerts::MetadataUpdate {
            artist: params.artist,
            album: params.album,
            description: None,
            set_list: params.set_list,
            musicians: vec![],
        },
    )?;

    if params.downloaded || params.split {
        db::lifecycle::try_mark_download_started(&conn, concert.id)?;
        db::lifecycle::mark_download_succeeded(&conn, concert.id, "mp4")?;
    }
    if params.split {
        db::lifecycle::try_mark_split_started(&conn, concert.id)?;
        db::lifecycle::mark_split_succeeded(&conn, concert.id)?;
    }
    if let Some(timestamps) = params.auto_timestamps {
        db::split_timestamps::set_auto_split_timestamps(&conn, concert.id, &timestamps)?;
    }
    if let Some(timestamps) = params.user_timestamps {
        db::split_timestamps::set_user_split_timestamps(&conn, concert.id, &timestamps)?;
    }
    if let Some(duration) = params.media_duration {
        db::split_timestamps::set_media_duration(&conn, concert.id, duration)?;
    }

    let concert = db::concerts::get_concert(&conn, concert.id)?;
    let downloaded = concert.download_status() == crate::model::DownloadStatus::Downloaded;
    let split = concert.split_status() == crate::model::SplitStatus::Split;
    Ok(SeedLifecycleConcertResult {
        id: concert.id,
        source_url: concert.source_url,
        title: concert.title,
        album: concert.album.unwrap_or_default(),
        downloaded,
        split,
    })
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
/// Deliberately out of scope for this first slice (see "Out Of Scope For
/// First Slice" in docs/change/2026-07-11-hurl-web-integration-tests.md):
/// this does not quiesce in-flight download/split jobs or the background
/// scrape worker. A reset run concurrently with one of those can still race
/// with writes it makes after reset returns. Job-command stubbing and scrape
/// queue controls are explicitly deferred to a later migration slice, and
/// the first slice's Hurl cases never trigger those paths.
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
pub async fn start(state: AppState, port: u16) -> anyhow::Result<(ServerHandle, SocketAddr)> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let server = ServerBuilder::new().build(addr).await?;
    let bound = server.local_addr()?;
    let handle = server.start(TestControlServer::new(state).rpc_module());
    Ok((handle, bound))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::concerts::NewListing;
    use crate::db::settings;
    use crate::jobs::scrape_queue::ScrapeQueue;
    use crate::jobs::{JobConfig, JobRegistry};
    use concert_types::SongTimestamp;
    use std::sync::{Arc, Mutex};

    fn test_state(conn: rusqlite::Connection, workdir: std::path::PathBuf) -> AppState {
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

    fn params(json: &'static str) -> Params<'static> {
        Params::new(Some(json))
    }

    fn listing_request(json: &'static str) -> SeedListingRequest {
        SeedListingRequest::parse(params(json)).unwrap()
    }

    fn scraped_request(json: &'static str) -> SeedScrapedConcertRequest {
        SeedScrapedConcertRequest::parse(params(json)).unwrap()
    }

    fn lifecycle_request(json: &'static str) -> SeedLifecycleConcertRequest {
        SeedLifecycleConcertRequest::parse(params(json)).unwrap()
    }

    fn fixture_number_from_source_url(source_url: &str) -> u64 {
        source_url
            .strip_prefix("https://example.test/tiny-desk/test-control-")
            .expect("generated source_url must use the example.test fixture prefix")
            .parse()
            .expect("generated source_url must end with a fixture number")
    }

    #[tokio::test]
    async fn seed_listing_accepts_empty_params_with_generated_defaults() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let server = TestControlServer::new(state);

        let result = server
            .seed_listing_rpc(listing_request("{}"))
            .await
            .unwrap();
        let n = fixture_number_from_source_url(&result.source_url);

        assert_eq!(result.title, format!("Test Listing {n}"));
        assert_eq!(result.concert_date.as_deref(), Some(DEFAULT_CONCERT_DATE));
    }

    #[tokio::test]
    async fn seed_scraped_concert_accepts_empty_params_with_three_default_tracks() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let server = TestControlServer::new(state.clone());

        let result = server
            .seed_scraped_concert_rpc(scraped_request("{}"))
            .await
            .unwrap();
        let n = fixture_number_from_source_url(&result.source_url);

        assert_eq!(result.title, format!("Test Scraped Concert {n}"));
        assert_eq!(result.album, format!("Test Scraped Album {n}"));
        let conn = state.db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, result.id).unwrap();
        assert_eq!(
            concert.set_list,
            vec![
                format!("Test Control Track {n}.1"),
                format!("Test Control Track {n}.2"),
                format!("Test Control Track {n}.3")
            ]
        );
    }

    #[tokio::test]
    async fn seed_lifecycle_concert_accepts_empty_params_with_inert_lifecycle_defaults() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let server = TestControlServer::new(state.clone());

        let result = server
            .seed_lifecycle_concert_rpc(lifecycle_request("{}"))
            .await
            .unwrap();
        let n = fixture_number_from_source_url(&result.source_url);

        assert_eq!(result.title, format!("Test Lifecycle Concert {n}"));
        assert_eq!(result.album, format!("Test Lifecycle Album {n}"));
        assert!(!result.downloaded);
        assert!(!result.split);
        let conn = state.db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, result.id).unwrap();
        assert_eq!(concert.set_list.len(), 3);
        assert!(concert.media_duration.is_none());
        let timestamps = db::split_timestamps::get_split_timestamps(&conn, result.id).unwrap();
        assert!(timestamps.auto.is_none());
        assert!(timestamps.user.is_none());
    }

    #[tokio::test]
    async fn seed_defaults_generate_unique_urls_across_methods_and_reset() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let server = TestControlServer::new(state);

        let listing = server
            .seed_listing_rpc(listing_request("{}"))
            .await
            .unwrap();
        reset_test_data(&server.state).unwrap();
        let scraped = server
            .seed_scraped_concert_rpc(scraped_request("{}"))
            .await
            .unwrap();
        let lifecycle = server
            .seed_lifecycle_concert_rpc(lifecycle_request("{}"))
            .await
            .unwrap();

        let urls = [
            &listing.source_url,
            &scraped.source_url,
            &lifecycle.source_url,
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
        let server = TestControlServer::new(state);

        let listing = server
            .seed_listing_rpc(listing_request(
                r#"{
                    "source_url": "https://npr.org/c/explicit-listing",
                    "title": "Explicit Listing",
                    "concert_date": "2024-01-15",
                    "teaser": "Visible teaser"
                }"#,
            ))
            .await
            .unwrap();
        let scraped = server
            .seed_scraped_concert_rpc(scraped_request(
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
        let lifecycle = server
            .seed_lifecycle_concert_rpc(lifecycle_request(
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

        assert_eq!(listing.title, "Explicit Listing");
        assert_eq!(listing.source_url, "https://npr.org/c/explicit-listing");
        assert_eq!(scraped.album, "Explicit Album");
        assert_eq!(lifecycle.album, "Explicit Lifecycle Album");
        assert!(lifecycle.downloaded);
        assert!(lifecycle.split);
    }

    #[tokio::test]
    async fn explicit_null_preserves_nullable_domain_absence() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let server = TestControlServer::new(state.clone());

        let listing = server
            .seed_listing_rpc(listing_request(r#"{"concert_date": null, "teaser": null}"#))
            .await
            .unwrap();
        let lifecycle = server
            .seed_lifecycle_concert_rpc(lifecycle_request(
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

        assert!(listing.concert_date.is_none());
        let conn = state.db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, lifecycle.id).unwrap();
        assert!(concert.concert_date.is_none());
        assert!(concert.set_list.is_empty());
        assert!(concert.media_duration.is_none());
        let timestamps = db::split_timestamps::get_split_timestamps(&conn, lifecycle.id).unwrap();
        assert!(timestamps.auto.is_none());
        assert!(timestamps.user.is_none());
    }

    #[tokio::test]
    async fn explicit_null_for_identity_strings_is_rejected() {
        let err =
            SeedLifecycleConcertRequest::parse(params(r#"{"source_url": null}"#)).unwrap_err();

        assert_eq!(
            err.code(),
            jsonrpsee::types::ErrorCode::InvalidParams.code()
        );
    }

    #[tokio::test]
    async fn seed_listing_returns_the_created_id_and_public_fields() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());

        let result = super::seed_listing(
            &state,
            "https://npr.org/c/seed-test".to_string(),
            "Seed Test Concert".to_string(),
            Some("2024-01-15".to_string()),
            None,
        )
        .unwrap();

        assert_eq!(result.source_url, "https://npr.org/c/seed-test");
        assert_eq!(result.title, "Seed Test Concert");
        assert_eq!(result.concert_date.as_deref(), Some("2024-01-15"));

        let conn = state.db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, result.id).unwrap();
        assert_eq!(concert.source_url, "https://npr.org/c/seed-test");
    }

    #[tokio::test]
    async fn seed_listing_reseeding_the_same_source_url_returns_the_same_id() {
        // upsert_listing is an INSERT ... ON CONFLICT DO UPDATE, so
        // Connection::last_insert_rowid would go stale on the second call —
        // this pins the look-up-by-source_url behavior that avoids that.
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());

        let first = super::seed_listing(
            &state,
            "https://npr.org/c/reseed".to_string(),
            "Original Title".to_string(),
            None,
            None,
        )
        .unwrap();
        let second = super::seed_listing(
            &state,
            "https://npr.org/c/reseed".to_string(),
            "Updated Title".to_string(),
            None,
            None,
        )
        .unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(second.title, "Updated Title");
    }

    #[tokio::test]
    async fn seed_scraped_concert_sets_metadata_scraped_at_and_returns_album() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());

        let result = super::seed_scraped_concert(
            &state,
            "https://npr.org/c/scraped-test".to_string(),
            "Scraped Test Concert".to_string(),
            Some("2024-01-15".to_string()),
            "Test Artist".to_string(),
            "Test Album".to_string(),
            vec!["Song One".to_string(), "Song Two".to_string()],
        )
        .unwrap();

        assert_eq!(result.source_url, "https://npr.org/c/scraped-test");
        assert_eq!(result.title, "Scraped Test Concert");
        assert_eq!(result.album, "Test Album");

        let conn = state.db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, result.id).unwrap();
        assert_eq!(concert.artist.as_deref(), Some("Test Artist"));
        assert_eq!(concert.set_list, vec!["Song One", "Song Two"]);
        // metadata_scraped_at is what moves the status fragment from
        // Available to NotDownloaded — see seed_scraped_concert's doc comment.
        assert!(concert.metadata_scraped_at.is_some());
    }

    #[tokio::test]
    async fn seed_scraped_concert_resets_stale_lifecycle_state_on_url_reuse() {
        // A source_url that was previously downloaded/split/archived (e.g. by
        // an earlier Hurl case, or a retried run) must still come back as a
        // clean NotDownloaded/NotSplit fixture — the method's whole contract
        // is "produce this state", not "produce it only on a brand-new URL".
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        super::seed_scraped_concert(
            &state,
            "https://npr.org/c/reused-scraped".to_string(),
            "First Pass".to_string(),
            None,
            "Artist".to_string(),
            "Album".to_string(),
            vec![],
        )
        .unwrap();
        {
            let conn = state.db.lock().unwrap();
            let concert =
                db::concerts::get_concert_by_url(&conn, "https://npr.org/c/reused-scraped")
                    .unwrap()
                    .unwrap();
            conn.execute(
                "UPDATE concerts SET
                     downloaded_at = datetime('now'), downloaded_extension = 'mp4',
                     split_at = datetime('now'), archived_at = datetime('now'),
                     download_errors_json = '[{\"at\":\"x\",\"message\":\"boom\"}]'
                 WHERE id = ?1",
                rusqlite::params![concert.id],
            )
            .unwrap();
        }

        let result = super::seed_scraped_concert(
            &state,
            "https://npr.org/c/reused-scraped".to_string(),
            "Second Pass".to_string(),
            None,
            "Artist".to_string(),
            "Album".to_string(),
            vec![],
        )
        .unwrap();

        let conn = state.db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, result.id).unwrap();
        assert_eq!(
            concert.download_status(),
            crate::model::DownloadStatus::NotDownloaded
        );
        assert_eq!(concert.split_status(), crate::model::SplitStatus::NotSplit);
        assert!(concert.archived_at.is_none());
        assert!(concert.download_errors.is_empty());
    }

    fn ts(title: &str, start_time: f64, end_time: f64) -> SongTimestamp {
        SongTimestamp {
            title: title.to_string(),
            start_time,
            end_time,
            duration: end_time - start_time,
        }
    }

    #[tokio::test]
    async fn seed_lifecycle_concert_marks_downloaded_and_split_without_files() {
        let conn = db::connection::open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let state = test_state(conn, workdir.path().to_path_buf());

        let result = super::seed_lifecycle_concert(
            &state,
            SeedLifecycleConcertParams {
                source_url: "https://npr.org/c/lifecycle-split".to_string(),
                title: "Lifecycle Split".to_string(),
                concert_date: Some("2024-06-01".to_string()),
                artist: "Lifecycle Artist".to_string(),
                album: "Lifecycle Album".to_string(),
                set_list: vec!["One".to_string()],
                downloaded: false,
                split: true,
                auto_timestamps: None,
                user_timestamps: None,
                media_duration: None,
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
    async fn seed_lifecycle_concert_sets_optional_timestamp_columns() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let auto = vec![ts("One", 0.0, 55.0), ts("Two", 60.0, 115.0)];
        let user = vec![ts("One", 1.0, 54.0), ts("Two", 61.0, 114.0)];

        let result = super::seed_lifecycle_concert(
            &state,
            SeedLifecycleConcertParams {
                source_url: "https://npr.org/c/lifecycle-timestamps".to_string(),
                title: "Lifecycle Timestamps".to_string(),
                concert_date: None,
                artist: "Lifecycle Artist".to_string(),
                album: "Lifecycle Timestamp Album".to_string(),
                set_list: vec!["One".to_string(), "Two".to_string()],
                downloaded: false,
                split: false,
                auto_timestamps: Some(auto.clone()),
                user_timestamps: Some(user.clone()),
                media_duration: Some(123.5),
            },
        )
        .unwrap();

        let conn = state.db.lock().unwrap();
        let stored = db::split_timestamps::get_split_timestamps(&conn, result.id).unwrap();
        assert_eq!(stored.auto, Some(auto));
        assert_eq!(stored.user, Some(user));
        let concert = db::concerts::get_concert(&conn, result.id).unwrap();
        assert_eq!(concert.media_duration, Some(123.5));
        assert_eq!(
            concert.download_status(),
            crate::model::DownloadStatus::NotDownloaded
        );
    }

    #[tokio::test]
    async fn assert_concert_state_passes_when_expectations_match() {
        let conn = db::connection::open_in_memory().unwrap();
        let state = test_state(conn, tempfile::tempdir().unwrap().path().to_path_buf());
        let seeded = super::seed_listing(
            &state,
            "https://npr.org/c/assert-pass".to_string(),
            "Assert Pass".to_string(),
            None,
            None,
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
            "https://npr.org/c/assert-fail".to_string(),
            "Assert Fail".to_string(),
            None,
            None,
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
            "https://npr.org/c/assert-partial".to_string(),
            "Assert Partial".to_string(),
            None,
            None,
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
            "https://npr.org/c/assert-empty".to_string(),
            "Assert Empty".to_string(),
            None,
            None,
        )
        .unwrap();

        let err = super::assert_concert_state(&state, seeded.id, None, None, None).unwrap_err();
        assert!(err.to_string().contains("no expectations"), "{err}");
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
}
