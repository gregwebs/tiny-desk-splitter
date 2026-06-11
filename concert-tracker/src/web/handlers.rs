use std::collections::HashMap;

use askama::Template;
use askama_axum::IntoResponse;
use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::Response,
    Json,
};
use rusqlite::Connection;

use crate::db;
use crate::jobs::download::start_download;
use crate::jobs::find_downloaded_file;
use crate::jobs::split::start_split;
use crate::jobs::{JobKey, JobKind};
use crate::model::{
    concert_dir, is_browser_playable, is_video_extension, ArchiveStatus, Concert, DownloadStatus,
    SplitStatus, TrackInfo,
};
use crate::sync::{concerts_needing_scrape, sync_month, synced_months_set, YearMonth};
use crate::web::AppState;

// ── Templates ────────────────────────────────────────────────────────────────

/// Shared layout context — embedded in every page template that extends
/// `layout.html`. Lives here (not in each handler) so that adding a new
/// layout-scoped value doesn't require touching five template structs.
pub struct Chrome {
    pub theme: db::Theme,
}

impl Chrome {
    pub fn from_state(state: &AppState) -> Self {
        // Default to System on any read error so a missing/corrupt settings
        // row never blocks a page render.
        let theme = state
            .db
            .lock()
            .ok()
            .and_then(|conn| db::get_settings(&conn).ok())
            .map(|s| s.theme)
            .unwrap_or(db::Theme::System);
        Self { theme }
    }
}

#[derive(Template)]
#[template(path = "list.html")]
struct ListTemplate {
    chrome: Chrome,
    rows: Vec<String>,
    /// (href, label, active_class)
    filters: Vec<(String, String, String)>,
}

#[derive(Template)]
#[template(path = "concert_card.html")]
struct RowTemplate {
    id: i64,
    title: String,
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
    track_count: usize,
    track_total: usize,
    can_archive: bool,
    can_unarchive: bool,
    /// Whether to show the download badge alongside slot contents.
    /// False only for the NotDownloaded "fresh" state.
    show_download_badge: bool,
    show_split_badge: bool,
    show_archive_badge: bool,
    archive_status: String,
    archive_status_label: String,
    is_in_progress: bool,
    /// Browser URL for the card's `card-thumb` image, or `None` when metadata
    /// hasn't been scraped (or the concert has no album). Listing cards use the
    /// small thumbnail; the detail-page card uses the full-size preview.
    card_image_url: Option<String>,
    /// Whether this concert is queued/in-flight in the background scrape worker.
    /// Drives the "loading…" placeholder and keeps the row polling until its
    /// thumbnail is ready.
    scrape_pending: bool,
    /// Track list rendered expanded inside `#tracks-{id}` (with the
    /// `tracks-open` card class). Empty renders the container collapsed, as
    /// `toggleTracks()` expects; non-empty is used by `delete_track` so the
    /// card swap keeps the list open and the tracks-button count fresh.
    tracks: Vec<TrackInfo>,
}

#[derive(Template)]
#[template(path = "concert_detail.html")]
struct DetailTemplate {
    chrome: Chrome,
    /// Concert id, mirrored out of `concert` so the shared `tracks.html` partial
    /// (included by this template) can reference `id` like `TracksTemplate` does.
    id: i64,
    concert: Concert,
    card_html: String,
    notes_value: String,
    tracks: Vec<TrackInfo>,
    events: Vec<crate::events::EventRow>,
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
    /// Whether each available track row gets a delete (trash) button. False
    /// for the read-mostly list on the detail page; deletion there happens via
    /// the concert card's expandable list instead.
    show_delete: bool,
}

