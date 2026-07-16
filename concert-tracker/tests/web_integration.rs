use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::process::Command;
use tower::ServiceExt;

use concert_tracker::{
    db::{
        self,
        concerts::{MetadataUpdate, NewListing},
    },
    jobs::{
        scrape_queue::{ScrapeItemFn, ScrapeQueue},
        DownloadJob, JobConfig, JobRegistry, SplitJob,
    },
    model::{concert_dir, sanitize_filename},
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

fn state_with_workdir(conn: rusqlite::Connection, workdir: PathBuf) -> AppState {
    disable_system_proxy_for_tests();
    AppState {
        db: Arc::new(Mutex::new(conn)),
        registry: Arc::new(JobRegistry::new()),
        scrape_queue: idle_scrape_queue(),
        jobs: JobConfig::test(workdir),
    }
}

fn seed_downloaded(conn: &rusqlite::Connection, url: &str, album: &str) {
    db::concerts::upsert_listing(
        conn,
        &NewListing {
            source_url: url.to_string(),
            title: "Downloaded Concert".to_string(),
            concert_date: Some("2024-01-15".to_string()),
            teaser: None,
        },
    )
    .unwrap();
    db::concerts::update_metadata(
        conn,
        1,
        &MetadataUpdate {
            artist: "X".to_string(),
            album: album.to_string(),
            description: None,
            set_list: vec![],
            musicians: vec![],
        },
    )
    .unwrap();
    db::lifecycle::try_mark_download_started(conn, 1).unwrap();
    db::lifecycle::mark_download_succeeded(conn, 1, "mp4").unwrap();
}

#[tokio::test]
async fn delete_download_removes_file_and_clears_state() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Some Album";
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    let mp4 = cd.join(format!("{}.mp4", album));
    std::fs::write(&mp4, b"fake mp4 bytes").unwrap();

    let conn = db::connection::open_in_memory().unwrap();
    seed_downloaded(&conn, "https://npr.org/d/1", album);
    let db_arc = Arc::new(Mutex::new(conn));
    let state = AppState {
        db: db_arc.clone(),
        registry: Arc::new(JobRegistry::new()),
        scrape_queue: idle_scrape_queue(),
        jobs: JobConfig::test(workdir.path().to_path_buf()),
    };
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/concerts/1/delete-download")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    // Swap just the card in place (so the JS player survives) — not a full reload.
    assert!(
        response.headers().get("HX-Refresh").is_none(),
        "delete must NOT trigger a full page reload"
    );
    assert_eq!(
        response
            .headers()
            .get("HX-Retarget")
            .and_then(|v| v.to_str().ok()),
        Some("#concert-1"),
        "delete must retarget the swap to the concert's card"
    );
    assert_eq!(
        response
            .headers()
            .get("HX-Reswap")
            .and_then(|v| v.to_str().ok()),
        Some("outerHTML")
    );
    assert!(!mp4.exists(), "mp4 file must be removed from disk");

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("id=\"concert-1\""),
        "body is the re-rendered card"
    );
    assert!(
        html.contains("/concerts/1/download\""),
        "Download button must reappear after the download is cleared"
    );

    let c = {
        let conn = db_arc.lock().unwrap();
        db::concerts::get_concert(&conn, 1).unwrap()
    };
    assert!(c.downloaded_at.is_none(), "downloaded_at must be cleared");
    assert!(c.split_at.is_none(), "split_at must be cleared");
}

