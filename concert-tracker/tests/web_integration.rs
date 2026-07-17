use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

use concert_tracker::{
    db::{
        self,
        concerts::{MetadataUpdate, NewListing},
    },
    jobs::{
        scrape_queue::{ScrapeItemFn, ScrapeQueue},
        JobConfig, JobRegistry,
    },
    web::{router, AppState},
};

/// Cadence for the async poll helpers below.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
/// Budget for a cheap in-memory predicate poll: ~2s.
const COND_MAX_POLLS: usize = 200;
/// Budget for awaiting the worker's result after it has run an item (waits on
/// real worker progress, not just a flag flip): ~5s.
const RECV_MAX_POLLS: usize = 500;

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

/// Await a value from the scrape worker's sync result channel without parking a
/// runtime worker thread.
///
/// Mirrors the helper in `jobs::scrape_queue`'s tests: `Receiver::recv_timeout`
/// is a synchronous blocking call that, from an async test, parks one of the
/// runtime's worker threads for the whole wait and can starve the scrape worker
/// under load — a flaky timeout. Polling `try_recv` between async `sleep`s frees
/// the worker thread each tick so the scheduler keeps the worker running.
async fn recv_soon<T>(rx: &std::sync::mpsc::Receiver<T>) -> T {
    use std::sync::mpsc::TryRecvError;
    for _ in 0..RECV_MAX_POLLS {
        match rx.try_recv() {
            Ok(v) => return v,
            Err(TryRecvError::Empty) => tokio::time::sleep(POLL_INTERVAL).await,
            Err(TryRecvError::Disconnected) => {
                panic!("worker dropped the result sender before sending a value")
            }
        }
    }
    panic!("no value received from the scrape worker in time");
}

/// Fetch the `/concerts/:id/status` fragment HTML (clones the router so it can be
/// called more than once).
async fn get_status_html(app: &axum::Router, id: i64) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/concerts/{id}/status"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A queued (not-yet-scraped) concert's card shows the "loading…" placeholder and
/// polls; once the background worker finishes it shows the thumbnail and stops
/// polling. Uses an injected stub item (no network), gated on a signal (no sleep).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_card_shows_loading_then_thumbnail() {
    use std::sync::mpsc as std_mpsc;

    let conn = db::connection::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/c/pending", "Pending Concert");
    let db = Arc::new(Mutex::new(conn));

    // Stub item: block until released, then set metadata (marks
    // metadata_scraped_at + album) so the card flips loading → thumbnail.
    let (release_tx, release_rx) = std_mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let (done_tx, done_rx) = std_mpsc::channel::<()>();
    let db_for_item = db.clone();
    let item: ScrapeItemFn = Arc::new(move |_db, _wd, req| {
        let _ = release_rx.lock().unwrap().recv();
        {
            let conn = db_for_item.lock().unwrap();
            db::concerts::update_metadata(
                &conn,
                req.concert_id,
                &MetadataUpdate {
                    artist: "Stub Artist".to_string(),
                    album: "Stub Album".to_string(),
                    description: None,
                    set_list: vec![],
                    musicians: vec![],
                },
            )
            .unwrap();
        }
        let _ = done_tx.send(());
    });

    let scrape_queue = ScrapeQueue::start_with(db.clone(), PathBuf::from("/tmp"), item);
    let state = AppState {
        db: db.clone(),
        registry: Arc::new(JobRegistry::new()),
        jobs: JobConfig::test(PathBuf::from("/tmp")),
        scrape_queue: scrape_queue.clone(),
    };

    assert!(scrape_queue.enqueue(1, "https://npr.org/c/pending".to_string()));
    let app = router(state);

    // While pending: polls + loading placeholder, no thumbnail yet.
    let body = get_status_html(&app, 1).await;
    assert!(
        body.contains("hx-trigger=\"every 3s\""),
        "pending card must poll: {body}"
    );
    assert!(
        body.contains("card-thumb-loading"),
        "pending card must show loading placeholder: {body}"
    );
    assert!(!body.contains("/thumbnails/"), "no thumbnail yet: {body}");

    // Release the stub; wait for completion + the worker to clear pending.
    release_tx.send(()).unwrap();
    recv_soon(&done_rx).await;
    for _ in 0..COND_MAX_POLLS {
        if !scrape_queue.is_pending(1) {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert!(
        !scrape_queue.is_pending(1),
        "pending must clear after scrape"
    );

    // Now scraped: thumbnail shown, polling stopped.
    let body = get_status_html(&app, 1).await;
    assert!(
        body.contains("/thumbnails/"),
        "scraped card must show thumbnail: {body}"
    );
    assert!(
        !body.contains("hx-trigger=\"every 3s\""),
        "scraped card must stop polling: {body}"
    );
}

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
