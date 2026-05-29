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
    jobs::{JobConfig, JobRegistry},
    model::{concert_dir, sanitize_filename},
    web::{router, AppState},
};

fn test_state(conn: rusqlite::Connection) -> AppState {
    AppState {
        db: Arc::new(Mutex::new(conn)),
        registry: Arc::new(JobRegistry::new()),
        jobs: JobConfig {
            working_dir: PathBuf::from("/tmp"),
            download_cmd: Arc::new(|_| Command::new("true")),
            split_cmd: Arc::new(|_| Command::new("true")),
        },
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
        jobs: JobConfig {
            working_dir: PathBuf::from("/tmp"),
            download_cmd: Arc::new(|_| Command::new("true")),
            split_cmd: Arc::new(|_| Command::new("true")),
        },
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
        jobs: JobConfig {
            working_dir: workdir,
            download_cmd: Arc::new(|_| Command::new("true")),
            split_cmd: Arc::new(|_| Command::new("true")),
        },
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
        jobs: JobConfig {
            working_dir: workdir.path().to_path_buf(),
            download_cmd: Arc::new(|_| Command::new("true")),
            split_cmd: Arc::new(|_| Command::new("true")),
        },
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
    assert_eq!(
        response
            .headers()
            .get("HX-Refresh")
            .and_then(|v| v.to_str().ok()),
        Some("true"),
        "successful delete must trigger a full htmx refresh"
    );
    assert!(!mp4.exists(), "mp4 file must be removed from disk");

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
        jobs: JobConfig {
            working_dir: workdir.path().to_path_buf(),
            download_cmd: Arc::new(|_| Command::new("true")),
            split_cmd: Arc::new(|_| Command::new("true")),
        },
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
async fn listen_button_visible_after_successful_split() {
    // Once tracks have been split, the source mp4 is still on disk and still
    // playable. With the old combined ProcessingStatus, `Split` shadowed
    // `Downloaded` and the Listen button disappeared. With the new
    // DownloadStatus / SplitStatus split, Listen is gated only on
    // DownloadStatus == Downloaded.
    let conn = db::open_in_memory().unwrap();
    seed_downloaded(&conn, "https://npr.org/d/split-listen", "Some Album");
    db::try_mark_split_started(&conn, 1).unwrap();
    db::mark_split_succeeded(&conn, 1).unwrap();
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
        "Listen button must remain visible after split; got: {}",
        html
    );
    // Split action button should be gone (already split), but delete-split X
    // should be present.
    assert!(
        !html.contains("/concerts/1/split\""),
        "Split action button must NOT appear once already split; got: {}",
        html
    );
    assert!(
        html.contains("/concerts/1/delete-split"),
        "Delete-tracks button must appear after split; got: {}",
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
        jobs: JobConfig {
            working_dir: workdir.path().to_path_buf(),
            download_cmd: Arc::new(|_| Command::new("true")),
            split_cmd: Arc::new(|_| Command::new("true")),
        },
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
    assert_eq!(
        response
            .headers()
            .get("HX-Refresh")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );
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
        jobs: JobConfig {
            working_dir: PathBuf::from("/tmp"),
            download_cmd: Arc::new(|_| Command::new("true")),
            split_cmd: Arc::new(|_| Command::new("true")),
        },
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
    assert_eq!(
        response
            .headers()
            .get("HX-Refresh")
            .and_then(|v| v.to_str().ok()),
        Some("true")
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
        jobs: JobConfig {
            working_dir: workdir.path().to_path_buf(),
            download_cmd: Arc::new(|_| Command::new("true")),
            split_cmd: Arc::new(|_| Command::new("true")),
        },
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
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("btn-like liked"), "filled star class expected");
    assert!(html.contains("★"), "filled star glyph expected");
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
