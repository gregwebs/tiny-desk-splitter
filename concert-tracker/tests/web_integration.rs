use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

use concert_tracker::{
    db::{self, concerts::NewListing},
    jobs::{scrape_queue::ScrapeQueue, JobConfig, JobRegistry},
    web::{router, AppState},
};

// Black-box product HTTP coverage lives in hurl/*.hurl; see hurl/README.md.

fn disable_system_proxy_for_tests() {
    tiny_desk_scraper::set_proxy_mode(tiny_desk_scraper::ProxyMode::None);
}

/// An idle background scrape queue for tests that never enqueue. Backed by a
/// throwaway in-memory DB; the worker stays parked.
fn idle_scrape_queue() -> ScrapeQueue {
    disable_system_proxy_for_tests();
    ScrapeQueue::start(
        Arc::new(Mutex::new(db::connection::open_in_memory().unwrap())),
        PathBuf::from("/tmp"),
    )
}

fn test_state(conn: rusqlite::Connection) -> AppState {
    disable_system_proxy_for_tests();
    let db = Arc::new(Mutex::new(conn));
    let registry = Arc::new(JobRegistry::new());
    let scrape_queue = idle_scrape_queue();
    let jobs = JobConfig::test(PathBuf::from("/tmp"));
    AppState {
        concerts: concert_tracker::concerts::Concerts::new(
            db.clone(),
            jobs.working_dir.clone(),
            registry.clone(),
            scrape_queue.clone(),
        ),
        db,
        registry,
        scrape_queue,
        jobs,
    }
}

fn seeded_concert(conn: &rusqlite::Connection, url: &str, title: &str) {
    db::concerts::upsert_listing(
        conn,
        &NewListing {
            source_url: url.to_string(),
            title: title.to_string(),
            concert_date: Some("2024-01-15".to_string()),
            teaser: None,
        },
    )
    .unwrap();
}

/// When a concert is opened for the first time and the scrape fails (e.g.
/// network down or NPR unreachable), the detail page must still render with
/// the listing-only data and `metadata_scraped_at` must stay NULL so the
/// next view can retry. The success path is covered by the unit tests for
/// `ensure_scraped` in src/web/handlers.rs — those use a stub closure and
/// avoid hitting the network, while this test exercises the real call path.
///
/// Intentionally Rust-only: this exercises the detail view's inline auto-scrape
/// (`ensure_scraped`), a real outbound connection-refused failure on a
/// synchronous call path that does not go through the background
/// `ScrapeQueue` at all — there is no Scrape Driver seam here to stand in
/// for it deterministically, by design.
#[tokio::test]
async fn detail_page_auto_scrape_failure_still_renders() {
    disable_system_proxy_for_tests();
    let conn = db::connection::open_in_memory().unwrap();
    // Port 1 with no listener — connection refuses immediately.
    seeded_concert(
        &conn,
        "http://127.0.0.1:1/never-resolves",
        "Unreachable Concert",
    );
    let db_arc = Arc::new(Mutex::new(conn));
    let registry = Arc::new(JobRegistry::new());
    let scrape_queue = idle_scrape_queue();
    let workdir = tempfile::tempdir().unwrap();
    let jobs = JobConfig::test(workdir.path().to_path_buf());
    let state = AppState {
        concerts: concert_tracker::concerts::Concerts::new(
            db_arc.clone(),
            jobs.working_dir.clone(),
            registry.clone(),
            scrape_queue.clone(),
        ),
        db: db_arc.clone(),
        registry,
        scrape_queue,
        jobs,
    };
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("Unreachable Concert"),
        "listing title must render even when scrape fails"
    );

    // metadata_scraped_at must remain NULL so the next view retries.
    let reread = {
        let conn = db_arc.lock().unwrap();
        db::concerts::get_concert(&conn, 1).unwrap()
    };
    assert!(reread.metadata_scraped_at.is_none());
    assert!(reread.artist.is_none());
}

/// `router(state)` (the production path, used by every other test in this file
/// and by `concert_web.rs` without `--dev`) must keep serving JS from the
/// `include_str!`-embedded copy and must never inject the dev-mode livereload
/// script. This pins the dev/prod divergence introduced by `RouterOpts::dev` —
/// see `web::router_with_opts`.
#[tokio::test]
async fn prod_router_serves_embedded_js_without_livereload() {
    let conn = db::connection::open_in_memory().unwrap();
    let app = router(test_state(conn));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/static/player.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap(),
        "application/javascript"
    );
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&bytes);
    assert!(
        body.contains("@generated by concert-tracker/frontend/build.mjs"),
        "must serve the embedded static/player.js (esbuild output from frontend/src), got: {body}"
    );

    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&bytes);
    assert!(
        !body.to_lowercase().contains("livereload"),
        "production responses must not include the dev-mode livereload script: {body}"
    );
}

/// The spec served at `/api-docs/openapi.json` must match
/// `web::built_api_doc()` exactly — that function is what `openapi-dump`
/// (`src/bin/openapi_dump.rs`) prints for the TypeScript frontend's
/// `just openapi-types` to consume. If these ever diverged, the generated
/// `.d.ts` would silently document a different API than the one actually
/// served, defeating the type-safety the frontend conversion relies on.
#[tokio::test]
async fn served_openapi_spec_matches_built_api_doc() {
    let conn = db::connection::open_in_memory().unwrap();
    let app = router(test_state(conn));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api-docs/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let served: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let expected: serde_json::Value =
        serde_json::from_str(&concert_tracker::web::built_api_doc().to_json().unwrap()).unwrap();

    assert_eq!(
        served, expected,
        "served /api-docs/openapi.json must match web::built_api_doc() \
         (openapi-dump's source) exactly"
    );
}
