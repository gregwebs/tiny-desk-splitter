//! Build a deterministic, isolated fixture (DB + media files) for the Playwright
//! e2e suite. Used by `e2e/global-setup.js`; never touches the real DB.
//!
//! Usage: `cargo run --example make_test_fixture -- <workdir> <db_path>`
//!
//! Produces a fresh SQLite DB whose concerts get autoincrement ids 1..=6 (in
//! insertion order) plus tiny, genuinely-playable media generated with ffmpeg,
//! laid out exactly where the server looks them up (`concert_dir` +
//! `sanitize_filename`). Requires `ffmpeg` on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use concert_tracker::db::{self, MetadataUpdate, NewListing};
use concert_tracker::model::{concert_dir, sanitize_album, sanitize_filename};
use concert_tracker::scrape::{ensure_thumbnail, preview_image_path};

/// One track in a fixture concert: display title + the file extension that
/// determines how the media endpoints classify it. Formats are chosen so
/// Playwright's Chromium can actually decode them (it lacks H.264/AAC): wav =
/// audio playable, webm (VP8, video-only) = video playable, mkv = found-but-not
/// browser-playable (the "non-playable" case, never decoded).
///
/// `present: false` models a deleted track: it stays in the set list but its
/// media file is never written and a `track_delete` event is recorded, so the
/// media endpoints 404 on it (exactly the production state after delete_track
/// removes an m4a/mp4). Used to exercise "skip the deleted first track".
struct Track {
    title: &'static str,
    ext: &'static str,
    present: bool,
}

struct FixtureConcert {
    url: &'static str,
    title: &'static str,
    album: &'static str,
    artist: &'static str,
    tracks: Vec<Track>,
    liked: Vec<bool>,
    /// When false the concert is downloaded but never split: no split state in
    /// the DB and no per-track files on disk — only the full-concert file.
    /// Exercises the automated prepare (split-on-play) flow.
    split: bool,
}

fn audio(title: &'static str) -> Track {
    Track {
        title,
        ext: "wav",
        present: true,
    }
}
fn video(title: &'static str) -> Track {
    Track {
        title,
        ext: "webm",
        present: true,
    }
}
/// A track that has been deleted: still in the set list, but no file on disk.
/// Note: a concert with *every* track deleted would, in production, also have
/// its split state cleared (see `delete_track`); model that too if needed.
fn deleted_audio(title: &'static str) -> Track {
    Track {
        title,
        ext: "wav",
        present: false,
    }
}

