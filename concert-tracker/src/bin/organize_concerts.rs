use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use concert_tracker::db;
use concert_tracker::model::{concert_dir, sanitize_album, Concert, Musician};
use concert_tracker::scan::scan;

#[derive(Parser)]
#[command(
    name = "organize-concerts",
    about = "Move per-concert files (mp4, preview jpg, metadata json, split tracks) \
             from the flat workspace layout into `concerts/<album>/` and reconcile DB state."
)]
struct Cli {
    #[arg(long, default_value = "concerts.db")]
    db: PathBuf,

    /// Workspace directory holding the loose files and the new `concerts/` tree.
    #[arg(long, default_value = ".")]
    working_dir: PathBuf,

    /// Print planned moves without touching the filesystem or the database.
    #[arg(long)]
    dry_run: bool,
}

/// A single planned filesystem move from `src` to `dst`.
#[derive(Debug, Clone)]
struct Move {
    src: PathBuf,
    dst: PathBuf,
}

/// Summary printed at the end.
#[derive(Default, Debug)]
struct Stats {
    mp4_moved: usize,
    preview_moved: usize,
    json_moved: usize,
    split_files_moved: usize,
    old_dirs_removed: usize,
    skipped_conflicts: usize,
    skipped_missing_album: usize,
}

/// Mirrors live-set-song-splitter's old `folder_name()` so we can locate
/// directories created with the dash convention (e.g. `Air - Tiny Desk Concert`).
fn splitter_legacy_folder(album: &str) -> String {
    album
        .replace(" : ", " - ")
        .replace(": ", " - ")
        .replace(':', "-")
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let working_dir = cli
        .working_dir
        .canonicalize()
        .with_context(|| format!("canonicalize working_dir {}", cli.working_dir.display()))?;

    tracing::info!(
        "organize-concerts starting (working_dir={}, dry_run={})",
        working_dir.display(),
        cli.dry_run
    );

    let conn =
        db::connection::open(&cli.db).with_context(|| format!("open db {}", cli.db.display()))?;
    let concerts = db::concerts::list_concerts(&conn)?;

    let mut stats = Stats::default();
    let mut plan: Vec<Move> = Vec::new();
    let mut empty_dirs_to_remove: Vec<PathBuf> = Vec::new();

    // Per-concert moves: mp4, preview, split dirs (both conventions).
    for c in &concerts {
        let Some(album) = c.album.as_deref() else {
            stats.skipped_missing_album += 1;
            continue;
        };
        let target = concert_dir(&working_dir, album);

        // mp4 at workspace root → target/{sanitized}.mp4
        let root_mp4 = working_dir.join(format!("{}.mp4", sanitize_album(album)));
        if root_mp4.is_file() {
            plan.push(Move {
                src: root_mp4,
                dst: target.join(format!("{}.mp4", sanitize_album(album))),
            });
        }

        // Preview at previews/{sanitized}.jpg → target/preview.jpg
        let preview = working_dir
            .join("previews")
            .join(format!("{}.jpg", sanitize_album(album)));
        if preview.is_file() {
            plan.push(Move {
                src: preview,
                dst: target.join("preview.jpg"),
            });
        }

        // Split tracks: handle either naming convention. The new-convention
        // dir (`{sanitized}`) at workspace root IS the legacy split-output
        // location, distinct from the target which lives under `concerts/`.
        let new_style = working_dir.join(sanitize_album(album));
        let legacy_style = working_dir.join(splitter_legacy_folder(album));
        for split_src in [&new_style, &legacy_style] {
            if !split_src.is_dir() {
                continue;
            }
            // Don't recurse into target itself if the user happens to have
            // already created a top-level dir matching that path.
            if split_src == &target {
                continue;
            }
            collect_dir_moves(split_src, &target, &mut plan)?;
            empty_dirs_to_remove.push(split_src.clone());
        }
    }

    // JSON metadata at workspace root → target/concert.json
    let json_moves = plan_json_moves(&working_dir)?;
    plan.extend(json_moves);

    // Apply the plan.
    for mv in &plan {
        if mv.src == mv.dst {
            continue;
        }
        // JSON-specific conflict resolution: if the in-dir concert.json is a
        // splitter-augmented copy (has a `timestamps` field), rename it to
        // `timestamps.json` so the richer scraper JSON can take the canonical name.
        if mv.dst.exists() && is_json_path(&mv.dst) && is_splitter_timestamps_json(&mv.dst) {
            let renamed = mv.dst.with_file_name("timestamps.json");
            if renamed.exists() {
                tracing::warn!(
                    "skip (both {} and {} already exist)",
                    mv.dst.display(),
                    renamed.display()
                );
                stats.skipped_conflicts += 1;
                continue;
            }
            if cli.dry_run {
                tracing::info!(
                    "[dry-run] rename {} -> {} (splitter timestamps)",
                    mv.dst.display(),
                    renamed.display()
                );
            } else {
                fs::rename(&mv.dst, &renamed).with_context(|| {
                    format!("rename {} -> {}", mv.dst.display(), renamed.display())
                })?;
                tracing::info!(
                    "renamed splitter output {} -> {}",
                    mv.dst.display(),
                    renamed.display()
                );
            }
        }
        if mv.dst.exists() {
            tracing::warn!(
                "skip (destination exists): {} -> {}",
                mv.src.display(),
                mv.dst.display()
            );
            stats.skipped_conflicts += 1;
            continue;
        }
        if cli.dry_run {
            tracing::info!(
                "[dry-run] move {} -> {}",
                mv.src.display(),
                mv.dst.display()
            );
        } else {
            if let Some(parent) = mv.dst.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create_dir_all {}", parent.display()))?;
            }
            fs::rename(&mv.src, &mv.dst)
                .with_context(|| format!("rename {} -> {}", mv.src.display(), mv.dst.display()))?;
            tracing::info!("moved {} -> {}", mv.src.display(), mv.dst.display());
        }
        // Classify for stats.
        match mv
            .dst
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "mp4" if mv.src.parent() == Some(&working_dir) => stats.mp4_moved += 1,
            "jpg" if mv.src.parent().and_then(|p| p.file_name()) == Some("previews".as_ref()) => {
                stats.preview_moved += 1
            }
            "json" if mv.src.parent() == Some(&working_dir) => stats.json_moved += 1,
            _ => stats.split_files_moved += 1,
        }
    }

    // Remove any source split dirs that are now empty.
    for dir in &empty_dirs_to_remove {
        if !dir.exists() {
            continue;
        }
        if dir == &working_dir {
            continue;
        }
        if cli.dry_run {
            tracing::info!(
                "[dry-run] would remove {} if empty after migration",
                dir.display()
            );
            continue;
        }
        match dir_is_empty(dir) {
            Ok(true) => match fs::remove_dir(dir) {
                Ok(()) => {
                    stats.old_dirs_removed += 1;
                    tracing::info!("removed empty dir {}", dir.display());
                }
                Err(e) => tracing::warn!("could not remove {}: {}", dir.display(), e),
            },
            Ok(false) => tracing::warn!("dir not empty, leaving in place: {}", dir.display()),
            Err(e) => tracing::warn!("could not stat {}: {}", dir.display(), e),
        }
    }

    // Reconcile DB: set downloaded_at / split_at where files now exist.
    if cli.dry_run {
        tracing::info!("[dry-run] skipping scan/db reconciliation");
    } else {
        let report = scan(&conn, &working_dir)?;
        tracing::info!(
            "reconcile: downloads_found={} splits_found={} errors={}",
            report.downloads_found,
            report.splits_found,
            report.errors.len()
        );
        for e in &report.errors {
            tracing::warn!("scan error: {}", e);
        }
    }

    // Backfill scraper-style metadata JSON from the DB for any concert dir
    // that doesn't already have one. Catches concerts processed entirely
    // through the in-app flow (no standalone scraper run produced a root JSON).
    let backfilled = backfill_metadata_json(&concerts, &working_dir, cli.dry_run)?;
    tracing::info!("metadata json backfilled: {}", backfilled);

    tracing::info!("done: {:?}", stats);
    Ok(())
}