#[derive(Template)]
#[template(path = "like_button.html")]
struct LikeButtonTemplate {
    id: i64,
    index: usize,
    liked: bool,
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
#[template(path = "settings.html")]
struct SettingsTemplate {
    chrome: Chrome,
    archive_location: String,
    saved: bool,
}

#[derive(Template)]
#[template(path = "delete_confirm.html")]
struct DeleteConfirmTemplate {
    id: i64,
}

struct JobRow {
    concert_id: i64,
    title: String,
    artist: String,
    kind_slug: &'static str,
    kind_label: &'static str,
    started_at: String,
}

struct FailedJobRow {
    id: i64,
    concert_id: i64,
    title: String,
    artist: String,
    kind_label: String,
    failed_at: String,
    failure_message: String,
}

#[derive(Template)]
#[template(path = "jobs.html")]
struct JobsTemplate {
    chrome: Chrome,
    jobs: Vec<JobRow>,
    failed_jobs: Vec<FailedJobRow>,
    failed_filter: String,
}

#[derive(Template)]
#[template(path = "job_log.html")]
struct JobLogTemplate {
    chrome: Chrome,
    job: db::FailedJob,
    content: String,
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
                tracing::error!("{e:#}");
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

/// Locate the full-concert media file. Returns 500 when the concert is
/// marked as downloaded but the file is missing on disk (data-integrity
/// issue — the DB and filesystem disagree), and 404 when the concert
/// genuinely has no download yet.
fn locate_full_concert_file(
    working_dir: &std::path::Path,
    concert_id: i64,
    album: Option<&str>,
    downloaded_at: Option<&str>,
) -> Result<std::path::PathBuf, AppError> {
    if let Some(a) = album {
        if let Some(p) = find_downloaded_file(working_dir, a) {
            return Ok(p);
        }
    }
    if downloaded_at.is_some() {
        Err(AppError::Internal(anyhow::anyhow!(
            "Concert {} is marked downloaded but the media file is missing on disk",
            concert_id
        )))
    } else {
        Err(AppError::NotFound)
    }
}

fn has_archive_location(state: &AppState) -> bool {
    let conn = state.db.lock().unwrap();
    db::get_settings(&conn)
        .map(|s| s.archive_location.is_some())
        .unwrap_or(false)
}

/// Re-render a single concert card (its `<div class="card" id="concert-{id}">`).
/// Used by handlers that mutate a concert and want to swap just that card in
/// place — instead of `HX-Refresh: true`, which would reload the whole page and
/// tear down the persistent (hx-preserve'd) JS player. Mirrors `status_row`.
fn render_card(state: &AppState, id: i64) -> Result<String, AppError> {
    render_card_with_tracks(state, id, vec![])
}

/// Like `render_card`, but renders `tracks` expanded inside the card's
/// `#tracks-{id}` container. Used by `delete_track` so the htmx card swap
/// keeps the track list open while refreshing the tracks-button count.
/// Must stay on the `render_row` path so the swapped card keeps its archive
/// and scrape-pending context.
fn render_card_with_tracks(
    state: &AppState,
    id: i64,
    tracks: Vec<TrackInfo>,
) -> Result<String, AppError> {
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?
    };
    render_row_inner(
        &concert,
        has_archive_location(state),
        concert.thumbnail_url_from_db(),
        scrape_pending(state, id),
        tracks,
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

/// Render a listing card for `c`, using the small listing thumbnail as the
/// card image. `scrape_pending` is true while the concert is queued/in-flight in
/// the background scrape worker (shows a "loading…" placeholder + keeps polling).
fn render_row(
    c: &Concert,
    has_archive_location: bool,
    scrape_pending: bool,
) -> Result<String, askama::Error> {
    render_row_inner(
        c,
        has_archive_location,
        c.thumbnail_url_from_db(),
        scrape_pending,
        vec![],
    )
}

/// Render the concert card shown at the top of the detail page. Same card
/// markup as the listing, but the card image is the full-size preview rather
/// than the thumbnail (the detail page no longer shows a separate full image).
/// The detail page isn't part of the background scrape queue, so it never shows
/// the "loading…" placeholder.
fn render_detail_card(c: &Concert, has_archive_location: bool) -> Result<String, askama::Error> {
    render_row_inner(
        c,
        has_archive_location,
        c.preview_image_url_from_db(),
        false,
        vec![],
    )
}

fn render_row_inner(
    c: &Concert,
    has_archive_location: bool,
    card_image_url: Option<String>,
    scrape_pending: bool,
    tracks: Vec<TrackInfo>,
) -> Result<String, askama::Error> {
    let ds = c.download_status();
    let ss = c.split_status();
    let archive_s = c.archive_status();
    let can_download = matches!(
        &ds,
        DownloadStatus::NotDownloaded | DownloadStatus::DownloadError
    );
    let can_delete_download = matches!(&ds, DownloadStatus::Downloaded);
    let can_split = matches!(&ds, DownloadStatus::Downloaded)
        && matches!(&ss, SplitStatus::NotSplit | SplitStatus::SplitError);
    let can_delete_split = matches!(&ss, SplitStatus::Split);
    let can_listen = matches!(&ds, DownloadStatus::Downloaded);
    let track_count = c.track_count();
    let track_total = c.track_total();
    let can_archive = has_archive_location
        && (c.downloaded_at.is_some() || c.split_at.is_some())
        && matches!(
            &archive_s,
            ArchiveStatus::NotArchived | ArchiveStatus::ArchiveError
        );
    let can_unarchive = matches!(&archive_s, ArchiveStatus::Archived);
    let show_download_badge = !matches!(&ds, DownloadStatus::NotDownloaded);
    let show_split_badge = !matches!(&ss, SplitStatus::NotSplit);
    let show_archive_badge = !matches!(&archive_s, ArchiveStatus::NotArchived);
    let is_in_progress = matches!(&ds, DownloadStatus::Downloading)
        || matches!(&ss, SplitStatus::Splitting)
        || matches!(&archive_s, ArchiveStatus::Archiving);
    let card_accent = if matches!(&archive_s, ArchiveStatus::Archived) {
        "archived"
    } else if matches!(&ss, SplitStatus::Split) {
        "split"
    } else if matches!(&ds, DownloadStatus::Downloaded) {
        "downloaded"
    } else {
        ""
    };
    let is_available = !c.ignored && !c.wanted;

    RowTemplate {
        id: c.id,
        title: c.title.trim_end_matches(": Tiny Desk Concert").to_string(),
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
        track_count,
        track_total,
        can_archive,
        can_unarchive,
        show_download_badge,
        show_split_badge,
        show_archive_badge,
        archive_status: archive_s.slug().to_string(),
        archive_status_label: archive_s.label().to_string(),
        is_in_progress,
        card_image_url,
        scrape_pending,
        tracks,
    }
    .render()
}

/// Whether `id` is currently queued/in-flight in the background scrape worker.
fn scrape_pending(state: &AppState, id: i64) -> bool {
    state.scrape_queue.is_pending(id)
}

fn tracks_from_events(set_list: &[String], events: &[crate::events::EventRow]) -> Vec<TrackInfo> {
    use std::collections::HashSet;
    let deleted_indices: HashSet<usize> = events
        .iter()
        .filter(|e| e.event == "track_delete")
        .filter_map(|e| {
            e.json
                .as_ref()
                .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
                .and_then(|v| v["track_index"].as_u64())
                .map(|i| i as usize)
        })
        .collect();
    crate::model::list_tracks_from_events(set_list, &deleted_indices)
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

    let (concerts, synced, earliest_date, has_archive_location) = {
        let conn = state.db.lock().unwrap();
        let concerts = db::list_concerts(&conn)?;
        let synced = synced_months_set(&conn)?;
        let earliest = db::earliest_concert_date(&conn)?;
        let has_al = db::get_settings(&conn)?.archive_location.is_some();
        (concerts, synced, earliest, has_al)
    };

    let filtered: Vec<&Concert> = concerts
        .iter()
        .filter(|c| matches_filter(c, &filter))
        .collect();

    let current = YearMonth::current();

    let mut by_month: HashMap<YearMonth, Vec<String>> = HashMap::new();
    let mut no_date_rows: Vec<String> = Vec::new();
    for c in &filtered {
        let row = render_row(c, has_archive_location, scrape_pending(&state, c.id))
            .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;
        match c.concert_date.as_deref().and_then(YearMonth::from_date_str) {
            Some(ym) => by_month.entry(ym).or_default().push(row),
            None => no_date_rows.push(row),
        }
    }

    let items = crate::month_walk::build_month_items(
        &current,
        earliest_date.as_deref(),
        &synced,
        by_month,
        no_date_rows,
    );

    Ok(ListTemplate {
        chrome: Chrome::from_state(&state),
        rows: items,
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

    let has_al = has_archive_location(&state);
    let card_html = render_detail_card(&concert, has_al)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;
    let notes_value = concert.notes.clone().unwrap_or_default();
    let mut tracks = crate::model::list_all_tracks_from_db(
        &concert.set_list,
        &concert.tracks_present,
        &concert.tracks_liked,
    );
    let events = {
        let conn = state.db.lock().unwrap();
        crate::events::list_for_concert(&conn, id)
    };

    if tracks.is_empty() && concert.archived_at.is_some() && !concert.set_list.is_empty() {
        tracks = tracks_from_events(&concert.set_list, &events);
    }

    Ok(DetailTemplate {
        chrome: Chrome::from_state(&state),
        id: concert.id,
        card_html,
        notes_value,
        tracks,
        events,
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
    if concert.ignored {
        if let Some(album) = concert.album.as_deref() {
            remove_file_if_present(
                &concert_dir(&state.jobs.working_dir, album).join("preview.jpg"),
            );
            remove_file_if_present(&crate::scrape::thumbnail_path(
                &state.jobs.working_dir,
                album,
            ));
        }
    }
    render_row(
        &concert,
        has_archive_location(&state),
        scrape_pending(&state, id),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

/// Delete `path`, ignoring a missing file but warning on any other error.
fn remove_file_if_present(path: &std::path::Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("failed to delete {}: {}", path.display(), e);
        }
    }
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
    render_row(
        &concert,
        has_archive_location(&state),
        scrape_pending(&state, id),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
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
    render_row(
        &concert,
        has_archive_location(&state),
        scrape_pending(&state, id),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
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
    render_row(
        &concert,
        has_archive_location(&state),
        scrape_pending(&state, id),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

pub async fn download(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    // Ensure metadata is scraped before downloading so we have artist, album,
    // set_list, preview image, etc.
    {
        let needs_scrape = {
            let conn = state.db.lock().unwrap();
            let c = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
            c.metadata_scraped_at.is_none()
        };
        if needs_scrape {
            let db = state.db.clone();
            let working_dir = state.jobs.working_dir.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let conn = db.lock().unwrap();
                let c = db::get_concert(&conn, id)?;
                ensure_scraped(&conn, c, |conn, url| {
                    crate::scrape::scrape_url(conn, url, &working_dir)
                });
                Ok::<_, anyhow::Error>(())
            })
            .await;
        }
    }

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
    render_row(
        &concert,
        has_archive_location(&state),
        scrape_pending(&state, id),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
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
    render_row(
        &concert,
        has_archive_location(&state),
        scrape_pending(&state, id),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
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

    // Swap just this card in place rather than HX-Refresh (a full page reload),
    // so the persistent JS player keeps playing. The initial trash button targets
    // `this` and the confirm button targets `.delete-confirm`, so retarget both to
    // the whole card by id.
    let body = render_card(&state, id)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        "content-type",
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        "HX-Retarget",
        HeaderValue::from_str(&format!("#concert-{id}"))
            .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?,
    );
    headers.insert("HX-Reswap", HeaderValue::from_static("outerHTML"));
    Ok((headers, body).into_response())
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

    // Swap just this card in place (its trash button targets `closest .card`)
    // rather than HX-Refresh, so the persistent JS player keeps playing.
    let body = render_card(&state, id)?;
    Ok(([("content-type", "text/html; charset=utf-8")], body).into_response())
}

pub async fn listen(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let album = {
        let conn = state.db.lock().unwrap();
        let concert = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        concert.album
    };

    let file_exists = album
        .as_deref()
        .and_then(|a| find_downloaded_file(&state.jobs.working_dir, a))
        .is_some();

    let render_state = if file_exists {
        tracing::info!("listen: recording listen event for concert {}", id);
        let conn = state.db.lock().unwrap();
        crate::events::record_now(&conn, id, crate::events::Event::Listen, None);
        "success"
    } else {
        tracing::warn!("listen: file not found for concert {}", id);
        "error"
    };

    ListenButtonTemplate {
        id,
        state: render_state,
    }
    .render()
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

#[derive(serde::Serialize)]
pub struct MediaInfo {
    pub url: String,
    pub title: String,
    pub artist: String,
    pub is_video: bool,
    pub playable: bool,
    pub track_index: Option<usize>,
    /// Whether a playable track exists after this one in the same concert, so the
    /// player can disable its Next button when there is nothing left to advance to.
    pub has_next: bool,
    /// Whether a playable track exists before this one in the same concert, so the
    /// player can disable its Back button when there is nothing to go back to.
    /// Always false for whole-album playback (no per-track navigation).
    pub has_prev: bool,
    /// Whether this track is liked, so the player bar can show its like star.
    /// Always false for whole-album playback (no per-track like).
    pub liked: bool,
}

/// Locate the next browser-playable track in `set_list` after `after_idx`.
/// Returns its index, title, media URL and whether it is a video. Mirrors the
/// auto-advance logic so `has_next` and `next_track_media_info` stay in sync.
fn find_next_playable_track(
    working_dir: &std::path::Path,
    album: &str,
    set_list: &[String],
    after_idx: usize,
) -> Option<(usize, String, String, bool)> {
    let sanitized_album = crate::model::sanitize_album(album);
    for next_idx in (after_idx + 1)..set_list.len() {
        let title = &set_list[next_idx];
        if let Some(filename) = crate::model::find_track_file(working_dir, album, title) {
            let ext = filename.rsplit('.').next().unwrap_or("");
            if !is_browser_playable(ext) {
                continue;
            }
            let url = format!("/concert-files/{}/{}", sanitized_album, filename);
            return Some((next_idx, title.clone(), url, is_video_extension(ext)));
        }
    }
    None
}

/// Locate the nearest browser-playable track in `set_list` before `before_idx`.
/// Returns its index, title, media URL and whether it is a video. The reverse of
/// [`find_next_playable_track`]; used by the player's Back button.
fn find_prev_playable_track(
    working_dir: &std::path::Path,
    album: &str,
    set_list: &[String],
    before_idx: usize,
) -> Option<(usize, String, String, bool)> {
    let sanitized_album = crate::model::sanitize_album(album);
    for prev_idx in (0..before_idx).rev() {
        let title = &set_list[prev_idx];
        if let Some(filename) = crate::model::find_track_file(working_dir, album, title) {
            let ext = filename.rsplit('.').next().unwrap_or("");
            if !is_browser_playable(ext) {
                continue;
            }
            let url = format!("/concert-files/{}/{}", sanitized_album, filename);
            return Some((prev_idx, title.clone(), url, is_video_extension(ext)));
        }
    }
    None
}

pub async fn media_info(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<MediaInfo>, AppError> {
    let (concert, working_dir) = {
        let conn = state.db.lock().unwrap();
        let concert = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        (concert, state.jobs.working_dir.clone())
    };

    let album = concert.album.as_deref().ok_or(AppError::NotFound)?;
    let path = locate_full_concert_file(
        &working_dir,
        id,
        Some(album),
        concert.downloaded_at.as_deref(),
    )?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let filename = path
        .file_name()
        .and_then(|f| f.to_str())
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("invalid filename")))?;
    let sanitized_album = crate::model::sanitize_album(album);
    let url = format!("/concert-files/{}/{}", sanitized_album, filename);

    Ok(Json(MediaInfo {
        url,
        title: album.to_string(),
        artist: concert.artist.unwrap_or_default(),
        is_video: is_video_extension(ext),
        playable: is_browser_playable(ext),
        track_index: None,
        // Whole-album playback does not auto-advance per track.
        has_next: false,
        // No per-track navigation for whole-album playback.
        has_prev: false,
        // No per-track like for whole-album playback; the star is hidden.
        liked: false,
    }))
}

pub async fn track_media_info(
    State(state): State<AppState>,
    Path((id, idx)): Path<(i64, usize)>,
) -> Result<Json<MediaInfo>, AppError> {
    let (concert, working_dir) = {
        let conn = state.db.lock().unwrap();
        let concert = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        (concert, state.jobs.working_dir.clone())
    };

    let title = concert.set_list.get(idx).ok_or(AppError::NotFound)?.clone();
    let album = concert.album.as_deref().ok_or(AppError::NotFound)?;
    let filename =
        crate::model::find_track_file(&working_dir, album, &title).ok_or(AppError::NotFound)?;
    let ext = filename.rsplit('.').next().unwrap_or("");
    let sanitized_album = crate::model::sanitize_album(album);
    let url = format!("/concert-files/{}/{}", sanitized_album, filename);
    let has_next = find_next_playable_track(&working_dir, album, &concert.set_list, idx).is_some();
    let has_prev = find_prev_playable_track(&working_dir, album, &concert.set_list, idx).is_some();
    let liked = concert.tracks_liked.get(idx).copied().unwrap_or(false);

    Ok(Json(MediaInfo {
        url,
        title,
        artist: concert.artist.unwrap_or_default(),
        is_video: is_video_extension(ext),
        playable: is_browser_playable(ext),
        track_index: Some(idx),
        has_next,
        has_prev,
        liked,
    }))
}

pub async fn next_track_media_info(
    State(state): State<AppState>,
    Path((id, idx)): Path<(i64, usize)>,
) -> Result<Json<MediaInfo>, AppError> {
    let (concert, working_dir) = {
        let conn = state.db.lock().unwrap();
        let concert = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        (concert, state.jobs.working_dir.clone())
    };

    let album = concert.album.as_deref().ok_or(AppError::NotFound)?;
    // Read the liked flags before `artist` is moved out of `concert` below.
    let tracks_liked = concert.tracks_liked.clone();
    let artist = concert.artist.unwrap_or_default();

    match find_next_playable_track(&working_dir, album, &concert.set_list, idx) {
        Some((next_idx, title, url, is_video)) => Ok(Json(MediaInfo {
            url,
            title,
            artist,
            is_video,
            playable: true,
            track_index: Some(next_idx),
            // Keep the Next button correct after auto-advancing onto this track.
            has_next: find_next_playable_track(&working_dir, album, &concert.set_list, next_idx)
                .is_some(),
            // There is always a playable track before this one (the one we came
            // from), so the Back button stays enabled after advancing.
            has_prev: find_prev_playable_track(&working_dir, album, &concert.set_list, next_idx)
                .is_some(),
            liked: tracks_liked.get(next_idx).copied().unwrap_or(false),
        })),
        None => Err(AppError::NotFound),
    }
}

/// Media info for the nearest playable track *before* `idx` (the Back button).
/// 404 when there is no earlier playable track. Mirrors [`next_track_media_info`].
pub async fn prev_track_media_info(
    State(state): State<AppState>,
    Path((id, idx)): Path<(i64, usize)>,
) -> Result<Json<MediaInfo>, AppError> {
    let (concert, working_dir) = {
        let conn = state.db.lock().unwrap();
        let concert = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        (concert, state.jobs.working_dir.clone())
    };

    let album = concert.album.as_deref().ok_or(AppError::NotFound)?;
    // Read the liked flags before `artist` is moved out of `concert` below.
    let tracks_liked = concert.tracks_liked.clone();
    let artist = concert.artist.clone().unwrap_or_default();

    match find_prev_playable_track(&working_dir, album, &concert.set_list, idx) {
        Some((prev_idx, title, url, is_video)) => Ok(Json(MediaInfo {
            url,
            title,
            artist,
            is_video,
            playable: true,
            track_index: Some(prev_idx),
            // There is always a playable track after this one (the one we came
            // from), so the Next button stays enabled after going back.
            has_next: find_next_playable_track(&working_dir, album, &concert.set_list, prev_idx)
                .is_some(),
            has_prev: find_prev_playable_track(&working_dir, album, &concert.set_list, prev_idx)
                .is_some(),
            liked: tracks_liked.get(prev_idx).copied().unwrap_or(false),
        })),
        None => Err(AppError::NotFound),
    }
}

pub async fn watch(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    let (album, downloaded_at, working_dir) = {
        let conn = state.db.lock().unwrap();
        let concert = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        (
            concert.album,
            concert.downloaded_at,
            state.jobs.working_dir.clone(),
        )
    };

    let path =
        locate_full_concert_file(&working_dir, id, album.as_deref(), downloaded_at.as_deref())?;

    tracing::info!("watch: opening {} for concert {}", path.display(), id);
    {
        let conn = state.db.lock().unwrap();
        crate::events::record_now(&conn, id, crate::events::Event::Watch, None);
    }
    let mut cmd = (state.jobs.open_cmd)(&path);
    match cmd.status().await {
        Ok(s) if s.success() => Ok(StatusCode::OK),
        Ok(s) => {
            tracing::warn!("watch: `open` exited {:?} for concert {}", s.code(), id);
            Ok(StatusCode::INTERNAL_SERVER_ERROR)
        }
        Err(e) => {
            tracing::warn!("watch: spawn `open` failed for concert {}: {}", id, e);
            Ok(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub async fn watch_track(
    State(state): State<AppState>,
    Path((id, idx)): Path<(i64, usize)>,
) -> Result<StatusCode, AppError> {
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?
    };

    let title = concert.set_list.get(idx).ok_or(AppError::NotFound)?;
    let album = concert.album.as_deref().ok_or(AppError::NotFound)?;
    let filename = crate::model::find_track_file(&state.jobs.working_dir, album, title)
        .ok_or(AppError::NotFound)?;
    let path = concert_dir(&state.jobs.working_dir, album).join(&filename);

    tracing::info!(
        "watch_track: opening {} for concert {} track {}",
        path.display(),
        id,
        idx
    );
    {
        let conn = state.db.lock().unwrap();
        let json = serde_json::json!({"track_index": idx, "track_title": title}).to_string();
        crate::events::record_now(&conn, id, crate::events::Event::Watch, Some(&json));
    }
    let mut cmd = (state.jobs.open_cmd)(&path);
    match cmd.status().await {
        Ok(s) if s.success() => Ok(StatusCode::OK),
        Ok(s) => {
            tracing::warn!(
                "watch_track: `open` exited {:?} for concert {}",
                s.code(),
                id
            );
            Ok(StatusCode::INTERNAL_SERVER_ERROR)
        }
        Err(e) => {
            tracing::warn!("watch_track: spawn `open` failed for concert {}: {}", id, e);
            Ok(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub async fn tracks(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?
    };

    let mut tracks = crate::model::list_all_tracks_from_db(
        &concert.set_list,
        &concert.tracks_present,
        &concert.tracks_liked,
    );

    if tracks.is_empty() && concert.archived_at.is_some() && !concert.set_list.is_empty() {
        let events = {
            let conn = state.db.lock().unwrap();
            crate::events::list_for_concert(&conn, id)
        };
        tracks = tracks_from_events(&concert.set_list, &events);
    }

    TracksTemplate {
        id,
        tracks,
        show_delete: true,
    }
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

    let title = concert.set_list.get(idx).ok_or(AppError::NotFound)?.clone();
    let album = concert.album.as_deref().ok_or(AppError::NotFound)?;
    let file_exists =
        crate::model::find_track_file(&state.jobs.working_dir, album, &title).is_some();

    let render_state = if file_exists {
        tracing::info!(
            "listen_track: recording listen event for concert {} track {}",
            id,
            idx
        );
        let conn = state.db.lock().unwrap();
        let json = serde_json::json!({"track_index": idx, "track_title": &title}).to_string();
        crate::events::record_now(&conn, id, crate::events::Event::Listen, Some(&json));
        "success"
    } else {
        tracing::warn!(
            "listen_track: file not found for concert {} track {}",
            id,
            idx
        );
        "error"
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

    let title = concert.set_list.get(idx).ok_or(AppError::NotFound)?.clone();
    let album = concert.album.as_deref().ok_or(AppError::NotFound)?;
    let stem = crate::model::sanitize_filename(&title);
    let dir = crate::model::concert_dir(&state.jobs.working_dir, album);

    for ext in &["mp4", "m4a"] {
        let path = dir.join(format!("{stem}.{ext}"));
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!("delete_track: failed to remove {}: {}", path.display(), e);
            } else {
                tracing::info!(
                    "delete_track: removed {} for concert {}",
                    path.display(),
                    id
                );
            }
        }
    }

    {
        let conn = state.db.lock().unwrap();
        let json = serde_json::json!({"track_index": idx, "track_title": &title}).to_string();
        crate::events::record_now(&conn, id, crate::events::Event::TrackDelete, Some(&json));
    }

    let mut tracks_present = concert.tracks_present.clone();
    if idx < tracks_present.len() {
        tracks_present[idx] = false;
    }

    let split_cleared = tracks_present.iter().all(|&p| !p);
    if split_cleared {
        let conn = state.db.lock().unwrap();
        db::clear_split_state(&conn, id)?;
        tracing::info!(
            "delete_track: no tracks remain, cleared split state for concert {}",
            id
        );
    } else {
        let conn = state.db.lock().unwrap();
        db::set_tracks_present(&conn, id, &tracks_present)?;
    }

    // Respond with the whole card so the swap refreshes everything derived
    // from the track state: the tracks-button count, the split badge, and —
    // when the last track was deleted — the not-split card (no tracks row,
    // Split button back). While the split survives, the list is embedded
    // expanded so the open track list stays open across the swap.
    let tracks = if split_cleared {
        vec![]
    } else {
        let concert = {
            let conn = state.db.lock().unwrap();
            db::get_concert(&conn, id)?
        };
        crate::model::list_all_tracks_from_db(
            &concert.set_list,
            &concert.tracks_present,
            &concert.tracks_liked,
        )
    };

    Ok(render_card_with_tracks(&state, id, tracks)?.into_response())
}

pub async fn like_track(
    State(state): State<AppState>,
    Path((id, idx)): Path<(i64, usize)>,
) -> Result<impl IntoResponse, AppError> {
    let concert = {
        let conn = state.db.lock().unwrap();
        let c = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        if idx >= c.set_list.len() {
            return Err(AppError::NotFound);
        }
        db::toggle_track_liked(&conn, id, idx)?;
        db::get_concert(&conn, id)?
    };

    // Swap only the star itself: the response is rendered into the clicked
    // button (hx-target="this"), so it works identically in every track-list
    // context. Row side effects (the liked row hiding its delete button) are
    // pure CSS via `li:has(.btn-like.liked)`.
    let liked = concert.tracks_liked.get(idx).copied().unwrap_or(false);
    LikeButtonTemplate {
        id,
        index: idx,
        liked,
    }
    .render()
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

pub async fn status_row(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?
    };
    render_row(
        &concert,
        has_archive_location(&state),
        scrape_pending(&state, id),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

pub async fn jobs_list(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, AppError> {
    let failed_filter = params.get("failed_filter").cloned().unwrap_or_default();

    let (concerts, failed) = {
        let conn = state.db.lock().unwrap();
        let concerts = db::list_in_progress(&conn)?;
        let failed = db::list_failed_jobs(&conn, 100)?;
        (concerts, failed)
    };

    let jobs: Vec<JobRow> = concerts
        .iter()
        .flat_map(|c| {
            let mut rows = Vec::new();
            if c.download_started_at.is_some() && c.downloaded_at.is_none() {
                rows.push(JobRow {
                    concert_id: c.id,
                    title: c.title.clone(),
                    artist: c.artist.clone().unwrap_or_default(),
                    kind_slug: "downloading",
                    kind_label: "Download",
                    started_at: c.download_started_at.clone().unwrap_or_default(),
                });
            }
            if c.split_started_at.is_some() && c.split_at.is_none() {
                rows.push(JobRow {
                    concert_id: c.id,
                    title: c.title.clone(),
                    artist: c.artist.clone().unwrap_or_default(),
                    kind_slug: "splitting",
                    kind_label: "Split",
                    started_at: c.split_started_at.clone().unwrap_or_default(),
                });
            }
            if c.archive_started_at.is_some() && c.archived_at.is_none() {
                rows.push(JobRow {
                    concert_id: c.id,
                    title: c.title.clone(),
                    artist: c.artist.clone().unwrap_or_default(),
                    kind_slug: "archiving",
                    kind_label: "Archive",
                    started_at: c.archive_started_at.clone().unwrap_or_default(),
                });
            }
            rows
        })
        .collect();

    let failed_jobs: Vec<FailedJobRow> = failed
        .into_iter()
        .filter(|j| match failed_filter.as_str() {
            "download" => j.name == "download",
            "split" => j.name == "split",
            "archive" => j.name == "archive",
            "scrape" => j.name == "scrape",
            _ => true,
        })
        .map(|j| {
            let kind_label = match j.name.as_str() {
                "download" => "Download",
                "split" => "Split",
                "archive" => "Archive",
                "scrape" => "Scrape",
                other => other,
            }
            .to_string();
            FailedJobRow {
                id: j.id,
                concert_id: j.concert_id,
                title: j.title,
                artist: j.artist,
                kind_label,
                failed_at: j.failed_at,
                failure_message: j.failure_message,
            }
        })
        .collect();

    Ok(JobsTemplate {
        chrome: Chrome::from_state(&state),
        jobs,
        failed_jobs,
        failed_filter,
    })
}

pub async fn job_log(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let job = {
        let conn = state.db.lock().unwrap();
        db::get_failed_job(&conn, id).map_err(|_| AppError::NotFound)?
    };

    let log_path = state.jobs.log_dir().join(format!("{}.log", id));
    let content = tokio::fs::read_to_string(&log_path)
        .await
        .unwrap_or_else(|_| "Log file not found.".to_string());

    Ok(JobLogTemplate {
        chrome: Chrome::from_state(&state),
        job,
        content,
    })
}

pub async fn jobs_count(State(state): State<AppState>) -> Result<String, AppError> {
    let count = {
        let conn = state.db.lock().unwrap();
        db::count_active_jobs(&conn)?
    };
    if count > 0 {
        Ok(format!(
            " <span class=\"badge badge-downloading\">{count}</span>"
        ))
    } else {
        Ok(String::new())
    }
}

pub async fn cancel_job(
    State(state): State<AppState>,
    Path((id, kind)): Path<(i64, String)>,
) -> Result<Response, AppError> {
    let job_kind = match kind.as_str() {
        "downloading" | "download" => JobKind::Download,
        "splitting" | "split" => JobKind::Split,
        "archiving" | "archive" => JobKind::Archive,
        _ => {
            return Err(AppError::Internal(anyhow::anyhow!(
                "unknown job kind: {}",
                kind
            )))
        }
    };

    let key = JobKey {
        concert_id: id,
        kind: job_kind,
    };

    let was_running = state.registry.cancel(&key);
    tracing::info!(
        "cancel_job: concert={} kind={} was_running={}",
        id,
        kind,
        was_running
    );

    {
        let conn = state.db.lock().unwrap();
        match job_kind {
            JobKind::Download => {
                db::mark_download_failed(&conn, id, "cancelled by user")?;
            }
            JobKind::Split => {
                db::mark_split_failed(&conn, id, "cancelled by user")?;
            }
            JobKind::Archive => {
                db::mark_archive_failed(&conn, id, "cancelled by user")?;
            }
        }
    }

    let mut headers = HeaderMap::new();
    headers.insert("HX-Redirect", "/jobs".parse().unwrap());
    Ok((headers, "").into_response())
}

pub async fn settings_page(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, AppError> {
    let settings = {
        let conn = state.db.lock().unwrap();
        db::get_settings(&conn)?
    };
    let saved = params.get("saved").map(|v| v == "1").unwrap_or(false);
    Ok(SettingsTemplate {
        chrome: Chrome {
            theme: settings.theme,
        },
        archive_location: settings.archive_location.unwrap_or_default(),
        saved,
    })
}

pub async fn settings_save(
    State(state): State<AppState>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let location = form
        .get("archive_location")
        .map(|s| s.as_str())
        .unwrap_or("");
    let theme = form
        .get("theme")
        .map(|s| db::Theme::parse(s))
        .transpose()
        .map_err(|_| AppError::Internal(anyhow::anyhow!("invalid theme value")))?;
    {
        let conn = state.db.lock().unwrap();
        db::update_archive_location(&conn, location)?;
        if let Some(t) = theme {
            db::update_theme(&conn, t)?;
        }
    }
    tracing::info!(
        "settings updated: archive_location={:?} theme={:?}",
        location,
        theme.map(|t| t.as_str())
    );

    Ok(axum::response::Redirect::to("/settings?saved=1").into_response())
}

pub async fn archive(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let archive_location = {
        let conn = state.db.lock().unwrap();
        db::get_settings(&conn)?
            .archive_location
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Archive location not configured")))?
    };

    crate::jobs::archive::start_archive(
        state.db.clone(),
        state.registry.clone(),
        &state.jobs.working_dir,
        &archive_location,
        id,
    )
    .await?;

    let mut headers = HeaderMap::new();
    headers.insert("HX-Refresh", "true".parse().unwrap());
    Ok((headers, "").into_response())
}

pub async fn unarchive(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let album = {
        let conn = state.db.lock().unwrap();
        let c = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        if c.archived_at.is_none() {
            return Err(AppError::Internal(anyhow::anyhow!(
                "concert {} is not archived",
                id
            )));
        }
        if c.archive_started_at.is_some() {
            return Err(AppError::Internal(anyhow::anyhow!(
                "concert {} has an archive job in flight; cannot unarchive",
                id
            )));
        }
        c.album
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("concert {} has no album", id)))?
    };

    let source_dir = concert_dir(&state.jobs.working_dir, &album);

    tracing::info!(
        "unarchive started for concert {} ({}) <- symlink at {}",
        id,
        album,
        source_dir.display()
    );

    let result = {
        let source = source_dir.clone();
        tokio::task::spawn_blocking(move || crate::jobs::archive::do_unarchive(&source))
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("task join: {}", e)))?
    };

    match result {
        Ok(()) => {
            let conn = state.db.lock().unwrap();
            db::clear_archive_state(&conn, id)?;
            tracing::info!("unarchive completed for concert {}", id);
        }
        Err(e) => {
            let error = format!("{:#}", e);
            tracing::warn!("unarchive failed for concert {}: {}", id, error);
            let conn = state.db.lock().unwrap();
            let _ = db::mark_archive_failed(&conn, id, &error);
            let _ = db::insert_failed_job(&conn, id, "unarchive", &error);
            return Err(AppError::Internal(anyhow::anyhow!(error)));
        }
    }

    let mut headers = HeaderMap::new();
    headers.insert("HX-Refresh", "true".parse().unwrap());
    Ok((headers, "").into_response())
}

pub async fn sync_month_handler(
    State(state): State<AppState>,
    Path((year, month)): Path<(i32, u32)>,
) -> Result<impl IntoResponse, AppError> {
    tracing::info!("sync started for {}/{:02}", year, month);

    // 1. Fetch the archive page + upsert listings under the lock (one fetch).
    let db = state.db.clone();
    let ym = YearMonth { year, month };
    let synced = tokio::task::spawn_blocking(move || {
        let conn = db.lock().unwrap();
        sync_month(&conn, &ym)
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("task join: {}", e)))??;
    let synced_count = synced.len();

    // 2. Hand each just-synced concert that still lacks metadata to the serial
    //    background scrape worker, then return immediately so the listing shows up
    //    right away. The worker scrapes one concert at a time (no parallel NPR
    //    requests); each queued card renders a "loading…" placeholder and polls
    //    /concerts/:id/status until its thumbnail is ready. Enqueue happens BEFORE
    //    we respond, so the post-HX-Refresh GET / sees these ids as pending.
    let to_scrape = concerts_needing_scrape(&synced);
    let mut queued = 0usize;
    for (id, url) in to_scrape {
        if state.scrape_queue.enqueue(id, url) {
            queued += 1;
        }
    }

    tracing::info!(
        "sync completed for {}/{:02}: synced {} listings, queued {} for background scrape",
        year,
        month,
        synced_count,
        queued
    );

    let mut headers = HeaderMap::new();
    headers.insert("HX-Refresh", "true".parse().unwrap());
    Ok((
        headers,
        format!("Synced {} concerts for {}/{:02}", synced_count, year, month),
    ))
}

pub async fn player_js() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        include_str!("../../static/player.js"),
    )
}

// Vendored htmx, served locally instead of from a CDN so the UI works offline
// and isn't subject to a third-party outage. Embedded at compile time like the
// player script above.
pub async fn htmx_js() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        include_str!("../../static/htmx.min.js"),
    )
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
    fn render_row_includes_thumbnail_when_scraped_with_album() {
        let conn = db::open_in_memory().unwrap();
        let url = "https://example.org/with-album";
        let id = seed_listing(&conn, url);
        db::update_metadata(
            &conn,
            id,
            &MetadataUpdate {
                artist: "Artist".to_string(),
                album: "Some Album".to_string(),
                description: None,
                set_list: vec![],
                musicians: vec![],
            },
        )
        .unwrap();
        let concert = db::get_concert(&conn, id).unwrap();

        // Listing card uses the small thumbnail.
        let html = render_row(&concert, false, false).unwrap();
        assert!(html.contains("class=\"card-thumb\""), "html: {html}");
        assert!(html.contains("/thumbnails/Some Album.jpg"), "html: {html}");

        // Detail-page card uses the full-size preview image instead.
        let detail_html = render_detail_card(&concert, false).unwrap();
        assert!(
            detail_html.contains("class=\"card-thumb\""),
            "html: {detail_html}"
        );
        assert!(
            detail_html.contains("/concert-files/Some Album/preview.jpg"),
            "html: {detail_html}"
        );
    }

    #[test]
    fn render_row_omits_thumbnail_when_not_scraped() {
        let conn = db::open_in_memory().unwrap();
        let url = "https://example.org/unscraped";
        let id = seed_listing(&conn, url);
        let concert = db::get_concert(&conn, id).unwrap();
        assert!(concert.metadata_scraped_at.is_none());

        let html = render_row(&concert, false, false).unwrap();
        assert!(!html.contains("card-thumb"), "html: {html}");
    }

    /// A downloaded+split concert with 4 set-list tracks, one already deleted
    /// (3 of 4 present).
    fn split_concert(conn: &Connection, url: &str) -> Concert {
        let id = seed_listing(conn, url);
        db::update_metadata(
            conn,
            id,
            &MetadataUpdate {
                artist: "Artist".to_string(),
                album: "Some Album".to_string(),
                description: None,
                set_list: vec![
                    "Song A".to_string(),
                    "Song B".to_string(),
                    "Song C".to_string(),
                    "Song D".to_string(),
                ],
                musicians: vec![],
            },
        )
        .unwrap();
        let mut concert = db::get_concert(&conn, id).unwrap();
        concert.downloaded_at = Some("2026-01-01T00:00:00Z".to_string());
        concert.split_at = Some("2026-01-01T01:00:00Z".to_string());
        concert.tracks_present = vec![true, true, false, true];
        concert
    }

    #[test]
    fn render_row_with_tracks_embeds_expanded_list_with_fresh_count() {
        let conn = db::open_in_memory().unwrap();
        let concert = split_concert(&conn, "https://example.org/split");
        let tracks = crate::model::list_all_tracks_from_db(
            &concert.set_list,
            &concert.tracks_present,
            &concert.tracks_liked,
        );

        // has_archive_location = true: the delete-path card render must keep
        // the archive context (Archive button) alongside the embedded list.
        let html = render_row_inner(&concert, true, None, false, tracks).unwrap();
        assert!(html.contains("tracks-open"), "html: {html}");
        assert!(html.contains("track-list"), "html: {html}");
        assert!(html.contains("(3/4)"), "html: {html}");
        // The card-embedded list keeps its delete buttons (show_delete=true).
        assert!(html.contains("/tracks/0/delete"), "html: {html}");
        assert!(html.contains(">Archive<"), "html: {html}");
    }

    #[test]
    fn render_row_without_tracks_renders_collapsed_card() {
        let conn = db::open_in_memory().unwrap();
        let concert = split_concert(&conn, "https://example.org/split-collapsed");

        let html = render_row(&concert, false, false).unwrap();
        assert!(!html.contains("tracks-open"), "html: {html}");
        assert!(!html.contains("track-list"), "html: {html}");
        // The tracks-button count renders regardless of expansion.
        assert!(html.contains("(3/4)"), "html: {html}");
    }

    fn one_track(available: bool) -> Vec<TrackInfo> {
        vec![TrackInfo {
            index: 0,
            title: "Song A".to_string(),
            available,
            is_video: false,
            liked: false,
        }]
    }

    #[test]
    fn tracks_template_show_delete_controls_trash_buttons() {
        let with_delete = TracksTemplate {
            id: 1,
            tracks: one_track(true),
            show_delete: true,
        }
        .render()
        .unwrap();
        assert!(with_delete.contains("/tracks/0/delete"), "{with_delete}");

        // The detail-page bottom list renders without trash icons but keeps
        // the listen and like controls.
        let without_delete = TracksTemplate {
            id: 1,
            tracks: one_track(true),
            show_delete: false,
        }
        .render()
        .unwrap();
        assert!(
            !without_delete.contains("/tracks/0/delete"),
            "{without_delete}"
        );
        assert!(
            without_delete.contains("Player.playTrack"),
            "{without_delete}"
        );
        assert!(
            without_delete.contains("/tracks/0/like"),
            "{without_delete}"
        );
    }

    #[test]
    fn like_button_renders_star_state_and_self_swap() {
        let liked = LikeButtonTemplate {
            id: 2,
            index: 1,
            liked: true,
        }
        .render()
        .unwrap();
        assert!(liked.contains("★"), "{liked}");
        assert!(liked.contains("liked"), "{liked}");
        assert!(liked.contains("hx-target=\"this\""), "{liked}");
        assert!(liked.contains("/concerts/2/tracks/1/like"), "{liked}");

        let unliked = LikeButtonTemplate {
            id: 2,
            index: 1,
            liked: false,
        }
        .render()
        .unwrap();
        assert!(unliked.contains("☆"), "{unliked}");
        assert!(!unliked.contains("liked"), "{unliked}");
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