fn fixtures() -> Vec<FixtureConcert> {
    vec![
        // id=1: primary audio concert — queue / navigation / delete / like / album playback.
        FixtureConcert {
            url: "https://npr.org/fixture/audio",
            title: "Audio Concert",
            album: "Audio Concert",
            artist: "Audio Artist",
            tracks: vec![
                audio("Celular"),
                audio("Limbo"),
                audio("Track Three"),
                audio("Dando Vueltas"),
            ],
            liked: vec![false, false, false, false],
            split: true,
        },
        // id=2: second audio concert — cross-concert enqueue / auto-advance / delete-another.
        FixtureConcert {
            url: "https://npr.org/fixture/second",
            title: "Second Concert",
            album: "Second Concert",
            artist: "Second Artist",
            tracks: vec![audio("Song One"), audio("Song Two"), audio("Song Three")],
            liked: vec![false, false, false],
            split: true,
        },
        // id=3: video concert — inline video. 0=video,1=audio (video->audio advance),
        // 2=video,3=video (video->video advance), 4=mkv (non-playable).
        FixtureConcert {
            url: "https://npr.org/fixture/video",
            title: "Video Concert",
            album: "Video Concert",
            artist: "Video Artist",
            tracks: vec![
                video("Clip One"),
                audio("Audio Song"),
                video("Clip Two"),
                video("Clip Three"),
                Track {
                    title: "Raw Take",
                    ext: "mkv",
                    present: true,
                },
            ],
            liked: vec![false, false, false, false, false],
            split: true,
        },
        // id=4: a concert whose only track is already liked — "already-starred hides delete".
        FixtureConcert {
            url: "https://npr.org/fixture/liked",
            title: "Liked Concert",
            album: "Liked Concert",
            artist: "Liked Artist",
            tracks: vec![audio("Liked Song")],
            liked: vec![true],
            split: true,
        },
        // id=5: first track deleted (no file on disk) — the tracks-row "Play" must
        // skip it and start the first track that still exists.
        FixtureConcert {
            url: "https://npr.org/fixture/deleted-first",
            title: "Deleted-First Concert",
            album: "Deleted-First Concert",
            artist: "Deleted-First Artist",
            tracks: vec![
                deleted_audio("Gone Opener"),
                audio("Survivor One"),
                audio("Survivor Two"),
            ],
            liked: vec![false, false, false],
            split: true,
        },
        // id=6: downloaded but never split — clicking a track (or the tracks
        // button) must run the automated split via /prepare and auto-play.
        FixtureConcert {
            url: "https://npr.org/fixture/unsplit",
            title: "Unsplit Concert",
            album: "Unsplit Concert",
            artist: "Unsplit Artist",
            tracks: vec![audio("First Song"), audio("Second Song")],
            liked: vec![false, false],
            split: false,
        },
    ]
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let workdir = PathBuf::from(
        args.next()
            .context("usage: make_test_fixture <workdir> <db_path>")?,
    );
    let db_path = PathBuf::from(
        args.next()
            .context("usage: make_test_fixture <workdir> <db_path>")?,
    );

    if which::which("ffmpeg").is_err() {
        bail!("ffmpeg not found on PATH — required to generate e2e test media");
    }

    // Start from a clean slate so autoincrement ids are deterministic (1..=N).
    let _ = std::fs::remove_file(&db_path);
    std::fs::create_dir_all(&workdir).context("create workdir")?;
    let conn = db::open(&db_path).context("open fixture db")?;

    for (i, fc) in fixtures().into_iter().enumerate() {
        let expected_id = (i + 1) as i64;
        build_concert(&conn, &workdir, &fc, expected_id)?;
    }

    // A representative failed background metadata scrape (e.g. an archived-NAS
    // write failure) so the Jobs page has a "Scrape" failed-job row for e2e to
    // assert on.
    db::insert_failed_job(
        &conn,
        1,
        concert_tracker::jobs::scrape_queue::SCRAPE_JOB_NAME,
        "Failed to write JSON file concerts/Fixture Artist Tiny Desk Concert/concert.json",
    )?;

    println!(
        "fixture built: {} ({} concerts)",
        db_path.display(),
        fixtures().len()
    );
    Ok(())
}

/// Contiguous automated timestamps, one per set-list song, over [0, TOTAL].
/// TOTAL stays under the ~20s full-concert file duration so they validate.
fn auto_timestamps(fc: &FixtureConcert) -> Vec<concert_types::SongTimestamp> {
    const TOTAL: f64 = 19.0;
    let n = fc.tracks.len();
    let seg = TOTAL / n as f64;
    fc.tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let start = i as f64 * seg;
            let end = if i + 1 == n {
                TOTAL
            } else {
                (i + 1) as f64 * seg
            };
            concert_types::SongTimestamp {
                title: t.title.to_string(),
                start_time: start,
                end_time: end,
                duration: end - start,
            }
        })
        .collect()
}