#[derive(Serialize)]
struct ScraperJson<'a> {
    artist: &'a str,
    source: &'a str,
    show: &'a str,
    date: Option<&'a str>,
    album: &'a str,
    description: Option<&'a str>,
    set_list: Vec<JsonSong<'a>>,
    musicians: Vec<JsonMusician<'a>>,
}

#[derive(Serialize)]
struct JsonSong<'a> {
    title: &'a str,
}

#[derive(Serialize)]
struct JsonMusician<'a> {
    name: &'a str,
    instruments: &'a [String],
}

/// For each concert with album + artist, write `concerts/<album>/concert.json`
/// from DB metadata if the file doesn't already exist. Returns count of files
/// written.
fn backfill_metadata_json(
    concerts: &[Concert],
    working_dir: &Path,
    dry_run: bool,
) -> Result<usize> {
    let mut count = 0;
    for c in concerts {
        let (Some(album), Some(artist)) = (c.album.as_deref(), c.artist.as_deref()) else {
            continue;
        };
        let dir = concert_dir(working_dir, album);
        if !dir.is_dir() {
            continue;
        }
        let dst = dir.join("concert.json");
        if dst.exists() {
            continue;
        }
        let payload = ScraperJson {
            artist,
            source: &c.source_url,
            show: "Tiny Desk Concerts",
            date: c.concert_date.as_deref(),
            album,
            description: c.description.as_deref(),
            set_list: c.set_list.iter().map(|t| JsonSong { title: t }).collect(),
            musicians: c
                .musicians
                .iter()
                .map(|m: &Musician| JsonMusician {
                    name: &m.name,
                    instruments: &m.instruments,
                })
                .collect(),
        };
        let json = serde_json::to_string_pretty(&payload)?;
        if dry_run {
            tracing::info!("[dry-run] would write metadata json {}", dst.display());
        } else {
            fs::write(&dst, json)
                .with_context(|| format!("write metadata json {}", dst.display()))?;
            tracing::info!("wrote metadata json {}", dst.display());
        }
        count += 1;
    }
    Ok(count)
}

