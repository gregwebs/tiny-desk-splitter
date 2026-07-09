use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::process::Command;
use tower::ServiceExt;

use concert_tracker::{
    db::{self, MetadataUpdate, NewListing},
    jobs::{
        scrape_queue::{ScrapeItemFn, ScrapeQueue},
        JobConfig, JobRegistry,
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
        Arc::new(Mutex::new(db::open_in_memory().unwrap())),
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

    let conn = db::open_in_memory().unwrap();
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
            db::update_metadata(
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
    db::upsert_listing(
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

#[tokio::test]
async fn list_page_renders_seeded_concert() {
    let conn = db::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/c/1", "Test Concert");
    let app = router(test_state(conn));

    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("Test Concert"));
}

#[tokio::test]
async fn ignore_endpoint_toggles_flag_and_returns_row() {
    let conn = db::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/c/2", "Another Concert");
    let app = router(test_state(conn));

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
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    // After the row redesign, the concert-status slot shows the "ignored"
    // badge alongside an ✕ button (which posts back to /ignore to clear).
    assert!(
        html.contains("badge-ignored"),
        "ignored badge must render in the slot"
    );
    assert!(
        html.contains("title=\"Clear ignored\""),
        "✕ to clear ignored must render alongside the badge"
    );
}

#[tokio::test]
async fn available_concert_row_shows_want_and_ignore_buttons() {
    // In the Available state the concert-status slot exposes the two action
    // buttons. The redesign moved them from a trailing actions row into the
    // status slot itself, replacing the prior "available" badge.
    let conn = db::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/c/avail", "Avail Concert");
    let app = router(test_state(conn));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("/concerts/1/want\""),
        "Want action must appear in slot when Available"
    );
    assert!(
        html.contains("/concerts/1/ignore\""),
        "Ignore action must appear in slot when Available"
    );
    // No "available" badge — that was the visual the buttons replace.
    assert!(!html.contains("badge-available"));
}

#[tokio::test]
async fn not_downloaded_row_hides_download_badge_and_shows_button() {
    // Replaces the prior "not-downloaded" grey badge with the Download
    // action button in the same slot.
    let conn = db::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/c/fresh", "Fresh Concert");
    db::update_metadata(
        &conn,
        1,
        &MetadataUpdate {
            artist: "X".to_string(),
            album: "Some Album".to_string(),
            description: None,
            set_list: vec![],
            musicians: vec![],
        },
    )
    .unwrap();
    let app = router(test_state(conn));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("/concerts/1/download\""),
        "Download button must appear when NotDownloaded"
    );
    assert!(
        !html.contains("badge-not-downloaded"),
        "no 'not-downloaded' badge in fresh state — the button replaces it"
    );
}

#[tokio::test]
async fn list_filter_by_status_narrows_results() {
    let conn = db::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/c/3", "Concert A");
    seeded_concert(&conn, "https://npr.org/c/4", "Concert B");
    db::toggle_ignored(&conn, 1).unwrap();
    let app = router(test_state(conn));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/?filter=ignored")
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
    assert!(html.contains("Concert A"));
    assert!(!html.contains("Concert B"));
}

#[tokio::test]
async fn notes_endpoint_persists_text() {
    let conn = db::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/c/5", "Notes Concert");
    let app = router(test_state(conn));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/concerts/1/notes")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("notes=great+show"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn download_endpoint_spawns_job_and_returns_row() {
    let conn = db::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/c/6", "Download Concert");
    db::update_metadata(
        &conn,
        1,
        &MetadataUpdate {
            artist: "Test".to_string(),
            album: "Test Album".to_string(),
            description: None,
            set_list: vec![],
            musicians: vec![],
        },
    )
    .unwrap();
    let app = router(test_state(conn));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/concerts/1/download")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

/// When a concert is opened for the first time and the scrape fails (e.g.
/// network down or NPR unreachable), the detail page must still render with
/// the listing-only data and `metadata_scraped_at` must stay NULL so the
/// next view can retry. The success path is covered by the unit tests for
/// `ensure_scraped` in src/web/handlers.rs — those use a stub closure and
/// avoid hitting the network, while this test exercises the real call path.
#[tokio::test]
async fn detail_page_auto_scrape_failure_still_renders() {
    disable_system_proxy_for_tests();
    let conn = db::open_in_memory().unwrap();
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
        db::get_concert(&conn, 1).unwrap()
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
    db::upsert_listing(
        conn,
        &NewListing {
            source_url: url.to_string(),
            title: "Downloaded Concert".to_string(),
            concert_date: Some("2024-01-15".to_string()),
            teaser: None,
        },
    )
    .unwrap();
    db::update_metadata(
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
    db::try_mark_download_started(conn, 1).unwrap();
    db::mark_download_succeeded(conn, 1, "mp4").unwrap();
}

#[tokio::test]
async fn delete_download_removes_file_and_clears_state() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Some Album";
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    let mp4 = cd.join(format!("{}.mp4", album));
    std::fs::write(&mp4, b"fake mp4 bytes").unwrap();

    let conn = db::open_in_memory().unwrap();
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
        db::get_concert(&conn, 1).unwrap()
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

    let conn = db::open_in_memory().unwrap();
    seed_downloaded(&conn, "https://npr.org/d/prior-err", album);
    db::try_mark_split_started(&conn, 1).unwrap();
    db::mark_split_failed(&conn, 1, "ffmpeg crashed").unwrap();

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

#[tokio::test]
async fn downloaded_filter_includes_split_concerts() {
    // With the new two-axis statuses, the "Downloaded" filter pill should
    // include every concert whose download badge reads "downloaded" — even
    // if it has also been split. (Previously the filter carried a hidden
    // `&& !Split` guard, which surprised users when their split concerts
    // disappeared from the Downloaded filter despite showing the badge.)
    let conn = db::open_in_memory().unwrap();
    // First concert: downloaded only. `seed_downloaded` hardcodes id=1.
    seed_downloaded(&conn, "https://npr.org/d/dl-only", "Album One");
    // Second concert: downloaded + split. Set up inline because seed_downloaded
    // only handles a single id=1 concert.
    db::upsert_listing(
        &conn,
        &NewListing {
            source_url: "https://npr.org/d/dl-split".to_string(),
            title: "Split Concert".to_string(),
            concert_date: Some("2024-01-16".to_string()),
            teaser: None,
        },
    )
    .unwrap();
    let id2 = db::get_concert_by_url(&conn, "https://npr.org/d/dl-split")
        .unwrap()
        .unwrap()
        .id;
    db::update_metadata(
        &conn,
        id2,
        &MetadataUpdate {
            artist: "Y".to_string(),
            album: "Album Two".to_string(),
            description: None,
            set_list: vec![],
            musicians: vec![],
        },
    )
    .unwrap();
    db::try_mark_download_started(&conn, id2).unwrap();
    db::mark_download_succeeded(&conn, id2, "mp4").unwrap();
    db::try_mark_split_started(&conn, id2).unwrap();
    db::mark_split_succeeded(&conn, id2).unwrap();

    let app = router(test_state(conn));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/?filter=downloaded")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);

    // Cards are rendered with id="concert-{id}". Both must appear.
    assert!(
        html.contains("id=\"concert-1\""),
        "downloaded-only concert (id 1) must appear under Downloaded filter"
    );
    assert!(
        html.contains(&format!("id=\"concert-{}\"", id2)),
        "split concert (id {}) must also appear under Downloaded filter — its badge reads 'downloaded'",
        id2
    );
}

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

    let conn = db::open_in_memory().unwrap();
    seed_downloaded(&conn, "https://npr.org/d/split-listen", album);
    db::update_metadata(
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
    db::try_mark_split_started(&conn, 1).unwrap();
    db::mark_split_succeeded(&conn, 1).unwrap();
    db::set_tracks_present(&conn, 1, &[true, true]).unwrap();
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

#[tokio::test]
async fn delete_download_missing_file_returns_confirm_fragment() {
    let workdir = tempfile::tempdir().unwrap();
    // Deliberately don't create the mp4 — the file is "missing".

    let conn = db::open_in_memory().unwrap();
    seed_downloaded(&conn, "https://npr.org/d/2", "Some Album");
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

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
    assert!(
        response.headers().get("HX-Refresh").is_none(),
        "missing-file response must NOT trigger a page refresh — it returns a confirm fragment"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("delete-confirm"),
        "body should be the confirm fragment, got: {}",
        html
    );
    assert!(html.contains("Yes, clear record"));
}

#[tokio::test]
async fn delete_download_force_clears_state_when_file_missing() {
    let workdir = tempfile::tempdir().unwrap();
    let conn = db::open_in_memory().unwrap();
    seed_downloaded(&conn, "https://npr.org/d/3", "Some Album");
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
                .uri("/concerts/1/delete-download?force=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().get("HX-Refresh").is_none(),
        "delete must NOT trigger a full page reload"
    );
    assert_eq!(
        response
            .headers()
            .get("HX-Retarget")
            .and_then(|v| v.to_str().ok()),
        Some("#concert-1")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("id=\"concert-1\""));
    let c = {
        let conn = db_arc.lock().unwrap();
        db::get_concert(&conn, 1).unwrap()
    };
    assert!(c.downloaded_at.is_none());
}

#[tokio::test]
async fn delete_split_clears_state() {
    let conn = db::open_in_memory().unwrap();
    seed_downloaded(&conn, "https://npr.org/d/4", "Some Album");
    db::try_mark_split_started(&conn, 1).unwrap();
    db::mark_split_succeeded(&conn, 1).unwrap();
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
                .method("POST")
                .uri("/concerts/1/delete-split")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().get("HX-Refresh").is_none(),
        "delete-split must NOT trigger a full page reload"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("id=\"concert-1\""),
        "body is the re-rendered card"
    );
    assert!(
        !html.contains("/concerts/1/split\""),
        "no Split button: splitting is automated via track play"
    );
    let c = {
        let conn = db_arc.lock().unwrap();
        db::get_concert(&conn, 1).unwrap()
    };
    assert!(c.split_at.is_none());
    assert!(c.downloaded_at.is_some(), "download must be untouched");
}

#[tokio::test]
async fn delete_split_when_not_split_returns_400() {
    let conn = db::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/d/5", "Not Split Concert");
    let app = router(test_state(conn));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/concerts/1/delete-split")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn detail_page_renders_set_list_and_state() {
    let conn = db::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/c/7", "Detail Concert");
    db::update_metadata(
        &conn,
        1,
        &MetadataUpdate {
            artist: "Detail Artist".to_string(),
            album: "Detail Album".to_string(),
            description: Some("Great show".to_string()),
            set_list: vec!["Song One".to_string(), "Song Two".to_string()],
            musicians: vec![],
        },
    )
    .unwrap();
    let app = router(test_state(conn));

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
    assert!(html.contains("Song One"));
    assert!(html.contains("Song Two"));
    assert!(html.contains("Detail Artist"));
}

#[tokio::test]
async fn ignore_deletes_preview_image() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Test Album";
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    let preview = cd.join("preview.jpg");
    std::fs::write(&preview, b"fake jpg").unwrap();
    assert!(preview.exists());

    let conn = db::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/c/ign", album);
    db::update_metadata(
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
    db::upsert_listing(
        conn,
        &NewListing {
            source_url: format!("https://npr.org/c/{}", album),
            title: album.to_string(),
            concert_date: Some("2024-01-15".to_string()),
            teaser: None,
        },
    )
    .unwrap();
    db::update_metadata(
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

#[tokio::test]
async fn next_media_info_returns_next_available_track() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Next Track Album";
    let conn = db::open_in_memory().unwrap();
    seed_split_concert(
        &conn,
        workdir.path(),
        album,
        vec!["Song A".into(), "Song B".into(), "Song C".into()],
        &[0, 1, 2],
    );
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/tracks/0/next-media-info")
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
    assert_eq!(info["title"], "Song B");
    assert_eq!(info["track_index"], 1);
    assert_eq!(info["playable"], true);
}

#[tokio::test]
async fn next_media_info_skips_unavailable_tracks() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Skip Album";
    let conn = db::open_in_memory().unwrap();
    // Track 1 ("Song B") has no file on disk — should be skipped
    seed_split_concert(
        &conn,
        workdir.path(),
        album,
        vec!["Song A".into(), "Song B".into(), "Song C".into()],
        &[0, 2],
    );
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/tracks/0/next-media-info")
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
    assert_eq!(info["title"], "Song C");
    assert_eq!(info["track_index"], 2);
}

#[tokio::test]
async fn next_media_info_returns_404_at_last_track() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Last Track Album";
    let conn = db::open_in_memory().unwrap();
    seed_split_concert(
        &conn,
        workdir.path(),
        album,
        vec!["Song A".into(), "Song B".into()],
        &[0, 1],
    );
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/tracks/1/next-media-info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn prev_media_info_returns_prev_available_track() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Prev Track Album";
    let conn = db::open_in_memory().unwrap();
    seed_split_concert(
        &conn,
        workdir.path(),
        album,
        vec!["Song A".into(), "Song B".into(), "Song C".into()],
        &[0, 1, 2],
    );
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/tracks/2/prev-media-info")
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
    assert_eq!(info["title"], "Song B");
    assert_eq!(info["track_index"], 1);
    assert_eq!(info["playable"], true);
    // Song A is still earlier, and Song C is still later.
    assert_eq!(info["has_prev"], true);
    assert_eq!(info["has_next"], true);
}

#[tokio::test]
async fn prev_media_info_skips_unavailable_tracks() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Prev Skip Album";
    let conn = db::open_in_memory().unwrap();
    // Track 1 ("Song B") has no file on disk — should be skipped going back.
    seed_split_concert(
        &conn,
        workdir.path(),
        album,
        vec!["Song A".into(), "Song B".into(), "Song C".into()],
        &[0, 2],
    );
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/tracks/2/prev-media-info")
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
    assert_eq!(info["title"], "Song A");
    assert_eq!(info["track_index"], 0);
    assert_eq!(info["has_prev"], false);
}

#[tokio::test]
async fn prev_media_info_returns_404_at_first_track() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "First Track Album";
    let conn = db::open_in_memory().unwrap();
    seed_split_concert(
        &conn,
        workdir.path(),
        album,
        vec!["Song A".into(), "Song B".into()],
        &[0, 1],
    );
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/tracks/0/prev-media-info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn track_media_info_reports_has_prev() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Has Prev Album";
    let conn = db::open_in_memory().unwrap();
    seed_split_concert(
        &conn,
        workdir.path(),
        album,
        vec!["Song A".into(), "Song B".into()],
        &[0, 1],
    );
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    // First track: nothing before it.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/concerts/1/tracks/0/media-info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(info["has_prev"], false);

    // Second track: Song A is before it.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/tracks/1/media-info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(info["has_prev"], true);
}

