use anyhow::{Context, Result};
use concert_types::ConcertInfo;
use rusqlite::Connection;
use serde::Serialize;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::NamedTempFile;

use crate::db;
use crate::jobs::{
    find_downloaded_file, persist_job_log, run_with_logging, JobConfig, JobKey, JobKind,
    JobRegistry, SplitJob, SplitMode,
};
use crate::model::{concert_dir, Concert, Musician};
use crate::split_timestamps::ValidatedTimestamps;

#[derive(Debug)]
pub enum StartOutcome {
    Spawned,
    AlreadyRunning,
    NotDownloaded,
    /// Source file was missing but split tracks already exist on disk; the
    /// concert's split state was reconciled from the filesystem instead of
    /// running the splitter. Used to recover imported concerts whose
    /// `split_at` was never recorded.
    AlreadySplit,
}

#[derive(Serialize)]
struct SplitterInput {
    artist: String,
    source: String,
    show: String,
    date: Option<String>,
    album: String,
    description: Option<String>,
    set_list: Vec<SplitterSong>,
    musicians: Vec<SplitterMusician>,
}

#[derive(Serialize)]
struct SplitterSong {
    title: String,
}

#[derive(Serialize)]
struct SplitterMusician {
    name: String,
    instruments: Vec<String>,
}

fn write_splitter_input(concert: &Concert) -> Result<NamedTempFile> {
    let set_list: Vec<SplitterSong> = concert
        .set_list
        .iter()
        .map(|title| SplitterSong {
            title: title.clone(),
        })
        .collect();
    let musicians: Vec<SplitterMusician> = concert
        .musicians
        .iter()
        .map(|m: &Musician| SplitterMusician {
            name: m.name.clone(),
            instruments: m.instruments.clone(),
        })
        .collect();

    let input = SplitterInput {
        artist: concert.artist.clone().unwrap_or_default(),
        source: concert.source_url.clone(),
        show: "Tiny Desk Concerts".to_string(),
        date: concert.concert_date.clone(),
        album: concert.album.clone().unwrap_or_default(),
        description: concert.description.clone(),
        set_list,
        musicians,
    };

    let mut file = NamedTempFile::new()?;
    let json = serde_json::to_string(&input)?;
    file.write_all(json.as_bytes())?;
    Ok(file)
}

