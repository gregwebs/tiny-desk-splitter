use anyhow::Result;
use clap::Parser;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
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

    /// Port to listen on. Use 0 for an ephemeral port (the chosen port is
    /// printed once the listener is bound).
    #[arg(long, default_value = "3000")]
    port: u16,

    /// Address to bind. Defaults to 127.0.0.1 (loopback-only). Set to 0.0.0.0
    /// to listen on all interfaces — required when running inside a container.
    /// Also read from the HOST environment variable.
    #[arg(long, env = "HOST", default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
    host: IpAddr,

    /// Path to the `live-set-splitter` binary. Defaults to a sibling of the
    /// running executable, falling back to a PATH lookup of `live-set-splitter`.
    #[arg(long)]
    splitter_bin: Option<PathBuf>,

    /// Program used to open a media file in the system player (the watch/Open
    /// buttons). Defaults to `open` (macOS). Override (e.g. `true`) to make it a
    /// no-op, mainly for tests.
    #[arg(long, default_value = "open")]
    open_cmd: String,

    /// Build HTTP clients with no proxy (direct egress). Skips reqwest's macOS
    /// SystemConfiguration proxy lookup, which is blocked (and panics) in some
    /// sandboxes. Defaults to using the system proxy.
    #[arg(long, default_value_t = false)]
    no_proxy: bool,

    /// Build HTTP clients using the proxy from the environment
    /// (`HTTPS_PROXY`/`HTTP_PROXY`/`ALL_PROXY`) while skipping the macOS
    /// SystemConfiguration lookup. For sandboxes that require an egress proxy but
    /// block that lookup. Mutually exclusive with `--no-proxy`.
    #[arg(long, default_value_t = false, conflicts_with = "no_proxy")]
    proxy_from_env: bool,
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
    // Apply the proxy setting before any scrape builds an HTTP client.
    tiny_desk_scraper::set_proxy_mode(tiny_desk_scraper::proxy_mode_from_flags(
        cli.no_proxy,
        cli.proxy_from_env,
    ));
    tracing::debug!("opening database: {:?}", cli.db);
    let conn = db::open(&cli.db)?;
    tracing::debug!("database opened");

    // Recover from unclean shutdowns: any row still flagged Downloading or
    // Splitting belongs to a process that's no longer running. Move them to
    // *Error so the slot UI exposes a retry button instead of pinning the
    // concert at an unactionable "splitting" / "downloading" badge.
    tracing::debug!("failing in-progress jobs");
    let (stale_dl, stale_sp, stale_ar) = db::fail_in_progress_jobs(&conn, "server restarted")?;
    if stale_dl + stale_sp + stale_ar > 0 {
        tracing::info!(
            "marked {} stale download(s), {} stale split(s), and {} stale archive(s) as failed on startup",
            stale_dl,
            stale_sp,
            stale_ar
        );
    }
    tracing::debug!("fail_in_progress_jobs done");

    tracing::debug!("backfilling tracks_present");
    let workdir_for_backfill = cli.workdir.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let count = concert_tracker::scan::backfill_tracks_present(&conn, &workdir_for_backfill);
        let _ = tx.send((conn, count));
    });
    let conn = match rx.recv_timeout(std::time::Duration::from_secs(30)) {
        Ok((conn, count)) => {
            if count > 0 {
                tracing::info!("backfilled tracks_present for {} concert(s)", count);
            }
            tracing::debug!("backfill done");
            conn
        }
        Err(_) => {
            tracing::warn!("tracks_present backfill timed out (NAS may be unavailable), skipping");
            db::open(&cli.db)?
        }
    };

    let splitter_bin = cli.splitter_bin.unwrap_or_else(default_splitter_bin);

    tracing::debug!("checking dependencies");
    for warning in check_dependencies(&splitter_bin) {
        tracing::warn!("{}", warning);
    }
    tracing::debug!("dependency check done");

    let db = Arc::new(Mutex::new(conn));
    let workdir = cli.workdir;
    let scrape_queue =
        concert_tracker::jobs::scrape_queue::ScrapeQueue::start(db.clone(), workdir.clone());
    let state = AppState {
        db,
        registry: Arc::new(JobRegistry::new()),
        jobs: JobConfig::production(workdir, splitter_bin, cli.open_cmd),
        scrape_queue,
    };

    let app = router(state.clone());
    let addr = SocketAddr::from((cli.host, cli.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    // Always print the *bound* local_addr (not cli.host) so callers learn the
    // real port when --port 0 was used (ephemeral). The e2e fixture parser in
    // e2e/fixtures.js matches "Listening on http://127.0.0.1:<port>" — that
    // pattern holds as long as the default --host stays at loopback; do not
    // change this line to echo cli.host or tests will break for container runs.
    let bound = listener.local_addr()?;
    println!("Listening on http://{}", bound);
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());

    // Streaming media connections (the JS player reads from /concert-files)
    // stay open for the whole song, so a pure graceful shutdown would block
    // until playback ends. Cap the drain at SHUTDOWN_GRACE and then bail.
    const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
    tokio::select! {
        res = server => res?,
        _ = async {
            tokio::signal::ctrl_c().await.ok();
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        } => {
            tracing::warn!(
                "graceful shutdown exceeded {:?}, abandoning open connections",
                SHUTDOWN_GRACE
            );
        }
    }

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
