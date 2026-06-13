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

pub fn router(state: AppState) -> Router {
    let concerts_dir = state.jobs.working_dir.join("concerts");
    let thumbnails_dir = state.jobs.working_dir.join("thumbnails");
    Router::new()
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
        .route("/sync/:year/:month", post(handlers::sync_month_handler))
        .route("/static/player.js", get(handlers::player_js))
        .route("/static/splitter.js", get(handlers::splitter_js))
        .route("/static/htmx.min.js", get(handlers::htmx_js))
        .nest_service("/concert-files", ServeDir::new(concerts_dir))
        .nest_service("/thumbnails", ServeDir::new(thumbnails_dir))
        .layer(middleware::from_fn(log_error_responses))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
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