#[tokio::test]
async fn delete_download_with_prior_split_error_restores_download_button() {
    // A concert that was downloaded and then had a failed split: deleting the
    // download clears only download columns, preserving split state. The
    // Download button reappears because DownloadStatus becomes NotDownloaded.
    let workdir = tempfile::tempdir().unwrap();
    let album = "Prior Split Err Album";
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    let mp4 = cd.join(format!("{}.mp4", album));
    std::fs::write(&mp4, b"fake mp4 bytes").unwrap();

    let conn = db::connection::open_in_memory().unwrap();
    seed_downloaded(&conn, "https://npr.org/d/prior-err", album);
    db::lifecycle::try_mark_split_started(&conn, 1).unwrap();
    db::lifecycle::mark_split_failed(&conn, 1, "ffmpeg crashed").unwrap();

    let db_arc = Arc::new(Mutex::new(conn));
    let state = AppState {
        db: db_arc.clone(),
        registry: Arc::new(JobRegistry::new()),
        scrape_queue: idle_scrape_queue(),
        jobs: JobConfig::test(workdir.path().to_path_buf()),
    };
    let app = router(state);

    let delete_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/concerts/1/delete-download")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_resp.status(), StatusCode::OK);

    // Fetch the row to inspect the action set the UI would render.
    let row_resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(row_resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(row_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("/concerts/1/download\""),
        "Download button must appear after delete; got: {}",
        html
    );
    assert!(
        !html.contains("/concerts/1/split\""),
        "Split button must NOT appear after delete; got: {}",
        html
    );
    assert!(
        !html.contains("/concerts/1/listen\""),
        "Listen button must NOT appear after delete; got: {}",
        html
    );
}

// downloaded_filter_includes_split_concerts migrated to hurl/listing_status.hurl.

#[tokio::test]
async fn play_button_visible_after_successful_split() {
    // Once tracks have been split, the source mp4 is still on disk and still
    // playable. With the old combined ProcessingStatus, `Split` shadowed
    // `Downloaded` and the play button disappeared. With the new
    // DownloadStatus / SplitStatus split, it is gated only on
    // DownloadStatus == Downloaded. (seed_downloaded uses an mp4 — a video —
    // so this also guards that there is no separate album Watch button.)
    // Create a workdir with a real source mp4 so can_play_concert is true.
    let workdir = tempfile::tempdir().unwrap();
    let album = "Some Album";
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    std::fs::write(cd.join(format!("{}.mp4", album)), b"fake mp4 bytes").unwrap();

    let conn = db::connection::open_in_memory().unwrap();
    seed_downloaded(&conn, "https://npr.org/d/split-listen", album);
    db::concerts::update_metadata(
        &conn,
        1,
        &MetadataUpdate {
            artist: "X".to_string(),
            album: album.to_string(),
            description: None,
            set_list: vec!["Song A".to_string(), "Song B".to_string()],
            musicians: vec![],
        },
    )
    .unwrap();
    db::lifecycle::try_mark_split_started(&conn, 1).unwrap();
    db::lifecycle::mark_split_succeeded(&conn, 1).unwrap();
    db::split_timestamps::set_tracks_present(&conn, 1, &[true, true]).unwrap();
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/status")
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
        html.contains("Player.playConcert(1)"),
        "Play concert button must remain visible after split; got: {}",
        html
    );
    assert!(
        html.contains(">Play concert</button>"),
        "the play-concert button is labelled 'Play concert'; got: {}",
        html
    );
    // The album-level Watch button was removed — video is chosen from the player.
    assert!(
        !html.contains("Player.watchDirect") && !html.contains(">Watch</button>"),
        "no separate album Watch button (even for a video download); got: {}",
        html
    );
    // The tracks button plays the split tracks, and the embedded track list
    // has per-track play buttons.
    assert!(
        html.contains("Player.playTracks(this, 1)"),
        "tracks button must play the tracks; got: {}",
        html
    );
    assert!(
        html.contains("Player.playTrack(this, 1, 0)"),
        "embedded track list must have per-track play buttons; got: {}",
        html
    );
    // The download-slot Play concert button comes before the delete-download trash.
    assert!(
        html.find("Player.playConcert").unwrap()
            < html.find("/concerts/1/delete-download").unwrap(),
        "download-slot 'Play concert' must come before the delete-download trash; got: {}",
        html
    );
    // The Split and delete-split buttons are gone: splitting is automated via
    // track play, and tracks are deleted one by one.
    assert!(
        !html.contains("/concerts/1/split\""),
        "Split action button must NOT appear; got: {}",
        html
    );
    assert!(
        !html.contains("/concerts/1/delete-split"),
        "delete-split button must NOT appear; got: {}",
        html
    );
}

// delete_download_missing_file_returns_confirm_fragment migrated to
// hurl/media_state_errors.hurl.