#[tokio::test]
async fn track_details_returns_200_without_album() {
    let conn = db::open_in_memory().unwrap();
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
    let conn = db::open_in_memory().unwrap();
    seed_split_concert(&conn, workdir.path(), album, vec!["Song A".into()], &[0]);
    db::set_tracks_present(&conn, 1, &[true]).unwrap();
    db::set_downloaded_at_if_missing(&conn, 1, "2026-07-07 00:00:00").unwrap();
    db::try_mark_split_started(&conn, 1).unwrap();
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

#[tokio::test]
async fn watch_returns_500_when_downloaded_but_file_missing() {
    // Concert is marked downloaded in the DB but the media file is not on
    // disk (e.g. an old import whose archive never contained the source).
    // The handler must signal a server-side data-integrity issue, not a 404,
    // so the UI can surface an error indicator to the user.
    let workdir = tempfile::tempdir().unwrap();
    let conn = db::open_in_memory().unwrap();
    seed_downloaded(&conn, "https://npr.org/d/no-file", "Missing File Album");
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/concerts/1/watch")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// Build an AppState whose opener runs `program` (e.g. "true"/"false") instead
/// of the real `open`, so watch tests never launch a media player.
fn state_with_opener(
    conn: rusqlite::Connection,
    workdir: PathBuf,
    program: &'static str,
) -> AppState {
    let mut jobs = JobConfig::test(workdir);
    jobs.open_cmd = Arc::new(move |path| {
        let mut c = Command::new(program);
        c.arg(path);
        c
    });
    AppState {
        db: Arc::new(Mutex::new(conn)),
        registry: Arc::new(JobRegistry::new()),
        scrape_queue: idle_scrape_queue(),
        jobs,
    }
}

#[tokio::test]
async fn watch_uses_injected_opener_and_succeeds() {
    // The injected opener (`true`) is invoked instead of the real `open`, so the
    // handler returns 200 without launching a media player.
    let workdir = tempfile::tempdir().unwrap();
    let conn = db::open_in_memory().unwrap();
    seed_downloaded(&conn, "https://npr.org/d/open-ok", "Opener Album");
    let cd = concert_dir(workdir.path(), "Opener Album");
    std::fs::create_dir_all(&cd).unwrap();
    std::fs::write(cd.join("Opener Album.mp4"), b"fake mp4 bytes").unwrap();
    let app = router(state_with_opener(
        conn,
        workdir.path().to_path_buf(),
        "true",
    ));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/concerts/1/watch")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn watch_returns_500_when_opener_fails() {
    // A failing opener (`false`, exit 1) escalates to 500 even though the file
    // exists — proving the handler runs the injected command and checks status.
    let workdir = tempfile::tempdir().unwrap();
    let conn = db::open_in_memory().unwrap();
    seed_downloaded(&conn, "https://npr.org/d/open-fail", "Opener Fail Album");
    let cd = concert_dir(workdir.path(), "Opener Fail Album");
    std::fs::create_dir_all(&cd).unwrap();
    std::fs::write(cd.join("Opener Fail Album.mp4"), b"fake mp4 bytes").unwrap();
    let app = router(state_with_opener(
        conn,
        workdir.path().to_path_buf(),
        "false",
    ));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/concerts/1/watch")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn media_info_returns_500_when_downloaded_but_file_missing() {
    let workdir = tempfile::tempdir().unwrap();
    let conn = db::open_in_memory().unwrap();
    seed_downloaded(&conn, "https://npr.org/d/no-file-mi", "Missing File Album");
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/media-info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn watch_returns_404_when_concert_not_downloaded() {
    // Regression: only the "downloaded but file missing" case escalates to
    // 500. A concert that simply hasn't been downloaded yet still 404s.
    let workdir = tempfile::tempdir().unwrap();
    let conn = db::open_in_memory().unwrap();
    db::upsert_listing(
        &conn,
        &NewListing {
            source_url: "https://npr.org/d/not-dl".to_string(),
            title: "Not Yet Downloaded".to_string(),
            concert_date: None,
            teaser: None,
        },
    )
    .unwrap();
    db::update_metadata(
        &conn,
        1,
        &MetadataUpdate {
            artist: "X".to_string(),
            album: "Pending Album".to_string(),
            description: None,
            set_list: vec![],
            musicians: vec![],
        },
    )
    .unwrap();
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/concerts/1/watch")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn like_track_toggles_state_and_renders_star() {
    let conn = db::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/c/1", "Concert");
    db::update_metadata(
        &conn,
        1,
        &MetadataUpdate {
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            description: None,
            set_list: vec!["Song A".to_string(), "Song B".to_string()],
            musicians: vec![],
        },
    )
    .unwrap();
    // Mark both tracks present; starring is only allowed on available tracks.
    db::set_tracks_present(&conn, 1, &[true, true]).unwrap();
    let app = router(test_state(conn));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/concerts/1/tracks/0/like")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("btn-like liked"),
        "filled star class expected"
    );
    assert!(html.contains("★"), "filled star glyph expected");
}

#[tokio::test]
async fn like_track_unavailable_returns_404() {
    // Starring a deleted / unavailable track must be rejected.
    let conn = db::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/c/2", "Concert");
    db::update_metadata(
        &conn,
        1,
        &MetadataUpdate {
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            description: None,
            set_list: vec!["Song A".to_string(), "Song B".to_string()],
            musicians: vec![],
        },
    )
    .unwrap();
    // Track 0 present, track 1 absent (simulates deletion).
    db::set_tracks_present(&conn, 1, &[true, false]).unwrap();
    let app = router(test_state(conn));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/concerts/1/tracks/1/like")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn track_media_info_reports_liked_state() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Liked Album";
    let conn = db::open_in_memory().unwrap();
    seed_split_concert(
        &conn,
        workdir.path(),
        album,
        vec!["Song A".into(), "Song B".into()],
        &[0, 1],
    );
    // Like only the second track.
    db::toggle_track_liked(&conn, 1, 1).unwrap();
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let liked: serde_json::Value = {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/concerts/1/tracks/1/media-info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    };
    assert_eq!(liked["liked"], true, "track 1 was liked");

    let unliked: serde_json::Value = {
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/concerts/1/tracks/0/media-info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    };
    assert_eq!(unliked["liked"], false, "track 0 was not liked");
}

#[tokio::test]
async fn track_media_info_liked_false_when_tracks_liked_unset() {
    // No like has ever been recorded, so `tracks_liked` is empty/shorter than
    // the set list; the .get(idx).unwrap_or(false) read must default to false
    // rather than panic.
    let workdir = tempfile::tempdir().unwrap();
    let album = "No Likes Album";
    let conn = db::open_in_memory().unwrap();
    seed_split_concert(
        &conn,
        workdir.path(),
        album,
        vec!["Song A".into(), "Song B".into()],
        &[0, 1],
    );
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/tracks/1/media-info")
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
    assert_eq!(info["liked"], false);
}

#[tokio::test]
async fn next_media_info_carries_liked_state() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Next Liked Album";
    let conn = db::open_in_memory().unwrap();
    seed_split_concert(
        &conn,
        workdir.path(),
        album,
        vec!["Song A".into(), "Song B".into(), "Song C".into()],
        &[0, 1, 2],
    );
    // Like the track that auto-advance will land on (index 1).
    db::toggle_track_liked(&conn, 1, 1).unwrap();
    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/concerts/1/tracks/0/next-media-info")
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
    assert_eq!(info["track_index"], 1);
    assert_eq!(info["liked"], true);
}

#[tokio::test]
async fn like_track_out_of_range_returns_404() {
    let conn = db::open_in_memory().unwrap();
    seeded_concert(&conn, "https://npr.org/c/1", "Concert");
    db::update_metadata(
        &conn,
        1,
        &MetadataUpdate {
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            description: None,
            set_list: vec!["Song A".to_string()],
            musicians: vec![],
        },
    )
    .unwrap();
    let app = router(test_state(conn));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/concerts/1/tracks/5/like")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── prepare endpoints ────────────────────────────────────────────────────────

/// Seed a scraped concert (id=1) with the given album and set list.
fn seed_scraped(conn: &rusqlite::Connection, album: &str, set_list: Vec<String>) {
    seeded_concert(conn, "https://npr.org/c/prepare", "Prepare Concert");
    db::update_metadata(
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

/// Full automated chain through the HTTP API: POST /prepare on a concert with
/// no source file runs download → split (real stub shell commands, no mocks)
/// and prepare-status converges on every track present.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_endpoint_runs_download_then_split_chain() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Chain Album";
    let cd = concert_dir(workdir.path(), album);
    let source = cd.join(format!("{}.mp4", album));
    let fetch = format!(
        "mkdir -p '{}' && touch '{}'",
        cd.display(),
        source.display()
    );
    let touch = format!(
        "touch '{}' '{}'",
        cd.join("Song A.m4a").display(),
        cd.join("Song B.m4a").display()
    );

    let conn = db::open_in_memory().unwrap();
    seed_scraped(
        &conn,
        album,
        vec!["Song A".to_string(), "Song B".to_string()],
    );
    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        registry: Arc::new(JobRegistry::new()),
        scrape_queue: idle_scrape_queue(),
        jobs: JobConfig {
            working_dir: workdir.path().to_path_buf(),
            download_cmd: Arc::new(move |_| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(fetch.clone());
                cmd
            }),
            split_cmd: Arc::new(move |_| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(touch.clone());
                cmd
            }),
            open_cmd: Arc::new(|_| Command::new("true")),
        },
    };
    let app = router(state);

    // Two POSTs back to back: the second must be a no-op, not a second chain.
    let (s1, j1) = post_json(&app, "/concerts/1/prepare").await;
    let (s2, _) = post_json(&app, "/concerts/1/prepare").await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(j1["tracks_present"], serde_json::json!([false, false]));

    // Poll prepare-status until the chain finishes.
    let mut done = false;
    for _ in 0..100 {
        let (status, j) = get_json(&app, "/concerts/1/prepare-status").await;
        assert_eq!(status, StatusCode::OK);
        if j["tracks_present"] == serde_json::json!([true, true]) {
            assert_eq!(j["download"], "downloaded");
            assert_eq!(j["split"], "split");
            assert_eq!(j["split_queued"], false);
            done = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(done, "chain never completed");
}

#[tokio::test]
async fn prepare_status_reports_filesystem_track_state() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Status Album";
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    // Only the second track exists on disk.
    std::fs::write(cd.join("Song B.m4a"), b"audio").unwrap();

    let conn = db::open_in_memory().unwrap();
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

#[tokio::test]
async fn prepare_returns_422_without_set_list() {
    let conn = db::open_in_memory().unwrap();
    seed_scraped(&conn, "Empty Album", vec![]);
    let app = router(test_state(conn));

    let (status, _) = post_json(&app, "/concerts/1/prepare").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn prepare_returns_404_for_unknown_concert() {
    let conn = db::open_in_memory().unwrap();
    let app = router(test_state(conn));

    let (status, _) = post_json(&app, "/concerts/99/prepare").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── Download auto-split tests ─────────────────────────────────────────────────

/// Helper: build an AppState backed by real shell commands. The download "fetches"
/// the source file by touching it; the splitter creates the per-song files.
fn state_with_chain(
    conn: rusqlite::Connection,
    workdir: &std::path::Path,
    album: &str,
    songs: &[&str],
) -> AppState {
    let wd = workdir.to_path_buf();
    let cd = concert_dir(&wd, album);
    let source = cd.join(format!("{}.mp4", album));
    let fetch = format!(
        "mkdir -p '{}' && touch '{}'",
        cd.display(),
        source.display()
    );
    let song_files: Vec<String> = songs
        .iter()
        .map(|s| format!("'{}'", cd.join(format!("{}.m4a", s)).display()))
        .collect();
    let touch_songs = format!("touch {}", song_files.join(" "));
    AppState {
        db: Arc::new(Mutex::new(conn)),
        registry: Arc::new(JobRegistry::new()),
        scrape_queue: idle_scrape_queue(),
        jobs: JobConfig {
            working_dir: wd,
            download_cmd: Arc::new(move |_| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(fetch.clone());
                cmd
            }),
            split_cmd: Arc::new(move |_| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(touch_songs.clone());
                cmd
            }),
            open_cmd: Arc::new(|_| Command::new("true")),
        },
    }
}

async fn wait_for_split(app: &axum::Router, id: i64, tracks: usize) {
    for _ in 0..200 {
        let (_, j) = get_json(app, &format!("/concerts/{id}/prepare-status")).await;
        let present: Vec<bool> =
            serde_json::from_value(j["tracks_present"].clone()).unwrap_or_default();
        // tracks_present is read from disk and can flip true before the split
        // job's DB write lands (src/jobs/split.rs writes the file, then only
        // afterwards marks split succeeded in the DB). Wait for both so callers
        // never observe a "split" assertion racing the DB update.
        if present.len() == tracks && present.iter().all(|&p| p) && j["split"] == "split" {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("chain never completed: split not finished with all tracks present after 10s");
}

/// POST /download on a not-downloaded concert with a set list runs the full
/// download → split chain without any track click.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn download_auto_split_runs_full_chain() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Auto Split Album";
    let songs = ["Song A", "Song B"];

    let conn = db::open_in_memory().unwrap();
    seed_scraped(&conn, album, songs.iter().map(|s| s.to_string()).collect());
    let app = router(state_with_chain(conn, workdir.path(), album, &songs));

    let (status, _) = post_json(&app, "/concerts/1/download").await;
    assert_eq!(status, StatusCode::OK);

    wait_for_split(&app, 1, 2).await;
    let (_, j) = get_json(&app, "/concerts/1/prepare-status").await;
    assert_eq!(j["download"], "downloaded");
    assert_eq!(j["split"], "split");
    assert_eq!(j["split_queued"], false);
}

/// Source file exists on disk but downloaded_at is NULL (manual copy). POST
/// /download should reconcile downloaded_at and start a split, not silently
/// no-op. Regression test for the rejected "extract chaining block" design.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn download_auto_split_reconciles_source_present_downloaded_at_null() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Reconcile Album";
    let songs = ["Track 1"];

    let conn = db::open_in_memory().unwrap();
    seed_scraped(&conn, album, songs.iter().map(|s| s.to_string()).collect());

    // Manually place the source file without setting downloaded_at.
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    std::fs::write(cd.join(format!("{}.mp4", album)), b"video").unwrap();

    let app = router(state_with_chain(conn, workdir.path(), album, &songs));

    let (status, _) = post_json(&app, "/concerts/1/download").await;
    assert_eq!(status, StatusCode::OK);

    // A split should start (not a no-op), resulting in the track file.
    wait_for_split(&app, 1, 1).await;
    let (_, j) = get_json(&app, "/concerts/1/prepare-status").await;
    assert_eq!(j["split"], "split");
}

/// Concert with recorded split errors (SplitError state) should have the chain
/// re-triggered on Download click, just like it would on a play-track click.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn download_auto_split_retries_on_split_error() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Split Error Album";
    let songs = ["Song X"];

    let conn = db::open_in_memory().unwrap();
    seed_scraped(&conn, album, songs.iter().map(|s| s.to_string()).collect());
    // Record a split failure (split_at stays NULL, split_started_at = NULL).
    db::mark_split_failed(&conn, 1, "previous split failed").unwrap();

    let app = router(state_with_chain(conn, workdir.path(), album, &songs));

    let (status, _) = post_json(&app, "/concerts/1/download").await;
    assert_eq!(status, StatusCode::OK);

    wait_for_split(&app, 1, 1).await;
    let (_, j) = get_json(&app, "/concerts/1/prepare-status").await;
    assert_eq!(j["split"], "split");
}

/// A concert with no set list should still download (plain download, no split
/// queued). Behavior unchanged from before this feature.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn download_no_set_list_plain_download_no_split_queued() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "No Setlist Album";

    let conn = db::open_in_memory().unwrap();
    seed_scraped(&conn, album, vec![]);

    let app = router(state_with_chain(conn, workdir.path(), album, &[]));

    let (status, _) = post_json(&app, "/concerts/1/download").await;
    assert_eq!(status, StatusCode::OK);

    // No split should be queued.
    let (_, j) = get_json(&app, "/concerts/1/prepare-status").await;
    assert_eq!(j["split_queued"], false);
    assert_ne!(
        j["download"], "not-downloaded",
        "download should have started"
    );
}

