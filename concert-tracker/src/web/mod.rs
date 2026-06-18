pub mod handlers;

use std::sync::{Arc, Mutex};

use axum::{
    extract::Request,
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use rusqlite::Connection;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::jobs::scrape_queue::ScrapeQueue;
use crate::jobs::{JobConfig, JobRegistry};

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub registry: Arc<JobRegistry>,
    pub jobs: JobConfig,
    /// Serial background metadata-scrape worker. `Sync` enqueues unscraped
    /// concerts here; listing cards poll until their thumbnail is ready.
    pub scrape_queue: ScrapeQueue,
}

/// Options controlling dev-only wiring. See [`router_with_opts`].
#[derive(Clone, Copy, Default)]
pub struct RouterOpts {
    /// Serve `static/*.js` from disk instead of the `include_str!`-embedded
    /// copies, and inject a livereload script so the browser refreshes
    /// whenever this process restarts. Templates and CSS are askama
    /// compile-time templates, so they still require a recompile to change —
    /// this only makes JS edits instant. Intended for `concert-web --dev`
    /// under a file watcher (see `just dev`); never set in production.
    pub dev: bool,
}

/// Production router: embedded JS, no livereload. Equivalent to
/// `router_with_opts(state, RouterOpts::default())`.
pub fn router(state: AppState) -> Router {
    router_with_opts(state, RouterOpts::default())
}

