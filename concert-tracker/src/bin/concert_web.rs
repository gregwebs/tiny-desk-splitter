use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use concert_tracker::db;
use concert_tracker::jobs::{default_splitter_bin, JobConfig, JobRegistry};
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

    let splitter_bin = cli.splitter_bin.unwrap_or_else(default_splitter_bin);
    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        registry: Arc::new(JobRegistry::new()),
        jobs: JobConfig::production(cli.workdir, splitter_bin),
    };

    let app = router(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], cli.port));
    println!("Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