/// Re-downloading a concert that is already split (e.g. source file deleted
/// out-of-band while tracks exist) should NOT re-split. Surviving track file
/// contents must be untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn download_does_not_resplit_already_split_concert() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Already Split Album";
    let songs = ["Keep Track"];

    let conn = db::open_in_memory().unwrap();
    seed_scraped(&conn, album, songs.iter().map(|s| s.to_string()).collect());

    // Set up a concert that is split (split_at set) with surviving track files.
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    std::fs::write(cd.join("Keep Track.m4a"), b"original-audio").unwrap();
    db::set_downloaded_at_if_missing(&conn, 1, "2024-01-01 00:00:00").unwrap();
    {
        let started = db::try_mark_split_started(&conn, 1).unwrap();
        assert!(started);
        db::mark_split_succeeded(&conn, 1).unwrap();
        db::set_tracks_present(&conn, 1, &[true]).unwrap();
    }
    // Simulate source file deleted (downloaded_at still set).
    // (The source file is not on disk — workdir has only the track file.)

    let app = router(state_with_chain(conn, workdir.path(), album, &songs));

    let (status, _) = post_json(&app, "/concerts/1/download").await;
    assert_eq!(status, StatusCode::OK);

    // Wait a little for the download to run, then confirm no split was queued.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let content = std::fs::read(cd.join("Keep Track.m4a")).unwrap();
    assert_eq!(
        content, b"original-audio",
        "track file must not be overwritten"
    );
}