// delete_download_force_clears_state_when_file_missing migrated to
// hurl/media_state_errors.hurl.

// delete_split_clears_state migrated to hurl/split_timestamps_state.hurl.

// delete_split_when_not_split_returns_400 migrated to
// hurl/split_timestamps_state.hurl.

// detail_page_renders_set_list_and_state migrated to
// hurl/detail_prepare_notes.hurl.

#[tokio::test]
async fn ignore_deletes_preview_image() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Test Album";
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    let preview = cd.join("preview.jpg");
    std::fs::write(&preview, b"fake jpg").unwrap();
    assert!(preview.exists());

    let conn = db::connection::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/c/ign", album);
    db::concerts::update_metadata(
        &conn,
        1,
        &MetadataUpdate {
            artist: "Test".to_string(),
            album: album.to_string(),
            description: None,
            set_list: vec![],
            musicians: vec![],
        },
    )
    .unwrap();
    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        registry: Arc::new(JobRegistry::new()),
        scrape_queue: idle_scrape_queue(),
        jobs: JobConfig::test(workdir.path().to_path_buf()),
    };
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/concerts/1/ignore")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        !preview.exists(),
        "preview.jpg should be deleted when concert is ignored"
    );
}

fn seed_split_concert(
    conn: &rusqlite::Connection,
    workdir: &std::path::Path,
    album: &str,
    set_list: Vec<String>,
    available_indices: &[usize],
) {
    db::concerts::upsert_listing(
        conn,
        &NewListing {
            source_url: format!("https://npr.org/c/{}", album),
            title: album.to_string(),
            concert_date: Some("2024-01-15".to_string()),
            teaser: None,
        },
    )
    .unwrap();
    db::concerts::update_metadata(
        conn,
        1,
        &MetadataUpdate {
            artist: "Test Artist".to_string(),
            album: album.to_string(),
            description: None,
            set_list: set_list.clone(),
            musicians: vec![],
        },
    )
    .unwrap();

    let cd = concert_dir(workdir, album);
    std::fs::create_dir_all(&cd).unwrap();
    for &idx in available_indices {
        let stem = sanitize_filename(&set_list[idx]);
        std::fs::write(cd.join(format!("{stem}.mp3")), b"fake").unwrap();
    }
}

// next/prev/track media-info navigation cases migrated to
// hurl/media_info_navigation.hurl.

#[tokio::test]
async fn track_details_returns_200_without_album() {
    let conn = db::connection::open_in_memory().unwrap();
    seeded_concert(
        &conn,
        "https://npr.org/c/no-album-details",
        "No Album Details",
    );
    let set_list_json = serde_json::to_string(&vec!["Song A"]).unwrap();
    let tracks_present_json = serde_json::to_string(&vec![true]).unwrap();
    let tracks_liked_json = serde_json::to_string(&vec![true]).unwrap();
    conn.execute(
        "UPDATE concerts
         SET set_list_json = ?1, tracks_present = ?2, tracks_liked = ?3
         WHERE id = 1",
        rusqlite::params![set_list_json, tracks_present_json, tracks_liked_json],
    )
    .unwrap();
    let app = router(test_state(conn));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/track-details")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(info["tracks_busy"], false);
    assert_eq!(info["tracks"][0]["title"], "Song A");
    assert_eq!(info["tracks"][0]["available"], true);
    assert_eq!(info["tracks"][0]["is_video"], false);
    assert_eq!(info["tracks"][0]["liked"], true);
}

#[tokio::test]
async fn track_details_reports_busy_from_handler_state() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Busy Details Album";
    let conn = db::connection::open_in_memory().unwrap();
    seed_split_concert(&conn, workdir.path(), album, vec!["Song A".into()], &[0]);
    db::split_timestamps::set_tracks_present(&conn, 1, &[true]).unwrap();
    db::lifecycle::set_downloaded_at_if_missing(&conn, 1, "2026-07-07 00:00:00").unwrap();
    db::lifecycle::try_mark_split_started(&conn, 1).unwrap();
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/track-details")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(info["tracks_busy"], true);
    assert_eq!(info["tracks"][0]["available"], true);
}

