use anyhow::{Context, Result};
use clap::Parser;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use concert_tracker::db;
use concert_tracker::jobs::{
    check_dependencies, resolve_splitter_cli, JobConfig, JobRegistry, SplitTarget, SplitterCli,
};
#[cfg(feature = "test-control")]
use concert_tracker::test_control::job_driver::{JobDriver, TestControlJobRunner};
#[cfg(feature = "test-control")]
use concert_tracker::test_control::scrape_driver::{scrape_item_fn, ScrapeDriver};
use concert_tracker::web::{router_with_opts, AppState, RouterOpts};

/// Which Concert Split adapter to use. `Library` (default, #141) calls the
/// splitter in-process — no separate `cargo build --bin live-set-splitter`
/// needed for `cargo run --bin concert-web` to split. `Cli` shells out to the
/// splitter binary, for process-level debugging and strict process-kill
/// cancellation. See docs/concert-split.md.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[clap(rename_all = "lowercase")]
enum SplitterMode {
    #[default]
    Library,
    Cli,
}

/// Combine `--splitter`/`--splitter-bin` into a resolved `SplitTarget`,
/// rejecting `--splitter-bin` outside CLI mode. `resolve` is injected (rather
/// than calling `resolve_splitter_cli` directly) so this decision — including
/// the rejection, which touches no filesystem/PATH state — is unit-testable
/// without a real environment.
fn build_split_target(
    mode: SplitterMode,
    splitter_bin: Option<PathBuf>,
    resolve: impl FnOnce(Option<PathBuf>) -> Result<SplitterCli, String>,
) -> Result<SplitTarget, String> {
    if mode == SplitterMode::Library && splitter_bin.is_some() {
        return Err("--splitter-bin requires --splitter cli".to_string());
    }
    match mode {
        SplitterMode::Library => Ok(SplitTarget::Library),
        SplitterMode::Cli => resolve(splitter_bin).map(SplitTarget::Cli),
    }
}

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

    /// Which Concert Split adapter to use: `library` (default, in-process, no
    /// separate splitter build needed) or `cli` (subprocess, for debugging and
    /// strict process-kill cancellation).
    #[arg(long, value_enum, default_value_t = SplitterMode::default())]
    splitter: SplitterMode,

    /// Path to the `live-set-splitter` binary, used only with `--splitter cli`.
    /// Defaults to a sibling of the running executable, falling back to PATH,
    /// falling back (debug builds only) to `cargo run --bin live-set-splitter`.
    /// Rejected (startup error) when `--splitter` is `library`.
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

    /// Dev mode: serve static/*.js from disk (no recompile needed for JS edits)
    /// and inject a livereload script so the browser auto-refreshes whenever
    /// this process restarts (e.g. under `just dev` / cargo-watch). Templates
    /// and CSS are compiled in via askama, so they still require a recompile.
    #[arg(long, default_value_t = false)]
    dev: bool,

    /// Start the feature-gated Test Control API (JSON-RPC, loopback-only) on
    /// this port; use 0 for an ephemeral port. Only available when built with
    /// `--features test-control` — see
    /// docs/change/2026-07-11-hurl-web-integration-tests.md. This flag alone
    /// does nothing without that feature, and the feature alone does not
    /// start the API without this flag.
    #[cfg(feature = "test-control")]
    #[arg(long)]
    test_control_port: Option<u16>,
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

    // Before any real work (no DB connection opened yet): resolve --splitter/
    // --splitter-bin into a SplitTarget, rejecting --splitter-bin outside CLI
    // mode rather than silently ignoring it under the default library adapter.
    let split_target =
        build_split_target(cli.splitter, cli.splitter_bin.clone(), resolve_splitter_cli)
            .map_err(anyhow::Error::msg)
            .context("resolving --splitter target")?;

    tracing::debug!("recovering interrupted Concert Split publications");
    let recovered = recover_split_publications_before_startup(&cli.workdir)?;
    if recovered > 0 {
        tracing::info!(
            "recovered {} interrupted Concert Split publication(s) before startup",
            recovered
        );
    }

    tracing::debug!("opening database: {:?}", cli.db);
    let conn = db::connection::open(&cli.db)?;
    tracing::debug!("database opened");

    // Recover from unclean shutdowns: any row still flagged Downloading or
    // Splitting belongs to a process that's no longer running. Move them to
    // *Error so the slot UI exposes a retry button instead of pinning the
    // concert at an unactionable "splitting" / "downloading" badge.
    tracing::debug!("failing in-progress jobs");
    let stale = concert_tracker::lifecycle::fail_in_progress_jobs(&conn, "server restarted")?;
    let (stale_dl, stale_sp, stale_ar) = stale.as_tuple();
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
            db::connection::open(&cli.db)?
        }
    };

    let splitter_cli_for_deps: Option<&SplitterCli> = match &split_target {
        SplitTarget::Library => None,
        SplitTarget::Cli(resolved) => Some(resolved),
    };

    tracing::debug!("checking dependencies");
    for warning in check_dependencies(splitter_cli_for_deps) {
        tracing::warn!("{}", warning);
    }
    tracing::debug!("dependency check done");

    let db = Arc::new(Mutex::new(conn));
    let workdir = cli.workdir;

    // The Job Driver and Scrape Driver only replace their production
    // counterparts when the Test Control API is actually going to be started
    // (feature compiled in AND --test-control-port passed) — a test-control
    // build run without that flag behaves exactly like a production build,
    // for both jobs and the scrape queue. See
    // docs/change/2026-07-15-job-driver-plan.md and
    // docs/change/2026-07-17-scrape-driver-hurl-migration.md.
    #[cfg(feature = "test-control")]
    type TestControlDrivers = Option<(Arc<JobDriver>, Arc<ScrapeDriver>)>;
    #[cfg(feature = "test-control")]
    let (scrape_queue, jobs, drivers): (
        concert_tracker::jobs::scrape_queue::ScrapeQueue,
        JobConfig,
        TestControlDrivers,
    ) = match cli.test_control_port {
        Some(_) => {
            let job_driver = Arc::new(JobDriver::new());
            let scrape_driver = Arc::new(ScrapeDriver::new());
            let scrape_queue = concert_tracker::jobs::scrape_queue::ScrapeQueue::start_with(
                db.clone(),
                workdir.clone(),
                scrape_item_fn(scrape_driver.clone()),
            );
            let jobs = JobConfig::with_runner(
                workdir.clone(),
                Arc::new(TestControlJobRunner::new(job_driver.clone())),
            );
            (scrape_queue, jobs, Some((job_driver, scrape_driver)))
        }
        None => (
            concert_tracker::jobs::scrape_queue::ScrapeQueue::start(db.clone(), workdir.clone()),
            JobConfig::production(workdir.clone(), split_target, cli.open_cmd.clone()),
            None,
        ),
    };
    #[cfg(not(feature = "test-control"))]
    let (scrape_queue, jobs) = (
        concert_tracker::jobs::scrape_queue::ScrapeQueue::start(db.clone(), workdir.clone()),
        JobConfig::production(workdir.clone(), split_target, cli.open_cmd.clone()),
    );

    let state = AppState {
        db,
        registry: Arc::new(JobRegistry::new()),
        jobs,
        scrape_queue,
    };

    // Bound to a top-level `main` local (not `_ = ...`) so the handle outlives
    // this statement: dropping a jsonrpsee `ServerHandle` stops that server.
    #[cfg(feature = "test-control")]
    let _test_control_handle = match drivers {
        Some((job_driver, scrape_driver)) => {
            let port = cli
                .test_control_port
                .expect("drivers are only built when test_control_port is Some");
            let (handle, bound) = concert_tracker::test_control::start(
                state.clone(),
                job_driver,
                scrape_driver,
                port,
            )
            .await?;
            println!("Test control listening on http://{}", bound);
            Some(handle)
        }
        None => None,
    };

    let app = router_with_opts(state.clone(), RouterOpts { dev: cli.dev });
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
        let counts = concert_tracker::lifecycle::fail_in_progress_jobs(&conn, "server shutdown")?;
        let (dl, sp, ar) = counts.as_tuple();
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

