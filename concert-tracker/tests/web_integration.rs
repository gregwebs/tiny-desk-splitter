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

// pending_card_shows_loading_then_thumbnail migrated to
// hurl/scrape_pending.hurl (Scrape Driver: test.scrape_set_plan block +
// test.scrape_enqueue + public GET /concerts/:id/status before/after
// test.scrape_release) — see
// docs/change/2026-07-17-scrape-driver-hurl-migration.md.

fn test_state(conn: rusqlite::Connection) -> AppState {
    disable_system_proxy_for_tests();
    AppState {
        db: Arc::new(Mutex::new(conn)),
        registry: Arc::new(JobRegistry::new()),
        scrape_queue: idle_scrape_queue(),
        jobs: JobConfig::test(PathBuf::from("/tmp")),
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

// list_page_renders_seeded_concert migrated to hurl/listing_status.hurl
// (test.seed_listing + GET / contains the seeded title) — see
// docs/change/2026-07-11-hurl-web-integration-tests.md.

// ignore_endpoint_toggles_flag_and_returns_row migrated to
// hurl/listing_status.hurl (POST /concerts/:id/ignore + badge-ignored / "Clear
// ignored" assertions) — see docs/change/2026-07-11-hurl-web-integration-tests.md.

// available_concert_row_shows_want_and_ignore_buttons migrated to
// hurl/listing_status.hurl.

// not_downloaded_row_hides_download_badge_and_shows_button migrated to
// hurl/listing_status.hurl (test.seed_scraped_concert + GET
// /concerts/:id/status) — see docs/change/2026-07-11-hurl-web-integration-tests.md.

// list_filter_by_status_narrows_results migrated to hurl/listing_status.hurl
// (GET /?filter=ignored includes the ignored concert, excludes the other) —
// see docs/change/2026-07-11-hurl-web-integration-tests.md.

// notes_endpoint_persists_text migrated to hurl/detail_prepare_notes.hurl.

// download_endpoint_spawns_job_and_returns_row migrated to
// hurl/job_chain.hurl — see docs/change/2026-07-15-job-driver-plan.md.

/// When a concert is opened for the first time and the scrape fails (e.g.
/// network down or NPR unreachable), the detail page must still render with
/// the listing-only data and `metadata_scraped_at` must stay NULL so the
/// next view can retry. The success path is covered by the unit tests for
/// `ensure_scraped` in src/web/handlers.rs — those use a stub closure and
/// avoid hitting the network, while this test exercises the real call path.
///
/// Intentionally Rust-only, unlike `pending_card_shows_loading_then_thumbnail`
/// above: this exercises the detail view's inline auto-scrape
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
    let state = AppState {
        db: db_arc.clone(),
        registry: Arc::new(JobRegistry::new()),
        scrape_queue: idle_scrape_queue(),
        jobs: JobConfig::test(PathBuf::from("/tmp")),
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

// delete_download_removes_file_and_clears_state migrated to
// hurl/media_files_lifecycle.hurl (test.seed_media_concert +
// GET .../concert-playback file-gone proof) — see
// docs/change/2026-07-16-scenario-seeds-hurl-migration.md.

// delete_download_with_prior_split_error_restores_download_button migrated
// to hurl/media_files_lifecycle.hurl (Job Driver split=fail + delete-download
// + GET .../status).

// play_button_visible_after_successful_split migrated to
// hurl/media_files_lifecycle.hurl (test.seed_media_concert + GET
// .../status).

// ignore_deletes_preview_image migrated to hurl/media_files_lifecycle.hurl
// (test.seed_media_concert preview_image + GET .../concert-files/.../
// preview.jpg).

// track_details_returns_200_without_album migrated to
// hurl/media_files_lifecycle.hurl (test.seed_album_null_concert).

// track_details_reports_busy_from_handler_state migrated to
// hurl/media_files_lifecycle.hurl (Job Driver split=block + POST /prepare +
// GET .../track-details).

// prepare_status_reports_filesystem_track_state migrated to
// hurl/media_files_lifecycle.hurl (test.seed_media_concert track_files +
// GET .../prepare-status).

// get_split_timestamps_lazy_backfill_from_timestamps_json migrated to
// hurl/split_timestamps_flow.hurl (test.seed_media_concert
// legacy_timestamps_json).

// set_split_timestamps_returns_422_on_count_mismatch migrated to
// hurl/split_timestamps_flow.hurl.

// set_split_timestamps_happy_path_returns_202_and_stores_user_column
// migrated to hurl/split_timestamps_flow.hurl (test.seed_media_concert
// source_file_kind: real_audio — the one Hurl case needing real ffprobe-
// backed media).

// reset_split_timestamps_happy_path_returns_202_and_clears_user_column
// migrated to hurl/split_timestamps_flow.hurl.

// concert_playback_source_mode_when_file_present,
// concert_playback_reconstruction_mode_when_source_gone, and
// concert_playback_reconstruction_includes_interlude migrated to
// hurl/concert_playback.hurl (test.seed_media_concert interlude_files).
// concert_playback_returns_404_when_nothing_playable already lives in
// hurl/media_state_errors.hurl.

// delete_interlude_removes_file_records_event_returns_fragment migrated to
// hurl/concert_playback.hurl (test.assert_concert_events — the first
// consumer of that new Test Control API).

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
