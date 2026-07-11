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

use jsonrpsee::core::{async_trait, RpcResult};
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::server::{ServerBuilder, ServerHandle};
use jsonrpsee::types::ErrorObjectOwned;
use serde::Serialize;

use crate::web::AppState;

#[rpc(server, namespace = "test", namespace_separator = ".")]
pub trait TestControlApi {
    /// Clear concert, event, playlist, job, and settings test data, and
    /// remove generated concert files/thumbnails under the configured
    /// workdir. Leaves the SQLite schema and server configuration intact.
    #[method(name = "reset")]
    async fn reset(&self) -> RpcResult<OkResult>;
}

#[derive(Clone, Serialize)]
pub struct OkResult {
    pub ok: bool,
}

pub struct TestControlServer {
    state: AppState,
}

impl TestControlServer {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[async_trait]
impl TestControlApiServer for TestControlServer {
    async fn reset(&self) -> RpcResult<OkResult> {
        reset_test_data(&self.state)
            .map(|()| OkResult { ok: true })
            .map_err(internal_error)
    }
}

fn internal_error(err: anyhow::Error) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(
        jsonrpsee::types::ErrorCode::InternalError.code(),
        err.to_string(),
        None::<()>,
    )
}

/// Removes generated files under `<workdir>/concerts` and
/// `<workdir>/thumbnails`, then deletes concert/event/playlist/job rows
/// (`playlist_items` cascades off `playlists`) and resets the singleton
/// `settings` row back to its migration defaults. Uses the same
/// connection/workdir as the app server so Test Control and product HTTP
/// requests observe the same state.
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
    let handle = server.start(TestControlServer::new(state).into_rpc());
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

        let workdir = tempfile::tempdir().unwrap();
        let concerts_dir = workdir.path().join("concerts");
        let thumbnails_dir = workdir.path().join("thumbnails");
        std::fs::create_dir_all(&concerts_dir).unwrap();
        std::fs::create_dir_all(&thumbnails_dir).unwrap();
        std::fs::write(concerts_dir.join("leftover.mp4"), b"x").unwrap();
        std::fs::write(thumbnails_dir.join("leftover.jpg"), b"x").unwrap();

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
