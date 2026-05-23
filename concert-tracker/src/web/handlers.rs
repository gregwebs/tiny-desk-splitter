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
use crate::jobs::find_downloaded_file;
use crate::jobs::split::start_split;
use crate::model::{Concert, DownloadStatus, SplitStatus, TrackInfo};
use crate::sync::{sync_month, YearMonth};
use crate::web::AppState;

// ── Templates ────────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "list.html")]
struct ListTemplate {
    rows: Vec<String>,
    /// (href, label, active_class)
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
    download_status: String,
    download_status_label: String,
    split_status: String,
    split_status_label: String,
    card_accent: &'static str,
    /// Concert-status slot: in Available the slot shows the Ignore/Want
    /// buttons; otherwise it shows the wanted/ignored badge + ✕.
    is_available: bool,
    ignored: bool,
    wanted: bool,
    can_download: bool,
    can_delete_download: bool,
    can_split: bool,
    can_delete_split: bool,
    can_listen: bool,
    /// Whether to show the download badge alongside slot contents.
    /// False only for the NotDownloaded "fresh" state.
    show_download_badge: bool,
    show_split_badge: bool,
    is_in_progress: bool,
}

#[derive(Template)]
#[template(path = "detail.html")]
struct DetailTemplate {
    concert: Concert,
    concert_status: String,
    download_status: String,
    download_status_label: String,
    split_status: String,
    split_status_label: String,
    can_download: bool,
    can_delete_download: bool,
    can_split: bool,
    can_delete_split: bool,
    can_listen: bool,
    notes_value: String,
    preview_url: Option<String>,
    tracks: Vec<TrackInfo>,
}

#[derive(Template)]
#[template(path = "listen_button.html")]
struct ListenButtonTemplate {
    id: i64,
    state: &'static str,
}

#[derive(Template)]
#[template(path = "tracks.html")]
struct TracksTemplate {
    id: i64,
    tracks: Vec<TrackInfo>,
}

#[derive(Template)]
#[template(path = "track_listen_button.html")]
struct TrackListenButtonTemplate {
    id: i64,
    index: usize,
    title: String,
    state: &'static str,
}

#[derive(Template)]
#[template(path = "delete_confirm.html")]
struct DeleteConfirmTemplate {
    id: i64,
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
    ("wanted", "Wanted"),
    ("available", "Available"),
    ("ignored", "Ignored"),
    ("downloaded", "Downloaded"),
    ("split", "Tracks"),
];