// watch_returns_500_when_downloaded_but_file_missing migrated to
// hurl/media_state_errors.hurl.

// watch_uses_injected_opener_and_succeeds and watch_returns_500_when_opener_fails
// migrated to hurl/job_chain.hurl — see docs/change/2026-07-15-job-driver-plan.md.

// media_info_returns_500_when_downloaded_but_file_missing migrated to
// hurl/media_state_errors.hurl.

// watch_returns_404_when_concert_not_downloaded migrated to
// hurl/media_state_errors.hurl.

// like_track_toggles_state_and_renders_star migrated to
// hurl/split_timestamps_state.hurl.

// like_track_unavailable_returns_404 migrated to
// hurl/split_timestamps_state.hurl.

// media-info liked-state cases migrated to hurl/media_info_navigation.hurl.

// like_track_out_of_range_returns_404 migrated to
// hurl/split_timestamps_state.hurl.

// ── prepare endpoints ────────────────────────────────────────────────────────

/// Seed a scraped concert (id=1) with the given album and set list.
fn seed_scraped(conn: &rusqlite::Connection, album: &str, set_list: Vec<String>) {
    seeded_concert(conn, "https://npr.org/c/prepare", "Prepare Concert");
    db::concerts::update_metadata(
        conn,
        1,
        &MetadataUpdate {
            artist: "Artist".to_string(),
            album: album.to_string(),
            description: None,
            set_list,
            musicians: vec![],
        },
    )
    .unwrap();
}

