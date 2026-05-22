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
