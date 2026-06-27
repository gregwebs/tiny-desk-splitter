use std::collections::HashMap;

use anyhow::Context;
use askama::Template;
use askama_axum::IntoResponse;
use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::Response,
    Json,
};
use rusqlite::Connection;
use utoipa::ToSchema;

use crate::db;
use crate::jobs::download::start_download;
use crate::jobs::find_downloaded_file;
use crate::jobs::split::{auto_timestamps_with_backfill, start_split};
use crate::jobs::{JobKey, JobKind, SplitMode};
use crate::model::{
    concert_dir, is_browser_playable, is_video_extension, ArchiveStatus, Concert, DownloadStatus,
    PlaybackItemKind, SplitStatus, TrackInfo,
};
use crate::split_timestamps::{song_timestamps_to_payload, TimestampPayload, ValidatedTimestamps};
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
    /// Whether the concert has a scraped set list: gates the tracks row (the
    /// tracks button + track list render even before the concert is split).
    has_set_list: bool,
    /// True while a split is running or queued behind a download — the tracks
    /// button and individual track buttons render disabled until it finishes.
    tracks_busy: bool,
    track_count: usize,
    track_total: usize,
    can_archive: bool,
    can_unarchive: bool,
    /// Whether to show the download badge alongside slot contents.
    /// False only for the NotDownloaded "fresh" state.
    show_download_badge: bool,
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
    /// True when the original source file is fully redundant and can be safely
    /// deleted (all song tracks + all interlude files present on disk).
    source_redundant: bool,
    /// True when "Play concert" is meaningful: either the source file is present
    /// (whole-album mode) or `build_reconstruction` produced at least one item
    /// (reconstruction mode). False only when there is truly nothing to play.
    can_play_concert: bool,
}

#[derive(Template)]
#[template(path = "concert_detail.html")]
struct DetailTemplate {
    chrome: Chrome,
    concert: Concert,
    card_html: String,
    notes_value: String,
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
    /// True while a split is running or queued behind a download: every track
    /// button renders disabled until the job finishes.
    tracks_busy: bool,
    /// True when rendering into the sidebar. Sidebar rows have no `.card`
    /// ancestor, so the delete button calls Player.sidebarDeleteTrack() via
    /// onclick instead of using hx-target="closest .card".
    sidebar: bool,
}

#[derive(Template)]
#[template(path = "concert_playback_tracks.html")]
struct ConcertPlaybackTracksTemplate {
    id: i64,
    items: Vec<crate::model::PlaybackItem>,
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
    /// A well-formed request that fails validation (bad reference, cycle, empty
    /// name, …). Surfaced as 422 with the message so the client can show it.
    BadRequest(String),
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        AppError::Internal(e.into())
    }
}