/// Start a split job for the given concert. Requires downloaded_at IS NOT NULL.
pub async fn start_split(
    db: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    config: JobConfig,
    concert_id: i64,
    mode: SplitMode,
) -> Result<StartOutcome> {
    let key = JobKey {
        concert_id,
        kind: JobKind::Split,
    };
    if registry.is_running(&key) {
        tracing::info!("split already running for concert {}", concert_id);
        return Ok(StartOutcome::AlreadyRunning);
    }

    let (ok, concert) = {
        let conn = db.lock().unwrap();
        let ok = db::try_mark_split_started(&conn, concert_id)?;
        let concert = db::get_concert(&conn, concert_id)?;
        (ok, concert)
    };

    if !ok {
        if concert.downloaded_at.is_none() {
            tracing::info!("split rejected: concert {} not yet downloaded", concert_id);
            return Ok(StartOutcome::NotDownloaded);
        }
        tracing::info!("split already running for concert {}", concert_id);
        return Ok(StartOutcome::AlreadyRunning);
    }

    let title = concert.title.clone();

    // For user/reset modes, validate that the provided timestamps still match the
    // current set_list (a concurrent re-scrape may have changed it since the handler
    // read it). This check is advisory — validation already ran in the handler.
    if let SplitMode::UserTimestamps(ts) | SplitMode::ResetToAuto(ts) = &mode {
        if ts.songs().len() != concert.set_list.len() {
            let e = anyhow::anyhow!(
                "Timestamp count {} does not match set_list length {} for concert {}",
                ts.songs().len(),
                concert.set_list.len(),
                concert_id
            );
            registry.drop_dependency_edges(&key);
            let conn = db.lock().unwrap();
            let _ = db::mark_split_failed(&conn, concert_id, &e.to_string());
            return Err(e);
        }
    }

    enum SetupResult {
        Ready(
            NamedTempFile,
            std::path::PathBuf,
            std::path::PathBuf,
            Option<NamedTempFile>,
            Option<std::path::PathBuf>,
        ),
        AlreadySplit,
    }

    let setup = (|| -> Result<SetupResult> {
        let album = concert.album.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "Concert {} has no album, cannot locate input file",
                concert_id
            )
        })?;
        if let Some(input_file) = find_downloaded_file(&config.working_dir, album) {
            let temp_file = write_splitter_input(&concert)?;
            let output_dir = concert_dir(&config.working_dir, album);

            // For user/reset modes, write the timestamps to a temp file for --timestamps-file.
            let (ts_temp, ts_path) = match &mode {
                SplitMode::Analyze => (None, None),
                SplitMode::UserTimestamps(ts) | SplitMode::ResetToAuto(ts) => {
                    let file = write_timestamps_file(ts)?;
                    let path = file.path().to_path_buf();
                    (Some(file), Some(path))
                }
            };

            return Ok(SetupResult::Ready(
                temp_file, input_file, output_dir, ts_temp, ts_path,
            ));
        }
        // Source file missing. Only Analyze mode supports auto-recovery from
        // existing split tracks — user/reset modes require the source file
        // to re-cut.
        if matches!(mode, SplitMode::Analyze) {
            let cd = concert_dir(&config.working_dir, album);
            if !concert.set_list.is_empty() && crate::scan::has_split_tracks(&cd, album) {
                let present: Vec<bool> = concert
                    .set_list
                    .iter()
                    .map(|title| {
                        crate::model::find_track_file(&config.working_dir, album, title).is_some()
                    })
                    .collect();
                let conn = db.lock().unwrap();
                db::set_tracks_present(&conn, concert_id, &present)?;
                db::mark_split_succeeded(&conn, concert_id)?;
                return Ok(SetupResult::AlreadySplit);
            }
        }
        Err(anyhow::anyhow!(
            "Downloaded file for concert {} (album {:?}) not found in {}",
            concert_id,
            album,
            config.working_dir.display()
        ))
    })();

    let (temp_file, input_file, output_dir, ts_temp, ts_path) = match setup {
        Ok(SetupResult::Ready(t, i, o, ts, tp)) => (t, i, o, ts, tp),
        Ok(SetupResult::AlreadySplit) => {
            tracing::info!(
                "split auto-recovered for concert {} ({}) — tracks already present on disk",
                concert_id,
                title
            );
            // The split is effectively complete, so anything queued behind it
            // should run now.
            crate::jobs::spawn_dependents(db.clone(), registry.clone(), config, &key);
            return Ok(StartOutcome::AlreadySplit);
        }
        Err(e) => {
            // Setup failed after we marked the split as started — clear the flag
            // so the user can retry, drop anything queued behind this split,
            // and surface the error.
            registry.drop_dependency_edges(&key);
            let conn = db.lock().unwrap();
            let _ = db::mark_split_failed(&conn, concert_id, &e.to_string());
            return Err(e);
        }
    };
    let json_path = temp_file.path().to_path_buf();
    let job = SplitJob {
        concert_id: concert.id,
        json_path,
        input_file,
        output_dir,
        mode,
        _temp_file: temp_file,
        _timestamps_temp_file: ts_temp,
        timestamps_path: ts_path,
    };

    tracing::info!(
        "split started for concert {} ({}) mode={}",
        concert_id,
        title,
        job.mode.name()
    );
    let handle = tokio::task::spawn(run_split(db.clone(), registry.clone(), config, job));
    registry.insert(key, handle);

    Ok(StartOutcome::Spawned)
}

fn write_timestamps_file(ts: &ValidatedTimestamps) -> Result<NamedTempFile> {
    let file_data = ts.to_timestamps_file();
    let json = serde_json::to_string(&file_data)?;
    let mut file = NamedTempFile::new()?;
    file.write_all(json.as_bytes())?;
    Ok(file)
}

/// Read the automated timestamps from the `timestamps.json` the splitter writes
/// into `output_dir` after analysis. Returns an error on I/O or parse failure.
pub fn read_analysis_timestamps(output_dir: &Path) -> Result<Vec<concert_types::SongTimestamp>> {
    let path = output_dir.join("timestamps.json");
    let file =
        std::fs::File::open(&path).with_context(|| format!("Failed to open {}", path.display()))?;
    let info: ConcertInfo = serde_json::from_reader(std::io::BufReader::new(file))
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    info.timestamps
        .ok_or_else(|| anyhow::anyhow!("timestamps.json has no timestamps field"))
}

