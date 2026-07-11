use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use concert_tracker::db;
use concert_tracker::import::import_dir;
use concert_tracker::jobs::split::{start_split, StartOutcome};
use concert_tracker::jobs::{
    check_dependencies, default_splitter_bin, JobConfig, JobKey, JobKind, JobRegistry, SplitMode,
};
use concert_tracker::model::{sanitize_album, Concert};
use concert_tracker::scan::scan;
use concert_tracker::scrape::{ensure_thumbnail, scrape_url, ThumbOutcome};
use concert_tracker::sync::{sync_months, YearMonth};

#[derive(Parser)]
#[command(name = "concert-db", about = "Tiny Desk concert database CLI")]
struct Cli {
    #[arg(long, default_value = "concerts.db")]
    db: PathBuf,

    /// Working directory where downloaded media and preview images live.
    #[arg(long, default_value = ".")]
    workdir: PathBuf,

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
    /// Reset stale download errors on downloads that were deleted after erroring
    ClearStaleDownloadErrors,
    /// Import JSON files + scan directory (one-time backfill)
    InitFromFiles { dir: PathBuf },
    /// Backfill missing teasers by re-scraping concert pages for og:description
    BackfillTeasers,
    /// Backfill listing thumbnails from existing preview images on disk
    BackfillThumbnails,
    /// Update concert JSON files on disk to include teasers from the database
    UpdateJsonTeasers,
    /// Backfill the events table from existing concert data
    BackfillEvents,
    /// Backfill track_delete events by comparing set_list against files on disk
    BackfillTrackDeletes,
    /// Backfill split events with track names and count
    BackfillSplitTracks,
    /// Import pre-archived concerts from an archive directory
    ImportArchive { dir: PathBuf },
    /// Normalize concert metadata: merge concert-metadata with in-dir timestamps,
    /// write concert.json, apply to DB, and clean up old files
    NormalizeMetadata {
        /// Directory containing rich metadata JSON files
        #[arg(long, default_value = "concert-metadata")]
        metadata_dir: PathBuf,
        /// Print what would happen without making changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Re-run automated splitting on concerts that were split (or errored) but
    /// have no user-edited timestamps — e.g. after splitter improvements.
    /// Concerts with user-edited timestamps are never touched.
    Resplit {
        /// List the concerts that would be re-split without making any changes.
        #[arg(long)]
        dry_run: bool,
        /// Required to actually mutate the database. Re-splitting rewrites
        /// split_at, tracks_present, and auto_split_timestamps_json across many
        /// rows. Run --dry-run first and back up the database before using this.
        #[arg(long)]
        confirm: bool,
    },
    /// Backfill media_duration for concerts split before the column existed.
    /// ffprobes the source file when it's still present (accurate); otherwise
    /// estimates from stored/on-disk timestamps or summed track durations —
    /// only safe because there's no source left to delete. See
    /// docs/change/2026-06-17-backfill-media-duration.md.
    BackfillMediaDuration {
        /// List the concerts that would be updated, with the value and source
        /// of each, without making any changes.
        #[arg(long)]
        dry_run: bool,
        /// Required to actually write to the database. Backs up the database
        /// file first (alongside the original, suffixed `.bak-<timestamp>`).
        #[arg(long)]
        confirm: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Apply the proxy setting before any scrape builds an HTTP client.
    tiny_desk_scraper::set_proxy_mode(tiny_desk_scraper::proxy_mode_from_flags(
        cli.no_proxy,
        cli.proxy_from_env,
    ));
    let conn = db::connection::open(&cli.db)?;

    match cli.command {
        Command::Sync { from, to } => {
            let current = YearMonth::current();
            let from_ym = from
                .as_deref()
                .map(YearMonth::parse)
                .transpose()?
                .unwrap_or(YearMonth {
                    year: current.year,
                    month: current.month,
                });
            let to_ym = to
                .as_deref()
                .map(YearMonth::parse)
                .transpose()?
                .unwrap_or(YearMonth {
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
            let concerts = db::concerts::list_concerts(&conn)?;
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
            db::concerts::toggle_ignored(&conn, id)?;
            println!("Toggled ignored for concert {}", id);
        }

        Command::Want { id } => {
            db::concerts::toggle_wanted(&conn, id)?;
            println!("Toggled wanted for concert {}", id);
        }

        Command::ResetInProgress => {
            let count = concert_tracker::lifecycle::reset_in_progress(&conn)?;
            println!("Cleared {} stale in-progress rows", count);
        }

        Command::ClearStaleDownloadErrors => {
            let count = db::lifecycle::clear_stale_download_errors(&conn)?;
            println!("Cleared stale download errors for {count} concert(s)");
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
            let concerts = db::concerts::list_concerts_missing_teaser(&conn)?;
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

        Command::BackfillThumbnails => {
            let concerts = db::concerts::list_concerts(&conn)?;
            let mut created = 0;
            let mut present = 0;
            let mut failed = 0;
            for c in &concerts {
                if c.metadata_scraped_at.is_none() {
                    continue;
                }
                let Some(album) = c.album.as_deref() else {
                    continue;
                };
                // Disk-only (no network): derive the thumbnail from the existing
                // preview.jpg. Already-thumbnailed concerts short-circuit before
                // touching the preview path, so an offline archive is a no-op.
                match ensure_thumbnail(&cli.workdir, album, None) {
                    Ok(ThumbOutcome::Created) => {
                        println!("  [{}] {} — thumbnail created", c.id, c.title);
                        created += 1;
                    }
                    Ok(ThumbOutcome::AlreadyPresent) => present += 1,
                    Ok(ThumbOutcome::SourceMissing) => {
                        eprintln!("  [{}] {} — no preview image on disk", c.id, c.title);
                        failed += 1;
                    }
                    Err(e) => {
                        eprintln!("  [{}] {} — error: {}", c.id, c.title, e);
                        failed += 1;
                    }
                }
            }
            println!(
                "Backfilled {created} thumbnails ({present} already present, {failed} missing/failed)"
            );
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

        Command::ImportArchive { dir } => {
            let report =
                concert_tracker::archive_import::import_archive(&conn, &dir, &cli.workdir)?;
            println!(
                "Imported {} concerts ({} skipped, {} errors)",
                report.imported,
                report.skipped,
                report.errors.len()
            );
            if !report.not_in_db.is_empty() {
                println!("\nNot found in database ({}):", report.not_in_db.len());
                for album in &report.not_in_db {
                    println!("  - {}", album);
                }
            }
            for e in &report.errors {
                eprintln!("  Error: {}", e);
            }
        }

        Command::NormalizeMetadata {
            metadata_dir,
            dry_run,
        } => {
            let report = concert_tracker::normalize::normalize_metadata(
                &conn,
                &cli.workdir,
                &metadata_dir,
                dry_run,
            )?;
            println!("Merged: {}", report.merged);
            println!("Scraped: {}", report.scraped);
            println!("Renamed: {}", report.renamed);
            println!("Already had concert.json: {}", report.already_ok);
            println!("Imported to DB: {}", report.imported_to_db);
            println!("Old files removed: {}", report.old_files_removed);
            if !report.missing_source.is_empty() {
                println!(
                    "\nStill missing source URL ({}):",
                    report.missing_source.len()
                );
                for dir in &report.missing_source {
                    println!("  - {}", dir);
                }
            }
            for e in &report.errors {
                eprintln!("Error: {}", e);
            }
        }

        Command::UpdateJsonTeasers => {
            let concerts = db::concerts::list_concerts(&conn)?;
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

        Command::Resplit { dry_run, confirm } => {
            let candidates = db::lifecycle::list_resplit_candidates(&conn)?;
            println!("Found {} resplit candidate(s)", candidates.len());

            if dry_run {
                for c in &candidates {
                    println!("  [{}] {} ({})", c.id, c.title, c.split_status().slug());
                }
                return Ok(());
            }

            if !confirm {
                eprintln!(
                    "WARNING: This will re-run automated splitting on {} concert(s) \
                     using the database {:?}.\n\
                     This rewrites split_at, tracks_present, and auto_split_timestamps_json \
                     across many rows.\n\
                     Run with --dry-run first to preview the affected concerts.\n\
                     Back up the database before proceeding.\n\
                     Re-run with --confirm to proceed.",
                    candidates.len(),
                    cli.db
                );
                return Ok(());
            }

            let splitter_bin = default_splitter_bin();
            for warning in check_dependencies(&splitter_bin) {
                eprintln!("WARNING: {}", warning);
            }

            // Snapshot id, title, and initial split_errors count before any mutation.
            // Capturing the error count up front lets us distinguish a failed re-split
            // from a successful one: a failed re-split keeps the old split_at (so the
            // status slug alone would misreport it as "split"), but always appends to
            // split_errors.
            let concert_infos: Vec<(i64, String, usize)> = candidates
                .iter()
                .map(|c| (c.id, c.title.clone(), c.split_errors.len()))
                .collect();

            // open_cmd "true" is a no-op placeholder; splitting never invokes the open command.
            let db = Arc::new(Mutex::new(conn));
            let registry = Arc::new(JobRegistry::new());
            let config =
                JobConfig::production(cli.workdir.clone(), splitter_bin, "true".to_string());

            let rt = tokio::runtime::Runtime::new()?;
            let (succeeded, failed, skipped_no_source, skipped_in_progress, errored) =
                rt.block_on(async {
                    let mut succeeded = 0usize;
                    let mut failed = 0usize;
                    let mut skipped_no_source = 0usize;
                    let mut skipped_in_progress = 0usize;
                    let mut errored = 0usize;

                    for (id, title, initial_errors) in &concert_infos {
                        let id = *id;
                        let initial_errors = *initial_errors;
                        let key = JobKey { concert_id: id, kind: JobKind::Split };

                        let label = match start_split(
                            db.clone(),
                            registry.clone(),
                            config.clone(),
                            id,
                            SplitMode::Analyze,
                        )
                        .await
                        {
                            Ok(StartOutcome::Spawned) => {
                                // Poll until the async split job finishes.
                                while registry.is_running(&key) {
                                    tokio::time::sleep(Duration::from_millis(200)).await;
                                }
                                // Determine outcome via split_errors count: a failed re-split
                                // keeps the old split_at but always appends an error entry.
                                let post_errors = {
                                    let conn = db.lock().unwrap();
                                    db::concerts::get_concert(&conn, id)
                                        .map(|c| c.split_errors.len())
                                        .unwrap_or(initial_errors + 1)
                                };
                                if post_errors > initial_errors {
                                    failed += 1;
                                    "FAILED"
                                } else {
                                    succeeded += 1;
                                    "OK"
                                }
                            }
                            Ok(StartOutcome::AlreadySplit) => {
                                succeeded += 1;
                                "OK (recovered from disk)"
                            }
                            Ok(StartOutcome::NotDownloaded) => {
                                skipped_no_source += 1;
                                "SKIPPED (source file missing)"
                            }
                            Ok(StartOutcome::AlreadyRunning) => {
                                skipped_in_progress += 1;
                                "SKIPPED (in progress — run `concert-db reset-in-progress` to clear)"
                            }
                            Err(e) => {
                                eprintln!("  [{}] {} — start error: {}", id, title, e);
                                errored += 1;
                                "ERROR"
                            }
                        };
                        println!("  [{}] {} ... {}", id, title, label);
                    }

                    (succeeded, failed, skipped_no_source, skipped_in_progress, errored)
                });

            println!(
                "Re-split complete: {} succeeded, {} failed, \
                 {} skipped (no source), {} skipped (in progress), {} errored",
                succeeded, failed, skipped_no_source, skipped_in_progress, errored
            );
        }

        Command::BackfillMediaDuration { dry_run, confirm } => {
            let report =
                concert_tracker::scan::backfill_media_duration(&conn, &cli.workdir, false)?;
            println!(
                "Found {} concert(s) eligible for media_duration backfill",
                report.planned.len()
            );
            for row in &report.planned {
                println!(
                    "  [{}] {} -> {:.1}s ({:?})",
                    row.id, row.title, row.duration, row.source
                );
            }
            if !report.skipped.is_empty() {
                println!("Skipped {} concert(s):", report.skipped.len());
                for (id, title, reason) in &report.skipped {
                    println!("  [{}] {} — {}", id, title, reason);
                }
            }

            if dry_run {
                return Ok(());
            }
            if !confirm {
                eprintln!(
                    "WARNING: This will write media_duration for {} concert(s) in the \
                     database {:?}.\n\
                     Run with --dry-run first to preview the affected concerts.\n\
                     Re-run with --confirm to back up the database and apply.",
                    report.planned.len(),
                    cli.db
                );
                return Ok(());
            }

            // Back up the database before mutating it (project rule: always back
            // up before changing database data). The connection runs in WAL mode
            // (see db::connection::open), so checkpoint first — otherwise a plain file copy
            // can miss writes still sitting in the `-wal` file and the backup
            // would silently omit recent data.
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .context("Failed to checkpoint WAL before backing up the database")?;
            let backup_path = backup_db_path(&cli.db);
            std::fs::copy(&cli.db, &backup_path).with_context(|| {
                format!(
                    "Failed to back up database to {} — aborting without writing",
                    backup_path.display()
                )
            })?;
            println!("Backed up database to {}", backup_path.display());

            let applied =
                concert_tracker::scan::backfill_media_duration(&conn, &cli.workdir, true)?;
            println!(
                "Wrote media_duration for {} concert(s)",
                applied.planned.len()
            );
        }
    }

    Ok(())
}

/// Path for a pre-backfill database backup: alongside the original, suffixed
/// with `.bak-<UTC timestamp>` so repeated runs never collide or overwrite.
fn backup_db_path(db_path: &std::path::Path) -> std::path::PathBuf {
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let file_name = db_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("concerts.db");
    db_path.with_file_name(format!("{file_name}.bak-{ts}"))
}

fn backfill_teaser(conn: &rusqlite::Connection, concert: &Concert) -> Result<bool> {
    let html = tiny_desk_scraper::fetch_html(&concert.source_url)?;
    match tiny_desk_scraper::extract_teaser_from_html(&html) {
        Some(teaser) => {
            db::concerts::set_teaser(conn, concert.id, &teaser)?;
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
