use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use concert_tracker::db;
use concert_tracker::jobs::{check_dependencies, default_splitter_bin, JobConfig, JobRegistry};
use concert_tracker::web::{router, AppState};

#[derive(Parser)]
#[command(name = "concert-web", about = "Tiny Desk concert web UI")]
struct Cli {
    #[arg(long, default_value = "concerts.db")]
    db: PathBuf,

    #[arg(long, default_value = ".")]
    workdir: PathBuf,

    #[arg(long, default_value = "3000")]
    port: u16,

    /// Path to the `live-set-splitter` binary. Defaults to a sibling of the
    /// running executable, falling back to a PATH lookup of `live-set-splitter`.
    #[arg(long)]
    splitter_bin: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let conn = db::open(&cli.db)?;

    // Recover from unclean shutdowns: any row still flagged Downloading or
    // Splitting belongs to a process that's no longer running. Move them to
    // *Error so the slot UI exposes a retry button instead of pinning the
    // concert at an unactionable "splitting" / "downloading" badge.
    let (stale_dl, stale_sp, stale_ar) = db::fail_in_progress_jobs(&conn, "server restarted")?;
    if stale_dl + stale_sp + stale_ar > 0 {
        tracing::info!(
            "marked {} stale download(s), {} stale split(s), and {} stale archive(s) as failed on startup",
            stale_dl,
            stale_sp,
            stale_ar
        );
    }

    let backfilled = concert_tracker::scan::backfill_tracks_present(&conn, &cli.workdir);
    if backfilled > 0 {
        tracing::info!("backfilled tracks_present for {} concert(s)", backfilled);
    }

    let splitter_bin = cli.splitter_bin.unwrap_or_else(default_splitter_bin);

    for warning in check_dependencies(&splitter_bin) {
        tracing::warn!("{}", warning);
    }

    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        registry: Arc::new(JobRegistry::new()),
        jobs: JobConfig::production(cli.workdir, splitter_bin),
    };

    let app = router(state.clone());
    let addr = SocketAddr::from(([127, 0, 0, 1], cli.port));
    println!("Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    let cancelled = state.registry.cancel_all();
    if cancelled > 0 {
        tracing::info!("cancelled {} running job(s) during shutdown", cancelled);
        let conn = state.db.lock().unwrap();
        let (dl, sp, ar) = db::fail_in_progress_jobs(&conn, "server shutdown")?;
        tracing::info!(
            "marked {} download(s), {} split(s), and {} archive(s) as failed on shutdown",
            dl,
            sp,
            ar
        );
    }

    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for Ctrl+C");
    tracing::info!("received Ctrl+C, shutting down");
}
