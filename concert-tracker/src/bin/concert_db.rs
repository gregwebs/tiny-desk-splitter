use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use concert_tracker::db;
use concert_tracker::import::import_dir;
use concert_tracker::model::{sanitize_album, Concert};
use concert_tracker::scan::scan;
use concert_tracker::scrape::scrape_url;
use concert_tracker::sync::{sync_months, YearMonth};

#[derive(Parser)]
#[command(name = "concert-db", about = "Tiny Desk concert database CLI")]
struct Cli {
    #[arg(long, default_value = "concerts.db")]
    db: PathBuf,

    /// Working directory where downloaded media and preview images live.
    #[arg(long, default_value = ".")]
    workdir: PathBuf,

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
    /// Backfill missing teasers by re-scraping concert pages for og:description
    BackfillTeasers,
    /// Update concert JSON files on disk to include teasers from the database
    UpdateJsonTeasers,
    /// Backfill the events table from existing concert data
    BackfillEvents,
    /// Backfill track_delete events by comparing set_list against files on disk
    BackfillTrackDeletes,
    /// Backfill split events with track names and count
    BackfillSplitTracks,
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
            scrape_url(&conn, &url, &cli.workdir)?;
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
                    "[{}] {} | {} | {}+{}",
                    c.id,
                    c.title,
                    c.concert_status().slug(),
                    c.download_status().slug(),
                    c.split_status().slug()
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

        Command::BackfillTeasers => {
            let concerts = db::list_concerts_missing_teaser(&conn)?;
            println!("Found {} concerts missing teasers", concerts.len());
            let mut success = 0;
            let mut failed = 0;
            for c in &concerts {
                match backfill_teaser(&conn, c) {
                    Ok(true) => {
                        println!("  [{}] {} — teaser set", c.id, c.title);
                        success += 1;
                    }
                    Ok(false) => {
                        println!("  [{}] {} — no og:description found", c.id, c.title);
                        failed += 1;
                    }
                    Err(e) => {
                        eprintln!("  [{}] {} — error: {}", c.id, c.title, e);
                        failed += 1;
                    }
                }
            }
            println!("Backfilled {} teasers ({} failed/missing)", success, failed);
        }

        Command::BackfillEvents => {
            let count = concert_tracker::events::backfill(&conn)?;
            println!("Backfilled {} events", count);
        }

        Command::BackfillTrackDeletes => {
            let count = concert_tracker::events::backfill_track_deletes(&conn, &cli.workdir)?;
            println!("Backfilled {} track_delete events", count);
        }

        Command::BackfillSplitTracks => {
            let count = concert_tracker::events::backfill_split_tracks(&conn)?;
            println!("Backfilled {} split events with track info", count);
        }

        Command::UpdateJsonTeasers => {
            let concerts = db::list_concerts(&conn)?;
            let mut updated = 0;
            let mut skipped = 0;
            for c in &concerts {
                let teaser = match c.teaser.as_deref() {
                    Some(t) if !t.is_empty() => t,
                    _ => {
                        skipped += 1;
                        continue;
                    }
                };
                let album = match c.album.as_deref() {
                    Some(a) => a,
                    None => {
                        skipped += 1;
                        continue;
                    }
                };
                let artist = match c.artist.as_deref() {
                    Some(a) => a,
                    None => {
                        skipped += 1;
                        continue;
                    }
                };
                match update_json_teaser(&cli.workdir, album, artist, teaser) {
                    Ok(true) => {
                        println!("  updated {}", c.title);
                        updated += 1;
                    }
                    Ok(false) => skipped += 1,
                    Err(e) => {
                        eprintln!("  [{}] {} — error: {}", c.id, c.title, e);
                        skipped += 1;
                    }
                }
            }
            println!(
                "Updated {} JSON files ({} skipped/missing)",
                updated, skipped
            );
        }
    }

    Ok(())
}

fn backfill_teaser(conn: &rusqlite::Connection, concert: &Concert) -> Result<bool> {
    let html = tiny_desk_scraper::fetch_html(&concert.source_url)?;
    match tiny_desk_scraper::extract_teaser_from_html(&html) {
        Some(teaser) => {
            db::set_teaser(conn, concert.id, &teaser)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

fn update_json_teaser(
    workdir: &std::path::Path,
    album: &str,
    _artist: &str,
    teaser: &str,
) -> Result<bool> {
    let dir = workdir.join("concerts").join(sanitize_album(album));
    let path = dir.join("concert.json");
    if !path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&path)?;
    let mut value: serde_json::Value = serde_json::from_str(&content)?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "teaser".to_string(),
            serde_json::Value::String(teaser.to_string()),
        );
    }
    let json = serde_json::to_string_pretty(&value)?;
    std::fs::write(&path, json)?;
    Ok(true)
}
