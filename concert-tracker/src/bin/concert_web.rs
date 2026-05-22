use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use concert_tracker::db;
use concert_tracker::jobs::{JobConfig, JobRegistry};
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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let conn = db::open(&cli.db)?;

    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        registry: Arc::new(JobRegistry::new()),
        jobs: JobConfig::production(cli.workdir),
    };

    let app = router(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], cli.port));
    println!("Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