async fn post_json(app: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn get_json(app: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

// prepare_endpoint_runs_download_then_split_chain migrated to
// hurl/job_chain.hurl — see docs/change/2026-07-15-job-driver-plan.md.

#[tokio::test]
async fn prepare_status_reports_filesystem_track_state() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Status Album";
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    // Only the second track exists on disk.
    std::fs::write(cd.join("Song B.m4a"), b"audio").unwrap();

    let conn = db::connection::open_in_memory().unwrap();
    seed_scraped(
        &conn,
        album,
        vec!["Song A".to_string(), "Song B".to_string()],
    );
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let (status, j) = get_json(&app, "/concerts/1/prepare-status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(j["tracks_present"], serde_json::json!([false, true]));
    assert_eq!(j["download"], "not-downloaded");
    assert_eq!(j["split"], "not-split");
    assert_eq!(j["split_queued"], false);
}

// prepare_returns_422_without_set_list migrated to
// hurl/detail_prepare_notes.hurl.

// prepare_returns_404_for_unknown_concert migrated to
// hurl/detail_prepare_notes.hurl.

// download_auto_split_runs_full_chain,
// download_auto_split_reconciles_source_present_downloaded_at_null,
// download_auto_split_retries_on_split_error,
// download_no_set_list_plain_download_no_split_queued,
// download_does_not_resplit_already_split_concert,
// download_double_click_does_not_drop_split_edge, and
// download_force_starts_when_tracks_present_but_source_missing migrated to
// hurl/job_chain.hurl — see docs/change/2026-07-15-job-driver-plan.md.

// ── split-timestamps API tests ────────────────────────────────────────────────

use concert_tracker::db::split_timestamps::{
    get_split_timestamps, set_auto_split_timestamps, set_user_split_timestamps,
};

fn sample_song_timestamps(songs: &[&str]) -> Vec<concert_types::SongTimestamp> {
    songs
        .iter()
        .enumerate()
        .map(|(i, title)| concert_types::SongTimestamp {
            title: title.to_string(),
            start_time: (i * 60) as f64,
            end_time: (i * 60 + 55) as f64,
            duration: 55.0,
        })
        .collect()
}

/// Seed a concert with scraped metadata and a set_list. Returns the concert id.
/// Uses a unique URL per album to avoid collisions between tests.
fn seed_ts_concert(conn: &rusqlite::Connection, album: &str, songs: &[&str]) -> i64 {
    db::concerts::upsert_listing(
        conn,
        &NewListing {
            source_url: format!("https://npr.org/ts/{}", album),
            title: format!("{} Concert", album),
            concert_date: Some("2024-06-01".to_string()),
            teaser: None,
        },
    )
    .unwrap();
    let id = conn
        .query_row(
            "SELECT id FROM concerts WHERE source_url = ?1",
            [format!("https://npr.org/ts/{}", album)],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    db::concerts::update_metadata(
        conn,
        id,
        &MetadataUpdate {
            artist: "Test Artist".to_string(),
            album: album.to_string(),
            description: None,
            set_list: songs.iter().map(|s| s.to_string()).collect(),
            musicians: vec![],
        },
    )
    .unwrap();
    id
}

/// Create a tiny real source file (a 5-second sine wave) using ffmpeg. Returns
/// None if ffmpeg is not available so tests can be skipped gracefully.
async fn create_test_audio(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let out = dir.join(format!("{}.m4a", name));
    let status = tokio::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=5",
            "-c:a",
            "aac",
            "-b:a",
            "32k",
        ])
        .arg(&out)
        .output()
        .await
        .ok()?;
    if status.status.success() {
        Some(out)
    } else {
        None
    }
}

// ── GET /concerts/:id/split-timestamps ───────────────────────────────────────

// get_split_timestamps_returns_404_for_unknown_id migrated to
// hurl/split_timestamps_state.hurl.

// get_split_timestamps_returns_null_auto_and_user_initially migrated to
// hurl/split_timestamps_state.hurl.

// get_split_timestamps_returns_seeded_auto_timestamps migrated to
// hurl/split_timestamps_state.hurl.

// get_split_timestamps_returns_both_auto_and_user migrated to
// hurl/split_timestamps_state.hurl.

#[tokio::test]
async fn get_split_timestamps_lazy_backfill_from_timestamps_json() {
    use concert_types::ConcertInfo;

    let workdir = tempfile::tempdir().unwrap();
    let album = "Backfill Album";
    let songs = ["Old Song A", "Old Song B"];

    let conn = db::connection::open_in_memory().unwrap();
    let id = seed_ts_concert(&conn, album, &songs);

    // Write timestamps.json to the concert dir (simulating a pre-feature split).
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    let ts_json = serde_json::to_string(&ConcertInfo {
        artist: "Test".to_string(),
        source: String::new(),
        show: String::new(),
        date: None,
        album: album.to_string(),
        description: None,
        set_list: vec![],
        musicians: vec![],
        preview_image_url: None,
        teaser: None,
        timestamps: Some(sample_song_timestamps(&songs)),
    })
    .unwrap();
    std::fs::write(cd.join("timestamps.json"), ts_json).unwrap();

    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));
    let (status, json) = get_json(&app, &format!("/concerts/{id}/split-timestamps")).await;
    assert_eq!(status, StatusCode::OK);
    let auto_arr = json["auto"].as_array().unwrap();
    assert_eq!(auto_arr.len(), 2, "backfill should populate auto from disk");
    assert_eq!(auto_arr[0]["title"], "Old Song A");
}

// get_split_timestamps_uses_stored_media_duration_when_source_missing migrated
// to hurl/split_timestamps_state.hurl.

// ── POST /concerts/:id/split-timestamps ──────────────────────────────────────