/// A second POST /download while one is already running must not drop the queued
/// split edge. After the download finishes, exactly one split should run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn download_double_click_does_not_drop_split_edge() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Double Click Album";
    let songs = ["Song 1"];

    let conn = db::open_in_memory().unwrap();
    seed_scraped(&conn, album, songs.iter().map(|s| s.to_string()).collect());

    // Slow download so the second POST arrives mid-job.
    let cd = concert_dir(workdir.path(), album);
    let source = cd.join(format!("{}.mp4", album));
    let fetch = format!(
        "sleep 0.2 && mkdir -p '{}' && touch '{}'",
        cd.display(),
        source.display()
    );
    let touch = format!("touch '{}'", cd.join("Song 1.m4a").display());
    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        registry: Arc::new(JobRegistry::new()),
        scrape_queue: idle_scrape_queue(),
        jobs: JobConfig {
            working_dir: workdir.path().to_path_buf(),
            download_cmd: Arc::new(move |_| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(fetch.clone());
                cmd
            }),
            split_cmd: Arc::new(move |_| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(touch.clone());
                cmd
            }),
            open_cmd: Arc::new(|_| Command::new("true")),
        },
    };
    let app = router(state);

    let (s1, _) = post_json(&app, "/concerts/1/download").await;
    let (s2, _) = post_json(&app, "/concerts/1/download").await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);

    wait_for_split(&app, 1, 1).await;
    let (_, j) = get_json(&app, "/concerts/1/prepare-status").await;
    assert_eq!(j["split"], "split");
    assert_eq!(j["split_queued"], false);
}

