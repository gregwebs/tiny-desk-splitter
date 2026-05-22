use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use concert_tracker::db;
use concert_tracker::import::import_dir;
use concert_tracker::model::Concert;
use concert_tracker::scan::scan;
use concert_tracker::scrape::scrape_url;
use concert_tracker::sync::{sync_months, YearMonth};

#[derive(Parser)]
#[command(name = "concert-db", about = "Tiny Desk concert database CLI")]
struct Cli {
    #[arg(long, default_value = "concerts.db")]
    db: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sync concert listings from the NPR archive for a range of months
    Sync {
        /// Start month in YYYY-MM format (defaults to current month)
        #[arg(long)]
        from: Option<String>,
        /// End month in YYYY-MM format (defaults to current month)
        #[arg(long)]
        to: Option<String>,
    },
    /// Scrape metadata for a single concert URL
    Scrape { url: String },
    /// Import concert JSON files from a directory
    Import { dir: PathBuf },
    /// Scan a directory for existing downloads and split dirs
    Scan { dir: PathBuf },
    /// List all concerts
    List {
        #[arg(long, default_value = "all")]
        filter: String,
    },
    /// Toggle ignored flag on a concert
    Ignore { id: i64 },
    /// Toggle wanted flag on a concert
    Want { id: i64 },
    /// Clear stale in-progress download/split flags
    ResetInProgress,
    /// Import JSON files + scan directory (one-time backfill)
    InitFromFiles { dir: PathBuf },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let conn = db::open(&cli.db)?;

    match cli.command {
        Command::Sync { from, to } => {
            let current = YearMonth::current();
            let from_ym = from
                .as_deref()
                .map(YearMonth::parse)
                .transpose()?
                .unwrap_or_else(|| YearMonth {
                    year: current.year,
                    month: current.month,
                });
            let to_ym = to
                .as_deref()
                .map(YearMonth::parse)
                .transpose()?
                .unwrap_or_else(|| YearMonth {
                    year: current.year,
                    month: current.month,
                });
            let count = sync_months(&conn, from_ym, to_ym)?;
            println!("Synced {} concerts", count);
        }

        Command::Scrape { url } => {
            scrape_url(&conn, &url)?;
            println!("Scraped {}", url);
        }

        Command::Import { dir } => {
            let count = import_dir(&conn, &dir)?;
            println!("Imported {} concerts", count);
        }

        Command::Scan { dir } => {
            let report = scan(&conn, &dir)?;
            println!(
                "Found {} downloads, {} splits",
                report.downloads_found, report.splits_found
            );
            for e in &report.errors {
                eprintln!("Error: {}", e);
            }
        }

        Command::List { filter } => {
            let concerts = db::list_concerts(&conn)?;
            let filtered: Vec<&Concert> = concerts
                .iter()
                .filter(|c| match filter.as_str() {
                    "wanted" => !c.ignored && c.wanted,
                    "ignored" => c.ignored,
                    "available" => !c.ignored && !c.wanted,
                    _ => true,
                })
                .collect();
            for c in filtered {
                println!(
                    "[{}] {} | {} | {}",
                    c.id,
                    c.title,
                    c.concert_status().slug(),
                    c.processing_status().slug()
                );
            }
        }

        Command::Ignore { id } => {
            db::toggle_ignored(&conn, id)?;
            println!("Toggled ignored for concert {}", id);
        }

        Command::Want { id } => {
            db::toggle_wanted(&conn, id)?;
            println!("Toggled wanted for concert {}", id);
        }

        Command::ResetInProgress => {
            let count = db::reset_in_progress(&conn)?;
            println!("Cleared {} stale in-progress rows", count);
        }

        Command::InitFromFiles { dir } => {
            let imported = import_dir(&conn, &dir)?;
            let report = scan(&conn, &dir)?;
            println!(
                "Imported {} concerts, found {} downloads, {} splits",
                imported, report.downloads_found, report.splits_found
            );
        }
    }

    Ok(())
}
