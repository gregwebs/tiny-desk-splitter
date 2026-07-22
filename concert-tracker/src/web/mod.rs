pub mod handlers;
pub mod openapi;

use std::sync::{Arc, Mutex};

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use rusqlite::Connection;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa_swagger_ui::SwaggerUi;

use crate::jobs::scrape_queue::ScrapeQueue;
use crate::jobs::{JobConfig, JobRegistry};
use openapi::ApiDoc;

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
    let (router, api) = api_router();
    let concert_files = Router::new()
        .fallback_service(ServeDir::new(concerts_dir.clone()))
        .layer(middleware::from_fn_with_state(
            concerts_dir,
            lock_concert_media,
        ));

    let router = router
        .merge(static_js_router(opts.dev))
        // Always-on (not gated by `opts.dev`): this is a self-hosted, single-user
        // tool, so exposing the JSON API's shape carries no real risk, and having
        // the docs handy in prod is worth more than hiding them. Mounted before
        // the trace/error-logging layers so Swagger UI's own requests are traced
        // like everything else.
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", api))
        .nest("/concert-files", concert_files)
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

async fn lock_concert_media(
    State(concerts_dir): State<std::path::PathBuf>,
    req: Request,
    next: Next,
) -> Response {
    let Some(encoded_album_dir) = req.uri().path().trim_start_matches('/').split('/').next() else {
        return next.run(req).await;
    };
    let album_dir = match percent_encoding::percent_decode_str(encoded_album_dir).decode_utf8() {
        Ok(album_dir) if !album_dir.is_empty() && !album_dir.contains(['/', '\\']) => album_dir,
        _ => return (StatusCode::BAD_REQUEST, "Invalid concert media path").into_response(),
    };
    if album_dir == "." || album_dir == ".." {
        return next.run(req).await;
    }
    let canonical_dir = concerts_dir.join(album_dir.as_ref());
    let lock = match tokio::task::spawn_blocking(move || {
        live_set_splitter::publication::SharedPublicationLock::acquire(&canonical_dir)
    })
    .await
    {
        Ok(Ok(lock)) => lock,
        Ok(Err(error)) => {
            tracing::error!(%error, "could not acquire Concert Split media read lock");
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
        Err(error) => {
            tracing::error!(%error, "Concert Split media read-lock task failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    };
    let response = next.run(req).await;
    drop(lock);
    response
}

/// Builds the full route table (HTML/htmx pages + JSON API) and the OpenAPI
/// doc for the JSON subset, split apart. Doesn't need an `AppState` *value* —
/// only its type — so [`crate::web::openapi::tests`] can call this directly to
/// inspect the doc actually served, without standing up a database.
///
/// Annotated JSON-API handlers use `.routes(routes!(...))`: the handler's
/// `#[utoipa::path]` attribute (in handlers.rs) is the single source of truth
/// for both the axum route and the OpenAPI doc entry — see web/openapi.rs.
/// Plain HTML/htmx handlers (no utoipa annotation) keep using `.route(...)`.
pub(crate) fn api_router() -> (Router<AppState>, utoipa::openapi::OpenApi) {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
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
        .routes(routes!(handlers::prepare_concert))
        .routes(routes!(handlers::prepare_status))
        .route("/concerts/:id/delete-split", post(handlers::delete_split))
        .routes(routes!(
            handlers::get_split_timestamps,
            handlers::set_split_timestamps
        ))
        .routes(routes!(handlers::reset_split_timestamps))
        .route(
            "/concerts/:id/delete-redundant-source",
            post(handlers::delete_redundant_source),
        )
        .routes(routes!(handlers::concert_playback))
        .route(
            "/concerts/:id/interludes/:idx/delete",
            post(handlers::delete_interlude),
        )
        .route("/concerts/:id/listen", post(handlers::listen))
        .route("/concerts/:id/watch", post(handlers::watch))
        .routes(routes!(handlers::media_info))
        .route("/concerts/:id/tracks", get(handlers::tracks))
        .routes(routes!(handlers::track_details))
        .route(
            "/concerts/:id/tracks/:idx/listen",
            post(handlers::listen_track),
        )
        .routes(routes!(handlers::track_media_info))
        .routes(routes!(handlers::next_track_media_info))
        .routes(routes!(handlers::prev_track_media_info))
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
        // with the Phase-2 HTML pages at /playlists and /playlists/:id. Routes +
        // OpenAPI paths both come from each handler's #[utoipa::path].
        .routes(routes!(handlers::list_playlists, handlers::create_playlist))
        .routes(routes!(
            handlers::get_playlist,
            handlers::update_playlist,
            handlers::delete_playlist
        ))
        .routes(routes!(handlers::add_playlist_item))
        .routes(routes!(handlers::reorder_playlist_items))
        .routes(routes!(handlers::remove_playlist_item))
        .routes(routes!(handlers::playlist_nested_in))
        .routes(routes!(handlers::concert_playlists))
        .routes(routes!(handlers::track_playlists))
        .route("/sync/:year/:month", post(handlers::sync_month_handler))
        .split_for_parts()
}

/// The OpenAPI doc as actually served, paths included. `ApiDoc::openapi()`
/// alone only carries info/tags/components — paths are contributed by
/// [`api_router`] at router-build time via `routes!`, not by the
/// `#[derive(OpenApi)]` macro. Used by `openapi`'s tests, and by the
/// `openapi-dump` binary to print the spec without standing up a database
/// (this needs only `AppState`'s *type*, not a value — `api_router` is
/// generic over the router's state type, never constructs one).
pub fn built_api_doc() -> utoipa::openapi::OpenApi {
    api_router().1
}

/// `/static/*` routes (JS + CSS). In prod, assets are embedded via
/// `include_str!` so the binary is self-contained. In dev, they're served
/// from disk so edits show up on a browser refresh without a recompile —
/// see [`RouterOpts::dev`].
fn static_js_router(dev: bool) -> Router<AppState> {
    if !dev {
        return Router::new()
            .route("/static/player.js", get(handlers::player_js))
            .route("/static/playlists.js", get(handlers::playlists_js))
            .route("/static/splitter.js", get(handlers::splitter_js))
            .route("/static/htmx.min.js", get(handlers::htmx_js))
            .route("/static/style.css", get(handlers::style_css));
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