impl AppError {
    /// Map the playlist-layer error onto HTTP semantics: missing playlist → 404,
    /// validation failure → 422, anything else → 500. Used via
    /// `.map_err(AppError::from_playlist)` rather than `From`/`?`, because the
    /// blanket `From<E: Into<anyhow::Error>>` would otherwise collapse every
    /// `PlaylistError` into a 500 and erase the 404/422 distinction.
    fn from_playlist(e: db::PlaylistError) -> Self {
        match e {
            db::PlaylistError::NotFound => AppError::NotFound,
            db::PlaylistError::Invalid(msg) => AppError::BadRequest(msg),
            db::PlaylistError::Db(err) => AppError::Internal(err),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not found").into_response(),
            AppError::BadRequest(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg).into_response(),
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
    ("archived", "Archived"),
];

fn matches_filter(c: &Concert, slug: &str, has_archive_location: bool) -> bool {
    match slug {
        "wanted" => !c.ignored && c.wanted,
        "ignored" => c.ignored,
        "available" => !c.ignored && !c.wanted,
        "downloaded" => matches!(c.download_status(), DownloadStatus::Downloaded),
        "split" => matches!(c.split_status(), SplitStatus::Split),
        "archived" if has_archive_location => matches!(c.archive_status(), ArchiveStatus::Archived),
        _ => !c.ignored, // default / unknown / gated-off slug: everything except ignored
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

/// True while a split is running or queued behind an in-flight download: the
/// tracks button and individual track buttons render disabled until it ends.
fn tracks_busy(c: &Concert, split_queued: bool) -> bool {
    matches!(c.split_status(), SplitStatus::Splitting)
        || (matches!(c.download_status(), DownloadStatus::Downloading) && split_queued)
}

/// Whether a split job is queued to start when this concert's download
/// finishes (the in-memory dependency edge created by `prepare`).
fn split_queued(state: &AppState, id: i64) -> bool {
    state.registry.has_dependent(
        &JobKey {
            concert_id: id,
            kind: JobKind::Download,
        },
        &JobKey {
            concert_id: id,
            kind: JobKind::Split,
        },
    )
}

/// Re-render a single concert card (its `<div class="card" id="concert-{id}">`)
/// with its full track list embedded. Used by handlers that mutate a concert
/// and want to swap just that card in place — instead of `HX-Refresh: true`,
/// which would reload the whole page and tear down the persistent
/// (hx-preserve'd) JS player. Embedding the tracks keeps a hover-visible track
/// list populated across the swap. Mirrors `status_row`.
fn render_card(state: &AppState, id: i64) -> Result<String, AppError> {
    let (concert, stored_ts) = {
        let conn = state.db.lock().unwrap();
        let concert = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        let stored_ts = db::get_split_timestamps(&conn, id)
            .map(|s| s.user)
            .unwrap_or(None);
        (concert, stored_ts)
    };
    let tracks = crate::model::list_all_tracks_from_db(
        &concert.set_list,
        &concert.tracks_present,
        &concert.tracks_liked,
    );
    let album = concert.album.as_deref().unwrap_or("");
    let source_redundant = crate::model::source_redundant(
        &state.jobs.working_dir,
        album,
        &concert.tracks_present,
        stored_ts.as_deref(),
        concert.media_duration,
    );
    let can_play_concert =
        compute_can_play_concert(&state.jobs.working_dir, &concert, stored_ts.as_deref());
    render_row_inner(
        &concert,
        has_archive_location(state),
        concert.thumbnail_url_from_db(),
        scrape_pending(state, id),
        split_queued(state, id),
        tracks,
        source_redundant,
        can_play_concert,
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

/// Render a listing card for `c`, using the small listing thumbnail as the
/// card image. `scrape_pending` is true while the concert is queued/in-flight in
/// the background scrape worker (shows a "loading…" placeholder + keeps polling).
/// The bulk listing leaves the track list empty (it is fetched on first hover);
/// single-card swaps go through `render_card`, which embeds it.
fn render_row(
    c: &Concert,
    has_archive_location: bool,
    scrape_pending: bool,
    split_queued: bool,
) -> Result<String, askama::Error> {
    // Listing cards don't show the delete-redundant-source button.
    // For can_play_concert use the downloaded status as a cheap proxy (avoids
    // a filesystem probe per card). Known limitation: once the source is deleted
    // as redundant, download_status() becomes NotDownloaded and the listing
    // hides "Play concert" even though reconstruction would succeed. The
    // detail/single-card paths use compute_can_play_concert for the real check.
    let can_play_concert = matches!(c.download_status(), DownloadStatus::Downloaded);
    render_row_inner(
        c,
        has_archive_location,
        c.thumbnail_url_from_db(),
        scrape_pending,
        split_queued,
        vec![],
        false,
        can_play_concert,
    )
}

/// Render the concert card shown at the top of the detail page. Same card
/// markup as the listing, but the card image is the full-size preview rather
/// than the thumbnail, and the embedded track list is always visible (the
/// hover-reveal CSS is scoped to the listing): the detail page shows picture
/// and tracks together. The detail page isn't part of the background scrape
/// queue, so it never shows the "loading…" placeholder.
fn render_detail_card(
    c: &Concert,
    has_archive_location: bool,
    split_queued: bool,
    stored_user_ts: Option<&[concert_types::SongTimestamp]>,
    working_dir: &std::path::Path,
) -> Result<String, askama::Error> {
    let tracks =
        crate::model::list_all_tracks_from_db(&c.set_list, &c.tracks_present, &c.tracks_liked);
    let album = c.album.as_deref().unwrap_or("");
    let source_redundant = crate::model::source_redundant(
        working_dir,
        album,
        &c.tracks_present,
        stored_user_ts,
        c.media_duration,
    );
    let can_play_concert = compute_can_play_concert(working_dir, c, stored_user_ts);
    render_row_inner(
        c,
        has_archive_location,
        c.preview_image_url_from_db(),
        false,
        split_queued,
        tracks,
        source_redundant,
        can_play_concert,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_row_inner(
    c: &Concert,
    has_archive_location: bool,
    card_image_url: Option<String>,
    scrape_pending: bool,
    split_queued: bool,
    tracks: Vec<TrackInfo>,
    source_redundant: bool,
    can_play_concert: bool,
) -> Result<String, askama::Error> {
    let ds = c.download_status();
    let ss = c.split_status();
    let archive_s = c.archive_status();
    let can_download = matches!(
        &ds,
        DownloadStatus::NotDownloaded | DownloadStatus::DownloadError
    );
    let can_delete_download = matches!(&ds, DownloadStatus::Downloaded);
    let track_count = c.track_count();
    let track_total = c.track_total();
    let has_set_list = track_total > 0;
    let tracks_busy = tracks_busy(c, split_queued);
    let can_archive = has_archive_location
        && (c.downloaded_at.is_some() || c.split_at.is_some())
        && matches!(
            &archive_s,
            ArchiveStatus::NotArchived | ArchiveStatus::ArchiveError
        );
    let can_unarchive = matches!(&archive_s, ArchiveStatus::Archived);
    let show_download_badge = !matches!(&ds, DownloadStatus::NotDownloaded);
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
        has_set_list,
        tracks_busy,
        track_count,
        track_total,
        can_archive,
        can_unarchive,
        show_download_badge,
        show_archive_badge,
        archive_status: archive_s.slug().to_string(),
        archive_status_label: archive_s.label().to_string(),
        is_in_progress,
        card_image_url,
        scrape_pending,
        tracks,
        source_redundant,
        can_play_concert,
    }
    .render()
}

/// Whether `id` is currently queued/in-flight in the background scrape worker.
fn scrape_pending(state: &AppState, id: i64) -> bool {
    state.scrape_queue.is_pending(id)
}

/// True when "Play concert" is meaningful for this concert: either the source
/// file is present on disk (whole-album mode) or at least one reconstruction
/// item exists (tracks + interludes cover something playable).
fn compute_can_play_concert(
    working_dir: &std::path::Path,
    concert: &Concert,
    user_ts: Option<&[concert_types::SongTimestamp]>,
) -> bool {
    if let Some(album) = concert.album.as_deref() {
        // Source present → can always play it as a whole album.
        if crate::jobs::find_downloaded_file(working_dir, album).is_some() {
            return true;
        }
        // Source gone → check whether reconstruction has anything.
        !crate::model::build_reconstruction(
            working_dir,
            album,
            &concert.set_list,
            &concert.tracks_present,
            &concert.tracks_liked,
            user_ts,
            concert.media_duration,
        )
        .is_empty()
    } else {
        false
    }
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
        .filter(|c| matches_filter(c, &filter, has_archive_location))
        .collect();

    let current = YearMonth::current();

    let mut by_month: HashMap<YearMonth, Vec<String>> = HashMap::new();
    let mut no_date_rows: Vec<String> = Vec::new();
    for c in &filtered {
        let row = render_row(
            c,
            has_archive_location,
            scrape_pending(&state, c.id),
            split_queued(&state, c.id),
        )
        .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;
        match c.concert_date.as_deref().and_then(YearMonth::from_date_str) {
            Some(ym) => by_month.entry(ym).or_default().push(row),
            None => no_date_rows.push(row),
        }
    }

    let hide_empty_months = !filter.is_empty();
    let items = crate::month_walk::build_month_items(
        &current,
        earliest_date.as_deref(),
        &synced,
        by_month,
        no_date_rows,
        hide_empty_months,
    );

    Ok(ListTemplate {
        chrome: Chrome::from_state(&state),
        rows: items,
        filters: FILTERS
            .iter()
            .filter(|(s, _)| *s != "archived" || has_archive_location)
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
    let queued = split_queued(&state, id);
    // Fetch user split timestamps for the source-redundant gate.
    let stored_user_ts = {
        let conn = state.db.lock().unwrap();
        db::get_split_timestamps(&conn, id)
            .map(|s| s.user)
            .unwrap_or(None)
    };
    // The card embeds the track list (always visible on the detail page), so
    // there is no separate tracks section to populate.
    let card_html = render_detail_card(
        &concert,
        has_al,
        queued,
        stored_user_ts.as_deref(),
        &state.jobs.working_dir,
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;
    let notes_value = concert.notes.clone().unwrap_or_default();
    let events = {
        let conn = state.db.lock().unwrap();
        crate::events::list_for_concert(&conn, id)
    };

    Ok(DetailTemplate {
        chrome: Chrome::from_state(&state),
        card_html,
        notes_value,
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
    render_card(&state, id)
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
    {
        let conn = state.db.lock().unwrap();
        db::toggle_wanted(&conn, id)?;
    }
    render_card(&state, id)
}

pub async fn notes(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<impl IntoResponse, AppError> {
    let text = form.get("notes").map(|s| s.as_str()).unwrap_or("");
    {
        let conn = state.db.lock().unwrap();
        db::set_notes(&conn, id, text)?;
    }
    render_card(&state, id)
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

    render_card(&state, id)
}

/// Ensure metadata is scraped before downloading/preparing so we have artist,
/// album, set_list, preview image, etc. Runs the scrape in a blocking task and
/// waits for it; scrape failures are logged and tolerated by `ensure_scraped`.
async fn ensure_scraped_blocking(state: &AppState, id: i64) -> Result<(), AppError> {
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
    Ok(())
}

pub async fn download(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    ensure_scraped_blocking(&state, id).await?;

    // Reload after scrape — metadata may have just been populated.
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?
    };

    let should_auto_split = concert.album.is_some()
        && !concert.set_list.is_empty()
        && matches!(
            concert.split_status(),
            SplitStatus::NotSplit | SplitStatus::SplitError
        );

    if should_auto_split {
        match crate::jobs::prepare::prepare(
            state.db.clone(),
            state.registry.clone(),
            state.jobs.clone(),
            id,
        )
        .await
        {
            Ok(crate::jobs::prepare::PrepareOutcome::Ready) => {
                // All track files exist but the source is missing (manual copies).
                // Force a download anyway so the user gets the source video.
                let album = concert.album.as_deref().expect("checked above");
                if find_downloaded_file(&state.jobs.working_dir, album).is_none() {
                    start_download(
                        state.db.clone(),
                        state.registry.clone(),
                        state.jobs.clone(),
                        id,
                    )
                    .await?;
                }
                // else: source already on disk; reconcile happened in prepare —
                // downloaded_at is now set so the Download button will disappear.
            }
            Ok(_) => {}
            Err(e)
                if e.downcast_ref::<crate::jobs::prepare::NoSetList>()
                    .is_some() =>
            {
                // TOCTOU: metadata cleared between our check and prepare's re-read.
                // Fall back to a plain download rather than a 500.
                start_download(
                    state.db.clone(),
                    state.registry.clone(),
                    state.jobs.clone(),
                    id,
                )
                .await?;
            }
            Err(e) => return Err(AppError::Internal(e)),
        }
    } else {
        start_download(
            state.db.clone(),
            state.registry.clone(),
            state.jobs.clone(),
            id,
        )
        .await?;
    }

    render_card(&state, id)
}

/// JSON shape returned by `POST /concerts/:id/prepare` and polled via
/// `GET /concerts/:id/prepare-status` while the player waits for a track.
#[derive(serde::Serialize, ToSchema)]
pub struct PrepareStatus {
    /// `DownloadStatus` slug, e.g. "downloading" / "download-error".
    download: String,
    /// `SplitStatus` slug, e.g. "splitting" / "split-error".
    split: String,
    /// Whether a split job is queued to start when the download succeeds.
    split_queued: bool,
    /// Per-set_list-index file existence, checked against the filesystem (the
    /// same source of truth media-info uses), not the DB column.
    tracks_present: Vec<bool>,
}

fn prepare_status_payload(state: &AppState, id: i64) -> Result<PrepareStatus, AppError> {
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?
    };
    let tracks_present = match concert.album.as_deref() {
        Some(album) => concert
            .set_list
            .iter()
            .map(|title| {
                crate::model::find_track_file(&state.jobs.working_dir, album, title).is_some()
            })
            .collect(),
        None => vec![false; concert.set_list.len()],
    };
    Ok(PrepareStatus {
        download: concert.download_status().slug().to_string(),
        split: concert.split_status().slug().to_string(),
        split_queued: state.registry.has_dependent(
            &JobKey {
                concert_id: id,
                kind: JobKind::Download,
            },
            &JobKey {
                concert_id: id,
                kind: JobKind::Split,
            },
        ),
        tracks_present,
    })
}

/// Idempotent "make every track playable": scrape if needed, then ensure the
/// download → split chain is running. Returns the same JSON as
/// `prepare_status` so the client can start polling from the response.
#[utoipa::path(
    post,
    path = "/concerts/{id}/prepare",
    tag = "playback",
    params(("id" = i64, Path, description = "Concert ID")),
    responses(
        (status = 200, description = "Prepare kicked off; current status", body = PrepareStatus),
        (status = 404, description = "Concert not found"),
        (status = 422, description = "Concert has no set list (scrape metadata first)", content_type = "text/plain"),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn prepare_concert(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    // 404 before scraping so an unknown id doesn't trigger a scrape attempt.
    {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
    }
    ensure_scraped_blocking(&state, id).await?;

    if let Err(e) = crate::jobs::prepare::prepare(
        state.db.clone(),
        state.registry.clone(),
        state.jobs.clone(),
        id,
    )
    .await
    {
        // No set list is user-correctable (scrape metadata first) — 422, not 500.
        if e.downcast_ref::<crate::jobs::prepare::NoSetList>()
            .is_some()
        {
            return Ok((StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response());
        }
        return Err(AppError::Internal(e));
    }

    Ok(Json(prepare_status_payload(&state, id)?).into_response())
}

#[utoipa::path(
    get,
    path = "/concerts/{id}/prepare-status",
    tag = "playback",
    params(("id" = i64, Path, description = "Concert ID")),
    responses(
        (status = 200, description = "Current prepare status", body = PrepareStatus),
        (status = 404, description = "Concert not found"),
    )
)]
pub async fn prepare_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<PrepareStatus>, AppError> {
    Ok(Json(prepare_status_payload(&state, id)?))
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
        SplitMode::Analyze,
    )
    .await?;
    render_card(&state, id)
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

/// Delete the original source (downloaded concert) file once it is fully
/// redundant — every second of `[0, media_duration]` is covered by song tracks
/// + interlude files on disk.
///
/// Returns 409 Conflict when the coverage gate fails (gate is re-checked
/// server-side, not trusted from the client). On success, clears download state
/// and refreshes the concert card like `delete_download` does.
pub async fn delete_redundant_source(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let (concert, source_path) = {
        let conn = state.db.lock().unwrap();
        let concert = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        let source_path = concert
            .album
            .as_deref()
            .and_then(|a| find_downloaded_file(&state.jobs.working_dir, a));
        (concert, source_path)
    };

    // Re-check gate server-side (fail closed).
    let album = concert.album.as_deref().unwrap_or("");
    let stored_ts = {
        let conn = state.db.lock().unwrap();
        db::get_split_timestamps(&conn, id)
            .map_err(AppError::Internal)?
            .user
    };
    let is_redundant = crate::model::source_redundant(
        &state.jobs.working_dir,
        album,
        &concert.tracks_present,
        stored_ts.as_deref(),
        concert.media_duration,
    );
    if !is_redundant {
        return Ok((
            StatusCode::CONFLICT,
            "Source file is not yet fully covered by song and interlude tracks; cannot delete.",
        )
            .into_response());
    }

    tracing::info!(
        "delete-redundant-source: source is fully covered for concert {} ({})",
        id,
        concert.title
    );

    if let Some(p) = source_path {
        if let Err(e) = std::fs::remove_file(&p) {
            tracing::warn!(
                "delete-redundant-source failed to remove {} for concert {}: {}",
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
        tracing::info!(
            "delete-redundant-source removed {} for concert {}",
            p.display(),
            id
        );
    }

    {
        let conn = state.db.lock().unwrap();
        db::clear_download_state(&conn, id)?;
        // Record a distinct event for auditing (separate from DownloadDelete).
        crate::events::record_now(&conn, id, crate::events::Event::SourceRedundantDelete, None);
    }

    tracing::info!("delete-redundant-source completed for concert {}", id);

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

// ── Concert playback ─────────────────────────────────────────────────────────

/// JSON payload for a single item in the reconstruction sequence.
#[derive(serde::Serialize, ToSchema)]
pub struct PlaybackItemJson {
    pub kind: &'static str,
    pub title: String,
    pub url: String,
    pub is_video: bool,
    pub artist: String,
    pub track_index: Option<usize>,
    pub interlude_index: Option<usize>,
    pub liked: bool,
}

/// Tagged-union response for `GET /concerts/:id/concert-playback`.
#[derive(serde::Serialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ConcertPlaybackResponse {
    /// Source file is present; client should play it as a whole album.
    Source { source: MediaInfo },
    /// Source file is absent; client plays items in order.
    Reconstruction { items: Vec<PlaybackItemJson> },
}

#[utoipa::path(
    get,
    path = "/concerts/{id}/concert-playback",
    tag = "playback",
    params(("id" = i64, Path, description = "Concert ID")),
    responses(
        (status = 200, description = "Playback plan: whole-source or reconstructed from tracks", body = ConcertPlaybackResponse),
        (status = 404, description = "Concert not found"),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn concert_playback(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<ConcertPlaybackResponse>, AppError> {
    let (concert, stored_ts, working_dir) = {
        let conn = state.db.lock().unwrap();
        let concert = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        let stored_ts = db::get_split_timestamps(&conn, id)
            .map(|s| s.user)
            .unwrap_or(None);
        let working_dir = state.jobs.working_dir.clone();
        (concert, stored_ts, working_dir)
    };

    let album = concert.album.as_deref().ok_or(AppError::NotFound)?;
    let sanitized_album = crate::model::sanitize_album(album);

    // If the source file is present, return source mode.
    if let Some(path) = find_downloaded_file(&working_dir, album) {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("invalid filename")))?;
        let url = format!("/concert-files/{}/{}", sanitized_album, filename);
        return Ok(Json(ConcertPlaybackResponse::Source {
            source: MediaInfo {
                url,
                title: album.to_string(),
                artist: concert.artist.unwrap_or_default(),
                is_video: is_video_extension(ext),
                playable: is_browser_playable(ext),
                track_index: None,
                has_next: false,
                has_prev: false,
                liked: false,
            },
        }));
    }

    // Source absent: build reconstruction.
    let items = crate::model::build_reconstruction(
        &working_dir,
        album,
        &concert.set_list,
        &concert.tracks_present,
        &concert.tracks_liked,
        stored_ts.as_deref(),
        concert.media_duration,
    );
    if items.is_empty() {
        return Err(AppError::NotFound);
    }
    let artist = concert.artist.unwrap_or_default();
    let json_items: Vec<PlaybackItemJson> = items
        .into_iter()
        .map(|item| {
            let url = format!("/concert-files/{}/{}", sanitized_album, item.filename);
            match item.kind {
                PlaybackItemKind::Song { track_index, liked } => PlaybackItemJson {
                    kind: "song",
                    title: item.title,
                    url,
                    is_video: item.is_video,
                    artist: artist.clone(),
                    track_index: Some(track_index),
                    interlude_index: None,
                    liked,
                },
                PlaybackItemKind::Interlude { index } => PlaybackItemJson {
                    kind: "interlude",
                    title: item.title,
                    url,
                    is_video: item.is_video,
                    artist: artist.clone(),
                    track_index: None,
                    interlude_index: Some(index),
                    liked: false,
                },
            }
        })
        .collect();
    Ok(Json(ConcertPlaybackResponse::Reconstruction {
        items: json_items,
    }))
}

pub async fn delete_interlude(
    State(state): State<AppState>,
    Path((id, idx)): Path<(i64, usize)>,
) -> Result<impl IntoResponse, AppError> {
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?
    };
    let album = concert.album.as_deref().ok_or(AppError::NotFound)?;
    let dir = crate::model::concert_dir(&state.jobs.working_dir, album);
    let stem = concert_types::interlude_filename_stem(idx);

    let mut removed = false;
    for ext in crate::model::INTERLUDE_EXTENSIONS {
        let path = dir.join(format!("{stem}.{ext}"));
        match std::fs::remove_file(&path) {
            Ok(()) => {
                tracing::info!(
                    "delete_interlude: removed {} for concert {}",
                    path.display(),
                    id
                );
                removed = true;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(AppError::Internal(anyhow::anyhow!(
                    "delete_interlude: failed to remove {}: {}",
                    path.display(),
                    e
                )));
            }
        }
    }

    if !removed {
        tracing::warn!(
            "delete_interlude: no file found for interlude {} concert {}",
            idx,
            id
        );
        // File was already absent — desired end-state holds; skip the event so
        // the audit trail reflects a real deletion, not a no-op.
    } else {
        let conn = state.db.lock().unwrap();
        let json = serde_json::json!({"interlude_index": idx}).to_string();
        crate::events::record_now(
            &conn,
            id,
            crate::events::Event::InterludeDelete,
            Some(&json),
        );
    }

    tracing::info!(
        "delete_interlude completed for concert {} interlude {} (removed={})",
        id,
        idx,
        removed
    );

    // Return the refreshed concert-playback sidebar fragment.
    concert_playback_tracks_fragment(&state, id).await
}

/// Render the `?playback=concert` sidebar fragment (used by the tracks handler
/// and as the delete_interlude response). Returns an HTML string.
async fn concert_playback_tracks_fragment(state: &AppState, id: i64) -> Result<String, AppError> {
    let (concert, stored_ts) = {
        let conn = state.db.lock().unwrap();
        let concert = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        let stored_ts = db::get_split_timestamps(&conn, id)
            .map(|s| s.user)
            .unwrap_or(None);
        (concert, stored_ts)
    };
    let album = concert.album.as_deref().unwrap_or("");
    let items = crate::model::build_reconstruction(
        &state.jobs.working_dir,
        album,
        &concert.set_list,
        &concert.tracks_present,
        &concert.tracks_liked,
        stored_ts.as_deref(),
        concert.media_duration,
    );
    ConcertPlaybackTracksTemplate { id, items }
        .render()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

#[derive(serde::Serialize, ToSchema)]
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
    for (next_idx, title) in set_list.iter().enumerate().skip(after_idx + 1) {
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

#[utoipa::path(
    get,
    path = "/concerts/{id}/media-info",
    tag = "playback",
    params(("id" = i64, Path, description = "Concert ID")),
    responses(
        (status = 200, description = "Whole-album playback info", body = MediaInfo),
        (status = 404, description = "Concert, album, or source file not found"),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
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

#[utoipa::path(
    get,
    path = "/concerts/{id}/tracks/{idx}/media-info",
    tag = "playback",
    params(
        ("id" = i64, Path, description = "Concert ID"),
        ("idx" = usize, Path, description = "0-based set-list track index"),
    ),
    responses(
        (status = 200, description = "Track playback info", body = MediaInfo),
        (status = 404, description = "Concert, track, or track file not found"),
    )
)]
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

#[utoipa::path(
    get,
    path = "/concerts/{id}/tracks/{idx}/next-media-info",
    tag = "playback",
    params(
        ("id" = i64, Path, description = "Concert ID"),
        ("idx" = usize, Path, description = "0-based set-list index to advance from"),
    ),
    responses(
        (status = 200, description = "Next playable track's playback info", body = MediaInfo),
        (status = 404, description = "No later playable track (or concert/track not found)"),
    )
)]
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
#[utoipa::path(
    get,
    path = "/concerts/{id}/tracks/{idx}/prev-media-info",
    tag = "playback",
    params(
        ("id" = i64, Path, description = "Concert ID"),
        ("idx" = usize, Path, description = "0-based set-list index to go back from"),
    ),
    responses(
        (status = 200, description = "Previous playable track's playback info", body = MediaInfo),
        (status = 404, description = "No earlier playable track (or concert/track not found)"),
    )
)]
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
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, AppError> {
    let sidebar = params
        .get("context")
        .map(|v| v == "sidebar")
        .unwrap_or(false);
    let playback_concert = params
        .get("playback")
        .map(|v| v == "concert")
        .unwrap_or(false);

    // Concert reconstruction sidebar: returns the interleaved song+interlude list.
    if sidebar && playback_concert {
        return concert_playback_tracks_fragment(&state, id).await;
    }

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
        tracks_busy: tracks_busy(&concert, split_queued(&state, id)),
        sidebar,
    }
    .render()
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

#[derive(serde::Serialize, ToSchema)]
pub struct TrackDetailsResponse {
    tracks_busy: bool,
    tracks: Vec<crate::model::TrackDetailItem>,
}

#[utoipa::path(
    get,
    path = "/concerts/{id}/track-details",
    tag = "playback",
    params(("id" = i64, Path, description = "Concert ID")),
    responses(
        (status = 200, description = "Per-track availability, video flag, and liked status for the sidebar track list", body = TrackDetailsResponse),
        (status = 404, description = "Concert not found"),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn track_details(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<TrackDetailsResponse>, AppError> {
    let concert = {
        let conn = state.db.lock().unwrap();
        db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?
    };
    let album = concert.album.as_deref().unwrap_or_default();
    let tracks = crate::model::list_all_track_details(
        &state.jobs.working_dir,
        album,
        &concert.set_list,
        &concert.tracks_present,
        &concert.tracks_liked,
    );
    let tracks_busy = tracks_busy(&concert, split_queued(&state, id));
    Ok(Json(TrackDetailsResponse {
        tracks_busy,
        tracks,
    }))
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
    // from the track state: the tracks-button count, the split badge, and the
    // embedded track list (deleted tracks now render as playable-unavailable
    // buttons that trigger a re-split).
    Ok(render_card(&state, id)?.into_response())
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
        if !crate::model::is_track_available(&c.tracks_present, idx) {
            tracing::debug!(
                concert_id = id,
                track_idx = idx,
                "like_track: track unavailable, ignoring"
            );
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
    render_card(&state, id)
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

/// Build the `path` component for the post-sync `HX-Location` redirect,
/// preserving any `filter` query param that was active when the user clicked
/// Sync. htmx sends the page URL via the `HX-Current-URL` request header.
///
/// Returns `"/"` when no active filter, `"/?filter={val}"` otherwise.
fn sync_location_path(current_url: Option<&str>) -> String {
    let filter = current_url
        .and_then(|url| url.find('?').map(|pos| &url[pos + 1..]))
        .and_then(|query| {
            query.split('&').find_map(|pair| {
                let mut parts = pair.splitn(2, '=');
                let key = parts.next()?;
                let val = parts.next().unwrap_or("");
                (key == "filter" && !val.is_empty()).then(|| val.to_owned())
            })
        })
        .unwrap_or_default();

    if filter.is_empty() {
        "/".to_string()
    } else {
        format!("/?filter={filter}")
    }
}

pub async fn sync_month_handler(
    State(state): State<AppState>,
    Path((year, month)): Path<(i32, u32)>,
    request_headers: HeaderMap,
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
    //    we respond, so the post-sync GET / sees these ids as pending.
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

    // Swap only #content so the persistent music player keeps playing. We use
    // HX-Location (htmx 1.9+) instead of HX-Refresh, which would force a full
    // page reload and destroy #player-container outside #content.
    //
    // htmx sends the current page URL via HX-Current-URL so we can preserve
    // any active ?filter= param in the location path.
    let current_url = request_headers
        .get("HX-Current-URL")
        .and_then(|v| v.to_str().ok());
    let path = sync_location_path(current_url);
    let location = serde_json::json!({
        "path": path,
        "target": "#content",
        "select": "#content",
        "swap": "outerHTML show:window:top"
    });
    let mut headers = HeaderMap::new();
    headers.insert(
        "HX-Location",
        HeaderValue::from_str(&location.to_string())
            .expect("HX-Location value is always valid ASCII"),
    );
    Ok((headers, ""))
}

pub async fn player_js() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        include_str!("../../static/player.js"),
    )
}

pub async fn splitter_js() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        include_str!("../../static/splitter.js"),
    )
}

pub async fn style_css() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/css")],
        include_str!("../../static/style.css"),
    )
}

// ── Split-timestamp API ───────────────────────────────────────────────────────

/// Response body for GET /concerts/:id/split-timestamps.
#[derive(serde::Serialize, ToSchema)]
pub struct SplitTimestampsResponse {
    pub set_list: Vec<String>,
    pub auto: Option<Vec<concert_types::SongTimestamp>>,
    pub user: Option<Vec<concert_types::SongTimestamp>>,
    /// Total source-media duration in seconds, from `ffprobe`. `None` when the
    /// source isn't downloaded or `ffprobe` is unavailable/fails. The frontend
    /// timeline uses this for its scale and right-edge clamp — independent of
    /// whether the source is browser-playable (e.g. `.mkv`), so the editor still
    /// works when only preview playback is unavailable.
    pub media_duration: Option<f64>,
}

#[utoipa::path(
    get,
    path = "/concerts/{id}/split-timestamps",
    tag = "splitting",
    params(("id" = i64, Path, description = "Concert ID")),
    responses(
        (status = 200, description = "Auto and user split timestamps plus source duration", body = SplitTimestampsResponse),
        (status = 404, description = "Concert not found"),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn get_split_timestamps(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<SplitTimestampsResponse>, AppError> {
    // Gather everything that needs the DB lock first, then release it before the
    // async ffprobe call (the MutexGuard must not be held across an await).
    let (set_list, auto, user, source_path, stored_media_duration) = {
        let conn = state.db.lock().unwrap();
        let concert = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        let auto = auto_timestamps_with_backfill(&conn, &state.jobs.working_dir, &concert)
            .map_err(AppError::Internal)?;
        let stored = db::get_split_timestamps(&conn, id).map_err(AppError::Internal)?;
        let source_path = concert
            .album
            .as_deref()
            .and_then(|a| find_downloaded_file(&state.jobs.working_dir, a));
        (
            concert.set_list,
            auto,
            stored.user,
            source_path,
            concert.media_duration,
        )
    };

    // Best-effort duration: prefer a fresh ffprobe of the source (most accurate),
    // then fall back to the persisted value from the last user-split (survives
    // source-file deletion), then degrade to None (editor falls back to the last
    // end time) rather than failing the whole request.
    let media_duration = match source_path {
        Some(path) => match ffprobe_duration(&path).await {
            Ok(d) => Some(d),
            Err(e) => {
                tracing::warn!(
                    "ffprobe failed for {}: {}; falling back to stored duration",
                    path.display(),
                    e
                );
                stored_media_duration
            }
        },
        None => stored_media_duration,
    };

    Ok(Json(SplitTimestampsResponse {
        set_list,
        auto,
        user,
        media_duration,
    }))
}

/// Success body for the split-start endpoints (`POST .../split-timestamps`,
/// `POST .../split-timestamps/reset`). Replaces what used to be ad-hoc
/// `serde_json::json!({"status": ...})` literals at each call site; the
/// `{"status": "..."}` wire shape is unchanged, only the value is now typed.
#[derive(serde::Serialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SplitStartStatus {
    /// A split job was spawned and is running in the background.
    Splitting,
    /// Reset no-op: the concert was already using the automatic split.
    AlreadyAuto,
}

#[derive(serde::Serialize, ToSchema)]
pub struct SplitStartResponse {
    pub status: SplitStartStatus,
}

#[utoipa::path(
    post,
    path = "/concerts/{id}/split-timestamps",
    tag = "splitting",
    params(("id" = i64, Path, description = "Concert ID")),
    request_body = TimestampPayload,
    responses(
        (status = 202, description = "Split job spawned", body = SplitStartResponse),
        (status = 404, description = "Concert not found"),
        (status = 409, description = "Source file not downloaded, or a split is already running", content_type = "text/plain"),
        (status = 422, description = "Timestamp count/title/ordering validation failed", content_type = "text/plain"),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn set_split_timestamps(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<TimestampPayload>,
) -> Result<Response, AppError> {
    let (concert, source_path) = {
        let conn = state.db.lock().unwrap();
        let concert = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        let source_path = concert
            .album
            .as_deref()
            .and_then(|a| find_downloaded_file(&state.jobs.working_dir, a));
        (concert, source_path)
    };

    let source_path = match source_path {
        Some(p) => p,
        None => {
            return Ok((
                StatusCode::CONFLICT,
                "Source file not found — download the concert first",
            )
                .into_response());
        }
    };

    // Quick count check before the ffprobe syscall so tests can exercise this
    // 422 without needing a real media file.
    if payload.songs.len() != concert.set_list.len() {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "Expected {} timestamps (one per set-list song), got {}",
                concert.set_list.len(),
                payload.songs.len()
            ),
        )
            .into_response());
    }

    let media_duration = ffprobe_duration(&source_path).await.map_err(|e| {
        tracing::warn!("ffprobe failed for {}: {}", source_path.display(), e);
        AppError::Internal(e)
    })?;

    let validated = match ValidatedTimestamps::validate(
        &concert.set_list,
        Some(media_duration),
        &payload.songs,
    ) {
        Ok(v) => v,
        Err(e) => {
            return Ok((StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response());
        }
    };

    // Reconcile downloaded_at if missing (e.g. manually copied source file).
    {
        let conn = state.db.lock().unwrap();
        if concert.downloaded_at.is_none() {
            let now = chrono::Utc::now().to_rfc3339();
            if let Err(e) = db::set_downloaded_at_if_missing(&conn, id, &now) {
                tracing::warn!(
                    "set_downloaded_at_if_missing failed for concert {}: {}",
                    id,
                    e
                );
            }
        }
    }

    let outcome = start_split(
        state.db.clone(),
        state.registry.clone(),
        state.jobs.clone(),
        id,
        SplitMode::UserTimestamps {
            ts: validated,
            media_duration,
        },
    )
    .await
    .map_err(AppError::Internal)?;

    match outcome {
        crate::jobs::split::StartOutcome::Spawned => Ok((
            StatusCode::ACCEPTED,
            Json(SplitStartResponse {
                status: SplitStartStatus::Splitting,
            }),
        )
            .into_response()),
        crate::jobs::split::StartOutcome::AlreadyRunning => Ok((
            StatusCode::CONFLICT,
            "A split job is already running for this concert",
        )
            .into_response()),
        crate::jobs::split::StartOutcome::NotDownloaded => {
            Ok((StatusCode::CONFLICT, "Concert source file not downloaded").into_response())
        }
        // Should not happen for user mode (AlreadySplit branch is Analyze-only).
        crate::jobs::split::StartOutcome::AlreadySplit => Ok((
            StatusCode::ACCEPTED,
            Json(SplitStartResponse {
                status: SplitStartStatus::Splitting,
            }),
        )
            .into_response()),
    }
}

#[utoipa::path(
    post,
    path = "/concerts/{id}/split-timestamps/reset",
    tag = "splitting",
    params(("id" = i64, Path, description = "Concert ID")),
    responses(
        (status = 200, description = "Already using the automatic split (no-op)", body = SplitStartResponse),
        (status = 202, description = "Split job spawned to reset to automatic timestamps", body = SplitStartResponse),
        (status = 404, description = "Concert not found"),
        (status = 409, description = "Source file not downloaded, or a split is already running", content_type = "text/plain"),
        (status = 422, description = "No automated timestamps available, or set list changed since analysis", content_type = "text/plain"),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn reset_split_timestamps(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let (concert, auto_ts) = {
        let conn = state.db.lock().unwrap();
        let concert = db::get_concert(&conn, id).map_err(|_| AppError::NotFound)?;
        let auto_ts = auto_timestamps_with_backfill(&conn, &state.jobs.working_dir, &concert)
            .map_err(AppError::Internal)?;
        (concert, auto_ts)
    };

    let auto_ts = match auto_ts {
        Some(ts) => ts,
        None => {
            return Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                "No automated split timestamps available — run analysis first",
            )
                .into_response());
        }
    };

    // If user column is already NULL, the tracks are already using auto timestamps.
    let user_is_null = {
        let conn = state.db.lock().unwrap();
        let stored = db::get_split_timestamps(&conn, id).map_err(AppError::Internal)?;
        stored.user.is_none()
    };
    if user_is_null {
        return Ok(Json(SplitStartResponse {
            status: SplitStartStatus::AlreadyAuto,
        })
        .into_response());
    }

    let payload_songs = song_timestamps_to_payload(&auto_ts);
    let validated = match ValidatedTimestamps::validate_for_reset(&concert.set_list, &payload_songs)
    {
        Ok(v) => v,
        Err(e) => {
            return Ok((StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response());
        }
    };

    // Reconcile downloaded_at if missing.
    {
        let conn = state.db.lock().unwrap();
        if concert.downloaded_at.is_none() {
            let now = chrono::Utc::now().to_rfc3339();
            if let Err(e) = db::set_downloaded_at_if_missing(&conn, id, &now) {
                tracing::warn!(
                    "set_downloaded_at_if_missing failed for concert {}: {}",
                    id,
                    e
                );
            }
        }
    }

    let outcome = start_split(
        state.db.clone(),
        state.registry.clone(),
        state.jobs.clone(),
        id,
        SplitMode::ResetToAuto(validated),
    )
    .await
    .map_err(AppError::Internal)?;

    match outcome {
        crate::jobs::split::StartOutcome::Spawned => Ok((
            StatusCode::ACCEPTED,
            Json(SplitStartResponse {
                status: SplitStartStatus::Splitting,
            }),
        )
            .into_response()),
        crate::jobs::split::StartOutcome::AlreadyRunning => Ok((
            StatusCode::CONFLICT,
            "A split job is already running for this concert",
        )
            .into_response()),
        crate::jobs::split::StartOutcome::NotDownloaded => {
            Ok((StatusCode::CONFLICT, "Concert source file not downloaded").into_response())
        }
        crate::jobs::split::StartOutcome::AlreadySplit => Ok((
            StatusCode::ACCEPTED,
            Json(SplitStartResponse {
                status: SplitStartStatus::Splitting,
            }),
        )
            .into_response()),
    }
}

/// Returns the duration in seconds of a media file using ffprobe. The actual
/// subprocess logic lives in [`crate::scan::ffprobe_duration_sync`] (shared
/// with the synchronous `concert_db` CLI backfill); here it just runs on a
/// blocking-pool thread since handlers must stay async.
async fn ffprobe_duration(path: &std::path::Path) -> anyhow::Result<f64> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || crate::scan::ffprobe_duration_sync(&path))
        .await
        .context("ffprobe task panicked")?
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

// ── Playlists (JSON API) ─────────────────────────────────────────────────────
//
// All playlist endpoints live under `/api/...` and speak JSON, distinct from the
// htmx HTML the rest of the app serves (and avoiding a collision with the Phase-2
// HTML pages that will live at `/playlists` and `/playlists/:id`).

#[derive(serde::Serialize, ToSchema)]
pub struct PlaylistJson {
    id: i64,
    name: String,
    description: Option<String>,
    inserted_at: String,
    updated_at: Option<String>,
}

impl From<crate::model::Playlist> for PlaylistJson {
    fn from(p: crate::model::Playlist) -> Self {
        PlaylistJson {
            id: p.id,
            name: p.name,
            description: p.description,
            inserted_at: p.inserted_at,
            updated_at: p.updated_at,
        }
    }
}

/// Intentionally narrower than `PlaylistJson`: the add-to-playlist sidebar only
/// needs id, name, and the representative item_id used to issue a DELETE.
#[derive(serde::Serialize, ToSchema)]
pub struct MembershipJson {
    id: i64,
    name: String,
    item_id: i64,
}

impl From<crate::db::PlaylistMembership> for MembershipJson {
    fn from(m: crate::db::PlaylistMembership) -> Self {
        MembershipJson {
            id: m.playlist.id,
            name: m.playlist.name,
            item_id: m.item_id,
        }
    }
}

#[derive(serde::Serialize, ToSchema)]
pub struct ResolvedTrackJson {
    concert_id: i64,
    track_index: usize,
    title: String,
    duration: Option<f64>,
    available: bool,
}

impl From<crate::model::ResolvedTrack> for ResolvedTrackJson {
    fn from(t: crate::model::ResolvedTrack) -> Self {
        ResolvedTrackJson {
            concert_id: t.concert_id,
            track_index: t.track_index,
            title: t.title,
            duration: t.duration,
            available: t.available,
        }
    }
}

#[derive(serde::Serialize, ToSchema)]
pub struct PlaylistSummaryJson {
    track_count: usize,
    known_duration_secs: f64,
    unknown_count: usize,
    first_track: Option<ResolvedTrackJson>,
}

impl From<crate::model::PlaylistSummary> for PlaylistSummaryJson {
    fn from(s: crate::model::PlaylistSummary) -> Self {
        PlaylistSummaryJson {
            track_count: s.track_count,
            known_duration_secs: s.known_duration_secs,
            unknown_count: s.unknown_count,
            first_track: s.first_track.map(Into::into),
        }
    }
}

#[derive(serde::Serialize, ToSchema)]
pub struct PlaylistListEntry {
    playlist: PlaylistJson,
    summary: PlaylistSummaryJson,
}

/// One raw playlist item (the un-flattened reference). `item_type` is "track" |
/// "concert" | "playlist"; the other fields are populated per kind.
#[derive(serde::Serialize, ToSchema)]
pub struct PlaylistItemJson {
    id: i64,
    position: i64,
    item_type: String,
    concert_id: Option<i64>,
    track_index: Option<usize>,
    child_playlist_id: Option<i64>,
}

impl From<crate::model::PlaylistItem> for PlaylistItemJson {
    fn from(i: crate::model::PlaylistItem) -> Self {
        use crate::model::PlaylistItemKind::*;
        let item_type = i.kind.type_str().to_string();
        let (concert_id, track_index, child_playlist_id) = match i.kind {
            Track {
                concert_id,
                track_index,
            } => (Some(concert_id), Some(track_index), None),
            Concert { concert_id } => (Some(concert_id), None, None),
            Playlist { child_playlist_id } => (None, None, Some(child_playlist_id)),
        };
        PlaylistItemJson {
            id: i.id,
            position: i.position,
            item_type,
            concert_id,
            track_index,
            child_playlist_id,
        }
    }
}

#[derive(serde::Serialize, ToSchema)]
pub struct PlaylistDetailJson {
    playlist: PlaylistJson,
    items: Vec<PlaylistItemJson>,
    resolved_tracks: Vec<ResolvedTrackJson>,
}

#[derive(serde::Deserialize, ToSchema)]
pub struct CreatePlaylistReq {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(serde::Deserialize, ToSchema)]
pub struct UpdatePlaylistReq {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(serde::Deserialize, ToSchema)]
pub struct AddItemReq {
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    concert_id: Option<i64>,
    #[serde(default)]
    track_index: Option<usize>,
    #[serde(default)]
    child_playlist_id: Option<i64>,
}

#[derive(serde::Deserialize, ToSchema)]
pub struct ReorderReq {
    item_ids: Vec<i64>,
}

#[derive(serde::Serialize, ToSchema)]
pub struct CreatedPlaylistJson {
    id: i64,
}

#[derive(serde::Serialize, ToSchema)]
pub struct CreatedItemJson {
    item_id: i64,
}

/// Validate the request body into a typed item kind (presence of the right
/// fields for the declared `type`); the deeper reference/cycle checks happen in
/// `db::add_playlist_item`.
fn parse_item_kind(req: &AddItemReq) -> Result<crate::model::PlaylistItemKind, AppError> {
    use crate::model::PlaylistItemKind;
    match req.item_type.as_str() {
        "track" => {
            let concert_id = req
                .concert_id
                .ok_or_else(|| AppError::BadRequest("track item requires concert_id".into()))?;
            let track_index = req
                .track_index
                .ok_or_else(|| AppError::BadRequest("track item requires track_index".into()))?;
            Ok(PlaylistItemKind::Track {
                concert_id,
                track_index,
            })
        }
        "concert" => {
            let concert_id = req
                .concert_id
                .ok_or_else(|| AppError::BadRequest("concert item requires concert_id".into()))?;
            Ok(PlaylistItemKind::Concert { concert_id })
        }
        "playlist" => {
            let child_playlist_id = req.child_playlist_id.ok_or_else(|| {
                AppError::BadRequest("playlist item requires child_playlist_id".into())
            })?;
            Ok(PlaylistItemKind::Playlist { child_playlist_id })
        }
        other => Err(AppError::BadRequest(format!("unknown item type: {other}"))),
    }
}

#[utoipa::path(
    post,
    path = "/api/playlists",
    tag = "playlists",
    request_body = CreatePlaylistReq,
    responses(
        (status = 200, description = "Playlist created", body = CreatedPlaylistJson),
        (status = 422, description = "Validation error (e.g. empty name)"),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn create_playlist(
    State(state): State<AppState>,
    Json(req): Json<CreatePlaylistReq>,
) -> Result<Json<CreatedPlaylistJson>, AppError> {
    let conn = state.db.lock().unwrap();
    let id = db::create_playlist(&conn, &req.name, req.description.as_deref())
        .map_err(AppError::from_playlist)?;
    Ok(Json(CreatedPlaylistJson { id }))
}

#[utoipa::path(
    get,
    path = "/api/playlists",
    tag = "playlists",
    responses(
        (status = 200, description = "All playlists with summaries", body = Vec<PlaylistListEntry>),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn list_playlists(
    State(state): State<AppState>,
) -> Result<Json<Vec<PlaylistListEntry>>, AppError> {
    let conn = state.db.lock().unwrap();
    let playlists = db::list_playlists(&conn)?;
    let mut out = Vec::with_capacity(playlists.len());
    for p in playlists {
        let summary = crate::playlist::summarize_playlist(&conn, p.id)?;
        out.push(PlaylistListEntry {
            playlist: p.into(),
            summary: summary.into(),
        });
    }
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/api/playlists/{id}",
    tag = "playlists",
    params(("id" = i64, Path, description = "Playlist ID")),
    responses(
        (status = 200, description = "Playlist detail with items and resolved tracks", body = PlaylistDetailJson),
        (status = 404, description = "Playlist not found"),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn get_playlist(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<PlaylistDetailJson>, AppError> {
    let conn = state.db.lock().unwrap();
    let playlist = db::get_playlist(&conn, id)?.ok_or(AppError::NotFound)?;
    let items = db::list_playlist_items(&conn, id)?;
    let resolved = crate::playlist::expand_playlist(&conn, id)?;
    Ok(Json(PlaylistDetailJson {
        playlist: playlist.into(),
        items: items.into_iter().map(Into::into).collect(),
        resolved_tracks: resolved.into_iter().map(Into::into).collect(),
    }))
}

#[utoipa::path(
    patch,
    path = "/api/playlists/{id}",
    tag = "playlists",
    params(("id" = i64, Path, description = "Playlist ID")),
    request_body = UpdatePlaylistReq,
    responses(
        (status = 204, description = "Playlist updated"),
        (status = 404, description = "Playlist not found"),
        (status = 422, description = "Validation error"),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn update_playlist(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdatePlaylistReq>,
) -> Result<StatusCode, AppError> {
    let conn = state.db.lock().unwrap();
    let existing = db::get_playlist(&conn, id)?.ok_or(AppError::NotFound)?;
    let name = req.name.unwrap_or(existing.name);
    // PATCH semantics: a provided description (incl. empty) replaces; omitted
    // keeps the current value. Clearing to NULL is not supported in this phase.
    let description = req.description.or(existing.description);
    db::update_playlist(&conn, id, &name, description.as_deref())
        .map_err(AppError::from_playlist)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/playlists/{id}",
    tag = "playlists",
    params(("id" = i64, Path, description = "Playlist ID")),
    responses(
        (status = 204, description = "Playlist deleted"),
        (status = 404, description = "Playlist not found"),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn delete_playlist(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    let conn = state.db.lock().unwrap();
    if db::delete_playlist(&conn, id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

#[utoipa::path(
    post,
    path = "/api/playlists/{id}/items",
    tag = "playlists",
    params(("id" = i64, Path, description = "Playlist ID")),
    request_body = AddItemReq,
    responses(
        (status = 200, description = "Item added", body = CreatedItemJson),
        (status = 404, description = "Playlist (or referenced concert/track/playlist) not found"),
        (status = 422, description = "Invalid item reference or would create a cycle"),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn add_playlist_item(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<AddItemReq>,
) -> Result<Json<CreatedItemJson>, AppError> {
    let kind = parse_item_kind(&req)?;
    let conn = state.db.lock().unwrap();
    let item_id = db::add_playlist_item(&conn, id, &kind).map_err(AppError::from_playlist)?;
    Ok(Json(CreatedItemJson { item_id }))
}

#[utoipa::path(
    delete,
    path = "/api/playlists/{id}/items/{item_id}",
    tag = "playlists",
    params(
        ("id" = i64, Path, description = "Playlist ID"),
        ("item_id" = i64, Path, description = "Playlist item ID"),
    ),
    responses(
        (status = 204, description = "Item removed"),
        (status = 404, description = "Playlist or item not found"),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn remove_playlist_item(
    State(state): State<AppState>,
    Path((id, item_id)): Path<(i64, i64)>,
) -> Result<StatusCode, AppError> {
    let conn = state.db.lock().unwrap();
    if db::remove_playlist_item(&conn, id, item_id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

#[utoipa::path(
    post,
    path = "/api/playlists/{id}/items/reorder",
    tag = "playlists",
    params(("id" = i64, Path, description = "Playlist ID")),
    request_body = ReorderReq,
    responses(
        (status = 204, description = "Items reordered"),
        (status = 404, description = "Playlist not found"),
        (status = 422, description = "item_ids do not match the playlist's current items"),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn reorder_playlist_items(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<ReorderReq>,
) -> Result<StatusCode, AppError> {
    let mut conn = state.db.lock().unwrap();
    db::reorder_playlist_items(&mut conn, id, &req.item_ids).map_err(AppError::from_playlist)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/concerts/{id}/tracks/{idx}/playlists",
    tag = "playlists",
    params(
        ("id" = i64, Path, description = "Concert ID"),
        ("idx" = usize, Path, description = "0-based set-list track index"),
    ),
    responses(
        (status = 200, description = "Playlists containing this track", body = Vec<MembershipJson>),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn track_playlists(
    State(state): State<AppState>,
    Path((id, idx)): Path<(i64, usize)>,
) -> Result<Json<Vec<MembershipJson>>, AppError> {
    let conn = state.db.lock().unwrap();
    let out = db::playlists_containing_track(&conn, id, idx)?;
    Ok(Json(out.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    get,
    path = "/api/concerts/{id}/playlists",
    tag = "playlists",
    params(("id" = i64, Path, description = "Concert ID")),
    responses(
        (status = 200, description = "Playlists containing this concert", body = Vec<MembershipJson>),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn concert_playlists(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<MembershipJson>>, AppError> {
    let conn = state.db.lock().unwrap();
    let out = db::playlists_containing_concert(&conn, id)?;
    Ok(Json(out.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    get,
    path = "/api/playlists/{id}/nested-in",
    tag = "playlists",
    params(("id" = i64, Path, description = "Playlist ID")),
    responses(
        (status = 200, description = "Parent playlists that nest this playlist", body = Vec<MembershipJson>),
        (status = 500, description = "Internal error", content_type = "text/plain"),
    )
)]
pub async fn playlist_nested_in(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<MembershipJson>>, AppError> {
    let conn = state.db.lock().unwrap();
    let out = db::playlists_nesting_playlist(&conn, id)?;
    Ok(Json(out.into_iter().map(Into::into).collect()))
}

// ── Playlist HTML pages (Phase 2a) ─────────────────────────────────────────────

#[derive(Template)]
#[template(path = "playlists.html")]
struct PlaylistsTemplate {
    chrome: Chrome,
    rows: Vec<PlaylistRow>,
}

/// One row on the `/playlists` list page. Display-ready: `total_time` is already
/// formatted and `first_track` is `""` when the playlist resolves to nothing.
struct PlaylistRow {
    id: i64,
    name: String,
    track_count: usize,
    total_time: String,
    first_track: String,
}

#[derive(Template)]
#[template(path = "playlist_detail.html")]
struct PlaylistDetailTemplate {
    chrome: Chrome,
    id: i64,
    name: String,
    description: String,
    track_count: usize,
    total_time: String,
    items: Vec<PlaylistItemRow>,
}

/// One raw playlist item rendered for the detail page. `href`/`sublabel` are `""`
/// when absent (a track item has no link; a nested playlist has no sublabel).
struct PlaylistItemRow {
    item_id: i64,
    kind: &'static str,
    label: String,
    sublabel: String,
    href: String,
    available: bool,
}

pub async fn playlists_page(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    // Lock for the page's own queries and drop the guard before
    // `Chrome::from_state` (which takes its own lock — the Mutex is not reentrant).
    let rows = {
        let conn = state.db.lock().unwrap();
        let playlists = db::list_playlists(&conn)?;
        let mut rows = Vec::with_capacity(playlists.len());
        for p in playlists {
            let summary = crate::playlist::summarize_playlist(&conn, p.id)?;
            rows.push(PlaylistRow {
                id: p.id,
                name: p.name,
                track_count: summary.track_count,
                total_time: crate::model::format_duration_summary(
                    summary.known_duration_secs,
                    summary.unknown_count,
                ),
                first_track: summary.first_track.map(|t| t.title).unwrap_or_default(),
            });
        }
        rows
    };
    Ok(PlaylistsTemplate {
        chrome: Chrome::from_state(&state),
        rows,
    })
}

pub async fn playlist_detail_page(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let (name, description, track_count, total_time, items) = {
        let conn = state.db.lock().unwrap();
        let playlist = db::get_playlist(&conn, id)?.ok_or(AppError::NotFound)?;
        let raw_items = db::list_playlist_items(&conn, id)?;
        let summary = crate::playlist::summarize_playlist(&conn, id)?;
        let mut items = Vec::with_capacity(raw_items.len());
        for it in raw_items {
            items.push(build_item_row(&conn, it)?);
        }
        (
            playlist.name,
            playlist.description.unwrap_or_default(),
            summary.track_count,
            crate::model::format_duration_summary(
                summary.known_duration_secs,
                summary.unknown_count,
            ),
            items,
        )
    };
    Ok(PlaylistDetailTemplate {
        chrome: Chrome::from_state(&state),
        id,
        name,
        description,
        track_count,
        total_time,
        items,
    })
}

/// Resolve one raw playlist item into its display row. A missing referenced
/// concert/playlist (live reference broken by a delete) renders as a clearly
/// labelled placeholder rather than erroring the whole page.
fn build_item_row(
    conn: &Connection,
    item: crate::model::PlaylistItem,
) -> Result<PlaylistItemRow, AppError> {
    use crate::model::PlaylistItemKind::*;
    let row = match item.kind {
        Track {
            concert_id,
            track_index,
        } => match db::get_concert_opt(conn, concert_id)? {
            Some(c) => PlaylistItemRow {
                item_id: item.id,
                kind: "track",
                label: c
                    .set_list
                    .get(track_index)
                    .cloned()
                    .unwrap_or_else(|| format!("Track {}", track_index + 1)),
                sublabel: c.title,
                href: String::new(),
                available: c.tracks_present.get(track_index).copied().unwrap_or(false),
            },
            None => PlaylistItemRow {
                item_id: item.id,
                kind: "track",
                label: format!("Track {}", track_index + 1),
                sublabel: "(missing concert)".to_string(),
                href: String::new(),
                available: false,
            },
        },
        Concert { concert_id } => {
            let concert = db::get_concert_opt(conn, concert_id)?;
            PlaylistItemRow {
                item_id: item.id,
                kind: "concert",
                label: concert
                    .as_ref()
                    .map(|c| c.title.clone())
                    .unwrap_or_else(|| "(missing concert)".to_string()),
                sublabel: concert
                    .as_ref()
                    .and_then(|c| c.artist.clone())
                    .unwrap_or_default(),
                href: format!("/concerts/{concert_id}"),
                available: true,
            }
        }
        Playlist { child_playlist_id } => PlaylistItemRow {
            item_id: item.id,
            kind: "playlist",
            label: db::get_playlist(conn, child_playlist_id)?
                .map(|p| p.name)
                .unwrap_or_else(|| "(missing playlist)".to_string()),
            sublabel: String::new(),
            href: format!("/playlists/{child_playlist_id}"),
            available: true,
        },
    };
    Ok(row)
}

pub async fn playlists_js() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        include_str!("../../static/playlists.js"),
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
        let html = render_row(&concert, false, false, false).unwrap();
        assert!(html.contains("class=\"card-thumb\""), "html: {html}");
        assert!(html.contains("/thumbnails/Some Album.jpg"), "html: {html}");

        // Detail-page card uses the full-size preview image instead.
        let detail_html =
            render_detail_card(&concert, false, false, None, std::path::Path::new("/tmp")).unwrap();
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

        let html = render_row(&concert, false, false, false).unwrap();
        assert!(!html.contains("card-thumb"), "html: {html}");
    }

    #[test]
    fn split_timestamps_response_serializes_media_duration() {
        let resp = SplitTimestampsResponse {
            set_list: vec!["Song A".to_string()],
            auto: Some(vec![concert_types::SongTimestamp {
                title: "Song A".to_string(),
                start_time: 0.0,
                end_time: 180.0,
                duration: 180.0,
            }]),
            user: None,
            media_duration: Some(212.5),
        };
        let v: serde_json::Value = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["media_duration"], 212.5);
        assert!(v["user"].is_null());
        assert_eq!(v["auto"][0]["end_time"], 180.0);

        // Null duration (ffprobe unavailable) still serializes the key.
        let resp2 = SplitTimestampsResponse {
            set_list: vec![],
            auto: None,
            user: None,
            media_duration: None,
        };
        let v2: serde_json::Value = serde_json::to_value(&resp2).unwrap();
        assert!(v2["media_duration"].is_null());
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
        let mut concert = db::get_concert(conn, id).unwrap();
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
        let html =
            render_row_inner(&concert, true, None, false, false, tracks, false, false).unwrap();
        assert!(html.contains("card-tracks-box"), "html: {html}");
        assert!(html.contains("track-list"), "html: {html}");
        assert!(html.contains("(3/4)"), "html: {html}");
        // The card-embedded list keeps its delete buttons (show_delete=true).
        assert!(html.contains("/tracks/0/delete"), "html: {html}");
        assert!(html.contains(">Archive<"), "html: {html}");
        // The deleted track is rendered as a playable-unavailable button that
        // triggers a re-split via the prepare flow.
        assert!(
            html.contains("track-title-unavailable") && html.contains("Song C"),
            "html: {html}"
        );
    }

    #[test]
    fn render_row_without_tracks_renders_collapsed_card() {
        let conn = db::open_in_memory().unwrap();
        let concert = split_concert(&conn, "https://example.org/split-collapsed");

        let html = render_row(&concert, false, false, false).unwrap();
        assert!(!html.contains("track-list"), "html: {html}");
        // The tracks-button count renders regardless of expansion.
        assert!(html.contains("(3/4)"), "html: {html}");
    }

    #[test]
    fn render_row_shows_tracks_row_before_split_without_action_buttons() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_listing(&conn, "https://example.org/unsplit");
        db::update_metadata(
            &conn,
            id,
            &MetadataUpdate {
                artist: "Artist".to_string(),
                album: "Some Album".to_string(),
                description: None,
                set_list: vec!["Song A".to_string(), "Song B".to_string()],
                musicians: vec![],
            },
        )
        .unwrap();
        let concert = db::get_concert(&conn, id).unwrap();

        let html = render_row(&concert, true, false, false).unwrap();
        // Tracks row renders pre-split with the not-split status and 0/N count.
        assert!(html.contains("not-split (0/2)"), "html: {html}");
        assert!(html.contains("Player.playTracks"), "html: {html}");
        // The Split button, tracks-row Play button, and delete-split button
        // are gone.
        assert!(!html.contains("/concerts/1/split"), "html: {html}");
        assert!(!html.contains(">Split<"), "html: {html}");
        assert!(!html.contains("/delete-split"), "html: {html}");
        assert!(
            !html.contains("btn-listen\" onclick=\"Player.playTracks"),
            "html: {html}"
        );
    }

    #[test]
    fn render_row_disables_tracks_button_while_busy() {
        let conn = db::open_in_memory().unwrap();
        let mut concert = split_concert(&conn, "https://example.org/busy");
        concert.split_at = None;
        concert.split_started_at = Some("2026-01-01T00:00:00Z".to_string());

        let html = render_row(&concert, false, false, false).unwrap();
        assert!(html.contains("disabled"), "html: {html}");
        assert!(html.contains("splitting"), "html: {html}");
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
            tracks_busy: false,
            sidebar: false,
        }
        .render()
        .unwrap();
        // Card variant: hx-post delete targeting closest .card.
        assert!(
            with_delete.contains("hx-target=\"closest .card\""),
            "{with_delete}"
        );
        assert!(with_delete.contains("/tracks/0/delete"), "{with_delete}");
        assert!(!with_delete.contains("sidebarDeleteTrack"), "{with_delete}");

        // The detail-page bottom list renders without trash icons but keeps
        // the listen and like controls.
        let without_delete = TracksTemplate {
            id: 1,
            tracks: one_track(true),
            show_delete: false,
            tracks_busy: false,
            sidebar: false,
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
    fn tracks_template_sidebar_uses_js_delete_not_htmx() {
        let sidebar = TracksTemplate {
            id: 5,
            tracks: one_track(true),
            show_delete: true,
            tracks_busy: false,
            sidebar: true,
        }
        .render()
        .unwrap();
        assert!(
            sidebar.contains("Player.sidebarDeleteTrack(5, 0)"),
            "{sidebar}"
        );
        assert!(
            !sidebar.contains("hx-target=\"closest .card\""),
            "{sidebar}"
        );
        assert!(sidebar.contains("Player.playTrack"), "{sidebar}");
        assert!(sidebar.contains("/tracks/0/like"), "{sidebar}");
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

    // ── matches_filter tests ──────────────────────────────────────────────────

    fn archived_concert(conn: &Connection, url: &str) -> Concert {
        let id = seed_listing(conn, url);
        db::mark_archive_succeeded(conn, id).unwrap();
        db::get_concert(conn, id).unwrap()
    }

    #[test]
    fn matches_filter_archived_enabled_returns_true_for_archived_concert() {
        let conn = db::open_in_memory().unwrap();
        let c = archived_concert(&conn, "https://example.org/archived-a");
        assert!(matches_filter(&c, "archived", true));
    }

    #[test]
    fn matches_filter_archived_enabled_returns_false_for_non_archived_concert() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_listing(&conn, "https://example.org/not-archived");
        let c = db::get_concert(&conn, id).unwrap();
        assert!(!matches_filter(&c, "archived", true));
    }

    #[test]
    fn matches_filter_archived_disabled_falls_back_to_default() {
        // When the archive feature is gated off (has_archive_location = false),
        // the "archived" slug must fall through to the default arm (!c.ignored),
        // not silently hide archived concerts.
        let conn = db::open_in_memory().unwrap();
        let c = archived_concert(&conn, "https://example.org/archived-b");
        // Falls through to default: not ignored → true.
        assert!(matches_filter(&c, "archived", false));
    }

    // ── sync_location_path ────────────────────────────────────────────────────

    #[test]
    fn sync_location_path_none_returns_root() {
        assert_eq!(sync_location_path(None), "/");
    }

    #[test]
    fn sync_location_path_no_query_returns_root() {
        assert_eq!(sync_location_path(Some("http://localhost:3000/")), "/");
    }

    #[test]
    fn sync_location_path_with_filter_preserves_it() {
        assert_eq!(
            sync_location_path(Some("http://localhost:3000/?filter=archived")),
            "/?filter=archived"
        );
    }

    #[test]
    fn sync_location_path_filter_among_multiple_params() {
        // filter= appears after another param
        assert_eq!(
            sync_location_path(Some("http://localhost:3000/?foo=bar&filter=liked")),
            "/?filter=liked"
        );
    }

    #[test]
    fn sync_location_path_empty_filter_returns_root() {
        // ?filter= with no value should not produce /?filter=
        assert_eq!(
            sync_location_path(Some("http://localhost:3000/?filter=")),
            "/"
        );
    }
}