fn build_concert(
    conn: &rusqlite::Connection,
    workdir: &Path,
    fc: &FixtureConcert,
    expected_id: i64,
) -> Result<()> {
    db::upsert_listing(
        conn,
        &NewListing {
            source_url: fc.url.to_string(),
            title: fc.title.to_string(),
            concert_date: Some("2026-01-01".to_string()),
            teaser: Some(format!("{} teaser", fc.title)),
        },
    )?;
    let concert = db::get_concert_by_url(conn, fc.url)?
        .with_context(|| format!("concert {} missing after upsert", fc.url))?;
    // Deterministic ids are load-bearing: the specs reference #concert-1..5.
    if concert.id != expected_id {
        bail!(
            "fixture concert {} got id {} (expected {}); was the DB not empty?",
            fc.url,
            concert.id,
            expected_id
        );
    }
    let id = concert.id;

    // Sets metadata_scraped_at, so preview/thumbnail URLs render in the UI.
    db::update_metadata(
        conn,
        id,
        &MetadataUpdate {
            artist: fc.artist.to_string(),
            album: fc.album.to_string(),
            description: Some(format!("{} description", fc.title)),
            set_list: fc.tracks.iter().map(|t| t.title.to_string()).collect(),
            musicians: vec![],
        },
    )?;
    db::try_mark_download_started(conn, id)?;
    db::mark_download_succeeded(conn, id, "wav")?;
    if fc.split {
        db::try_mark_split_started(conn, id)?;
        db::mark_split_succeeded(conn, id)?;
        let tracks_present: Vec<bool> = fc.tracks.iter().map(|t| t.present).collect();
        db::set_tracks_present(conn, id, &tracks_present)?;
        // Seed automated split timestamps (one contiguous segment per set-list
        // song, spread over the ~20s full-concert file with a margin under its
        // duration) so the splitter editor has data to load.
        db::set_auto_split_timestamps(conn, id, &auto_timestamps(fc))?;
    }
    db::set_tracks_liked(conn, id, &fc.liked)?;
    // Record a delete event for each absent track so the track list shows it
    // unavailable, mirroring the post-delete_track state in production.
    for (idx, t) in fc.tracks.iter().enumerate() {
        if !t.present {
            let json = serde_json::json!({"track_index": idx, "track_title": t.title}).to_string();
            concert_tracker::events::record_now(
                conn,
                id,
                concert_tracker::events::Event::TrackDelete,
                Some(&json),
            );
        }
    }

    let dir = concert_dir(workdir, fc.album);
    std::fs::create_dir_all(&dir).context("create concert dir")?;

    // Full-concert file (album playback) — found by stem == sanitize_album(album).
    gen_audio(&dir.join(format!("{}.wav", sanitize_album(fc.album))))?;

    // Per-track split files. Absent (deleted) tracks get no file, so the media
    // endpoints 404 on them. Unsplit concerts get no track files at all — the
    // automated split (stub splitter in e2e) creates them on demand.
    for t in &fc.tracks {
        if !t.present || !fc.split {
            continue;
        }
        let path = dir.join(format!("{}.{}", sanitize_filename(t.title), t.ext));
        match t.ext {
            "wav" => gen_audio(&path)?,
            "webm" => gen_video(&path)?,
            "mkv" => gen_mkv(&path)?,
            other => bail!("unsupported fixture ext: {other}"),
        }
    }

    // Preview image + always-local thumbnail (so thumbnails.spec.js passes).
    gen_jpeg(&preview_image_path(workdir, fc.album))?;
    ensure_thumbnail(workdir, fc.album, None)?;

    Ok(())
}

/// ~20s low-rate silent PCM WAV — universally decodable by Chromium, long enough
/// that back/forward-nav tests still find the track playing, and tiny (~320 KB)
/// so per-test fixture copies stay cheap.
fn gen_audio(dest: &Path) -> Result<()> {
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "anullsrc=r=8000:cl=mono",
        "-t",
        "20",
        "-c:a",
        "pcm_s16le",
        &dest.to_string_lossy(),
    ])
}

/// ~30s tiny VP8 WebM (video-only — avoids depending on a webm audio encoder).
/// Chromium plays VP8/WebM; it cannot decode mp4/H.264. Long enough that
/// timing-sensitive video tests don't hit a real `ended` before dispatching
/// one, even with the hover-reveal interactions slowing the flow down.
fn gen_video(dest: &Path) -> Result<()> {
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=128x72:r=10",
        "-t",
        "30",
        "-c:v",
        "libvpx",
        &dest.to_string_lossy(),
    ])
}

/// A real .mkv: located by `find_track_file` but not browser-playable, so the
/// media endpoints report `playable:false` (the "non-playable" case; never decoded).
fn gen_mkv(dest: &Path) -> Result<()> {
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=128x72:r=5",
        "-t",
        "2",
        "-c:v",
        "libvpx",
        &dest.to_string_lossy(),
    ])
}

/// Single-frame JPEG preview.
fn gen_jpeg(dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=blue:s=320x180",
        "-frames:v",
        "1",
        &dest.to_string_lossy(),
    ])
}

fn ffmpeg(args: &[&str]) -> Result<()> {
    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .args(args)
        .status()
        .context("spawn ffmpeg")?;
    if !status.success() {
        bail!("ffmpeg failed ({status}) for args {args:?}");
    }
    Ok(())
}