fn matches_filter(c: &Concert, slug: &str) -> bool {
    match slug {
        "wanted" => !c.ignored && c.wanted,
        "ignored" => c.ignored,
        "available" => !c.ignored && !c.wanted,
        "downloaded" => matches!(c.download_status(), DownloadStatus::Downloaded),
        "split" => matches!(c.split_status(), SplitStatus::Split),
        _ => !c.ignored,
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
    tracing::info!(
        "auto-scrape started for concert {} ({})",
        concert.id,
        concert.title
    );
    match fetch_and_apply(conn, &concert.source_url) {
        Ok(()) => {
            tracing::info!("auto-scrape completed for concert {}", concert.id);
            db::get_concert(conn, concert.id).unwrap_or(concert)
        }
        Err(e) => {
            tracing::warn!("auto-scrape failed for concert {}: {}", concert.id, e);
            concert
        }
    }
}

fn render_row(c: &Concert) -> Result<String, askama::Error> {
    let ds = c.download_status();
    let ss = c.split_status();
    let can_download = matches!(
        &ds,
        DownloadStatus::NotDownloaded | DownloadStatus::DownloadError
    ) && c.album.is_some();
    let can_delete_download = matches!(&ds, DownloadStatus::Downloaded);
    let can_split = matches!(&ds, DownloadStatus::Downloaded)
        && matches!(&ss, SplitStatus::NotSplit | SplitStatus::SplitError);
    let can_delete_split = matches!(&ss, SplitStatus::Split);
    let can_listen = matches!(&ds, DownloadStatus::Downloaded);
    let show_download_badge = !matches!(&ds, DownloadStatus::NotDownloaded);
    let show_split_badge = !matches!(&ss, SplitStatus::NotSplit);
    let is_in_progress =
        matches!(&ds, DownloadStatus::Downloading) || matches!(&ss, SplitStatus::Splitting);
    let card_accent = if matches!(&ss, SplitStatus::Split) {
        "split"
    } else if matches!(&ds, DownloadStatus::Downloaded) {
        "downloaded"
    } else {
        ""
    };
    let is_available = !c.ignored && !c.wanted;

    RowTemplate {
        id: c.id,
        title: c.title.clone(),
        artist: c.artist.clone().unwrap_or_default(),
        concert_date: c.display_date().unwrap_or_default(),
        teaser: c.teaser.clone().unwrap_or_default(),
        download_status: ds.slug().to_string(),
        download_status_label: ds.label().to_string(),
        split_status: ss.slug().to_string(),
        split_status_label: ss.label().to_string(),
        card_accent,
        is_available,
        ignored: c.ignored,
        wanted: c.wanted,
        can_download,
        can_delete_download,
        can_split,
        can_delete_split,
        can_listen,
        show_download_badge,
        show_split_badge,
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
        .unwrap_or("")
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
                let active = *s == filter;
                let href = if active {
                    "/".to_string()
                } else {
                    format!("/?filter={s}")
                };
                let active_class = if active { "active" } else { "" };
                (href, l.to_string(), active_class.to_string())
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
        let working_dir = state.jobs.working_dir.clone();
        match tokio::task::spawn_blocking(move || {
            let conn = db.lock().unwrap();
            ensure_scraped(&conn, initial_for_task, |c, u| {
                crate::scrape::scrape_url(c, u, &working_dir)
            })
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

    let ds = concert.download_status();
    let ss = concert.split_status();
    let can_download = matches!(
        &ds,
        DownloadStatus::NotDownloaded | DownloadStatus::DownloadError
    ) && concert.album.is_some();
    let can_delete_download = matches!(&ds, DownloadStatus::Downloaded);
    let can_split = matches!(&ds, DownloadStatus::Downloaded)
        && matches!(&ss, SplitStatus::NotSplit | SplitStatus::SplitError);
    let can_delete_split = matches!(&ss, SplitStatus::Split);
    let can_listen = matches!(&ds, DownloadStatus::Downloaded);
    let notes_value = concert.notes.clone().unwrap_or_default();
    let preview_url = concert.preview_image_url(&state.jobs.working_dir);
    let tracks = if matches!(&ss, SplitStatus::Split) {
        concert
            .album
            .as_deref()
            .map(|a| crate::model::list_tracks(&state.jobs.working_dir, a, &concert.set_list))
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    Ok(DetailTemplate {
        concert_status: concert.concert_status().slug().to_string(),
        download_status: ds.slug().to_string(),
        download_status_label: ds.label().to_string(),
        split_status: ss.slug().to_string(),
        split_status_label: ss.label().to_string(),
        can_download,
        can_delete_download,
        can_split,
        can_delete_split,
        can_listen,
        notes_value,
        preview_url,
        tracks,
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
    let (url, title) = {
        let conn = state.db.lock().unwrap();
        let c = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        (c.source_url, c.title)
    };

    tracing::info!("re-scrape started for concert {} ({})", id, title);

    // reqwest::blocking cannot run inside a tokio runtime; offload to a blocking thread.
    let db = state.db.clone();
    let working_dir = state.jobs.working_dir.clone();
    let scrape_result = tokio::task::spawn_blocking(move || {
        let conn = db.lock().unwrap();
        crate::scrape::scrape_url(&conn, &url, &working_dir)
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("task join: {}", e)))?;

    match &scrape_result {
        Ok(()) => tracing::info!("re-scrape completed for concert {}", id),
        Err(e) => tracing::warn!("re-scrape failed for concert {}: {}", id, e),
    }
    scrape_result?;

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
    start_download(
        state.db.clone(),
        state.registry.clone(),
        state.jobs.clone(),
        id,
    )
    .await?;
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
    start_split(
        state.db.clone(),
        state.registry.clone(),
        state.jobs.clone(),
        id,
    )
    .await?;
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id)?
    };
    render_row(&concert).map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

pub async fn delete_download(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let force = params.get("force").map(|v| v == "true").unwrap_or(false);

    let (downloaded_at, album, title) = {
        let conn = state.db.lock().unwrap();
        let c = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        (c.downloaded_at, c.album, c.title)
    };

    if downloaded_at.is_none() {
        return Ok((
            StatusCode::BAD_REQUEST,
            "Concert has no downloaded file to delete",
        )
            .into_response());
    }

    tracing::info!(
        "delete-download started for concert {} ({}) force={}",
        id,
        title,
        force
    );

    if !force {
        let path = album
            .as_deref()
            .and_then(|a| find_downloaded_file(&state.jobs.working_dir, a));
        match path {
            Some(p) => {
                if let Err(e) = std::fs::remove_file(&p) {
                    tracing::warn!(
                        "delete-download failed to remove {} for concert {}: {}",
                        p.display(),
                        id,
                        e
                    );
                    return Err(AppError::Internal(anyhow::anyhow!(
                        "Failed to remove {}: {}",
                        p.display(),
                        e
                    )));
                }
                tracing::info!("delete-download removed {} for concert {}", p.display(), id);
            }
            None => {
                tracing::info!(
                    "delete-download file not found for concert {}, returning confirm prompt",
                    id
                );
                let body = DeleteConfirmTemplate { id }
                    .render()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;
                return Ok(([("content-type", "text/html; charset=utf-8")], body).into_response());
            }
        }
    } else {
        tracing::info!(
            "delete-download force=true for concert {}, skipping file check",
            id
        );
    }

    {
        let conn = state.db.lock().unwrap();
        db::clear_download_state(&conn, id)?;
    }
    tracing::info!("delete-download completed for concert {}", id);

    let mut headers = HeaderMap::new();
    headers.insert("HX-Refresh", "true".parse().unwrap());
    Ok((headers, "").into_response())
}

pub async fn delete_split(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let (split_at, title) = {
        let conn = state.db.lock().unwrap();
        let c = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        (c.split_at, c.title)
    };

    if split_at.is_none() {
        return Ok((
            StatusCode::BAD_REQUEST,
            "Concert has no split state to delete",
        )
            .into_response());
    }

    tracing::info!("delete-split started for concert {} ({})", id, title);
    {
        let conn = state.db.lock().unwrap();
        db::clear_split_state(&conn, id)?;
    }
    tracing::info!("delete-split completed for concert {}", id);

    let mut headers = HeaderMap::new();
    headers.insert("HX-Refresh", "true".parse().unwrap());
    Ok((headers, "").into_response())
}

pub async fn listen(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let (album, working_dir) = {
        let conn = state.db.lock().unwrap();
        let concert = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        (concert.album, state.jobs.working_dir.clone())
    };

    let render_state = match album
        .as_deref()
        .and_then(|a| find_downloaded_file(&working_dir, a))
    {
        None => {
            tracing::warn!("listen: file not found for concert {}", id);
            "error"
        }
        Some(path) => {
            tracing::info!("listen: opening {} for concert {}", path.display(), id);
            match tokio::process::Command::new("open")
                .arg(&path)
                .status()
                .await
            {
                Ok(status) if status.success() => "success",
                Ok(status) => {
                    tracing::warn!(
                        "listen: `open` exited {:?} for concert {}",
                        status.code(),
                        id
                    );
                    "error"
                }
                Err(e) => {
                    tracing::warn!("listen: spawn `open` failed for concert {}: {}", id, e);
                    "error"
                }
            }
        }
    };

    ListenButtonTemplate {
        id,
        state: render_state,
    }
    .render()
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

pub async fn tracks(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?
    };

    let tracks = concert
        .album
        .as_deref()
        .map(|a| crate::model::list_tracks(&state.jobs.working_dir, a, &concert.set_list))
        .unwrap_or_default();

    TracksTemplate { id, tracks }
        .render()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

pub async fn listen_track(
    State(state): State<AppState>,
    Path((id, idx)): Path<(i64, usize)>,
) -> Result<impl IntoResponse, AppError> {
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?
    };

    let title = concert
        .set_list
        .get(idx)
        .ok_or(AppError::NotFound)?
        .clone();
    let album = concert.album.as_deref().ok_or(AppError::NotFound)?;
    let stem = crate::model::sanitize_filename(&title);
    let dir = crate::model::concert_dir(&state.jobs.working_dir, album);
    let mp4 = dir.join(format!("{stem}.mp4"));
    let path = if mp4.exists() {
        mp4
    } else {
        dir.join(format!("{stem}.m4a"))
    };

    let render_state = if !path.exists() {
        tracing::warn!("listen_track: file not found for concert {} track {}", id, idx);
        "error"
    } else {
        tracing::info!("listen_track: opening {} for concert {}", path.display(), id);
        match tokio::process::Command::new("open")
            .arg(&path)
            .status()
            .await
        {
            Ok(status) if status.success() => "success",
            Ok(status) => {
                tracing::warn!("listen_track: `open` exited {:?} for concert {}", status.code(), id);
                "error"
            }
            Err(e) => {
                tracing::warn!("listen_track: spawn `open` failed for concert {}: {}", id, e);
                "error"
            }
        }
    };

    TrackListenButtonTemplate {
        id,
        index: idx,
        title,
        state: render_state,
    }
    .render()
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

pub async fn delete_track(
    State(state): State<AppState>,
    Path((id, idx)): Path<(i64, usize)>,
) -> Result<Response, AppError> {
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?
    };

    let title = concert
        .set_list
        .get(idx)
        .ok_or(AppError::NotFound)?
        .clone();
    let album = concert.album.as_deref().ok_or(AppError::NotFound)?;
    let stem = crate::model::sanitize_filename(&title);
    let dir = crate::model::concert_dir(&state.jobs.working_dir, album);

    for ext in &["mp4", "m4a"] {
        let path = dir.join(format!("{stem}.{ext}"));
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!("delete_track: failed to remove {}: {}", path.display(), e);
            } else {
                tracing::info!("delete_track: removed {} for concert {}", path.display(), id);
            }
        }
    }

    let remaining_tracks = crate::model::list_tracks(
        &state.jobs.working_dir,
        album,
        &concert.set_list,
    );
    if remaining_tracks.is_empty() {
        let conn = state.db.lock().unwrap();
        db::clear_split_state(&conn, id)?;
        tracing::info!("delete_track: no tracks remain, cleared split state for concert {}", id);
    }

    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id)?
    };

    let tracks = concert
        .album
        .as_deref()
        .map(|a| crate::model::list_tracks(&state.jobs.working_dir, a, &concert.set_list))
        .unwrap_or_default();

    Ok(TracksTemplate { id, tracks }
        .render()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?
        .into_response())
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

    tracing::info!(
        "sync completed: {} concerts for {}/{:02}",
        count,
        year,
        month
    );

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

        assert!(
            !called.get(),
            "scrape closure must not be called when already scraped"
        );
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

        assert!(
            called.get(),
            "scrape closure must run when metadata is missing"
        );
        assert_eq!(result.artist.as_deref(), Some("Fetched"));
        assert_eq!(
            result.set_list,
            vec!["Song 1".to_string(), "Song 2".to_string()]
        );
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
