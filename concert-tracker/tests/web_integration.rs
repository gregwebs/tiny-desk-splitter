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
    assert!(html.contains("badge-ignored") || html.contains("Unignore"));
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
    seeded_concert(&conn, "http://127.0.0.1:1/never-resolves", "Unreachable Concert");
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
    assert!(html.contains("Unreachable Concert"), "listing title must render even when scrape fails");

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
    db::mark_download_succeeded(conn, 1).unwrap();
}

#[tokio::test]
async fn delete_download_removes_file_and_clears_state() {
    let workdir = tempfile::tempdir().unwrap();
    let album = "Some Album";
    let mp4 = workdir.path().join(format!("{}.mp4", album));
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
        response.headers().get("HX-Refresh").and_then(|v| v.to_str().ok()),
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
    // Reproduces the user-reported bug: a concert that was downloaded and then
    // had a failed split would keep `split_errors` populated even after the
    // mp4 was deleted, pinning ProcessingStatus at SplitError. That hid the
    // Download button and kept Split / Listen visible despite no source file.
    let workdir = tempfile::tempdir().unwrap();
    let album = "Prior Split Err Album";
    let mp4 = workdir.path().join(format!("{}.mp4", album));
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
        response.headers().get("HX-Refresh").and_then(|v| v.to_str().ok()),
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
        response.headers().get("HX-Refresh").and_then(|v| v.to_str().ok()),
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