/// Return automated timestamps for a concert from the DB, falling back to
/// parsing `{concert_dir}/timestamps.json` from disk (lazy backfill for
/// concerts split before this feature). Persists the backfilled value into
/// the DB column on success.
pub fn auto_timestamps_with_backfill(
    conn: &Connection,
    working_dir: &Path,
    concert: &Concert,
) -> Result<Option<Vec<concert_types::SongTimestamp>>> {
    let stored = db::get_split_timestamps(conn, concert.id)?;
    if stored.auto.is_some() {
        return Ok(stored.auto);
    }
    // Attempt disk backfill
    let album = match concert.album.as_deref() {
        Some(a) => a,
        None => return Ok(None),
    };
    let output_dir = concert_dir(working_dir, album);
    match read_analysis_timestamps(&output_dir) {
        Ok(ts) => {
            db::set_auto_split_timestamps(conn, concert.id, &ts)?;
            tracing::debug!(
                "auto_timestamps_with_backfill: backfilled {} timestamps for concert {} from disk",
                ts.len(),
                concert.id
            );
            Ok(Some(ts))
        }
        Err(e) => {
            tracing::debug!(
                "auto_timestamps_with_backfill: no timestamps.json for concert {}: {}",
                concert.id,
                e
            );
            Ok(None)
        }
    }
}

