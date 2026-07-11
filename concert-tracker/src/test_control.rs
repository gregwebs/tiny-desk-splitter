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

/// Deletes concert/event/playlist/job rows (`playlist_items` cascades off
/// `playlists`), resets the singleton `settings` row back to its migration
/// defaults, and removes generated files under `<workdir>/concerts` and
/// `<workdir>/thumbnails`. Uses the same connection/workdir as the app server
/// so Test Control and product HTTP requests observe the same state.
///
/// `settings` is reset in place (never deleted): migration 0002 inserts its
/// `id = 1` row exactly once at first connection-open, so a bare `DELETE`
/// would leave every later request against that singleton 404ing on a
/// "Query returned no rows" error for the lifetime of the process.
fn reset_test_data(state: &AppState) -> anyhow::Result<()> {
    {
        let conn = state.db.lock().unwrap();
        conn.execute_batch(
            "DELETE FROM playlist_items;
             DELETE FROM playlists;
             DELETE FROM jobs;
             DELETE FROM events;
             DELETE FROM concerts;
             UPDATE settings SET archive_location = NULL, theme = 'system' WHERE id = 1;",
        )?;
    }
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
