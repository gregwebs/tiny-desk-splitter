use std::collections::HashMap;

use askama::Template;
use askama_axum::IntoResponse;
use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use rusqlite::Connection;

use crate::db;
use crate::jobs::download::start_download;
use crate::jobs::split::start_split;
use crate::model::{Concert, ProcessingStatus};
use crate::sync::{sync_month, YearMonth};
use crate::web::AppState;

// ── Templates ────────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "list.html")]
struct ListTemplate {
    rows: Vec<String>,
    /// (slug, label, active_class) — active_class is "active" or ""
    filters: Vec<(String, String, String)>,
}

#[derive(Template)]
#[template(path = "row.html")]
struct RowTemplate {
    id: i64,
    title: String,
    artist: String,
    concert_date: String,
    teaser: String,
    concert_status: String,
    processing_status: String,
    ignored: bool,
    wanted: bool,
    can_download: bool,
    can_split: bool,
    is_in_progress: bool,
}

#[derive(Template)]
#[template(path = "detail.html")]
struct DetailTemplate {
    concert: Concert,
    concert_status: String,
    processing_status: String,
    can_download: bool,
    can_split: bool,
    notes_value: String,
}

// ── Error type ───────────────────────────────────────────────────────────────

pub enum AppError {
    NotFound,
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        AppError::Internal(e.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not found").into_response(),
            AppError::Internal(e) => {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
            }
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

const FILTERS: &[(&str, &str)] = &[
    ("all", "All"),
    ("wanted", "Wanted"),
    ("available", "Available"),
    ("ignored", "Ignored"),
    ("downloaded", "Downloaded"),
    ("split", "Split"),
];

fn matches_filter(c: &Concert, slug: &str) -> bool {
    match slug {
        "wanted" => !c.ignored && c.wanted,
        "ignored" => c.ignored,
        "available" => !c.ignored && !c.wanted,
        "downloaded" => matches!(c.processing_status(), ProcessingStatus::Downloaded),
        "split" => matches!(c.processing_status(), ProcessingStatus::Split),
        _ => true,
    }
}

/// If `concert` has not yet been fully scraped, run `fetch_and_apply` to fetch
/// the per-concert page and write metadata, then reload the row. Failures are
/// logged and tolerated — the original `concert` is returned and the page
/// renders with listing-only data.
fn ensure_scraped<F>(conn: &Connection, concert: Concert, fetch_and_apply: F) -> Concert
where
    F: FnOnce(&Connection, &str) -> anyhow::Result<()>,
{
    if concert.metadata_scraped_at.is_some() {
        return concert;
    }
    match fetch_and_apply(conn, &concert.source_url) {
        Ok(()) => db::get_concert(conn, concert.id).unwrap_or(concert),
        Err(e) => {
            tracing::warn!("auto-scrape failed for concert {}: {}", concert.id, e);
            concert
        }
    }
}

fn render_row(c: &Concert) -> Result<String, askama::Error> {
    let ps = c.processing_status();
    let can_download = matches!(
        &ps,
        ProcessingStatus::NotStarted | ProcessingStatus::DownloadError
    ) && c.album.is_some();
    let can_split = matches!(&ps, ProcessingStatus::Downloaded | ProcessingStatus::SplitError);
    let is_in_progress = matches!(
        &ps,
        ProcessingStatus::Downloading | ProcessingStatus::Splitting
    );

    RowTemplate {
        id: c.id,
        title: c.title.clone(),
        artist: c.artist.clone().unwrap_or_default(),
        concert_date: c.concert_date.clone().unwrap_or_default(),
        teaser: c.teaser.clone().unwrap_or_default(),
        concert_status: c.concert_status().slug().to_string(),
        processing_status: ps.slug().to_string(),
        ignored: c.ignored,
        wanted: c.wanted,
        can_download,
        can_split,
        is_in_progress,
    }
    .render()
}

// ── Handlers ─────────────────────────────────────────────────────────────────

pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, AppError> {
    let filter = params
        .get("filter")
        .map(|s| s.as_str())
        .unwrap_or("all")
        .to_string();

    let concerts = {
        let conn = state.db.lock().unwrap();
        db::list_concerts(&conn)?
    };

    let rows: Vec<String> = concerts
        .iter()
        .filter(|c| matches_filter(c, &filter))
        .map(render_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;

    Ok(ListTemplate {
        rows,
        filters: FILTERS
            .iter()
            .map(|(s, l)| {
                let active = if *s == filter { "active" } else { "" };
                (s.to_string(), l.to_string(), active.to_string())
            })
            .collect(),
    })
}

pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let initial = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?
    };

    // Auto-scrape on first view. The scrape itself is blocking (reqwest::blocking
    // can't run inside the tokio runtime), so wrap the whole step in spawn_blocking.
    let concert = if initial.metadata_scraped_at.is_none() {
        let db = state.db.clone();
        let initial_for_task = initial.clone();
        match tokio::task::spawn_blocking(move || {
            let conn = db.lock().unwrap();
            ensure_scraped(&conn, initial_for_task, crate::scrape::scrape_url)
        })
        .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("auto-scrape task join failed for concert {}: {}", id, e);
                initial
            }
        }
    } else {
        initial
    };

    let ps = concert.processing_status();
    let can_download = matches!(
        &ps,
        ProcessingStatus::NotStarted | ProcessingStatus::DownloadError
    ) && concert.album.is_some();
    let can_split = matches!(&ps, ProcessingStatus::Downloaded | ProcessingStatus::SplitError);
    let notes_value = concert.notes.clone().unwrap_or_default();

    Ok(DetailTemplate {
        concert_status: concert.concert_status().slug().to_string(),
        processing_status: ps.slug().to_string(),
        can_download,
        can_split,
        notes_value,
        concert,
    })
}