async fn post_body_json(
    app: &axum::Router,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn post_body_text(
    app: &axum::Router,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

// set_split_timestamps_returns_404_for_unknown_concert migrated to
// hurl/split_timestamps_state.hurl.

// set_split_timestamps_returns_409_when_source_missing migrated to
// hurl/split_timestamps_state.hurl.

#[tokio::test]
async fn set_split_timestamps_returns_422_on_count_mismatch() {
    // Source file must exist to pass the 409 check before we reach the count check.
    let workdir = tempfile::tempdir().unwrap();
    let album = "Count Mismatch Album";
    let songs = ["A", "B"];

    let conn = db::connection::open_in_memory().unwrap();
    let id = seed_ts_concert(&conn, album, &songs);

    // Touch a fake source file so the handler gets past the 409 check.
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    std::fs::write(cd.join(format!("{}.mp4", album)), b"fake").unwrap();

    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    // Submit 1 timestamp but set_list has 2 songs → 422 before ffprobe.
    let body = serde_json::json!({"songs": [
        {"title": "A", "start_time": 0.0, "end_time": 55.0}
    ]});
    let (status, text) =
        post_body_text(&app, &format!("/concerts/{id}/split-timestamps"), body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(text, "Expected 2 timestamps (one per set-list song), got 1");
}

/// Happy path: POST user timestamps → 202 and eventually the user column is set.
/// Skips if ffmpeg is unavailable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_split_timestamps_happy_path_returns_202_and_stores_user_column() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "User TS Album";
    let songs = ["Song One", "Song Two"];

    let conn = db::connection::open_in_memory().unwrap();
    let id = seed_ts_concert(&conn, album, &songs);

    // Create real audio file for ffprobe.
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    let source = match create_test_audio(&cd, album).await {
        Some(p) => p,
        None => {
            eprintln!("skipping: ffmpeg not available");
            return;
        }
    };

    let db_arc = Arc::new(Mutex::new(conn));
    // Touch output track files so the split cmd "succeeds".
    let song_files: Vec<String> = songs
        .iter()
        .map(|s| {
            format!(
                "'{}'",
                cd.join(format!("{}.m4a", sanitize_filename(s))).display()
            )
        })
        .collect();
    let touch_songs = format!("touch {}", song_files.join(" "));
    let state = AppState {
        db: db_arc.clone(),
        registry: Arc::new(JobRegistry::new()),
        scrape_queue: idle_scrape_queue(),
        jobs: JobConfig::from_commands(
            workdir.path().to_path_buf(),
            Arc::new(|_: &DownloadJob| Command::new("true")),
            Arc::new(move |_: &SplitJob| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(touch_songs.clone());
                cmd
            }),
            Arc::new(|_| Command::new("true")),
        ),
    };
    let app = router(state);

    // The source exists but downloaded_at is NULL — handler reconciles it.
    let _ = source; // just for clarity; file is already on disk
    let body = serde_json::json!({"songs": [
        {"title": "Song One", "start_time": 0.0, "end_time": 2.5},
        {"title": "Song Two", "start_time": 2.5, "end_time": 5.0}
    ]});
    let (status, json) =
        post_body_json(&app, &format!("/concerts/{id}/split-timestamps"), body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "expected 202: {:?}", json);
    assert_eq!(json["status"], "splitting");

    // Wait for the job to finish (user column set).
    for _ in 0..200 {
        let stored = {
            let conn = db_arc.lock().unwrap();
            get_split_timestamps(&conn, id).unwrap()
        };
        if stored.user.is_some() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("user_split_timestamps_json never set after split job completed");
}

// ── POST /concerts/:id/split-timestamps/reset ────────────────────────────────

// reset_split_timestamps_returns_404_for_unknown_concert migrated to
// hurl/split_timestamps_state.hurl.

// reset_split_timestamps_returns_422_when_no_auto_timestamps migrated to
// hurl/split_timestamps_state.hurl.

// reset_split_timestamps_returns_already_auto_when_user_is_null migrated to
// hurl/split_timestamps_state.hurl.

/// Happy path for reset: user column is non-NULL + auto available → 202 and
/// eventually user column is cleared. Skips if ffmpeg is unavailable (need
/// source file for start_split to proceed).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_split_timestamps_happy_path_returns_202_and_clears_user_column() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Reset Album";
    let songs = ["Reset A", "Reset B"];

    let conn = db::connection::open_in_memory().unwrap();
    let id = seed_ts_concert(&conn, album, &songs);

    // Need a real source file for start_split (downloaded_at reconcile + path check).
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    let source_path = cd.join(format!("{}.mp4", album));
    // Use ffmpeg if available, else a fake file (start_split only checks existence).
    if create_test_audio(&cd, album).await.is_none() {
        // Create a zero-byte file; start_split only checks file existence.
        std::fs::write(&source_path, b"fake").unwrap();
    }

    let auto_ts = sample_song_timestamps(&songs);
    set_auto_split_timestamps(&conn, id, &auto_ts).unwrap();
    let user_ts: Vec<concert_types::SongTimestamp> = songs
        .iter()
        .enumerate()
        .map(|(i, title)| concert_types::SongTimestamp {
            title: title.to_string(),
            start_time: (i * 60 + 1) as f64,
            end_time: (i * 60 + 54) as f64,
            duration: 53.0,
        })
        .collect();
    set_user_split_timestamps(&conn, id, &user_ts).unwrap();

    let db_arc = Arc::new(Mutex::new(conn));
    let song_files: Vec<String> = songs
        .iter()
        .map(|s| {
            format!(
                "'{}'",
                cd.join(format!("{}.m4a", sanitize_filename(s))).display()
            )
        })
        .collect();
    let touch_songs = format!("touch {}", song_files.join(" "));
    let state = AppState {
        db: db_arc.clone(),
        registry: Arc::new(JobRegistry::new()),
        scrape_queue: idle_scrape_queue(),
        jobs: JobConfig::from_commands(
            workdir.path().to_path_buf(),
            Arc::new(|_: &DownloadJob| Command::new("true")),
            Arc::new(move |_: &SplitJob| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(touch_songs.clone());
                cmd
            }),
            Arc::new(|_| Command::new("true")),
        ),
    };
    let app = router(state);

    let (status, json) = post_json(&app, &format!("/concerts/{id}/split-timestamps/reset")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "expected 202: {:?}", json);
    assert_eq!(json["status"], "splitting");

    // Wait for job to clear user column.
    for _ in 0..200 {
        let stored = {
            let conn = db_arc.lock().unwrap();
            get_split_timestamps(&conn, id).unwrap()
        };
        if stored.user.is_none() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("user_split_timestamps_json not cleared after reset job completed");
}

// delete_split_preserves_split_timestamp_columns migrated to
// hurl/split_timestamps_state.hurl.

// playlist_api_crud_and_resolution, playlist_api_validation_status_codes,
// playlists_html_pages_render, and playlist_detail_page_unknown_id_is_404
// migrated to hurl/playlists.hurl.

// ── concert_playback endpoint tests ──────────────────────────────────────────

fn seed_split_concert_with_files(
    album: &str,
    songs: &[&str],
    workdir: &std::path::Path,
) -> rusqlite::Connection {
    let conn = db::connection::open_in_memory().unwrap();
    db::concerts::upsert_listing(
        &conn,
        &NewListing {
            source_url: "https://npr.org/d/recon".to_string(),
            title: "Recon Concert".to_string(),
            concert_date: Some("2024-03-01".to_string()),
            teaser: None,
        },
    )
    .unwrap();
    db::concerts::update_metadata(
        &conn,
        1,
        &MetadataUpdate {
            artist: "X".to_string(),
            album: album.to_string(),
            description: None,
            set_list: songs.iter().map(|s| s.to_string()).collect(),
            musicians: vec![],
        },
    )
    .unwrap();
    db::lifecycle::try_mark_download_started(&conn, 1).unwrap();
    db::lifecycle::mark_download_succeeded(&conn, 1, "mp4").unwrap();
    db::lifecycle::try_mark_split_started(&conn, 1).unwrap();
    db::lifecycle::mark_split_succeeded(&conn, 1).unwrap();
    db::split_timestamps::set_tracks_present(&conn, 1, &vec![true; songs.len()]).unwrap();

    let cd = concert_dir(workdir, album);
    std::fs::create_dir_all(&cd).unwrap();
    for song in songs {
        let stem = sanitize_filename(song);
        std::fs::write(cd.join(format!("{stem}.m4a")), b"fake audio").unwrap();
    }
    conn
}

#[tokio::test]
async fn concert_playback_source_mode_when_file_present() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Recon Album";
    let conn = seed_split_concert_with_files(album, &["Song A", "Song B"], workdir.path());
    let cd = concert_dir(workdir.path(), album);
    std::fs::write(cd.join(format!("{album}.mp4")), b"fake source").unwrap();

    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));
    let (status, json) = get_json(&app, "/concerts/1/concert-playback").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["mode"], "source", "expected source mode: {json}");
    assert!(json["source"].is_object(), "source key must be MediaInfo");
}