async fn run_split(
    db: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    config: JobConfig,
    job: SplitJob,
) {
    let concert_id = job.concert_id;
    let key = JobKey {
        concert_id,
        kind: JobKind::Split,
    };
    let cmd = (config.split_cmd)(&job);

    let log_dir = config.log_dir();
    let temp_file = match std::fs::create_dir_all(&log_dir)
        .and_then(|_| NamedTempFile::new_in(&log_dir).map_err(Into::into))
    {
        Ok(f) => Some(f),
        Err(e) => {
            tracing::warn!("failed to create temp log file: {}", e);
            None
        }
    };
    let temp_path = temp_file.as_ref().map(|f| f.path().to_path_buf());

    match run_with_logging(cmd, "split", concert_id, temp_path.as_deref()).await {
        Ok((status, _)) if status.success() => {
            tracing::info!(
                "split completed for concert {} mode={}",
                concert_id,
                job.mode.name()
            );
            drop(temp_file);
            {
                let conn = db.lock().unwrap();
                let _ = db::mark_split_succeeded(&conn, concert_id);
                let concert = db::get_concert(&conn, concert_id);
                if let Ok(c) = concert {
                    if let Some(album) = c.album.as_deref() {
                        let present: Vec<bool> = c
                            .set_list
                            .iter()
                            .map(|title| {
                                crate::model::find_track_file(&config.working_dir, album, title)
                                    .is_some()
                            })
                            .collect();
                        let _ = db::set_tracks_present(&conn, concert_id, &present);
                    }
                }
                // Persist timestamp state by mode. Never fail the job over a
                // metadata error — warn and continue.
                match &job.mode {
                    SplitMode::Analyze => {
                        match read_analysis_timestamps(&job.output_dir) {
                            Ok(ts) => {
                                if let Err(e) =
                                    db::set_auto_split_timestamps(&conn, concert_id, &ts)
                                {
                                    tracing::warn!(
                                        "failed to store auto timestamps for concert {}: {}",
                                        concert_id,
                                        e
                                    );
                                }
                                // Successful re-analysis supersedes any user cut.
                                if let Err(e) = db::clear_user_split_timestamps(&conn, concert_id) {
                                    tracing::warn!(
                                        "failed to clear user timestamps for concert {}: {}",
                                        concert_id,
                                        e
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "failed to read timestamps.json for concert {}: {}",
                                    concert_id,
                                    e
                                );
                            }
                        }
                    }
                    SplitMode::UserTimestamps(ts) => {
                        if let Err(e) = db::set_user_split_timestamps(&conn, concert_id, ts.songs())
                        {
                            tracing::warn!(
                                "failed to store user timestamps for concert {}: {}",
                                concert_id,
                                e
                            );
                        }
                    }
                    SplitMode::ResetToAuto(_) => {
                        if let Err(e) = db::clear_user_split_timestamps(&conn, concert_id) {
                            tracing::warn!(
                                "failed to clear user timestamps for concert {}: {}",
                                concert_id,
                                e
                            );
                        }
                    }
                }
            }
            crate::jobs::spawn_dependents(db, registry, config, &key);
        }
        Ok((status, stderr_tail)) => {
            let error = format!("exit {:?}: {}", status.code(), stderr_tail.trim());
            tracing::warn!("split failed for concert {}: {}", concert_id, error);
            registry.drop_dependency_edges(&key);
            let conn = db.lock().unwrap();
            let _ = db::mark_split_failed(&conn, concert_id, &error);
            persist_job_log(&conn, concert_id, "split", &error, temp_file, &log_dir);
        }
        Err(e) => {
            let hint = if e.kind() == std::io::ErrorKind::NotFound {
                ". Is live-set-splitter built? Run: cargo build --bin live-set-splitter"
            } else {
                ""
            };
            let error = format!("spawn error: {}{}", e, hint);
            tracing::warn!("split failed for concert {}: {}", concert_id, error);
            registry.drop_dependency_edges(&key);
            let conn = db.lock().unwrap();
            let _ = db::mark_split_failed(&conn, concert_id, &error);
            persist_job_log(&conn, concert_id, "split", &error, temp_file, &log_dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{DownloadJob, JobConfig, JobRegistry};
    use std::fs;
    use std::path::PathBuf;
    use tokio::process::Command;

    fn config_for(working_dir: PathBuf) -> JobConfig {
        JobConfig {
            working_dir,
            download_cmd: Arc::new(|_: &DownloadJob| Command::new("true")),
            // Long sleep so a real spawn would be visibly running. Tests that
            // exercise the auto-recover path expect the splitter to never run;
            // tests that exercise the spawn path check the registry, not exit.
            split_cmd: Arc::new(|_| {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "sleep 10"]);
                cmd
            }),
            open_cmd: Arc::new(|_| Command::new("true")),
        }
    }

    fn seeded_db(album: &str, set_list: Vec<String>) -> Arc<Mutex<Connection>> {
        let conn = db::open_in_memory().unwrap();
        db::upsert_listing(
            &conn,
            &db::NewListing {
                source_url: format!("https://npr.org/c/{}", album),
                title: album.to_string(),
                concert_date: None,
                teaser: None,
            },
        )
        .unwrap();
        db::update_metadata(
            &conn,
            1,
            &db::MetadataUpdate {
                artist: "Test Artist".to_string(),
                album: album.to_string(),
                description: None,
                set_list,
                musicians: vec![],
            },
        )
        .unwrap();
        db::set_downloaded_at_if_missing(&conn, 1, "2024-01-01T00:00:00Z").unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[tokio::test]
    async fn auto_recovers_when_tracks_present_and_source_missing() {
        // Imported-concert scenario: no source file in the concert dir, but
        // all set_list tracks already exist as audio sidecars. Album includes
        // a colon to also exercise the sanitize_album path end-to-end.
        let tmp = tempfile::tempdir().unwrap();
        let album = "Bloc Party: Tiny Desk Concert";
        let set_list = vec![
            "Banquet".to_string(),
            "Signs".to_string(),
            "Mercury".to_string(),
            "Blue".to_string(),
        ];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        for t in &set_list {
            fs::write(cd.join(format!("{}.m4a", t)), b"audio").unwrap();
            fs::write(cd.join(format!("{}.mp4", t)), b"video").unwrap();
        }

        let db = seeded_db(album, set_list.clone());
        let registry = Arc::new(JobRegistry::new());
        let outcome = start_split(
            db.clone(),
            registry.clone(),
            config_for(tmp.path().to_path_buf()),
            1,
            SplitMode::Analyze,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, StartOutcome::AlreadySplit));
        let conn = db.lock().unwrap();
        let c = db::get_concert(&conn, 1).unwrap();
        assert!(c.split_at.is_some(), "split_at should be set");
        assert_eq!(c.tracks_present, vec![true; 4]);
        assert!(c.split_errors.is_empty(), "no error should be recorded");
        // Auto-recovery must not spawn a splitter job.
        assert!(!registry.is_running(&JobKey {
            concert_id: 1,
            kind: JobKind::Split,
        }));
    }

    #[tokio::test]
    async fn auto_recovers_partial_tracks() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Partial Album";
        let set_list = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        // Only A and C present on disk.
        fs::write(cd.join("A.m4a"), b"audio").unwrap();
        fs::write(cd.join("C.m4a"), b"audio").unwrap();

        let db = seeded_db(album, set_list);
        let registry = Arc::new(JobRegistry::new());
        let outcome = start_split(
            db.clone(),
            registry,
            config_for(tmp.path().to_path_buf()),
            1,
            SplitMode::Analyze,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, StartOutcome::AlreadySplit));
        let conn = db.lock().unwrap();
        let c = db::get_concert(&conn, 1).unwrap();
        assert!(c.split_at.is_some());
        assert_eq!(c.tracks_present, vec![true, false, true]);
    }

    #[tokio::test]
    async fn errors_when_no_source_and_no_tracks() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Empty Album";
        let set_list = vec!["Song".to_string()];
        // Concert dir does not exist at all — no source, no tracks.

        let db = seeded_db(album, set_list);
        let registry = Arc::new(JobRegistry::new());
        let err = start_split(
            db.clone(),
            registry,
            config_for(tmp.path().to_path_buf()),
            1,
            SplitMode::Analyze,
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("Downloaded file"));
        let conn = db.lock().unwrap();
        let c = db::get_concert(&conn, 1).unwrap();
        assert!(c.split_at.is_none());
        assert!(!c.split_errors.is_empty(), "split error should be recorded");
    }

    #[tokio::test]
    async fn spawns_splitter_when_source_present() {
        // Source file IS present — must spawn the splitter job, not auto-recover.
        let tmp = tempfile::tempdir().unwrap();
        let album = "Has Source";
        let set_list = vec!["S1".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("Has Source.mp4"), b"video").unwrap();

        let db = seeded_db(album, set_list);
        let registry = Arc::new(JobRegistry::new());
        let outcome = start_split(
            db.clone(),
            registry.clone(),
            config_for(tmp.path().to_path_buf()),
            1,
            SplitMode::Analyze,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, StartOutcome::Spawned));
        // The splitter command we configured `sleep 10`, so the job is running.
        assert!(registry.is_running(&JobKey {
            concert_id: 1,
            kind: JobKind::Split,
        }));
        // Stop the background sleeper so the test exits promptly.
        registry.cancel(&JobKey {
            concert_id: 1,
            kind: JobKind::Split,
        });
    }

    /// Config whose splitter writes a fake timestamps.json into the output_dir on success.
    fn config_with_fake_analyze(working_dir: PathBuf, set_list: &[String]) -> JobConfig {
        let songs_json: String = set_list
            .iter()
            .enumerate()
            .map(|(i, title)| {
                let start = i as f64 * 100.0;
                let end = start + 90.0;
                format!(
                    r#"{{"title":"{}","start_time":{},"end_time":{},"duration":90.0}}"#,
                    title, start, end
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        // The splitter writes ConcertInfo JSON (with timestamps) to timestamps.json
        let ts_json = format!(
            r#"{{"artist":"A","source":"","show":"","album":"","set_list":[],"musicians":[],"timestamps":[{}]}}"#,
            songs_json
        );
        let script = format!(
            "mkdir -p \"$2\" && printf '{}' > \"$2/timestamps.json\"",
            ts_json.replace('\'', "'\\''")
        );
        JobConfig {
            working_dir,
            download_cmd: Arc::new(|_: &DownloadJob| Command::new("true")),
            split_cmd: Arc::new(move |job: &SplitJob| {
                let output_dir = job.output_dir.to_str().unwrap().to_string();
                let mut cmd = Command::new("sh");
                cmd.args(["-c", &script, "--", "", &output_dir]);
                cmd
            }),
            open_cmd: Arc::new(|_| Command::new("true")),
        }
    }

    /// Config whose splitter records the --timestamps-file content and creates track stubs.
    fn config_with_timestamps_check(working_dir: PathBuf, set_list: &[String]) -> JobConfig {
        let touch_cmds: Vec<String> = set_list
            .iter()
            .map(|t| format!("touch \"$2/{}.m4a\"", t))
            .collect();
        let touch = touch_cmds.join("; ");
        let script = format!("mkdir -p \"$2\" && {}", touch);
        JobConfig {
            working_dir,
            download_cmd: Arc::new(|_: &DownloadJob| Command::new("true")),
            split_cmd: Arc::new(move |job: &SplitJob| {
                let output_dir = job.output_dir.to_str().unwrap().to_string();
                // Verify --timestamps-file is present in the command args
                let ts_flag = job.timestamps_path.is_some();
                assert!(ts_flag, "user/reset mode must pass --timestamps-file");
                let mut cmd = Command::new("sh");
                cmd.args(["-c", &script, "--", "", &output_dir]);
                cmd
            }),
            open_cmd: Arc::new(|_| Command::new("true")),
        }
    }

    #[tokio::test]
    async fn analyze_mode_stores_auto_timestamps_on_success() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Analyze Album";
        let set_list = vec!["Track One".to_string(), "Track Two".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("Analyze Album.mp4"), b"video").unwrap();

        let db = seeded_db(album, set_list.clone());
        let registry = Arc::new(JobRegistry::new());
        let config = config_with_fake_analyze(tmp.path().to_path_buf(), &set_list);
        let outcome = start_split(db.clone(), registry.clone(), config, 1, SplitMode::Analyze)
            .await
            .unwrap();
        assert!(matches!(outcome, StartOutcome::Spawned));

        // Wait for job to finish
        for _ in 0..100 {
            if !registry.is_running(&JobKey {
                concert_id: 1,
                kind: JobKind::Split,
            }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let conn = db.lock().unwrap();
        let stored = db::get_split_timestamps(&conn, 1).unwrap();
        assert!(stored.auto.is_some(), "auto timestamps should be stored");
        assert!(stored.user.is_none(), "user timestamps should be cleared");
        let auto = stored.auto.unwrap();
        assert_eq!(auto.len(), 2);
        assert_eq!(auto[0].title, "Track One");
    }

    #[tokio::test]
    async fn user_timestamps_mode_stores_user_column() {
        use crate::split_timestamps::{TimestampPayloadSong, ValidatedTimestamps};

        let tmp = tempfile::tempdir().unwrap();
        let album = "User Album";
        let set_list = vec!["Alpha".to_string(), "Beta".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("User Album.mp4"), b"video").unwrap();

        let db = seeded_db(album, set_list.clone());
        let registry = Arc::new(JobRegistry::new());
        let config = config_with_timestamps_check(tmp.path().to_path_buf(), &set_list);

        let payload = vec![
            TimestampPayloadSong {
                title: "Alpha".to_string(),
                start_time: 0.0,
                end_time: 95.0,
            },
            TimestampPayloadSong {
                title: "Beta".to_string(),
                start_time: 100.0,
                end_time: 200.0,
            },
        ];
        let ts = ValidatedTimestamps::validate(&set_list, None, &payload).unwrap();

        let outcome = start_split(
            db.clone(),
            registry.clone(),
            config,
            1,
            SplitMode::UserTimestamps(ts),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, StartOutcome::Spawned));

        for _ in 0..100 {
            if !registry.is_running(&JobKey {
                concert_id: 1,
                kind: JobKind::Split,
            }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let conn = db.lock().unwrap();
        let stored = db::get_split_timestamps(&conn, 1).unwrap();
        assert!(stored.user.is_some(), "user timestamps should be stored");
        let user = stored.user.unwrap();
        assert_eq!(user.len(), 2);
        assert_eq!(user[0].title, "Alpha");
        assert_eq!(user[0].start_time, 0.0);
        assert_eq!(user[0].end_time, 95.0);
    }

    #[tokio::test]
    async fn user_mode_does_not_auto_recover_when_source_missing() {
        use crate::split_timestamps::{TimestampPayloadSong, ValidatedTimestamps};

        let tmp = tempfile::tempdir().unwrap();
        let album = "No Source Album";
        let set_list = vec!["Song A".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        // Track exists on disk but NO source file
        fs::write(cd.join("Song A.m4a"), b"audio").unwrap();

        let db = seeded_db(album, set_list.clone());
        let registry = Arc::new(JobRegistry::new());
        let config = config_for(tmp.path().to_path_buf());

        let payload = vec![TimestampPayloadSong {
            title: "Song A".to_string(),
            start_time: 0.0,
            end_time: 90.0,
        }];
        let ts = ValidatedTimestamps::validate(&set_list, None, &payload).unwrap();

        let err = start_split(
            db.clone(),
            registry,
            config,
            1,
            SplitMode::UserTimestamps(ts),
        )
        .await
        .unwrap_err();

        // Should error (not auto-recover), because user mode requires the source file.
        assert!(err.to_string().contains("Downloaded file"));
    }

    /// Config whose splitter always exits non-zero, recording a split failure.
    fn config_with_failing_split(working_dir: PathBuf) -> JobConfig {
        JobConfig {
            working_dir,
            download_cmd: Arc::new(|_: &DownloadJob| Command::new("true")),
            split_cmd: Arc::new(|_| {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "exit 1"]);
                cmd
            }),
            open_cmd: Arc::new(|_| Command::new("true")),
        }
    }

    /// Simulate a concert that was already split (split_at IS NOT NULL) and re-split
    /// it successfully. Confirms the outcome is detected as success via split_errors
    /// count (since split_at stays set regardless, the status slug alone is unreliable).
    #[tokio::test]
    async fn resplit_success_reports_ok_via_error_count() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Resplit Success";
        let set_list = vec!["Track One".to_string(), "Track Two".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("Resplit Success.mp4"), b"video").unwrap();

        let db = seeded_db(album, set_list.clone());
        // Simulate a prior successful split
        {
            let conn = db.lock().unwrap();
            db::try_mark_split_started(&conn, 1).unwrap();
            db::mark_split_succeeded(&conn, 1).unwrap();
        }
        let initial_errors = {
            let conn = db.lock().unwrap();
            db::get_concert(&conn, 1).unwrap().split_errors.len()
        };

        let registry = Arc::new(JobRegistry::new());
        let config = config_with_fake_analyze(tmp.path().to_path_buf(), &set_list);
        let outcome = start_split(db.clone(), registry.clone(), config, 1, SplitMode::Analyze)
            .await
            .unwrap();
        assert!(matches!(outcome, StartOutcome::Spawned));

        let key = JobKey {
            concert_id: 1,
            kind: JobKind::Split,
        };
        for _ in 0..100 {
            if !registry.is_running(&key) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let post_errors = {
            let conn = db.lock().unwrap();
            db::get_concert(&conn, 1).unwrap().split_errors.len()
        };
        assert_eq!(
            post_errors, initial_errors,
            "successful re-split must not add errors"
        );
    }

    /// Simulate a concert that was already split and re-split it with a failing
    /// splitter. Confirms the outcome is detected as failure via split_errors count
    /// (the old split_at stays set, so status slug alone would misreport this).
    #[tokio::test]
    async fn resplit_failure_detected_via_error_count() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Resplit Failure";
        let set_list = vec!["Song".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("Resplit Failure.mp4"), b"video").unwrap();

        let db = seeded_db(album, set_list.clone());
        // Simulate a prior successful split
        {
            let conn = db.lock().unwrap();
            db::try_mark_split_started(&conn, 1).unwrap();
            db::mark_split_succeeded(&conn, 1).unwrap();
        }
        let initial_errors = {
            let conn = db.lock().unwrap();
            db::get_concert(&conn, 1).unwrap().split_errors.len()
        };

        let registry = Arc::new(JobRegistry::new());
        let config = config_with_failing_split(tmp.path().to_path_buf());
        let outcome = start_split(db.clone(), registry.clone(), config, 1, SplitMode::Analyze)
            .await
            .unwrap();
        assert!(matches!(outcome, StartOutcome::Spawned));

        let key = JobKey {
            concert_id: 1,
            kind: JobKind::Split,
        };
        for _ in 0..100 {
            if !registry.is_running(&key) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let post_errors = {
            let conn = db.lock().unwrap();
            db::get_concert(&conn, 1).unwrap().split_errors.len()
        };
        assert!(
            post_errors > initial_errors,
            "failing re-split must append to split_errors (initial={}, post={})",
            initial_errors,
            post_errors
        );
    }
}