/// Walk every file directly under `src` and queue a move to `dst/{filename}`.
/// Subdirectories are not recursed — split outputs are flat. Dotfiles
/// (leftover splitter scratch like `.tmp*`) are skipped.
fn collect_dir_moves(src: &Path, dst: &Path, plan: &mut Vec<Move>) -> Result<()> {
    let entries = fs::read_dir(src).with_context(|| format!("read_dir {}", src.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .expect("entry has a filename")
            .to_os_string();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        plan.push(Move {
            src: path,
            dst: dst.join(name),
        });
    }
    Ok(())
}

#[derive(Deserialize)]
struct JsonMeta {
    album: Option<String>,
}

/// Read every `*.json` directly under `working_dir`, parse out `album`, and
/// queue a move into `concerts/<sanitize_album(album)>/`. `listing_*.json`
/// (month archives) are skipped — they describe many concerts, not one.
fn plan_json_moves(working_dir: &Path) -> Result<Vec<Move>> {
    let mut moves = Vec::new();
    let entries =
        fs::read_dir(working_dir).with_context(|| format!("read_dir {}", working_dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".json") {
            continue;
        }
        if name.starts_with("listing_") {
            continue;
        }
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("skip {}: read error {}", path.display(), e);
                continue;
            }
        };
        let meta: JsonMeta = match serde_json::from_str(&raw) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("skip {}: parse error {}", path.display(), e);
                continue;
            }
        };
        let Some(album) = meta.album else {
            tracing::warn!("skip {}: no `album` field", path.display());
            continue;
        };
        let dst = concert_dir(working_dir, &album).join("concert.json");
        moves.push(Move { src: path, dst });
    }
    Ok(moves)
}