fn recover_split_publications_before_startup(workdir: &Path) -> Result<usize> {
    live_set_splitter::concert_split::recover_publications(&workdir.join("concerts"))
        .context("recovering interrupted Concert Split publications before startup")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_recovery_accepts_a_workdir_without_concerts() {
        let workdir = tempfile::tempdir().unwrap();
        assert_eq!(
            recover_split_publications_before_startup(workdir.path()).unwrap(),
            0
        );
    }

    #[test]
    fn rejects_splitter_bin_under_library_mode() {
        let result = build_split_target(SplitterMode::Library, Some(PathBuf::from("/x")), |_| {
            panic!("resolve must not be called when rejecting up front")
        });
        assert_eq!(
            result,
            Err("--splitter-bin requires --splitter cli".to_string())
        );
    }

    #[test]
    fn library_mode_without_splitter_bin_is_library_target() {
        let result = build_split_target(SplitterMode::Library, None, |_| {
            panic!("resolve must not be called for library mode")
        });
        assert_eq!(result, Ok(SplitTarget::Library));
    }

    #[test]
    fn cli_mode_delegates_resolution_and_wraps_the_result() {
        let result = build_split_target(SplitterMode::Cli, Some(PathBuf::from("/x")), |bin| {
            Ok(SplitterCli::Executable(bin.unwrap()))
        });
        assert_eq!(
            result,
            Ok(SplitTarget::Cli(SplitterCli::Executable(PathBuf::from(
                "/x"
            ))))
        );
    }

    #[test]
    fn cli_mode_propagates_a_resolution_error() {
        let result = build_split_target(SplitterMode::Cli, None, |_| {
            Err("no executable found".to_string())
        });
        assert_eq!(result, Err("no executable found".to_string()));
    }
}
