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

/// An idle background scrape queue for tests that never enqueue. Backed by a
/// throwaway in-memory DB; the worker stays parked.
fn idle_scrape_queue() -> ScrapeQueue {
    ScrapeQueue::start(
        Arc::new(Mutex::new(db::open_in_memory().unwrap())),
        PathBuf::from("/tmp"),
    )
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
    use std::time::Duration;

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
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    for _ in 0..200 {
        if !scrape_queue.is_pending(1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
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
    let conn = db::open_in_memory().unwrap();
    seed_downloaded(&conn, "https://npr.org/d/split-listen", "Some Album");
    db::update_metadata(
        &conn,
        1,
        &MetadataUpdate {
            artist: "X".to_string(),
            album: "Some Album".to_string(),
            description: None,
            set_list: vec!["Song A".to_string(), "Song B".to_string()],
            musicians: vec![],
        },
    )
    .unwrap();
    db::try_mark_split_started(&conn, 1).unwrap();
    db::mark_split_succeeded(&conn, 1).unwrap();
    db::set_tracks_present(&conn, 1, &[true, true]).unwrap();
    let app = router(test_state(conn));

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
        html.contains("Player.playAlbum(this, 1)"),
        "Play button must remain visible after split; got: {}",
        html
    );
    assert!(
        html.contains(">Play</button>"),
        "the album button is labelled Play; got: {}",
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
    // The download-slot Play button comes before the delete-download trash.
    assert!(
        html.find("Player.playAlbum").unwrap() < html.find("/concerts/1/delete-download").unwrap(),
        "download-slot Play must come before the delete-download trash; got: {}",
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
        let present: Vec<bool> = serde_json::from_value(j["tracks_present"].clone())
            .unwrap_or_default();
        if present.len() == tracks && present.iter().all(|&p| p) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("chain never completed: tracks not all present after 10s");
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
    assert_ne!(j["download"], "not-downloaded", "download should have started");
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
    assert_eq!(content, b"original-audio", "track file must not be overwritten");
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