/// All track files exist on disk but the source video is missing and
/// downloaded_at is NULL (manual track-file copies, no source). The Download
/// button must still start a download so the user gets the source video.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn download_force_starts_when_tracks_present_but_source_missing() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Tracks Present Album";
    let songs = ["Existing Track"];

    let conn = db::open_in_memory().unwrap();
    seed_scraped(&conn, album, songs.iter().map(|s| s.to_string()).collect());

    // Track files exist on disk but no source video.
    let cd = concert_dir(workdir.path(), album);
    std::fs::create_dir_all(&cd).unwrap();
    std::fs::write(cd.join("Existing Track.m4a"), b"audio").unwrap();

    let app = router(state_with_chain(conn, workdir.path(), album, &songs));

    let (status, _) = post_json(&app, "/concerts/1/download").await;
    assert_eq!(status, StatusCode::OK);

    // The source file should eventually exist (download ran).
    for _ in 0..100 {
        if cd.join(format!("{}.mp4", album)).exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("source file never created by forced download");
}

// ── split-timestamps API tests ────────────────────────────────────────────────

use concert_tracker::db::{
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
    db::upsert_listing(
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
    db::update_metadata(
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

#[tokio::test]
async fn get_split_timestamps_returns_404_for_unknown_id() {
    let conn = db::open_in_memory().unwrap();
    let app = router(test_state(conn));
    let (status, _) = get_json(&app, "/concerts/999/split-timestamps").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_split_timestamps_returns_null_auto_and_user_initially() {
    let conn = db::open_in_memory().unwrap();
    let songs = ["Song A", "Song B"];
    let id = seed_ts_concert(&conn, "Null Album", &songs);
    let app = router(test_state(conn));

    let (status, json) = get_json(&app, &format!("/concerts/{id}/split-timestamps")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["auto"], serde_json::Value::Null);
    assert_eq!(json["user"], serde_json::Value::Null);
    let set_list = json["set_list"].as_array().unwrap();
    assert_eq!(set_list.len(), 2);
    assert_eq!(set_list[0], "Song A");
}

#[tokio::test]
async fn get_split_timestamps_returns_seeded_auto_timestamps() {
    let conn = db::open_in_memory().unwrap();
    let songs = ["Track One", "Track Two"];
    let id = seed_ts_concert(&conn, "Auto Album", &songs);
    let ts = sample_song_timestamps(&songs);
    set_auto_split_timestamps(&conn, id, &ts).unwrap();

    let app = router(test_state(conn));
    let (status, json) = get_json(&app, &format!("/concerts/{id}/split-timestamps")).await;
    assert_eq!(status, StatusCode::OK);
    let auto_arr = json["auto"].as_array().unwrap();
    assert_eq!(auto_arr.len(), 2);
    assert_eq!(auto_arr[0]["title"], "Track One");
    assert_eq!(json["user"], serde_json::Value::Null);
}

#[tokio::test]
async fn get_split_timestamps_returns_both_auto_and_user() {
    let conn = db::open_in_memory().unwrap();
    let songs = ["Alpha", "Beta"];
    let id = seed_ts_concert(&conn, "Both Album", &songs);
    let auto_ts = sample_song_timestamps(&songs);
    set_auto_split_timestamps(&conn, id, &auto_ts).unwrap();
    let user_ts: Vec<concert_types::SongTimestamp> = songs
        .iter()
        .enumerate()
        .map(|(i, title)| concert_types::SongTimestamp {
            title: title.to_string(),
            start_time: (i * 60 + 2) as f64,
            end_time: (i * 60 + 57) as f64,
            duration: 55.0,
        })
        .collect();
    set_user_split_timestamps(&conn, id, &user_ts).unwrap();

    let app = router(test_state(conn));
    let (status, json) = get_json(&app, &format!("/concerts/{id}/split-timestamps")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["auto"].is_array());
    assert!(json["user"].is_array());
    assert_eq!(json["auto"].as_array().unwrap().len(), 2);
    assert_eq!(json["user"].as_array().unwrap().len(), 2);
    assert_eq!(json["auto"][0]["start_time"], 0.0);
    assert_eq!(json["user"][0]["start_time"], 2.0);
}

#[tokio::test]
async fn get_split_timestamps_lazy_backfill_from_timestamps_json() {
    use concert_types::ConcertInfo;

    let workdir = tempfile::tempdir().unwrap();
    let album = "Backfill Album";
    let songs = ["Old Song A", "Old Song B"];

    let conn = db::open_in_memory().unwrap();
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

#[tokio::test]
async fn get_split_timestamps_uses_stored_media_duration_when_source_missing() {
    let conn = db::open_in_memory().unwrap();
    let songs = ["Song A", "Song B"];
    let id = seed_ts_concert(&conn, "Stored Duration Album", &songs);
    db::set_media_duration(&conn, id, 321.5).unwrap();

    let app = router(test_state(conn));
    let (status, json) = get_json(&app, &format!("/concerts/{id}/split-timestamps")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["media_duration"], 321.5);
}

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

#[tokio::test]
async fn set_split_timestamps_returns_404_for_unknown_concert() {
    let conn = db::open_in_memory().unwrap();
    let app = router(test_state(conn));
    let body = serde_json::json!({"songs": []});
    let (status, _) = post_body_json(&app, "/concerts/999/split-timestamps", body).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn set_split_timestamps_returns_409_when_source_missing() {
    let conn = db::open_in_memory().unwrap();
    let songs = ["A", "B"];
    let id = seed_ts_concert(&conn, "No Source Album", &songs);
    // No source file on disk — workdir points to /tmp (no files).
    let app = router(test_state(conn));

    let body = serde_json::json!({"songs": [
        {"title": "A", "start_time": 0.0, "end_time": 55.0},
        {"title": "B", "start_time": 60.0, "end_time": 115.0}
    ]});
    let (status, text) =
        post_body_text(&app, &format!("/concerts/{id}/split-timestamps"), body).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(text, "Source file not found — download the concert first");
}

#[tokio::test]
async fn set_split_timestamps_returns_422_on_count_mismatch() {
    // Source file must exist to pass the 409 check before we reach the count check.
    let workdir = tempfile::tempdir().unwrap();
    let album = "Count Mismatch Album";
    let songs = ["A", "B"];

    let conn = db::open_in_memory().unwrap();
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

    let conn = db::open_in_memory().unwrap();
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
        jobs: JobConfig {
            working_dir: workdir.path().to_path_buf(),
            download_cmd: Arc::new(|_| Command::new("true")),
            split_cmd: Arc::new(move |_| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(touch_songs.clone());
                cmd
            }),
            open_cmd: Arc::new(|_| Command::new("true")),
        },
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

#[tokio::test]
async fn reset_split_timestamps_returns_404_for_unknown_concert() {
    let conn = db::open_in_memory().unwrap();
    let app = router(test_state(conn));
    let (status, _) = post_json(&app, "/concerts/999/split-timestamps/reset").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reset_split_timestamps_returns_422_when_no_auto_timestamps() {
    let conn = db::open_in_memory().unwrap();
    let songs = ["A", "B"];
    let id = seed_ts_concert(&conn, "No Auto Album", &songs);
    let app = router(test_state(conn));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/concerts/{id}/split-timestamps/reset"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        text,
        "No automated split timestamps available — run analysis first"
    );
}

#[tokio::test]
async fn reset_split_timestamps_returns_already_auto_when_user_is_null() {
    let conn = db::open_in_memory().unwrap();
    let songs = ["A", "B"];
    let id = seed_ts_concert(&conn, "Already Auto Album", &songs);
    let ts = sample_song_timestamps(&songs);
    // auto is set, user is NULL → already-auto
    set_auto_split_timestamps(&conn, id, &ts).unwrap();

    let app = router(test_state(conn));
    let (status, json) = get_json(&app, &format!("/concerts/{id}/split-timestamps")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["auto"].is_array());
    assert_eq!(json["user"], serde_json::Value::Null);

    let (status, json) = post_json(&app, &format!("/concerts/{id}/split-timestamps/reset")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "already-auto");
}

/// Happy path for reset: user column is non-NULL + auto available → 202 and
/// eventually user column is cleared. Skips if ffmpeg is unavailable (need
/// source file for start_split to proceed).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_split_timestamps_happy_path_returns_202_and_clears_user_column() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Reset Album";
    let songs = ["Reset A", "Reset B"];

    let conn = db::open_in_memory().unwrap();
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
        jobs: JobConfig {
            working_dir: workdir.path().to_path_buf(),
            download_cmd: Arc::new(|_| Command::new("true")),
            split_cmd: Arc::new(move |_| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(touch_songs.clone());
                cmd
            }),
            open_cmd: Arc::new(|_| Command::new("true")),
        },
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

#[tokio::test]
async fn delete_split_preserves_split_timestamp_columns() {
    // delete-split (clear_split_state) must NOT wipe the timestamp columns —
    // the invariant is that they reflect what's on disk, and delete-split does
    // not delete the track files.
    let workdir = tempfile::tempdir().unwrap();
    let album = "Preserve TS Album";
    let songs = ["Keep A", "Keep B"];

    let conn = db::open_in_memory().unwrap();
    let id = seed_ts_concert(&conn, album, &songs);

    // Put the concert in split state so delete-split accepts it.
    db::try_mark_download_started(&conn, id).unwrap();
    db::mark_download_succeeded(&conn, id, "mp4").unwrap();
    db::try_mark_split_started(&conn, id).unwrap();
    db::mark_split_succeeded(&conn, id).unwrap();

    let auto_ts = sample_song_timestamps(&songs);
    set_auto_split_timestamps(&conn, id, &auto_ts).unwrap();
    set_user_split_timestamps(&conn, id, &auto_ts).unwrap();

    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));

    let (status, _) = post_json(&app, &format!("/concerts/{id}/delete-split")).await;
    assert_eq!(status, StatusCode::OK);

    // Verify both columns survive.
    let (status, json) = get_json(&app, &format!("/concerts/{id}/split-timestamps")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        json["auto"].is_array(),
        "auto timestamps must survive delete-split"
    );
    assert!(
        json["user"].is_array(),
        "user timestamps must survive delete-split"
    );
}

// ── Playlists JSON API ───────────────────────────────────────────────────────

async fn delete_req(app: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
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

fn seed_playlist_concert(
    conn: &rusqlite::Connection,
    url: &str,
    album: &str,
    songs: &[&str],
) -> i64 {
    db::upsert_listing(
        conn,
        &NewListing {
            source_url: url.to_string(),
            title: album.to_string(),
            concert_date: None,
            teaser: None,
        },
    )
    .unwrap();
    let id = db::get_concert_by_url(conn, url).unwrap().unwrap().id;
    db::update_metadata(
        conn,
        id,
        &MetadataUpdate {
            artist: "Artist".to_string(),
            album: album.to_string(),
            description: None,
            set_list: songs.iter().map(|s| s.to_string()).collect(),
            musicians: vec![],
        },
    )
    .unwrap();
    id
}

#[tokio::test]
async fn playlist_api_crud_and_resolution() {
    let conn = db::open_in_memory().unwrap();
    let cid = seed_playlist_concert(&conn, "https://npr.org/p1", "Album One", &["t0", "t1"]);
    set_auto_split_timestamps(&conn, cid, &sample_song_timestamps(&["t0", "t1"])).unwrap();
    let app = router(test_state(conn));

    // Create a playlist.
    let (status, json) =
        post_body_json(&app, "/api/playlists", serde_json::json!({"name": "Mix"})).await;
    assert_eq!(status, StatusCode::OK);
    let pid = json["id"].as_i64().unwrap();

    // Add a track item, then a whole-concert item.
    let (s, _) = post_body_json(
        &app,
        &format!("/api/playlists/{pid}/items"),
        serde_json::json!({"type": "track", "concert_id": cid, "track_index": 0}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = post_body_json(
        &app,
        &format!("/api/playlists/{pid}/items"),
        serde_json::json!({"type": "concert", "concert_id": cid}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // Detail: 2 raw items flatten to 3 resolved tracks (t0 + [t0, t1]).
    let (s, detail) = get_json(&app, &format!("/api/playlists/{pid}")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(detail["items"].as_array().unwrap().len(), 2);
    let resolved = detail["resolved_tracks"].as_array().unwrap();
    assert_eq!(resolved.len(), 3);
    assert_eq!(resolved[0]["title"], "t0");

    // List page payload carries the summary.
    let (s, list) = get_json(&app, "/api/playlists").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["summary"]["track_count"], 3);

    // Membership of a track — response now includes item_id for sidebar remove.
    let (s, m) = get_json(&app, &format!("/api/concerts/{cid}/tracks/0/playlists")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(m.as_array().unwrap().len(), 1);
    assert_eq!(m[0]["id"].as_i64().unwrap(), pid);
    // item_id lets the sidebar call DELETE /api/playlists/{pid}/items/{item_id}.
    let track_item_id = m[0]["item_id"].as_i64().unwrap();
    assert!(track_item_id > 0);
    // Confirm the round-trip: remove via item_id, membership disappears.
    let (s, _) = delete_req(&app, &format!("/api/playlists/{pid}/items/{track_item_id}")).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, m2) = get_json(&app, &format!("/api/concerts/{cid}/tracks/0/playlists")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(m2.as_array().unwrap().is_empty(), "removed from playlist");
    // Re-add so the rest of the test (reorder / delete) still has an item.
    let (s, _) = post_body_json(
        &app,
        &format!("/api/playlists/{pid}/items"),
        serde_json::json!({"type": "track", "concert_id": cid, "track_index": 0}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // Re-fetch detail after the remove/re-add above so item ids are current.
    let (_, detail) = get_json(&app, &format!("/api/playlists/{pid}")).await;

    // Reorder the two items (reverse), then remove the first.
    let ids: Vec<i64> = detail["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_i64().unwrap())
        .collect();
    let reversed: Vec<i64> = ids.iter().rev().copied().collect();
    let (s, _) = post_body_json(
        &app,
        &format!("/api/playlists/{pid}/items/reorder"),
        serde_json::json!({ "item_ids": reversed }),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let (s, _) = delete_req(&app, &format!("/api/playlists/{pid}/items/{}", ids[0])).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, detail2) = get_json(&app, &format!("/api/playlists/{pid}")).await;
    assert_eq!(detail2["items"].as_array().unwrap().len(), 1);

    // Delete the playlist; it then 404s.
    let (s, _) = delete_req(&app, &format!("/api/playlists/{pid}")).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = get_json(&app, &format!("/api/playlists/{pid}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn playlist_api_validation_status_codes() {
    let conn = db::open_in_memory().unwrap();
    let cid = seed_playlist_concert(&conn, "https://npr.org/p1", "Album One", &["t0"]);
    let app = router(test_state(conn));

    // Empty name → 422.
    let (s, _) = post_body_json(&app, "/api/playlists", serde_json::json!({"name": "   "})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    let (_, j) = post_body_json(&app, "/api/playlists", serde_json::json!({"name": "P"})).await;
    let pid = j["id"].as_i64().unwrap();

    // Out-of-range track index → 422.
    let (s, _) = post_body_json(
        &app,
        &format!("/api/playlists/{pid}/items"),
        serde_json::json!({"type": "track", "concert_id": cid, "track_index": 9}),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // Cycle: P2 nests P, then P nesting P2 → 422.
    let (_, j2) = post_body_json(&app, "/api/playlists", serde_json::json!({"name": "P2"})).await;
    let p2 = j2["id"].as_i64().unwrap();
    let (s, _) = post_body_json(
        &app,
        &format!("/api/playlists/{p2}/items"),
        serde_json::json!({"type": "playlist", "child_playlist_id": pid}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = post_body_json(
        &app,
        &format!("/api/playlists/{pid}/items"),
        serde_json::json!({"type": "playlist", "child_playlist_id": p2}),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // Unknown playlist → 404.
    let (s, _) = get_json(&app, "/api/playlists/4242").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// Fetch an HTML page, returning its status and body text.
async fn get_html(app: &axum::Router, uri: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn playlists_html_pages_render() {
    let conn = db::open_in_memory().unwrap();
    let cid = seed_playlist_concert(
        &conn,
        "https://npr.org/h1",
        "Album H",
        &["Song Zero", "Song One"],
    );
    set_auto_split_timestamps(
        &conn,
        cid,
        &sample_song_timestamps(&["Song Zero", "Song One"]),
    )
    .unwrap();
    let app = router(test_state(conn));

    // Create a playlist and add a track + the whole concert.
    let (_, j) = post_body_json(
        &app,
        "/api/playlists",
        serde_json::json!({"name": "My Mix"}),
    )
    .await;
    let pid = j["id"].as_i64().unwrap();
    let (s, _) = post_body_json(
        &app,
        &format!("/api/playlists/{pid}/items"),
        serde_json::json!({"type": "track", "concert_id": cid, "track_index": 1}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = post_body_json(
        &app,
        &format!("/api/playlists/{pid}/items"),
        serde_json::json!({"type": "concert", "concert_id": cid}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // List page shows the playlist name and links to its detail page.
    let (s, list) = get_html(&app, "/playlists").await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        list.contains("My Mix"),
        "list must name the playlist: {list}"
    );
    assert!(
        list.contains(&format!("/playlists/{pid}")),
        "list must link to the detail page"
    );
    // Nav reshuffle: Playlists link present.
    assert!(
        list.contains("href=\"/playlists\""),
        "nav must link Playlists"
    );

    // Detail page shows the playlist name and a known track title.
    let (s, detail) = get_html(&app, &format!("/playlists/{pid}")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(detail.contains("My Mix"), "detail must name the playlist");
    assert!(
        detail.contains("Song One"),
        "detail must list a track title: {detail}"
    );
    assert!(
        detail.contains(&format!("data-playlist-id=\"{pid}\"")),
        "detail must carry the playlist id for the JS"
    );
}

#[tokio::test]
async fn playlist_detail_page_unknown_id_is_404() {
    let conn = db::open_in_memory().unwrap();
    let app = router(test_state(conn));
    let (s, _) = get_html(&app, "/playlists/999").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

// ── concert_playback endpoint tests ──────────────────────────────────────────

fn seed_split_concert_with_files(
    album: &str,
    songs: &[&str],
    workdir: &std::path::Path,
) -> rusqlite::Connection {
    let conn = db::open_in_memory().unwrap();
    db::upsert_listing(
        &conn,
        &NewListing {
            source_url: "https://npr.org/d/recon".to_string(),
            title: "Recon Concert".to_string(),
            concert_date: Some("2024-03-01".to_string()),
            teaser: None,
        },
    )
    .unwrap();
    db::update_metadata(
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
    db::try_mark_download_started(&conn, 1).unwrap();
    db::mark_download_succeeded(&conn, 1, "mp4").unwrap();
    db::try_mark_split_started(&conn, 1).unwrap();
    db::mark_split_succeeded(&conn, 1).unwrap();
    db::set_tracks_present(&conn, 1, &vec![true; songs.len()]).unwrap();

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
    db::set_media_duration(&conn, 1, 120.0).unwrap();

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

#[tokio::test]
async fn concert_playback_returns_404_when_nothing_playable() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Empty Album";
    let conn = db::open_in_memory().unwrap();
    db::upsert_listing(
        &conn,
        &NewListing {
            source_url: "https://npr.org/d/empty".to_string(),
            title: "Empty Concert".to_string(),
            concert_date: None,
            teaser: None,
        },
    )
    .unwrap();
    db::update_metadata(
        &conn,
        1,
        &MetadataUpdate {
            artist: "X".to_string(),
            album: album.to_string(),
            description: None,
            set_list: vec!["Song A".to_string()],
            musicians: vec![],
        },
    )
    .unwrap();
    db::try_mark_download_started(&conn, 1).unwrap();
    db::mark_download_succeeded(&conn, 1, "mp4").unwrap();
    db::try_mark_split_started(&conn, 1).unwrap();
    db::mark_split_succeeded(&conn, 1).unwrap();
    db::set_tracks_present(&conn, 1, &[false]).unwrap();
    std::fs::create_dir_all(concert_dir(workdir.path(), album)).unwrap();

    let app = router(state_with_workdir(conn, workdir.path().to_path_buf()));
    let (status, _) = get_json(&app, "/concerts/1/concert-playback").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

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
    db::set_media_duration(&conn, 1, 120.0).unwrap();
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
    let conn = db::open_in_memory().unwrap();
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
    let conn = db::open_in_memory().unwrap();
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