pub async fn ignore(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let concert = {
        let conn = state.db.lock().unwrap();
        db::toggle_ignored(&conn, id)?;
        db::get_concert(&conn, id)?
    };
    render_row(&concert).map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

pub async fn want(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let concert = {
        let conn = state.db.lock().unwrap();
        db::toggle_wanted(&conn, id)?;
        db::get_concert(&conn, id)?
    };
    render_row(&concert).map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

pub async fn notes(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<impl IntoResponse, AppError> {
    let text = form.get("notes").map(|s| s.as_str()).unwrap_or("");
    let concert = {
        let conn = state.db.lock().unwrap();
        db::set_notes(&conn, id, text)?;
        db::get_concert(&conn, id)?
    };
    render_row(&concert).map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

pub async fn scrape_concert(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let url = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id)
            .map_err(|_| AppError::NotFound)?
            .source_url
    };

    // reqwest::blocking cannot run inside a tokio runtime; offload to a blocking thread.
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.lock().unwrap();
        crate::scrape::scrape_url(&conn, &url)
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("task join: {}", e)))??;

    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id)?
    };
    render_row(&concert).map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

pub async fn download(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    start_download(state.db.clone(), state.registry.clone(), state.jobs.clone(), id).await?;
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id)?
    };
    render_row(&concert).map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

pub async fn split(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    start_split(state.db.clone(), state.registry.clone(), state.jobs.clone(), id).await?;
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id)?
    };
    render_row(&concert).map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

pub async fn status_row(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?
    };
    render_row(&concert).map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

