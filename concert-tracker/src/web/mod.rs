pub mod handlers;

use std::sync::{Arc, Mutex};

use axum::{
    routing::{get, post},
    Router,
};
use rusqlite::Connection;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::jobs::{JobConfig, JobRegistry};

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub registry: Arc<JobRegistry>,
    pub jobs: JobConfig,
}

pub fn router(state: AppState) -> Router {
    let previews_dir = state.jobs.working_dir.join("previews");
    Router::new()
        .route("/", get(handlers::list))
        .route("/concerts/:id", get(handlers::detail))
        .route("/concerts/:id/ignore", post(handlers::ignore))
        .route("/concerts/:id/want", post(handlers::want))
        .route("/concerts/:id/notes", post(handlers::notes))
        .route("/concerts/:id/scrape", post(handlers::scrape_concert))
        .route("/concerts/:id/download", post(handlers::download))
        .route("/concerts/:id/delete-download", post(handlers::delete_download))
        .route("/concerts/:id/split", post(handlers::split))
        .route("/concerts/:id/delete-split", post(handlers::delete_split))
        .route("/concerts/:id/listen", post(handlers::listen))
        .route("/concerts/:id/status", get(handlers::status_row))
        .route("/sync", post(handlers::sync_now))
        .nest_service("/previews", ServeDir::new(previews_dir))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
