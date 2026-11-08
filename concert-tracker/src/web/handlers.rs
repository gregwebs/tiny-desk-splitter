use std::collections::HashMap;

use askama::Template;
use askama_axum::IntoResponse;
use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::Response,
    Json,
};
use utoipa::ToSchema;

use crate::concerts::{
    CommandRejection, ConcertCommandError, ConcertFilter, ConcertPlaybackError, ConcertQueryError,
    ConcertState, DeletionConfirmation, DeletionOutcome, JobRejection, JobRequestOutcome,
    ListenTarget, Observation, ScrapeAdmission,
};
use crate::db;
use crate::jobs::JobKind;
use crate::jobs::OpenMediaOutcome;
use crate::model::{
    ArchiveStatus, Concert, ConcertStatus, DownloadStatus, PlaybackItemKind, SplitStatus, TrackInfo,
};
use crate::playback::{PlaybackLookupError, PlaybackPlan, SourceMedia, TrackMedia};
use crate::split_timestamps::{
    SplitStartOutcome, SplitStartResponse, SplitTimestampsResponse, TimestampPayload,
};
use crate::sync::{sync_month, YearMonth};
use crate::web::AppState;

// ── Templates ────────────────────────────────────────────────────────────────

/// Shared layout context — embedded in every page template that extends
/// `layout.html`. Lives here (not in each handler) so that adding a new
/// layout-scoped value doesn't require touching five template structs.
pub struct Chrome {
    pub theme: db::settings::Theme,
}