pub async fn sync_now(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let ym = YearMonth::current();
    let (year, month) = (ym.year, ym.month);
    tracing::info!("sync started for {}/{:02}", year, month);

    // reqwest::blocking cannot run inside a tokio runtime; offload to a blocking thread.
    let db = state.db.clone();
    let count = tokio::task::spawn_blocking(move || {
        let conn = db.lock().unwrap();
        sync_month(&conn, &ym)
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("task join: {}", e)))??;

    tracing::info!("sync completed: {} concerts for {}/{:02}", count, year, month);

    // Tell htmx to reload the page so the new concerts appear in the list.
    let mut headers = HeaderMap::new();
    headers.insert("HX-Refresh", "true".parse().unwrap());
    Ok((
        headers,
        format!("Synced {} concerts for {}/{:02}", count, year, month),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{self, MetadataUpdate, NewListing};
    use crate::model::Musician;
    use std::cell::Cell;

    fn seed_listing(conn: &Connection, url: &str) -> i64 {
        db::upsert_listing(
            conn,
            &NewListing {
                source_url: url.to_string(),
                title: "Test Concert".to_string(),
                concert_date: Some("2026-05-20".to_string()),
                teaser: Some("a teaser".to_string()),
            },
        )
        .unwrap();
        conn.query_row(
            "SELECT id FROM concerts WHERE source_url = ?1",
            [url],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    }

    #[test]
    fn ensure_scraped_skips_when_already_scraped() {
        let conn = db::open_in_memory().unwrap();
        let url = "https://example.org/already";
        let id = seed_listing(&conn, url);
        db::update_metadata(
            &conn,
            id,
            &MetadataUpdate {
                artist: "Existing".to_string(),
                album: "Existing Album".to_string(),
                description: Some("desc".to_string()),
                set_list: vec!["Song A".to_string()],
                musicians: vec![Musician {
                    name: "Player".to_string(),
                    instruments: vec!["guitar".to_string()],
                }],
            },
        )
        .unwrap();
        let concert = db::get_concert(&conn, id).unwrap();
        assert!(concert.metadata_scraped_at.is_some());

        let called = Cell::new(false);
        let result = ensure_scraped(&conn, concert, |_conn, _url| {
            called.set(true);
            Ok(())
        });

        assert!(!called.get(), "scrape closure must not be called when already scraped");
        assert_eq!(result.artist.as_deref(), Some("Existing"));
        assert_eq!(result.set_list, vec!["Song A".to_string()]);
    }

    #[test]
    fn ensure_scraped_runs_closure_when_missing_and_merges_result() {
        let conn = db::open_in_memory().unwrap();
        let url = "https://example.org/fresh";
        let id = seed_listing(&conn, url);
        let concert = db::get_concert(&conn, id).unwrap();
        assert!(concert.metadata_scraped_at.is_none());
        assert!(concert.set_list.is_empty());

        let called = Cell::new(false);
        let result = ensure_scraped(&conn, concert, |conn, source_url| {
            called.set(true);
            assert_eq!(source_url, url);
            db::update_metadata(
                conn,
                id,
                &MetadataUpdate {
                    artist: "Fetched".to_string(),
                    album: "Fetched Album".to_string(),
                    description: None,
                    set_list: vec!["Song 1".to_string(), "Song 2".to_string()],
                    musicians: vec![],
                },
            )
        });

        assert!(called.get(), "scrape closure must run when metadata is missing");
        assert_eq!(result.artist.as_deref(), Some("Fetched"));
        assert_eq!(result.set_list, vec!["Song 1".to_string(), "Song 2".to_string()]);
        assert!(result.metadata_scraped_at.is_some());
    }

    #[test]
    fn ensure_scraped_tolerates_failure_and_returns_listing_only() {
        let conn = db::open_in_memory().unwrap();
        let url = "https://example.org/broken";
        let id = seed_listing(&conn, url);
        let concert = db::get_concert(&conn, id).unwrap();

        let result = ensure_scraped(&conn, concert, |_conn, _url| {
            Err(anyhow::anyhow!("simulated network failure"))
        });

        assert_eq!(result.title, "Test Concert");
        assert_eq!(result.teaser.as_deref(), Some("a teaser"));
        assert!(result.artist.is_none());
        assert!(result.set_list.is_empty());
        assert!(
            result.metadata_scraped_at.is_none(),
            "metadata_scraped_at must remain NULL after a failed scrape so the next view retries"
        );

        let reread = db::get_concert(&conn, id).unwrap();
        assert!(reread.metadata_scraped_at.is_none());
    }
}