#[tokio::test]
async fn concert_playback_reconstruction_mode_when_source_gone() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Recon Album";
    let conn = seed_split_concert_with_files(album, &["Song A", "Song B"], workdir.path());

    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));
    let (status, json) = get_json(&app, "/concerts/1/concert-playback").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json["mode"], "reconstruction",
        "expected reconstruction mode: {json}"
    );
    let items = json["items"].as_array().expect("items must be array");
    assert_eq!(items.len(), 2, "two songs in reconstruction: {json}");
    assert_eq!(items[0]["kind"], "song");
    assert_eq!(items[1]["kind"], "song");
}

#[tokio::test]
async fn concert_playback_reconstruction_includes_interlude() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Recon Album";
    let conn = seed_split_concert_with_files(album, &["Song A", "Song B"], workdir.path());

    let cd = concert_dir(workdir.path(), album);
    std::fs::write(cd.join("interlude_01.m4a"), b"fake interlude").unwrap();
    // Song A at 0–55s, Song B at 60–115s, gap at 55–60s → interlude_01.
    let ts = vec![
        concert_types::SongTimestamp {
            title: "Song A".to_string(),
            start_time: 0.0,
            end_time: 55.0,
            duration: 55.0,
        },
        concert_types::SongTimestamp {
            title: "Song B".to_string(),
            start_time: 60.0,
            end_time: 115.0,
            duration: 55.0,
        },
    ];
    set_user_split_timestamps(&conn, 1, &ts).unwrap();
    db::split_timestamps::set_media_duration(&conn, 1, 120.0).unwrap();

    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));
    let (status, json) = get_json(&app, "/concerts/1/concert-playback").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["mode"], "reconstruction");
    let items = json["items"].as_array().expect("items array");
    assert_eq!(items.len(), 3, "song + interlude + song: {json}");
    assert_eq!(items[0]["kind"], "song");
    assert_eq!(items[1]["kind"], "interlude");
    assert_eq!(items[2]["kind"], "song");
}