pub fn router_with_opts(state: AppState, opts: RouterOpts) -> Router {
    let concerts_dir = state.jobs.working_dir.join("concerts");
    let thumbnails_dir = state.jobs.working_dir.join("thumbnails");
    let router = Router::new()
        .route("/", get(handlers::list))
        .route("/concerts/:id", get(handlers::detail))
        .route("/concerts/:id/ignore", post(handlers::ignore))
        .route("/concerts/:id/want", post(handlers::want))
        .route("/concerts/:id/notes", post(handlers::notes))
        .route("/concerts/:id/scrape", post(handlers::scrape_concert))
        .route("/concerts/:id/download", post(handlers::download))
        .route(
            "/concerts/:id/delete-download",
            post(handlers::delete_download),
        )
        // /split and /delete-split have no UI buttons anymore (splitting is
        // automated via /prepare; tracks are deleted one by one). Both stay
        // routed deliberately, as curl-able escape hatches for manual
        // recovery/administration.
        .route("/concerts/:id/split", post(handlers::split))
        .route("/concerts/:id/prepare", post(handlers::prepare_concert))
        .route(
            "/concerts/:id/prepare-status",
            get(handlers::prepare_status),
        )
        .route("/concerts/:id/delete-split", post(handlers::delete_split))
        .route(
            "/concerts/:id/split-timestamps",
            get(handlers::get_split_timestamps).post(handlers::set_split_timestamps),
        )
        .route(
            "/concerts/:id/split-timestamps/reset",
            post(handlers::reset_split_timestamps),
        )
        .route(
            "/concerts/:id/delete-redundant-source",
            post(handlers::delete_redundant_source),
        )
        .route(
            "/concerts/:id/concert-playback",
            get(handlers::concert_playback),
        )
        .route(
            "/concerts/:id/interludes/:idx/delete",
            post(handlers::delete_interlude),
        )
        .route("/concerts/:id/listen", post(handlers::listen))
        .route("/concerts/:id/watch", post(handlers::watch))
        .route("/concerts/:id/media-info", get(handlers::media_info))
        .route("/concerts/:id/tracks", get(handlers::tracks))
        .route(
            "/concerts/:id/tracks/:idx/listen",
            post(handlers::listen_track),
        )
        .route(
            "/concerts/:id/tracks/:idx/media-info",
            get(handlers::track_media_info),
        )
        .route(
            "/concerts/:id/tracks/:idx/next-media-info",
            get(handlers::next_track_media_info),
        )
        .route(
            "/concerts/:id/tracks/:idx/prev-media-info",
            get(handlers::prev_track_media_info),
        )
        .route(
            "/concerts/:id/tracks/:idx/watch",
            post(handlers::watch_track),
        )
        .route(
            "/concerts/:id/tracks/:idx/delete",
            post(handlers::delete_track),
        )
        .route("/concerts/:id/tracks/:idx/like", post(handlers::like_track))
        .route("/concerts/:id/archive", post(handlers::archive))
        .route("/concerts/:id/unarchive", post(handlers::unarchive))
        .route("/concerts/:id/status", get(handlers::status_row))
        .route("/jobs", get(handlers::jobs_list))
        .route("/jobs/count", get(handlers::jobs_count))
        .route("/jobs/:id/log", get(handlers::job_log))
        .route("/jobs/:id/cancel/:kind", post(handlers::cancel_job))
        .route(
            "/settings",
            get(handlers::settings_page).post(handlers::settings_save),
        )
        // Playlists HTML pages (Phase 2a). Distinct from the /api/playlists JSON
        // surface below, which these pages hydrate from.
        .route("/playlists", get(handlers::playlists_page))
        .route("/playlists/:id", get(handlers::playlist_detail_page))
        // Playlists JSON API (Phase 1). Mounted under /api so it doesn't collide
        // with the Phase-2 HTML pages at /playlists and /playlists/:id.
        .route(
            "/api/playlists",
            get(handlers::list_playlists).post(handlers::create_playlist),
        )
        .route(
            "/api/playlists/:id",
            get(handlers::get_playlist)
                .patch(handlers::update_playlist)
                .delete(handlers::delete_playlist),
        )
        .route(
            "/api/playlists/:id/items",
            post(handlers::add_playlist_item),
        )
        .route(
            "/api/playlists/:id/items/reorder",
            post(handlers::reorder_playlist_items),
        )
        .route(
            "/api/playlists/:id/items/:item_id",
            axum::routing::delete(handlers::remove_playlist_item),
        )
        .route(
            "/api/playlists/:id/nested-in",
            get(handlers::playlist_nested_in),
        )
        .route(
            "/api/concerts/:id/playlists",
            get(handlers::concert_playlists),
        )
        .route(
            "/api/concerts/:id/tracks/:idx/playlists",
            get(handlers::track_playlists),
        )
        .route("/sync/:year/:month", post(handlers::sync_month_handler))
        .merge(static_js_router(opts.dev))
        .nest_service("/concert-files", ServeDir::new(concerts_dir))
        .nest_service("/thumbnails", ServeDir::new(thumbnails_dir))
        .layer(middleware::from_fn(log_error_responses))
        .layer(TraceLayer::new_for_http());
    let router = if opts.dev {
        router.layer(tower_livereload::LiveReloadLayer::new())
    } else {
        router
    };
    router.with_state(state)
}

/// `/static/*.js` routes. In prod, JS is embedded via `include_str!` so the
/// binary is self-contained. In dev, it's served from disk so edits show up
/// without a recompile — see [`RouterOpts::dev`].
fn static_js_router(dev: bool) -> Router<AppState> {
    if !dev {
        return Router::new()
            .route("/static/player.js", get(handlers::player_js))
            .route("/static/playlists.js", get(handlers::playlists_js))
            .route("/static/splitter.js", get(handlers::splitter_js))
            .route("/static/htmx.min.js", get(handlers::htmx_js));
    }
    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/static");
    if !std::path::Path::new(static_dir).is_dir() {
        tracing::error!(
            dir = static_dir,
            "dev mode: static dir not found, JS routes will 404"
        );
    }
    Router::new().nest_service("/static", ServeDir::new(static_dir))
}

/// Log any response with a 4xx/5xx status at info level or above so that
/// failing requests (e.g. a button hitting a missing route) are visible in the
/// default `info` logs. The `TraceLayer` only logs responses at debug level.
async fn log_error_responses(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let response = next.run(req).await;
    let status = response.status();
    if status.is_server_error() {
        tracing::error!(%method, %uri, status = status.as_u16(), "request failed");
    } else if status.is_client_error() {
        tracing::warn!(%method, %uri, status = status.as_u16(), "request failed");
    }
    response
}