fn dir_is_empty(dir: &Path) -> Result<bool> {
    Ok(fs::read_dir(dir)?.next().is_none())
}

fn is_json_path(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
}

/// True when `path` parses as JSON containing a `timestamps` field — the
/// telltale of a live-set-song-splitter output JSON (vs the scraper's richer
/// metadata, which has no timestamps).
fn is_splitter_timestamps_json(path: &Path) -> bool {
    #[derive(Deserialize)]
    struct Probe {
        timestamps: Option<serde_json::Value>,
    }
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(probe) = serde_json::from_str::<Probe>(&raw) else {
        return false;
    };
    matches!(probe.timestamps, Some(serde_json::Value::Array(ref a)) if !a.is_empty())
}

/// Cache for resolving album → directory from JSON metadata. Currently unused
/// because we build the plan in a single pass, but kept around so a future
/// caller can pre-scan and reuse the lookup. The `_` underscore avoids dead
/// code warnings without changing behavior.
#[allow(dead_code)]
type AlbumLookup = HashMap<String, PathBuf>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::TempDir;

    fn write(p: &Path, content: &[u8]) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut f = File::create(p).unwrap();
        use std::io::Write;
        f.write_all(content).unwrap();
    }

    #[test]
    fn splitter_legacy_folder_inserts_dash() {
        assert_eq!(
            splitter_legacy_folder("Air: Tiny Desk Concert"),
            "Air - Tiny Desk Concert"
        );
        assert_eq!(splitter_legacy_folder("No Colons"), "No Colons");
    }

    #[test]
    fn plan_json_moves_skips_listing_and_handles_missing_album() {
        let td = TempDir::new().unwrap();
        let wd = td.path();
        write(
            &wd.join("air.json"),
            br#"{"album":"Air: Tiny Desk Concert"}"#,
        );
        write(&wd.join("listing_2026_05.json"), br#"[]"#);
        write(&wd.join("noalbum.json"), br#"{}"#);
        write(&wd.join("broken.json"), b"not json");

        let moves = plan_json_moves(wd).unwrap();
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].src, wd.join("air.json"));
        assert_eq!(
            moves[0].dst,
            wd.join("concerts")
                .join("Air Tiny Desk Concert")
                .join("concert.json")
        );
    }

    #[test]
    fn is_splitter_timestamps_json_detects_timestamps_array() {
        let td = TempDir::new().unwrap();
        let with_ts = td.path().join("a.json");
        write(
            &with_ts,
            br#"{"artist":"x","timestamps":[{"title":"t","start_time":0,"end_time":1,"duration":1}]}"#,
        );
        let without_ts = td.path().join("b.json");
        write(&without_ts, br#"{"artist":"x","description":"none"}"#);
        let empty_ts = td.path().join("c.json");
        write(&empty_ts, br#"{"artist":"x","timestamps":[]}"#);

        assert!(is_splitter_timestamps_json(&with_ts));
        assert!(!is_splitter_timestamps_json(&without_ts));
        assert!(!is_splitter_timestamps_json(&empty_ts));
    }

    #[test]
    fn collect_dir_moves_targets_flat_filenames() {
        let td = TempDir::new().unwrap();
        let src = td.path().join("Foo - Tiny Desk Concert");
        write(&src.join("Song A.mp4"), b"");
        write(&src.join("Song A.m4a"), b"");
        let dst = td.path().join("concerts").join("Foo Tiny Desk Concert");

        let mut plan = Vec::new();
        collect_dir_moves(&src, &dst, &mut plan).unwrap();
        assert_eq!(plan.len(), 2);
        for m in &plan {
            assert_eq!(m.dst.parent().unwrap(), dst.as_path());
            assert_eq!(m.src.file_name().unwrap(), m.dst.file_name().unwrap());
        }
    }
}