// concert_playback_returns_404_when_nothing_playable migrated to
// hurl/media_state_errors.hurl.

#[tokio::test]
async fn delete_interlude_removes_file_records_event_returns_fragment() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Recon Album";
    let conn = seed_split_concert_with_files(album, &["Song A", "Song B"], workdir.path());

    let cd = concert_dir(workdir.path(), album);
    let interlude_path = cd.join("interlude_01.m4a");
    std::fs::write(&interlude_path, b"fake interlude").unwrap();
    let ts = vec![
        concert_types::SongTimestamp {
            title: "Song A".to_string(),
            start_time: 0.0,
            end_time: 55.0,
            duration: 55.0,
        },
        concert_types::SongTimestamp {
            title: "Song B".to_string(),
            start_time: 60.0,
            end_time: 115.0,
            duration: 55.0,
        },
    ];
    set_user_split_timestamps(&conn, 1, &ts).unwrap();
    db::split_timestamps::set_media_duration(&conn, 1, 120.0).unwrap();
    let db_arc = Arc::new(Mutex::new(conn));
    let state = AppState {
        db: db_arc.clone(),
        registry: Arc::new(JobRegistry::new()),
        scrape_queue: idle_scrape_queue(),
        jobs: JobConfig::test(workdir.path().to_path_buf()),
    };
    let app = router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/concerts/1/interludes/1/delete")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&bytes);

    assert_eq!(status, StatusCode::OK, "response: {body}");
    assert!(!interlude_path.exists(), "interlude file must be deleted");
    assert!(
        body.contains("track-list"),
        "response must be sidebar HTML fragment: {body}"
    );

    let conn = db_arc.lock().unwrap();
    let events = concert_tracker::events::list_for_concert(&conn, 1);
    assert!(
        events.iter().any(|e| e.event == "interlude_delete"),
        "interlude_delete event must be recorded: {events:?}"
    );
    assert!(
        !events.iter().any(|e| e.event == "track_delete"),
        "track_delete must NOT be recorded for interlude: {events:?}"
    );
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