impl Chrome {
    pub fn from_state(state: &AppState) -> Self {
        // Default to System on any read error so a missing/corrupt settings
        // row never blocks a page render.
        let theme = state
            .db
            .lock()
            .ok()
            .and_then(|conn| db::settings::get_settings(&conn).ok())
            .map(|s| s.theme)
            .unwrap_or(db::settings::Theme::System);
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
    job: db::failed_jobs::FailedJob,
    content: String,
}

// ── Error type ───────────────────────────────────────────────────────────────

pub enum AppError {
    NotFound,
    /// A well-formed request that fails validation (bad reference, cycle, empty
    /// name, …). Surfaced as 422 with the message so the client can show it.
    BadRequest(String),
    /// A well-formed request that conflicts with the concert's current state.
    /// Surfaced as 409 with an explanation.
    Conflict(String),
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
    fn from_playlist(e: db::playlists::PlaylistError) -> Self {
        match e {
            db::playlists::PlaylistError::NotFound => AppError::NotFound,
            db::playlists::PlaylistError::Invalid(msg) => AppError::BadRequest(msg),
            db::playlists::PlaylistError::Db(err) => AppError::Internal(err),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not found").into_response(),
            AppError::BadRequest(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg).into_response(),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
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

/// Which card an adapter is rendering. Everything else a card shows comes from
/// canonical Concert State, so no variant can offer an action the interface
/// would reject; the variant only decides presentation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CardVariant {
    /// A listing card: the small thumbnail, no embedded track list (the listing
    /// fetches it on first hover, which renders collapsed), and the
    /// background-scrape placeholder while the concert waits its turn.
    Listing,
    /// A single card swapped in place after a command: the small thumbnail and
    /// an embedded track list, so a hover-visible list survives the swap.
    Swap,
    /// The card at the top of the detail page: the full-size preview and an
    /// always-visible track list (the hover-reveal CSS is scoped to the
    /// listing). The detail page is not part of the background scrape queue, so
    /// it never shows the placeholder.
    Detail,
}

impl CardVariant {
    fn image_url(self, state: &ConcertState) -> Option<String> {
        match self {
            Self::Detail => state.concert.preview_image_url_from_db(),
            Self::Listing | Self::Swap => state.concert.thumbnail_url_from_db(),
        }
    }

    fn embeds_track_list(self) -> bool {
        matches!(self, Self::Swap | Self::Detail)
    }

    fn shows_scrape_placeholder(self) -> bool {
        matches!(self, Self::Listing | Self::Swap)
    }

    /// Only a card the user is looking at directly offers the destructive
    /// "source is redundant" action; the listing keeps its original controls.
    fn shows_redundant_source_action(self) -> bool {
        matches!(self, Self::Swap | Self::Detail)
    }
}

/// Render the card markup for one Concert State.
fn render_card_html(state: &ConcertState, variant: CardVariant) -> Result<String, askama::Error> {
    let c = &state.concert;
    let actions = &state.permitted_actions;
    let ds = c.download_status();
    let ss = c.split_status();
    let archive_s = c.archive_status();
    let track_count = c.track_count();
    let track_total = c.track_total();
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
        can_download: actions.can_download,
        can_delete_download: actions.can_delete_download,
        has_set_list: track_total > 0,
        tracks_busy: actions.tracks_busy,
        track_count,
        track_total,
        can_archive: actions.can_archive,
        can_unarchive: actions.can_unarchive,
        show_download_badge,
        show_archive_badge,
        archive_status: archive_s.slug().to_string(),
        archive_status_label: archive_s.label().to_string(),
        is_in_progress,
        card_image_url: variant.image_url(state),
        scrape_pending: variant.shows_scrape_placeholder() && state.active_work.scrape_pending,
        tracks: if variant.embeds_track_list() {
            card_tracks(state)
        } else {
            Vec::new()
        },
        source_redundant: variant.shows_redundant_source_action()
            && actions.can_delete_redundant_source,
        can_play_concert: actions.can_play_concert,
    }
    .render()
}

/// The card's track list, projected from canonical state.
fn card_tracks(state: &ConcertState) -> Vec<TrackInfo> {
    state
        .tracks
        .iter()
        .map(|track| TrackInfo {
            index: track.index,
            title: track.title.clone(),
            available: track.persisted_available,
            is_video: false,
            liked: track.liked,
        })
        .collect()
}

/// Re-render a single concert card (its `<div class="card" id="concert-{id}">`)
/// with its full track list embedded. Used by handlers that mutate a concert
/// and want to swap just that card in place — instead of `HX-Refresh: true`,
/// which would reload the whole page and tear down the persistent
/// (hx-preserve'd) JS player. Mirrors `status_row`.
fn render_card(state: &AppState, id: i64) -> Result<String, AppError> {
    let concert_state = state.concerts.get(id).map_err(concert_query_error)?;
    render_card_from(&concert_state)
}

/// Render the swapped-in card for a state a command already returned, so a
/// command response needs no second observation.
fn render_card_from(concert_state: &ConcertState) -> Result<String, AppError> {
    render_card_html(concert_state, CardVariant::Swap)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

/// Render a listing card.
fn render_listing_card(state: &ConcertState) -> Result<String, askama::Error> {
    render_card_html(state, CardVariant::Listing)
}

/// Render the concert card shown at the top of the detail page.
fn render_detail_card(state: &ConcertState) -> Result<String, askama::Error> {
    render_card_html(state, CardVariant::Detail)
}

// ── Health / identity ────────────────────────────────────────────────────────

/// This service's self-declared identity string. Returned by [`health`] and
/// compared (exact match, not a substring) by `scripts/local-api-request.sh`
/// before it issues a caller's actual request. This is a **misdirection
/// guardrail, not authentication** — any process on loopback could return
/// this same string, so it only confines the allow-listed script to
/// concert-web instances rather than proving a hostile process isn't lying.
/// Kept in sync by hand with the literal the shell script compares against
/// (no shared symbol is possible across the language boundary) — see the
/// comment above the probe in `local-api-request.sh`.
pub const SERVICE_IDENTITY: &str = "concert-web";

/// JSON identity payload for `Accept: application/json` requests to
/// [`health`]. The plain-text variant (the *default* response — see the
/// handler doc) is intentionally not modeled in OpenAPI; only this JSON shape
/// is documented, per `docs/adr/0008-health-endpoint-content-negotiation.md`.
#[derive(serde::Serialize, ToSchema)]
pub struct HealthIdentity {
    service: String,
}

/// The two response shapes [`health`] can negotiate to. Kept as a case
/// analysis (rather than the overlapping booleans an ad hoc `.contains()`
/// chain would produce) so a media range can only ever resolve to exactly
/// one outcome.
enum HealthFormat {
    Text,
    Json,
}

/// Exact media ranges named by an `Accept` header — the part of each
/// comma-separated entry before any `;q=...`/`;charset=...` parameter,
/// trimmed and case-folded per RFC 7231 (media types are case-insensitive).
/// Deliberately exact-match, not substring: a naive `.contains("application/
/// json")` would spuriously accept `application/jsonlines` or
/// `application/json-patch+json`.
fn accept_media_ranges(accept: &str) -> impl Iterator<Item = String> + '_ {
    accept
        .split(',')
        .map(|entry| {
            entry
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|range| !range.is_empty())
}

/// Resolves an `Accept` header to one of [`HealthFormat`]'s two shapes, or
/// `None` for 406. No q-value parsing; text wins ties/wildcards. See
/// [`health`]'s doc comment for the full ladder this implements.
fn negotiate_health_format(accept: &str) -> Option<HealthFormat> {
    if accept.trim().is_empty() {
        return Some(HealthFormat::Text);
    }
    let ranges: Vec<String> = accept_media_ranges(accept).collect();
    if ranges.iter().any(|r| r == "*/*") {
        return Some(HealthFormat::Text);
    }
    if ranges.iter().any(|r| r == "text/plain" || r == "text/*") {
        return Some(HealthFormat::Text);
    }
    if ranges
        .iter()
        .any(|r| r == "application/json" || r == "application/*")
    {
        return Some(HealthFormat::Json);
    }
    None
}

/// Self-identification endpoint, content-negotiated on `Accept`. Exists so
/// `scripts/local-api-request.sh`'s allow-listed prefix can confirm it's
/// talking to concert-web before issuing a caller's real request, rather than
/// silently sending it to whatever else happens to be listening on that
/// loopback port.
///
/// Negotiation (no q-values; text wins ties/wildcards):
/// 1. `Accept` absent or contains `*/*` → text/plain (the default)
/// 2. else accepts `text/plain` / `text/*` → text/plain
/// 3. else accepts `application/json` / `application/*` → JSON
/// 4. else → 406 Not Acceptable
///
/// The text body is line-oriented: line 1 is the identity token alone.
/// Future lines (version, health detail) are reserved but not implemented.
#[utoipa::path(
    get,
    path = "/health",
    tag = "meta",
    responses(
        (status = 200, description = "Service identity (JSON shown; the default response for an absent/`*/*` Accept is actually text/plain — see handler doc)", body = HealthIdentity),
        (status = 406, description = "Accept header names a concrete type this endpoint does not serve"),
    )
)]
pub async fn health(headers: HeaderMap) -> Response {
    let accept = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    match negotiate_health_format(accept) {
        Some(HealthFormat::Text) => {
            tracing::debug!(%accept, "GET /health negotiated text/plain");
            (
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                format!("{SERVICE_IDENTITY}\n"),
            )
                .into_response()
        }
        Some(HealthFormat::Json) => {
            tracing::debug!(%accept, "GET /health negotiated application/json");
            Json(HealthIdentity {
                service: SERVICE_IDENTITY.to_string(),
            })
            .into_response()
        }
        None => {
            tracing::debug!(%accept, "GET /health rejected unsupported Accept header");
            StatusCode::NOT_ACCEPTABLE.into_response()
        }
    }
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

    let states = state
        .catalog
        .browse(ConcertFilter::from_slug(&filter))
        .map_err(concert_query_error)?;
    let archive_configured = state
        .catalog
        .archive_configured()
        .map_err(concert_query_error)?;
    let listing_context = state
        .catalog
        .listing_context()
        .map_err(concert_query_error)?;
    let synced = listing_context.synced_months;
    let earliest_date = listing_context.earliest_date;

    let current = YearMonth::current();

    let mut by_month: HashMap<YearMonth, Vec<String>> = HashMap::new();
    let mut no_date_rows: Vec<String> = Vec::new();
    for concert_state in &states {
        let row = render_listing_card(concert_state)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;
        match concert_state
            .concert
            .concert_date
            .as_deref()
            .and_then(YearMonth::from_date_str)
        {
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
            .filter(|(s, _)| *s != "archived" || archive_configured)
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
    // Auto-scrape on first view. The interface tolerates a failed scrape, so the
    // page renders with listing-only data when NPR cannot be reached.
    state
        .concerts
        .ensure_scraped(id)
        .await
        .map_err(concert_command_error)?;

    let concert_state = state.concerts.get(id).map_err(concert_query_error)?;
    // The card embeds the track list (always visible on the detail page), so
    // there is no separate tracks section to populate.
    let card_html = render_detail_card(&concert_state)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;
    let notes_value = concert_state.concert.notes.clone().unwrap_or_default();
    let events = state
        .concerts
        .event_history(id)
        .map_err(concert_query_error)?;

    Ok(DetailTemplate {
        chrome: Chrome::from_state(&state),
        card_html,
        notes_value,
        events,
        concert: concert_state.concert,
    })
}

fn concert_query_error(error: ConcertQueryError) -> AppError {
    match error {
        ConcertQueryError::NotFound { .. } => AppError::NotFound,
        ConcertQueryError::Operational(error) => AppError::Internal(error),
    }
}

/// Map a command outcome onto HTTP semantics: an expected rejection becomes a
/// client error, an operational failure stays a server error.
fn concert_command_error(error: ConcertCommandError) -> AppError {
    match error {
        ConcertCommandError::Rejected(rejection) => match rejection {
            CommandRejection::NotFound { .. } => AppError::NotFound,
            // A command naming a track or media that does not exist is a
            // well-formed request for something absent: the same 404 the
            // handlers returned before the interface owned the decision.
            CommandRejection::NoSuchTrack { .. } | CommandRejection::MissingMedia { .. } => {
                AppError::NotFound
            }
            // The source file is still needed; the request is well formed but
            // conflicts with the concert's current media.
            CommandRejection::SourceNotRedundant { .. } => AppError::Conflict(
                "Source file is not yet fully covered by song and interlude tracks; cannot delete."
                    .to_string(),
            ),
            CommandRejection::Conflict { message, .. } => AppError::Conflict(message),
            CommandRejection::Invalid { message, .. } => AppError::BadRequest(message),
        },
        ConcertCommandError::Operational(error) => AppError::Internal(error),
    }
}

/// Toggle Ignored. The route is a toggle in the UI's markup — the card posts it
/// both to ignore a concert and, from the ✕, to clear the ignore — so the
/// adapter turns the click into the desired Concert Status and the interface
/// stores that status. Markup, routes, and behaviour stay as they were.
pub async fn ignore(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let concert_state = set_toggled_status(&state, id, ConcertStatus::Ignored)?;
    render_card_from(&concert_state)
}

/// Toggle Wanted, for the same reason as [`ignore`].
pub async fn want(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let concert_state = set_toggled_status(&state, id, ConcertStatus::Wanted)?;
    render_card_from(&concert_state)
}

/// Translate a status toggle into the Concert Status the user asked for — the
/// toggled status when it is already set, otherwise the requested one — and set
/// it through [`Concerts`].
fn set_toggled_status(
    state: &AppState,
    id: i64,
    requested: ConcertStatus,
) -> Result<ConcertState, AppError> {
    let current = state.concerts.get(id).map_err(concert_query_error)?;
    let desired = if current.concert.concert_status() == requested {
        ConcertStatus::Available
    } else {
        requested
    };
    state
        .concerts
        .set_status(id, desired)
        .map_err(concert_command_error)
}

pub async fn notes(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<impl IntoResponse, AppError> {
    let text = form.get("notes").map(|s| s.as_str()).unwrap_or("");
    let concert_state = state
        .concerts
        .set_notes(id, text)
        .map_err(concert_command_error)?;
    render_card_from(&concert_state)
}

pub async fn scrape_concert(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let concert_state = state
        .concerts
        .scrape(id)
        .await
        .map_err(concert_command_error)?;
    render_card_from(&concert_state)
}

pub async fn download(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = state
        .concerts
        .request_download(id)
        .await
        .map_err(concert_command_error)?;
    job_request_response(outcome)
}

/// Render a Job Request's outcome. A rejection names what the concert could not
/// do; the interface attaches the Concert State to it for adapters richer than
/// this HTML one, which has only a status and a message to work with.
fn job_request_response(outcome: JobRequestOutcome) -> Result<String, AppError> {
    match outcome {
        JobRequestOutcome::Accepted(result) => render_card_from(&result.state),
        JobRequestOutcome::Rejected(JobRejection { reason, .. }) => {
            Err(concert_command_error(ConcertCommandError::Rejected(reason)))
        }
    }
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

/// Project canonical Concert State onto the status shape the player polls. Every
/// value here is observed state; the adapter only chooses the shape.
fn prepare_status_of(concert_state: &ConcertState) -> PrepareStatus {
    PrepareStatus {
        download: concert_state.concert.download_status().slug().to_string(),
        split: concert_state.concert.split_status().slug().to_string(),
        split_queued: concert_state.active_work.split_queued_after_download,
        // An unobservable published split reports every track as absent, which is
        // what this protocol has always answered and keeps its shape stable.
        tracks_present: match &concert_state.media.published_split {
            Observation::Known(split) => split.tracks_present_on_disk.clone(),
            Observation::Unknown(_) => vec![false; concert_state.tracks.len()],
        },
    }
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
    match state.concerts.prepare(id).await {
        Ok(JobRequestOutcome::Accepted(result)) => {
            Ok(Json(prepare_status_of(&result.state)).into_response())
        }
        // No set list is user-correctable (scrape metadata first) — 422 with the
        // reason itself, not 500.
        Ok(JobRequestOutcome::Rejected(JobRejection {
            reason: CommandRejection::Invalid { message, .. },
            ..
        })) => Ok((StatusCode::UNPROCESSABLE_ENTITY, message).into_response()),
        Ok(JobRequestOutcome::Rejected(JobRejection { reason, .. })) => {
            Err(concert_command_error(ConcertCommandError::Rejected(reason)))
        }
        Err(error) => Err(concert_command_error(error)),
    }
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
    let concert_state = state.concerts.get(id).map_err(concert_query_error)?;
    Ok(Json(prepare_status_of(&concert_state)))
}

pub async fn split(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = state
        .concerts
        .request_split(id)
        .await
        .map_err(concert_command_error)?;
    job_request_response(outcome)
}

pub async fn delete_download(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let force = params.get("force").map(|v| v == "true").unwrap_or(false);

    let confirmation = if force {
        DeletionConfirmation::Confirmed
    } else {
        DeletionConfirmation::Unconfirmed
    };
    let outcome = state
        .concerts
        .delete_download(id, confirmation)
        .map_err(concert_command_error)?;

    if matches!(outcome, DeletionOutcome::ConfirmationRequired(_)) {
        tracing::info!(
            concert_id = id,
            "download file already gone; asking for confirmation before repairing the record"
        );
        let body = DeleteConfirmTemplate { id }
            .render()
            .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))?;
        return Ok(([("content-type", "text/html; charset=utf-8")], body).into_response());
    }

    // Swap just this card in place rather than HX-Refresh (a full page reload),
    // so the persistent JS player keeps playing. The initial trash button targets
    // `this` and the confirm button targets `.delete-confirm`, so retarget both to
    // the whole card by id.
    card_swap_response(outcome.state())
}

/// Respond to a card-mutating handler by swapping the whole card in place.
/// Needed for the buttons that target a narrower element (`this`, or the
/// delete-confirm prompt), which would otherwise replace only that element.
fn card_swap_response(concert_state: &ConcertState) -> Result<Response, AppError> {
    let id = concert_state.concert.id;
    let body = render_card_from(concert_state)?;
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
    let outcome = state
        .concerts
        .delete_redundant_source(id)
        .map_err(concert_command_error)?;
    card_swap_response(outcome.state())
}

pub async fn delete_split(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let outcome = state
        .concerts
        .delete_split(id)
        .map_err(concert_command_error)?;

    // Swap just this card in place (its trash button targets `closest .card`)
    // rather than HX-Refresh, so the persistent JS player keeps playing.
    let body = render_card_from(outcome.state())?;
    Ok(([("content-type", "text/html; charset=utf-8")], body).into_response())
}

pub async fn listen(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let render_state = media_command_state(
        state.concerts.record_listen(id, ListenTarget::Source),
        "listen",
    )?;
    ListenButtonTemplate {
        id,
        state: render_state,
    }
    .render()
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{}", e)))
}

/// A listen/watch button's own pass/fail state, taken from the interface's
/// command outcome. A rejection is the button's "error" state rather than an
/// HTTP error: the click is well formed, the media simply is not there.
fn media_command_state(
    outcome: Result<ConcertState, ConcertCommandError>,
    action: &'static str,
) -> Result<&'static str, AppError> {
    match outcome {
        Ok(_) => Ok("success"),
        // An absent concert is the page's own 404, not a button state.
        Err(ConcertCommandError::Rejected(CommandRejection::NotFound { .. })) => {
            Err(AppError::NotFound)
        }
        Err(ConcertCommandError::Rejected(rejection)) => {
            tracing::warn!(%rejection, action, "media command rejected");
            Ok("error")
        }
        Err(ConcertCommandError::Operational(error)) => Err(AppError::Internal(error)),
    }
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
    let concert = state.concerts.get(id).map_err(concert_query_error)?.concert;
    let album = concert.album.as_deref().ok_or(AppError::NotFound)?;
    let artist = concert.artist.clone().unwrap_or_default();
    let sanitized_album = crate::model::sanitize_album(album);

    match state
        .concerts
        .playback(id)
        .map_err(concert_playback_error)?
    {
        PlaybackPlan::Source(source) => Ok(Json(ConcertPlaybackResponse::Source {
            source: media_info_from_source(album, source),
        })),
        PlaybackPlan::Reconstruction(items) => {
            let json_items: Vec<PlaybackItemJson> = items
                .into_iter()
                .map(|item| playback_item_json(&sanitized_album, &artist, item))
                .collect();
            Ok(Json(ConcertPlaybackResponse::Reconstruction {
                items: json_items,
            }))
        }
    }
}

pub async fn delete_interlude(
    State(state): State<AppState>,
    Path((id, idx)): Path<(i64, usize)>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = state
        .concerts
        .delete_interlude(id, idx)
        .map_err(concert_command_error)?;

    // Return the refreshed concert-playback sidebar fragment from the state the
    // deletion already returned. An unobservable split has no rows to show, which
    // is what this fragment has always rendered for "nothing to play".
    let items = match &outcome.state().media.published_split {
        Observation::Known(split) => split.reconstruction_items.clone(),
        Observation::Unknown(_) => Vec::new(),
    };
    render_playback_tracks(id, items)
}

/// Render the `?playback=concert` sidebar fragment (used by the tracks handler
/// and as the delete_interlude response). Returns an HTML string.
async fn concert_playback_tracks_fragment(state: &AppState, id: i64) -> Result<String, AppError> {
    let items = match state.concerts.reconstruction_items(id) {
        Ok(items) => items,
        // Nothing to reconstruct is not an error for this fragment: the sidebar
        // simply has no rows, as it did before the interface owned resolution.
        Err(ConcertPlaybackError::Lookup(PlaybackLookupError::NotPlayable)) => Vec::new(),
        Err(error) => return Err(concert_playback_error(error)),
    };
    render_playback_tracks(id, items)
}

/// Render the concert-playback sidebar rows for one concert.
fn render_playback_tracks(
    id: i64,
    items: Vec<crate::model::PlaybackItem>,
) -> Result<String, AppError> {
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

fn media_info_from_source(album: &str, source: SourceMedia) -> MediaInfo {
    let sanitized_album = crate::model::sanitize_album(album);
    MediaInfo {
        url: format!("/concert-files/{}/{}", sanitized_album, source.filename),
        title: source.title,
        artist: source.artist,
        is_video: source.is_video,
        playable: source.playable,
        track_index: None,
        has_next: false,
        has_prev: false,
        liked: false,
    }
}

fn media_info_from_track(album: &str, track: TrackMedia) -> MediaInfo {
    let sanitized_album = crate::model::sanitize_album(album);
    MediaInfo {
        url: format!("/concert-files/{}/{}", sanitized_album, track.filename),
        title: track.title,
        artist: track.artist,
        is_video: track.is_video,
        playable: track.playable,
        track_index: Some(track.track_index),
        has_next: track.has_next,
        has_prev: track.has_prev,
        liked: track.liked,
    }
}

fn playback_item_json(
    sanitized_album: &str,
    artist: &str,
    item: crate::model::PlaybackItem,
) -> PlaybackItemJson {
    let url = format!("/concert-files/{}/{}", sanitized_album, item.filename);
    match item.kind {
        PlaybackItemKind::Song { track_index, liked } => PlaybackItemJson {
            kind: "song",
            title: item.title,
            url,
            is_video: item.is_video,
            artist: artist.to_string(),
            track_index: Some(track_index),
            interlude_index: None,
            liked,
        },
        PlaybackItemKind::Interlude { index } => PlaybackItemJson {
            kind: "interlude",
            title: item.title,
            url,
            is_video: item.is_video,
            artist: artist.to_string(),
            track_index: None,
            interlude_index: Some(index),
            liked: false,
        },
    }
}

fn concert_playback_error(error: ConcertPlaybackError) -> AppError {
    match error {
        ConcertPlaybackError::NotFound { .. } => AppError::NotFound,
        ConcertPlaybackError::Query(error) => concert_query_error(error),
        ConcertPlaybackError::Lookup(error) => playback_error_to_app_error(error),
    }
}

fn playback_error_to_app_error(e: PlaybackLookupError) -> AppError {
    match e {
        PlaybackLookupError::NotPlayable => AppError::NotFound,
        PlaybackLookupError::MarkedDownloadedButMissing { concert_id } => {
            AppError::Internal(anyhow::anyhow!(
                "Concert {} is marked downloaded but the media file is missing on disk",
                concert_id
            ))
        }
        PlaybackLookupError::InvalidFilename => {
            AppError::Internal(anyhow::anyhow!("invalid filename"))
        }
    }
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
    let album = album_of(&state, id)?;
    let source = state
        .concerts
        .source_media(id)
        .map_err(concert_playback_error)?;

    Ok(Json(media_info_from_source(&album, source)))
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
    let album = album_of(&state, id)?;
    let media = state
        .concerts
        .track_media(id, idx)
        .map_err(concert_playback_error)?;

    Ok(Json(media_info_from_track(&album, media)))
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
    let album = album_of(&state, id)?;
    let media = state
        .concerts
        .next_track_media(id, idx)
        .map_err(concert_playback_error)?;
    Ok(Json(media_info_from_track(&album, media)))
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
    let album = album_of(&state, id)?;
    let media = state
        .concerts
        .prev_track_media(id, idx)
        .map_err(concert_playback_error)?;
    Ok(Json(media_info_from_track(&album, media)))
}

pub async fn watch(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    open_in_system_player(&state, id, ListenTarget::Source, "watch").await
}

/// Resolve the file for `target`, record that the listener opened it, and hand
/// it to the configured system player. Which file to open is resolved by
/// `Concerts`; how to open it is this adapter's choice.
async fn open_in_system_player(
    state: &AppState,
    id: i64,
    target: ListenTarget,
    action: &'static str,
) -> Result<StatusCode, AppError> {
    let path = state
        .concerts
        .media_file(id, target)
        .map_err(concert_playback_error)?;
    state
        .concerts
        .record_watch(id, target)
        .map_err(concert_command_error)?;

    tracing::info!(concert_id = id, media = %path.display(), action, "opening media");
    match state.jobs.open_media(id, &path).await {
        OpenMediaOutcome::Succeeded => Ok(StatusCode::OK),
        OpenMediaOutcome::Failed { message } => {
            tracing::warn!(concert_id = id, action, %message, "media opener failed");
            Ok(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub async fn watch_track(
    State(state): State<AppState>,
    Path((id, idx)): Path<(i64, usize)>,
) -> Result<StatusCode, AppError> {
    open_in_system_player(
        &state,
        id,
        ListenTarget::Track { index: idx },
        "watch-track",
    )
    .await
}

/// The album name a playback URL is built from. A concert with no album has no
/// media to describe, which the handlers report as absent.
fn album_of(state: &AppState, id: i64) -> Result<String, AppError> {
    state
        .concerts
        .get(id)
        .map_err(concert_query_error)?
        .concert
        .album
        .ok_or(AppError::NotFound)
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

    let concert_state = state.concerts.get(id).map_err(concert_query_error)?;

    TracksTemplate {
        id,
        tracks: card_tracks(&concert_state),
        show_delete: true,
        tracks_busy: concert_state.permitted_actions.tracks_busy,
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
    let tracks_busy = state
        .concerts
        .get(id)
        .map_err(concert_query_error)?
        .permitted_actions
        .tracks_busy;
    let tracks = state
        .concerts
        .track_details(id)
        .map_err(concert_playback_error)?;
    Ok(Json(TrackDetailsResponse {
        tracks_busy,
        tracks,
    }))
}

pub async fn listen_track(
    State(state): State<AppState>,
    Path((id, idx)): Path<(i64, usize)>,
) -> Result<impl IntoResponse, AppError> {
    // The track's title is the button's label, so it is read regardless of
    // whether the listen itself succeeds.
    let title = state
        .concerts
        .get(id)
        .map_err(concert_query_error)?
        .concert
        .set_list
        .get(idx)
        .cloned()
        .ok_or(AppError::NotFound)?;
    let render_state = media_command_state(
        state
            .concerts
            .record_listen(id, ListenTarget::Track { index: idx }),
        "listen-track",
    )?;

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
    let outcome = state
        .concerts
        .delete_track(id, idx)
        .map_err(concert_command_error)?;

    // Respond with the whole card so the swap refreshes everything derived
    // from the track state: the tracks-button count, the split badge, and the
    // embedded track list (deleted tracks now render as playable-unavailable
    // buttons that trigger a re-split).
    Ok(render_card_from(outcome.state())?.into_response())
}

pub async fn like_track(
    State(state): State<AppState>,
    Path((id, idx)): Path<(i64, usize)>,
) -> Result<impl IntoResponse, AppError> {
    // The route is a toggle in the card markup, so the adapter turns the click
    // into the desired like state and the interface stores that state.
    let current = state.concerts.get(id).map_err(concert_query_error)?;
    let liked_now = current
        .concert
        .tracks_liked
        .get(idx)
        .copied()
        .unwrap_or(false);
    let concert_state = state
        .concerts
        .set_track_liked(id, idx, !liked_now)
        .map_err(concert_command_error)?;

    // Swap only the star itself: the response is rendered into the clicked
    // button (hx-target="this"), so it works identically in every track-list
    // context. Row side effects (the liked row hiding its delete button) are
    // pure CSS via `li:has(.btn-like.liked)`.
    let liked = concert_state
        .concert
        .tracks_liked
        .get(idx)
        .copied()
        .unwrap_or(false);
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
        let concerts = db::lifecycle::list_in_progress(&conn)?;
        let failed = db::failed_jobs::list_failed_jobs(&conn, 100)?;
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
        db::failed_jobs::get_failed_job(&conn, id).map_err(|_| AppError::NotFound)?
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
        db::lifecycle::count_active_jobs(&conn)?
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

    match state.concerts.cancel_job(id, job_kind) {
        Ok(_) => {}
        // This endpoint's protocol is "always redirect back to the jobs page", and
        // a concert that is not there has nothing to cancel.
        Err(ConcertCommandError::Rejected(CommandRejection::NotFound { .. })) => {}
        Err(error) => return Err(concert_command_error(error)),
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
        db::settings::get_settings(&conn)?
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
        .map(|s| db::settings::Theme::parse(s))
        .transpose()
        .map_err(|_| AppError::Internal(anyhow::anyhow!("invalid theme value")))?;
    {
        let conn = state.db.lock().unwrap();
        db::settings::update_archive_location(&conn, location)?;
        if let Some(t) = theme {
            db::settings::update_theme(&conn, t)?;
        }
    }
    tracing::info!(
        "settings updated: archive_location={:?} theme={:?}",
        location,
        theme.map(|t| t.as_str())
    );

    Ok(axum::response::Redirect::to("/settings?saved=1").into_response())
}

/// Archive and unarchive answer 500 for a request the concert cannot satisfy
/// (no archive location configured, not archived, an archive in flight). That is
/// this endpoint's long-standing protocol, so the adapter keeps it while the
/// interface decides what the answer is.
fn archive_command_error(error: ConcertCommandError) -> AppError {
    match error {
        ConcertCommandError::Rejected(rejection) => {
            AppError::Internal(anyhow::anyhow!("{rejection}"))
        }
        ConcertCommandError::Operational(error) => AppError::Internal(error),
    }
}

pub async fn archive(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    state
        .concerts
        .request_archive(id)
        .await
        .map_err(archive_command_error)?;

    let mut headers = HeaderMap::new();
    headers.insert("HX-Refresh", "true".parse().unwrap());
    Ok((headers, "").into_response())
}

pub async fn unarchive(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    state
        .concerts
        .unarchive(id)
        .await
        .map_err(archive_command_error)?;

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
    let mut queued = 0usize;
    for synced_concert in &synced {
        if matches!(
            state
                .concerts
                .enqueue_scrape(synced_concert.id)
                .map_err(concert_command_error)?,
            ScrapeAdmission::Enqueued
        ) {
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

/// A split-timestamp read failure. Everything but an operational failure is a
/// client-side absence or conflict.
fn split_timestamp_error_to_app_error(error: ConcertCommandError) -> AppError {
    match error {
        ConcertCommandError::Rejected(rejection) => match rejection {
            CommandRejection::NotFound { .. } => AppError::NotFound,
            CommandRejection::Conflict { message, .. } => AppError::Conflict(message),
            CommandRejection::Invalid { message, .. } => AppError::BadRequest(message),
            other => AppError::BadRequest(other.to_string()),
        },
        ConcertCommandError::Operational(error) => AppError::Internal(error),
    }
}

/// A split-timestamp change that the interface rejected. These endpoints answer
/// with a plain-text body and the protocol's own status, which is why the
/// rejection (not an `AppError`) decides the response here.
fn split_timestamp_command_response(error: ConcertCommandError) -> Result<Response, AppError> {
    match error {
        ConcertCommandError::Rejected(rejection) => match rejection {
            CommandRejection::NotFound { .. } => Err(AppError::NotFound),
            CommandRejection::Conflict { message, .. } => {
                Ok((StatusCode::CONFLICT, message).into_response())
            }
            CommandRejection::Invalid { message, .. } => {
                Ok((StatusCode::UNPROCESSABLE_ENTITY, message).into_response())
            }
            other => Ok((StatusCode::UNPROCESSABLE_ENTITY, other.to_string()).into_response()),
        },
        ConcertCommandError::Operational(error) => Err(AppError::Internal(error)),
    }
}

fn split_start_response(outcome: SplitStartOutcome) -> Response {
    match outcome {
        SplitStartOutcome::Splitting => (
            StatusCode::ACCEPTED,
            Json(SplitStartResponse::from(outcome)),
        )
            .into_response(),
        SplitStartOutcome::AlreadyAuto => Json(SplitStartResponse::from(outcome)).into_response(),
    }
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
    let read = state
        .concerts
        .split_timestamps(id)
        .await
        .map_err(split_timestamp_error_to_app_error)?;
    Ok(Json(read.into()))
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
    match state.concerts.set_split_timestamps(id, payload).await {
        Ok(start) => Ok(split_start_response(start.outcome)),
        Err(error) => split_timestamp_command_response(error),
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
    match state.concerts.reset_split_timestamps(id).await {
        Ok(start) => Ok(split_start_response(start.outcome)),
        Err(error) => split_timestamp_command_response(error),
    }
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

impl From<crate::db::playlists::PlaylistMembership> for MembershipJson {
    fn from(m: crate::db::playlists::PlaylistMembership) -> Self {
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
/// `db::playlists::add_playlist_item`.
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
    let id = db::playlists::create_playlist(&conn, &req.name, req.description.as_deref())
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
    let playlists = db::playlists::list_playlists(&conn)?;
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
    let playlist = db::playlists::get_playlist(&conn, id)?.ok_or(AppError::NotFound)?;
    let items = db::playlists::list_playlist_items(&conn, id)?;
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
    let existing = db::playlists::get_playlist(&conn, id)?.ok_or(AppError::NotFound)?;
    let name = req.name.unwrap_or(existing.name);
    // PATCH semantics: a provided description (incl. empty) replaces; omitted
    // keeps the current value. Clearing to NULL is not supported in this phase.
    let description = req.description.or(existing.description);
    db::playlists::update_playlist(&conn, id, &name, description.as_deref())
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
    if db::playlists::delete_playlist(&conn, id)? {
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
    let item_id =
        db::playlists::add_playlist_item(&conn, id, &kind).map_err(AppError::from_playlist)?;
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
    if db::playlists::remove_playlist_item(&conn, id, item_id)? {
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
    db::playlists::reorder_playlist_items(&mut conn, id, &req.item_ids)
        .map_err(AppError::from_playlist)?;
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
    let out = db::playlists::playlists_containing_track(&conn, id, idx)?;
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
    let out = db::playlists::playlists_containing_concert(&conn, id)?;
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
    let out = db::playlists::playlists_nesting_playlist(&conn, id)?;
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
        let playlists = db::playlists::list_playlists(&conn)?;
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
    let (name, description, track_count, total_time, raw_items) = {
        let conn = state.db.lock().unwrap();
        let playlist = db::playlists::get_playlist(&conn, id)?.ok_or(AppError::NotFound)?;
        let raw_items = db::playlists::list_playlist_items(&conn, id)?;
        let summary = crate::playlist::summarize_playlist(&conn, id)?;
        (
            playlist.name,
            playlist.description.unwrap_or_default(),
            summary.track_count,
            crate::model::format_duration_summary(
                summary.known_duration_secs,
                summary.unknown_count,
            ),
            raw_items,
        )
    };
    // Rows are built after the lock is dropped: they observe concerts through the
    // interface, which takes its own lock.
    let mut items = Vec::with_capacity(raw_items.len());
    for item in raw_items {
        items.push(build_item_row(&state, item)?);
    }
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
    state: &AppState,
    item: crate::model::PlaylistItem,
) -> Result<PlaylistItemRow, AppError> {
    use crate::model::PlaylistItemKind::*;
    let row = match item.kind {
        Track {
            concert_id,
            track_index,
        } => match state
            .concerts
            .get_opt(concert_id)
            .map_err(concert_query_error)?
        {
            Some(concert_state) => PlaylistItemRow {
                item_id: item.id,
                kind: "track",
                label: concert_state
                    .concert
                    .set_list
                    .get(track_index)
                    .cloned()
                    .unwrap_or_else(|| format!("Track {}", track_index + 1)),
                sublabel: concert_state.concert.title,
                href: String::new(),
                available: concert_state
                    .tracks
                    .get(track_index)
                    .map(|track| track.persisted_available)
                    .unwrap_or(false),
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
            let concert_state = state
                .concerts
                .get_opt(concert_id)
                .map_err(concert_query_error)?;
            PlaylistItemRow {
                item_id: item.id,
                kind: "concert",
                label: concert_state
                    .as_ref()
                    .map(|concert_state| concert_state.concert.title.clone())
                    .unwrap_or_else(|| "(missing concert)".to_string()),
                sublabel: concert_state
                    .as_ref()
                    .and_then(|concert_state| concert_state.concert.artist.clone())
                    .unwrap_or_default(),
                href: format!("/concerts/{concert_id}"),
                available: true,
            }
        }
        Playlist { child_playlist_id } => PlaylistItemRow {
            item_id: item.id,
            kind: "playlist",
            label: {
                let conn = state.db.lock().unwrap();
                db::playlists::get_playlist(&conn, child_playlist_id)?
                    .map(|p| p.name)
                    .unwrap_or_else(|| "(missing playlist)".to_string())
            },
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
    use crate::db;
    use rusqlite::Connection;

    // `/health` negotiation's full matrix (absent/`*/*`/`text/*`/`application/*`
    // /406) is black-box-tested in `hurl/health.hurl`, matching this repo's
    // convention of putting Accept-header/status-code HTTP behavior there
    // (see `hurl/README.md`). These stay minimal: one direct call per branch
    // to pin the handler's in-process behavior.
    async fn health_body(accept: Option<&str>) -> (StatusCode, String, String) {
        let mut headers = HeaderMap::new();
        if let Some(accept) = accept {
            headers.insert(axum::http::header::ACCEPT, accept.parse().unwrap());
        }
        let response = health(headers).await;
        let status = response.status();
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        (status, content_type, body_text(response).await)
    }

    /// The text a response renders. Markup assertions read this, because what a
    /// person sees in the swapped fragment is the behaviour under test.
    async fn body_text(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// The smallest `AppState` a request handler needs: a real interface over an
    /// in-memory database, a detached scrape queue (nothing enqueues a scrape
    /// here) and an empty Job Registry.
    fn app_state(conn: Connection, workdir: &std::path::Path) -> AppState {
        use crate::jobs::scrape_queue::ScrapeQueue;
        use crate::jobs::{JobConfig, JobRegistry};
        use std::sync::{Arc, Mutex};

        let db = Arc::new(Mutex::new(conn));
        let registry = Arc::new(JobRegistry::new());
        let scrape_queue = ScrapeQueue::detached();
        let jobs = JobConfig::test(workdir.to_path_buf());
        let concerts = crate::concerts::Concerts::new(
            db.clone(),
            workdir.to_path_buf(),
            registry.clone(),
            scrape_queue.clone(),
            jobs.clone(),
        );
        let catalog = concerts.catalog();
        AppState {
            concerts,
            catalog,
            db,
            registry,
            scrape_queue,
            jobs,
        }
    }

    /// A concert that exists and has a set list but no media on disk.
    fn concert_without_media(conn: &Connection) -> i64 {
        db::seeds::SeedContext::new(conn)
            .seed_lifecycle_concert(db::seeds::SeedLifecycleConcert {
                set_list: Some(vec!["Song A".to_string()]),
                ..db::seeds::SeedLifecycleConcert::default()
            })
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn health_defaults_to_text_identity_first_line() {
        let (status, content_type, body) = health_body(None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(content_type.starts_with("text/plain"), "{content_type}");
        assert_eq!(body.lines().next(), Some(SERVICE_IDENTITY));
    }

    #[tokio::test]
    async fn health_returns_json_for_accept_application_json() {
        let (status, content_type, body) = health_body(Some("application/json")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, "application/json");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["service"], SERVICE_IDENTITY);
    }

    #[tokio::test]
    async fn health_rejects_unsupported_accept() {
        let (status, _, _) = health_body(Some("application/xml")).await;
        assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
    }

    /// A superstring of a supported media type must not match — negotiation
    /// compares exact media ranges, not substrings, so `application/
    /// jsonlines` is rejected even though it contains "application/json".
    #[tokio::test]
    async fn health_rejects_type_that_merely_contains_a_supported_one() {
        let (status, _, _) = health_body(Some("application/jsonlines")).await;
        assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
    }

    fn seed_listing(conn: &Connection, url: &str) -> i64 {
        db::seeds::SeedContext::new(conn)
            .seed_listing(db::seeds::SeedListing {
                source_url: Some(url.to_string()),
                title: Some("Test Concert".to_string()),
                concert_date: Some("2026-05-20".to_string()),
                teaser: Some("a teaser".to_string()),
            })
            .unwrap()
            .id
    }

    /// Observe one concert through the interface, exactly as an adapter does.
    fn concert_state(conn: Connection, workdir: &std::path::Path, id: i64) -> ConcertState {
        use crate::concerts::Concerts;
        use std::sync::{Arc, Mutex};
        Concerts::without_workers(Arc::new(Mutex::new(conn)), workdir.to_path_buf())
            .get(id)
            .unwrap()
    }

    fn scraped_concert(
        conn: &Connection,
        workdir: &std::path::Path,
        album: &str,
        set_list: Vec<String>,
    ) -> i64 {
        let id = db::seeds::SeedContext::new(conn)
            .seed_media_concert(
                workdir,
                crate::db::seeds::SeedMediaConcert {
                    lifecycle: crate::db::seeds::SeedLifecycleConcert {
                        album: Some(album.to_string()),
                        artist: Some("Artist".to_string()),
                        set_list: Some(set_list),
                        ..crate::db::seeds::SeedLifecycleConcert::default()
                    },
                    ..crate::db::seeds::SeedMediaConcert::default()
                },
            )
            .unwrap();
        assert!(id.metadata_scraped_at.is_some());
        id.id
    }

    #[test]
    fn listing_card_uses_thumbnail_and_detail_card_uses_preview() {
        let workdir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let id = scraped_concert(&conn, workdir.path(), "Some Album", vec![]);
        let state = concert_state(conn, workdir.path(), id);

        let listing = render_listing_card(&state).unwrap();
        assert!(listing.contains("class=\"card-thumb\""), "html: {listing}");
        assert!(
            listing.contains("/thumbnails/Some Album.jpg"),
            "html: {listing}"
        );

        let detail = render_detail_card(&state).unwrap();
        assert!(detail.contains("class=\"card-thumb\""), "html: {detail}");
        assert!(
            detail.contains("/concert-files/Some Album/preview.jpg"),
            "html: {detail}"
        );
    }

    #[test]
    fn listing_card_omits_thumbnail_when_not_scraped() {
        let conn = db::connection::open_in_memory().unwrap();
        let id = seed_listing(&conn, "https://example.org/unscraped");
        let workdir = tempfile::tempdir().unwrap();
        let state = concert_state(conn, workdir.path(), id);
        assert!(state.concert.metadata_scraped_at.is_none());

        let html = render_listing_card(&state).unwrap();
        assert!(!html.contains("card-thumb"), "html: {html}");
    }

    /// A downloaded+split concert with 4 set-list tracks, one already deleted
    /// (3 of 4 present).
    fn split_concert(conn: &Connection, workdir: &std::path::Path) -> i64 {
        db::seeds::SeedContext::new(conn)
            .seed_media_concert(
                workdir,
                crate::db::seeds::SeedMediaConcert {
                    lifecycle: crate::db::seeds::SeedLifecycleConcert {
                        album: Some("Some Album".to_string()),
                        artist: Some("Artist".to_string()),
                        set_list: Some(vec![
                            "Song A".to_string(),
                            "Song B".to_string(),
                            "Song C".to_string(),
                            "Song D".to_string(),
                        ]),
                        downloaded: true,
                        split: true,
                        tracks_present: Some(vec![true, true, false, true]),
                        ..crate::db::seeds::SeedLifecycleConcert::default()
                    },
                    source_file: true,
                    track_files: Some(vec![0, 1, 3]),
                    ..crate::db::seeds::SeedMediaConcert::default()
                },
            )
            .unwrap()
            .id
    }

    #[test]
    fn swapped_card_embeds_expanded_list_with_fresh_count() {
        let workdir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let id = split_concert(&conn, workdir.path());
        db::settings::update_archive_location(&conn, "/archive").unwrap();
        let state = concert_state(conn, workdir.path(), id);

        let html = render_card_html(&state, CardVariant::Swap).unwrap();
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
    fn listing_card_renders_collapsed_without_an_embedded_track_list() {
        let workdir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let id = split_concert(&conn, workdir.path());
        let state = concert_state(conn, workdir.path(), id);

        let html = render_listing_card(&state).unwrap();
        assert!(!html.contains("track-list"), "html: {html}");
        // The tracks-button count renders regardless of expansion.
        assert!(html.contains("(3/4)"), "html: {html}");
    }

    #[test]
    fn card_shows_tracks_row_before_split_without_action_buttons() {
        let workdir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let id = scraped_concert(
            &conn,
            workdir.path(),
            "Some Album",
            vec!["Song A".to_string(), "Song B".to_string()],
        );
        let state = concert_state(conn, workdir.path(), id);

        let html = render_listing_card(&state).unwrap();
        // Tracks row renders pre-split with the not-split status and 0/N count.
        assert!(html.contains("not-split (0/2)"), "html: {html}");
        assert!(html.contains("Player.playTracks"), "html: {html}");
        // The Split button, tracks-row Play button, and delete-split button
        // are gone.
        assert!(
            !html.contains(&format!("/concerts/{id}/split")),
            "html: {html}"
        );
        assert!(!html.contains(">Split<"), "html: {html}");
        assert!(!html.contains("/delete-split"), "html: {html}");
        assert!(
            !html.contains("btn-listen\" onclick=\"Player.playTracks"),
            "html: {html}"
        );
    }

    #[test]
    fn card_disables_tracks_button_while_split_is_active() {
        let workdir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let id = split_concert(&conn, workdir.path());
        db::lifecycle::try_mark_split_started(&conn, id).unwrap();
        let state = concert_state(conn, workdir.path(), id);
        assert!(state.permitted_actions.tracks_busy);

        let html = render_listing_card(&state).unwrap();
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

    // ── command error mapping ────────────────────────────────────────────────

    /// A rejection is an expected domain outcome and an operational error is a
    /// fault, and the HTTP adapter must keep them apart: every rejection becomes
    /// a client error, only a fault becomes a 500. Collapsing a rejection into a
    /// 500 would turn "this request cannot apply to the concert" into "the server
    /// is broken".
    #[test]
    fn command_rejections_are_client_errors_and_faults_are_server_errors() {
        let rejections = [
            CommandRejection::NotFound { concert_id: 7 },
            CommandRejection::NoSuchTrack {
                concert_id: 7,
                track_index: 3,
            },
            CommandRejection::MissingMedia {
                concert_id: 7,
                target: ListenTarget::Source,
            },
            CommandRejection::SourceNotRedundant { concert_id: 7 },
            CommandRejection::Conflict {
                concert_id: 7,
                message: "conflicts with the current state".to_string(),
            },
            CommandRejection::Invalid {
                concert_id: 7,
                message: "not a change the concert can accept".to_string(),
            },
        ];
        for rejection in rejections {
            let status = concert_command_error(ConcertCommandError::Rejected(rejection.clone()))
                .into_response()
                .status();
            assert!(
                status.is_client_error(),
                "{rejection} is an expected rejection, so it must be a client error, got {status}"
            );
        }

        let status = concert_command_error(ConcertCommandError::Operational(anyhow::anyhow!(
            "the database is gone"
        )))
        .into_response()
        .status();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    // ── Job Request outcomes ─────────────────────────────────────────────────

    /// A rejected Job Request answers with the reason's own client-error status,
    /// not a server error: the request was well formed, the concert cannot do
    /// it. This is the mapping the download and split routes rely on now that a
    /// rejection arrives as an outcome value.
    #[test]
    fn a_rejected_job_request_answers_with_the_reasons_client_error() {
        let rejection = JobRequestOutcome::Rejected(JobRejection {
            reason: CommandRejection::NotFound { concert_id: 7 },
            state: None,
        });

        let status = match job_request_response(rejection) {
            Ok(_) => panic!("a rejected Job Request is not a card"),
            Err(error) => error.into_response().status(),
        };

        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// Download and split for a concert that is not there stay 404s: the
    /// interface refuses them without a state, and the adapter reports the
    /// reason rather than failing.
    #[tokio::test]
    async fn job_requests_for_an_absent_concert_stay_404s() {
        let workdir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let state = app_state(conn, workdir.path());

        let download_status = match download(State(state.clone()), Path(i64::MAX)).await {
            Ok(_) => panic!("an absent concert cannot render a card"),
            Err(error) => error.into_response().status(),
        };
        assert_eq!(download_status, StatusCode::NOT_FOUND);

        let split_status = match split(State(state), Path(i64::MAX)).await {
            Ok(_) => panic!("an absent concert cannot render a card"),
            Err(error) => error.into_response().status(),
        };
        assert_eq!(split_status, StatusCode::NOT_FOUND);
    }

    /// `POST /prepare` on a concert with no set list stays the historical 422
    /// with the reason as the body — user-correctable, not a server error. The
    /// rejection now carries the Concert State, which this HTML adapter has no
    /// place for.
    #[tokio::test]
    async fn preparing_without_a_set_list_answers_422_not_a_server_error() {
        let workdir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let id = db::seeds::SeedContext::new(&conn)
            .seed_scraped_concert(db::seeds::SeedScrapedConcert {
                set_list: Some(vec![]),
                ..db::seeds::SeedScrapedConcert::default()
            })
            .unwrap()
            .id;
        let state = app_state(conn, workdir.path());

        let Some(response) = prepare_concert(State(state), Path(id)).await.ok() else {
            panic!("no set list is user-correctable, not a fault");
        };

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = body_text(response).await;
        assert!(body.contains("no set list"), "{body}");
    }

    // ── playback error mapping ───────────────────────────────────────────────

    /// Playback resolution separates three situations and the HTTP adapter must
    /// keep them apart: an absent concert and "nothing to play" are both 404s,
    /// while a concert recorded as downloaded whose file is gone is a
    /// data-integrity fault. Answering the last with 404 would make a broken
    /// record look like an ordinary un-played concert.
    #[test]
    fn playback_errors_map_absence_to_404_and_integrity_to_500() {
        let status =
            |error: ConcertPlaybackError| concert_playback_error(error).into_response().status();

        assert_eq!(
            status(ConcertPlaybackError::NotFound { concert_id: 7 }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(ConcertPlaybackError::Lookup(
                PlaybackLookupError::NotPlayable
            )),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(ConcertPlaybackError::Lookup(
                PlaybackLookupError::MarkedDownloadedButMissing { concert_id: 7 }
            )),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status(ConcertPlaybackError::Lookup(
                PlaybackLookupError::InvalidFilename
            )),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status(ConcertPlaybackError::Query(ConcertQueryError::Operational(
                anyhow::anyhow!("the database is gone")
            ))),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    /// The listen/watch buttons carry their own pass/fail state, so a rejection
    /// is the button's error state rather than an HTTP error: the click is well
    /// formed and only the media is missing. An absent concert is still the
    /// page's 404 and a fault is still a fault.
    #[test]
    fn media_command_rejections_are_button_states_not_http_errors() {
        assert_eq!(
            media_command_state(
                Err(ConcertCommandError::Rejected(
                    CommandRejection::MissingMedia {
                        concert_id: 7,
                        target: ListenTarget::Source,
                    }
                )),
                "listen"
            )
            .ok(),
            Some("error")
        );

        let failure_status = |outcome: Result<ConcertState, ConcertCommandError>| {
            media_command_state(outcome, "listen")
                .err()
                .map(|error| error.into_response().status())
        };
        assert_eq!(
            failure_status(Err(ConcertCommandError::Rejected(
                CommandRejection::NotFound { concert_id: 7 }
            ))),
            Some(StatusCode::NOT_FOUND)
        );
        assert_eq!(
            failure_status(Err(ConcertCommandError::Operational(anyhow::anyhow!(
                "the database is gone"
            )))),
            Some(StatusCode::INTERNAL_SERVER_ERROR)
        );
    }

    /// A listen button reports its own failure instead of failing the request,
    /// and a play that never happened is not recorded. The hurl suite cannot
    /// carry this case (see the Change Record), so the handler is exercised
    /// directly: 200, the button's `error` state, no `listen` event.
    #[tokio::test]
    async fn listen_without_media_is_a_button_state_not_an_http_error() {
        let workdir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let id = concert_without_media(&conn);
        let state = app_state(conn, workdir.path());

        let Some(listen_response) = listen(State(state.clone()), Path(id))
            .await
            .ok()
            .map(askama_axum::IntoResponse::into_response)
        else {
            panic!("a rejected listen is the button's state, not an HTTP error");
        };
        assert_eq!(listen_response.status(), StatusCode::OK);
        let listen_body = body_text(listen_response).await;
        assert!(listen_body.contains("btn-listen-error"), "{listen_body}");

        let Some(track_response) = listen_track(State(state.clone()), Path((id, 0)))
            .await
            .ok()
            .map(askama_axum::IntoResponse::into_response)
        else {
            panic!("a rejected track listen is the button's state, not an HTTP error");
        };
        assert_eq!(track_response.status(), StatusCode::OK);
        let track_body = body_text(track_response).await;
        assert!(track_body.contains("btn-listen-error"), "{track_body}");

        assert!(
            state
                .concerts
                .event_history(id)
                .unwrap()
                .iter()
                .all(|event| event.event != "listen"),
            "a listen whose media is absent must not become audit history"
        );
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
