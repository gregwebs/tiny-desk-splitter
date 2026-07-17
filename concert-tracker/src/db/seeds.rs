//! Database Seed API — test-only fixture creation for both Rust tests and the
//! Test Control API (`crate::test_control`). Compiled for `cfg(test)` builds
//! and `test-control` builds so plain non-test-control release builds never
//! see this module at all.
//!
//! [`SeedContext`] pairs a `&Connection` with a [`FixtureIds`] allocator and
//! exposes one method per fixture shape (`seed_listing`,
//! `seed_scraped_concert`, `seed_lifecycle_concert`). Each seed input struct
//! implements `Default` and deserializes with struct-level
//! `#[serde(default)]`, so a missing JSON field takes the Rust default and an
//! explicit JSON `null` deserializes to `None` — these differ exactly when a
//! field's `Default` is `Some(...)` (see `concert_date`/`teaser` below), which
//! is what lets a seed call explicitly request a NULL domain value instead of
//! a generated one. See `docs/adr/0003-test-control-seed-defaults.md` and
//! `docs/change/2026-07-13-db-seed-api-design.md`.
//!
//! Seed methods compose the same domain persistence functions the product
//! code uses (`db::concerts`, `db::lifecycle`, `db::split_timestamps`), so
//! seeded fixtures emit the same events a real user action would. Direct SQL
//! is used only for fixture *normalization* — resolving `NewListing`'s
//! COALESCE-on-conflict semantics, and clearing stale lifecycle/timestamp
//! state on a reused `source_url` — where the equivalent domain functions
//! (`lifecycle::clear_download_state`/`clear_split_state`) would record
//! misleading delete events for state that was never really downloaded or
//! split.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use concert_types::{derive_interludes, interlude_filename_stem, ConcertInfo, Song, SongTimestamp};
use rusqlite::{params, Connection};
use serde::Deserialize;

use crate::db::concerts::{self, MetadataUpdate, NewListing};
use crate::db::lifecycle;
use crate::db::split_timestamps;
use crate::model::Concert;

const DEFAULT_CONCERT_DATE: &str = "2026-01-01";

/// Shared sentinel bytes for every dummy fixture file this module and
/// [`crate::test_control::job_driver`] write — neither valid audio/video nor
/// meant to be; handlers that read these files must only check existence,
/// extension, or (for legacy timestamps.json) JSON shape.
pub(crate) const SENTINEL_BYTES: &[u8] = b"test-control dummy media\n";

/// Write one sentinel file per title into `output_dir`, named the same way
/// the real splitter names track output (`sanitize_filename(title).{ext}`).
/// Shared by [`write_seed_media_files`] (a caller-chosen `ext`, defaulting to
/// "mp3") and `test_control::job_driver::write_split_output` (always "m4a",
/// matching the real splitter's split output).
pub(crate) fn write_track_sentinels<'a>(
    output_dir: &Path,
    titles: impl Iterator<Item = &'a str>,
    ext: &str,
) -> Result<()> {
    for title in titles {
        let stem = crate::model::sanitize_filename(title);
        std::fs::write(output_dir.join(format!("{stem}.{ext}")), SENTINEL_BYTES)?;
    }
    Ok(())
}

/// Write one sentinel interlude file per gap `derive_interludes` finds
/// between `songs` and `media_duration`, named the same way the real
/// splitter names interlude output (`interlude_filename_stem(index).m4a`).
pub(crate) fn write_interlude_sentinels(
    output_dir: &Path,
    songs: &[SongTimestamp],
    media_duration: f64,
) -> Result<()> {
    for interlude in derive_interludes(songs, media_duration) {
        let stem = interlude_filename_stem(interlude.index);
        std::fs::write(output_dir.join(format!("{stem}.m4a")), SENTINEL_BYTES)?;
    }
    Ok(())
}

/// Write a legacy on-disk `timestamps.json` (the `ConcertInfo` shape the real
/// splitter used to write before per-song timestamps moved into the DB),
/// readable back by `jobs::split::read_analysis_timestamps`. Used both for
/// `SplitMode::Analyze`'s test-control completion and for seeding a
/// "legacy concert" fixture that predates the DB columns.
pub(crate) fn write_legacy_timestamps_json(
    output_dir: &Path,
    songs: &[SongTimestamp],
) -> Result<()> {
    let info = ConcertInfo {
        artist: String::new(),
        source: String::new(),
        show: String::new(),
        date: None,
        album: String::new(),
        description: None,
        set_list: songs
            .iter()
            .map(|s| Song {
                title: s.title.clone(),
            })
            .collect(),
        musicians: vec![],
        preview_image_url: None,
        teaser: None,
        timestamps: Some(songs.to_vec()),
    };
    let json = serde_json::to_string(&info)?;
    std::fs::write(output_dir.join("timestamps.json"), json)?;
    Ok(())
}

/// Deterministic fake per-song timestamps for a fixture that has no real
/// analysis to report: 90s spans with a 10s gap between songs, so
/// `derive_interludes` has real gaps to find for callers that check
/// interlude behavior. Mirrors `jobs::split`'s own
/// `config_with_fake_analyze` test helper.
pub(crate) fn fake_analysis_timestamps(titles: &[String]) -> Vec<SongTimestamp> {
    let mut cursor = 0.0;
    titles
        .iter()
        .map(|title| {
            let start = cursor;
            let end = start + 90.0;
            cursor = end + 10.0;
            SongTimestamp {
                title: title.clone(),
                start_time: start,
                end_time: end,
                duration: end - start,
            }
        })
        .collect()
}

fn fixture_source_url(n: u64) -> String {
    format!("https://example.test/tiny-desk/test-control-{n}")
}

fn fixture_set_list(n: u64) -> Vec<String> {
    vec![
        format!("Test Control Track {n}.1"),
        format!("Test Control Track {n}.2"),
        format!("Test Control Track {n}.3"),
    ]
}

/// Cloneable handle to a monotonic fixture-number allocator, starting at `1`.
/// A [`SeedContext`] built with [`SeedContext::new`] gets a fresh allocator;
/// callers that need one allocator shared across many contexts (Test Control
/// holds one for the lifetime of the server process, per ADR 0003 — not reset
/// by `test.reset`) construct a `FixtureIds` once (via `FixtureIds::default()`)
/// and pass clones to [`SeedContext::with_ids`].
#[derive(Clone)]
pub struct FixtureIds(Arc<AtomicU64>);

impl Default for FixtureIds {
    fn default() -> Self {
        Self(Arc::new(AtomicU64::new(1)))
    }
}

impl FixtureIds {
    fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

/// A test-only handle carrying the SQLite connection and fixture-id allocator
/// that every Database Seed API call needs.
pub struct SeedContext<'a> {
    conn: &'a Connection,
    ids: FixtureIds,
}

impl<'a> SeedContext<'a> {
    /// A `SeedContext` with its own fresh fixture-number allocator, starting
    /// at 1. Use this from Rust tests, where each test gets an isolated
    /// in-memory database and does not need cross-test uniqueness.
    pub fn new(conn: &'a Connection) -> Self {
        Self::with_ids(conn, FixtureIds::default())
    }

    /// A `SeedContext` sharing an existing allocator — for callers (Test
    /// Control) that need fixture numbers unique across many seed calls
    /// against the same long-lived connection/process.
    pub fn with_ids(conn: &'a Connection, ids: FixtureIds) -> Self {
        Self { conn, ids }
    }

    pub fn seed_listing(&self, seed: SeedListing) -> Result<Concert> {
        let n = self.ids.next();
        let source_url = seed.source_url.unwrap_or_else(|| fixture_source_url(n));
        let title = seed.title.unwrap_or_else(|| format!("Test Listing {n}"));
        // `concert_date`/`teaser` are already resolved by `SeedListing::default()`
        // (whose `Default` is `Some(...)`): a missing field became `Some(...)` via
        // `#[serde(default)]`, and an explicit JSON `null` became `None` — no
        // further defaulting needed here.
        let concert_date = seed.concert_date;
        let teaser = seed.teaser;

        let concert = upsert_and_fetch(
            self.conn,
            &NewListing {
                source_url,
                title,
                concert_date: concert_date.clone(),
                teaser: teaser.clone(),
            },
        )?;
        normalize_listing_fields(self.conn, concert.id, &concert_date, &teaser)?;
        concerts::get_concert(self.conn, concert.id)
    }

    pub fn seed_scraped_concert(&self, seed: SeedScrapedConcert) -> Result<Concert> {
        let n = self.ids.next();
        let source_url = seed.source_url.unwrap_or_else(|| fixture_source_url(n));
        let title = seed
            .title
            .unwrap_or_else(|| format!("Test Scraped Concert {n}"));
        let concert_date = seed.concert_date;
        let artist = seed
            .artist
            .unwrap_or_else(|| format!("Test Scraped Artist {n}"));
        let album = seed
            .album
            .unwrap_or_else(|| format!("Test Scraped Album {n}"));
        let set_list = seed.set_list.unwrap_or_else(|| fixture_set_list(n));

        let concert = upsert_and_fetch(
            self.conn,
            &NewListing {
                source_url,
                title,
                concert_date: concert_date.clone(),
                teaser: None,
            },
        )?;
        normalize_listing_fields(self.conn, concert.id, &concert_date, &None)?;
        reset_fixture_lifecycle_state(self.conn, concert.id)?;
        concerts::update_metadata(
            self.conn,
            concert.id,
            &MetadataUpdate {
                artist,
                album,
                description: None,
                set_list,
                musicians: vec![],
            },
        )?;
        concerts::get_concert(self.conn, concert.id)
    }

    pub fn seed_lifecycle_concert(&self, seed: SeedLifecycleConcert) -> Result<Concert> {
        let n = self.ids.next();
        let source_url = seed.source_url.unwrap_or_else(|| fixture_source_url(n));
        let title = seed
            .title
            .unwrap_or_else(|| format!("Test Lifecycle Concert {n}"));
        let concert_date = seed.concert_date;
        let artist = seed
            .artist
            .unwrap_or_else(|| format!("Test Lifecycle Artist {n}"));
        let album = seed
            .album
            .unwrap_or_else(|| format!("Test Lifecycle Album {n}"));
        let set_list = seed.set_list.unwrap_or_else(|| fixture_set_list(n));

        let concert = upsert_and_fetch(
            self.conn,
            &NewListing {
                source_url,
                title,
                concert_date: concert_date.clone(),
                teaser: None,
            },
        )?;
        normalize_listing_fields(self.conn, concert.id, &concert_date, &None)?;
        reset_fixture_lifecycle_state(self.conn, concert.id)?;
        concerts::update_metadata(
            self.conn,
            concert.id,
            &MetadataUpdate {
                artist,
                album,
                description: None,
                set_list,
                musicians: vec![],
            },
        )?;

        if seed.downloaded || seed.split {
            lifecycle::try_mark_download_started(self.conn, concert.id)?;
            lifecycle::mark_download_succeeded(self.conn, concert.id, "mp4")?;
        }
        if seed.split {
            lifecycle::try_mark_split_started(self.conn, concert.id)?;
            lifecycle::mark_split_succeeded(self.conn, concert.id)?;
        }
        if let Some(timestamps) = &seed.auto_timestamps {
            split_timestamps::set_auto_split_timestamps(self.conn, concert.id, timestamps)?;
        }
        if let Some(timestamps) = &seed.user_timestamps {
            split_timestamps::set_user_split_timestamps(self.conn, concert.id, timestamps)?;
        }
        if let Some(duration) = seed.media_duration {
            split_timestamps::set_media_duration(self.conn, concert.id, duration)?;
        }
        if let Some(tracks_present) = &seed.tracks_present {
            split_timestamps::set_tracks_present(self.conn, concert.id, tracks_present)?;
        }
        if let Some(tracks_liked) = &seed.tracks_liked {
            split_timestamps::set_tracks_liked(self.conn, concert.id, tracks_liked)?;
        }

        concerts::get_concert(self.conn, concert.id)
    }

    pub fn seed_media_concert(&self, workdir: &Path, seed: SeedMediaConcert) -> Result<Concert> {
        validate_seed_media_request(&seed)?;
        let concert = self.seed_lifecycle_concert(seed.lifecycle.clone())?;
        write_seed_media_files(workdir, &concert, &seed)?;
        concerts::get_concert(self.conn, concert.id)
    }

    /// A concert with track state but `album = NULL` — a historical/defensive
    /// shape no current product write path produces (`update_metadata`
    /// requires an `album: String`), which is exactly why
    /// `track_details`/`track-details` must tolerate a NULL album on the
    /// row. Direct SQL for the track-state columns, same
    /// fixture-normalization rationale as [`reset_fixture_lifecycle_state`].
    pub fn seed_album_null_concert(&self, seed: SeedAlbumNullConcert) -> Result<Concert> {
        let n = self.ids.next();
        let source_url = seed.source_url.unwrap_or_else(|| fixture_source_url(n));
        let title = seed
            .title
            .unwrap_or_else(|| format!("Test Album-Null Concert {n}"));
        let set_list = seed.set_list.unwrap_or_else(|| fixture_set_list(n));

        let concert = upsert_and_fetch(
            self.conn,
            &NewListing {
                source_url,
                title,
                concert_date: Some(DEFAULT_CONCERT_DATE.to_string()),
                teaser: None,
            },
        )?;
        normalize_listing_fields(
            self.conn,
            concert.id,
            &Some(DEFAULT_CONCERT_DATE.to_string()),
            &None,
        )?;
        reset_fixture_lifecycle_state(self.conn, concert.id)?;

        // No product function sets set_list without also requiring an album
        // (`update_metadata` writes both together) — direct SQL for this
        // column only, same fixture-normalization rationale as
        // `reset_fixture_lifecycle_state`. `tracks_present`/`tracks_liked` do
        // have real product setters, so those compose the normal way.
        let set_list_json = serde_json::to_string(&set_list)?;
        self.conn.execute(
            "UPDATE concerts SET set_list_json = ?1 WHERE id = ?2",
            params![set_list_json, concert.id],
        )?;
        if let Some(tracks_present) = &seed.tracks_present {
            split_timestamps::set_tracks_present(self.conn, concert.id, tracks_present)?;
        }
        if let Some(tracks_liked) = &seed.tracks_liked {
            split_timestamps::set_tracks_liked(self.conn, concert.id, tracks_liked)?;
        }

        concerts::get_concert(self.conn, concert.id)
    }
}

fn validate_seed_media_request(seed: &SeedMediaConcert) -> Result<()> {
    resolved_media_extension(&seed.track_file_extension)?;
    if seed.source_file {
        resolved_media_extension(&seed.source_file_extension)?;
    }
    let set_list_len = seed
        .lifecycle
        .set_list
        .as_ref()
        .map_or_else(|| fixture_set_list(0).len(), Vec::len);
    if let Some(indices) = &seed.track_files {
        for &index in indices {
            if index >= set_list_len {
                anyhow::bail!(
                    "track_files index {index} is out of range for set_list length {set_list_len}"
                );
            }
        }
    }

    if seed.interlude_files {
        if seed.lifecycle.user_timestamps.is_none() && seed.lifecycle.auto_timestamps.is_none() {
            anyhow::bail!(
                "interlude_files requires user_timestamps or auto_timestamps to derive gaps from"
            );
        }
        if seed.lifecycle.media_duration.is_none() {
            anyhow::bail!("interlude_files requires media_duration to derive gaps against");
        }
    }

    if seed.legacy_timestamps_json && seed.lifecycle.auto_timestamps.is_some() {
        anyhow::bail!(
            "legacy_timestamps_json conflicts with auto_timestamps: a legacy concert has no \
             auto column by definition — its automated timestamps live only in the on-disk \
             timestamps.json this seed writes"
        );
    }

    if seed.source_file_kind == SourceFileKind::RealAudio {
        if !seed.source_file {
            anyhow::bail!("source_file_kind: real_audio requires source_file: true");
        }
        match seed.source_file_extension.as_deref() {
            None | Some("m4a") => {}
            Some(other) => anyhow::bail!(
                "source_file_kind: real_audio only supports the m4a container ffmpeg \
                 generates, got extension {other:?}"
            ),
        }
    }

    Ok(())
}

fn write_seed_media_files(
    workdir: &Path,
    concert: &Concert,
    seed: &SeedMediaConcert,
) -> Result<()> {
    let album = concert
        .album
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("seed_media_concert requires an album"))?;
    let dir = crate::model::concert_dir(workdir, album);
    std::fs::create_dir_all(&dir)?;

    if seed.source_file {
        let stem = crate::model::sanitize_album(album);
        match seed.source_file_kind {
            SourceFileKind::Sentinel => {
                let ext = resolved_media_extension(&seed.source_file_extension)?;
                std::fs::write(dir.join(format!("{stem}.{ext}")), SENTINEL_BYTES)?;
            }
            SourceFileKind::RealAudio => {
                // validate_seed_media_request already confirmed
                // source_file_extension is None or "m4a" — always write the
                // container ffmpeg actually generates rather than falling
                // through resolved_media_extension's unrelated "mp3" default
                // for an omitted extension.
                generate_real_audio_m4a(&dir.join(format!("{stem}.m4a")))?;
            }
        }
    }

    if let Some(indices) = &seed.track_files {
        let ext = resolved_media_extension(&seed.track_file_extension)?;
        let titles: Vec<&str> = indices
            .iter()
            .map(|&index| {
                concert
                    .set_list
                    .get(index)
                    .map(String::as_str)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "track_files index {index} is out of range for set_list length {}",
                            concert.set_list.len()
                        )
                    })
            })
            .collect::<Result<_>>()?;
        write_track_sentinels(&dir, titles.into_iter(), ext)?;
    }

    if seed.preview_image {
        std::fs::write(dir.join("preview.jpg"), SENTINEL_BYTES)?;
    }

    if seed.legacy_timestamps_json {
        let songs = fake_analysis_timestamps(&concert.set_list);
        write_legacy_timestamps_json(&dir, &songs)?;
    }

    if seed.interlude_files {
        // Both already validated present by validate_seed_media_request; the
        // ok_or_else messages here are defensive, not reachable in practice.
        let media_duration = seed
            .lifecycle
            .media_duration
            .ok_or_else(|| anyhow::anyhow!("interlude_files requires media_duration"))?;
        let songs = seed
            .lifecycle
            .user_timestamps
            .as_ref()
            .or(seed.lifecycle.auto_timestamps.as_ref())
            .ok_or_else(|| {
                anyhow::anyhow!("interlude_files requires user_timestamps or auto_timestamps")
            })?;
        write_interlude_sentinels(&dir, songs, media_duration)?;
    }

    Ok(())
}

/// Generate a short (~5s), genuinely playable m4a via `ffmpeg` — needed only
/// for routes whose public behavior depends on real `ffprobe` output (the
/// split-timestamps POST happy path bounds-checks proposed end times against
/// the source's real duration). Fails loudly with a clear "requires ffmpeg on
/// PATH" message rather than silently falling back to a sentinel — a silent
/// fallback would surface as that route 500ing on a real ffprobe failure
/// instead of the seed call itself failing with an actionable message.
fn generate_real_audio_m4a(dest: &Path) -> Result<()> {
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=5",
            "-c:a",
            "aac",
            "-b:a",
            "32k",
        ])
        .arg(dest)
        .output()
        .context("spawning ffmpeg — source_file_kind: real_audio seeds require ffmpeg on PATH")?;
    anyhow::ensure!(
        output.status.success(),
        "ffmpeg failed to generate real audio fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn resolved_media_extension(extension: &Option<String>) -> Result<&str> {
    let ext = extension.as_deref().unwrap_or("mp3");
    match ext {
        "mp4" | "m4a" | "webm" | "mkv" | "mp3" | "ogg" | "opus" | "wav" | "flac" => Ok(ext),
        _ => anyhow::bail!("unsupported seed media extension: {ext}"),
    }
}

/// Inserts (or updates, on a `source_url` collision) a listing through the
/// same `db::concerts::upsert_listing` path the real scraper uses. Looks the
/// row back up by `source_url` rather than trusting
/// `Connection::last_insert_rowid` — `upsert_listing` is an `INSERT ... ON
/// CONFLICT DO UPDATE`, and SQLite only advances `last_insert_rowid` for the
/// `INSERT` branch, so it would silently return a stale id whenever a seed
/// call reseeds an already-used `source_url`.
fn upsert_and_fetch(conn: &Connection, listing: &NewListing) -> Result<Concert> {
    concerts::upsert_listing(conn, listing)?;
    concerts::get_concert_by_url(conn, &listing.source_url)?.ok_or_else(|| {
        anyhow::anyhow!(
            "upsert_listing succeeded but the row is not readable back by source_url: {}",
            listing.source_url
        )
    })
}

/// `upsert_listing`'s `ON CONFLICT` branch uses `COALESCE(excluded.x,
/// concerts.x)` for `concert_date`/`teaser`, so a seed's resolved `None`
/// value would silently preserve whatever a prior seed left in place on a
/// reused `source_url` instead of clearing it. This direct UPDATE writes the
/// already-resolved values so every seed call ends with exactly the fields it
/// resolved, regardless of prior state at that `source_url`.
///
/// Conditional on the values actually changing: `concerts` has an `AFTER
/// UPDATE` trigger (migration 0003) that bumps `updated_at` for any ordinary
/// update, and this fixture-normalization write must not move `updated_at`
/// when it is a no-op — `db::tests::seed` relies on producing the exact same
/// row byte-for-byte across repeated calls.
fn normalize_listing_fields(
    conn: &Connection,
    id: i64,
    concert_date: &Option<String>,
    teaser: &Option<String>,
) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET concert_date = ?1, teaser = ?2
         WHERE id = ?3 AND (concert_date IS NOT ?1 OR teaser IS NOT ?2)",
        params![concert_date, teaser, id],
    )?;
    Ok(())
}

/// Resets download/split/archive/timestamp/media-duration state to inert
/// defaults so seeding a reused `source_url` never leaves state from a prior
/// seed call behind. Uses a direct `UPDATE`, not `lifecycle::clear_download_state`
/// / `clear_split_state` (which are for genuine user-initiated deletes and each
/// record a `DownloadDelete`/`SplitDelete` event) — that would be a false audit
/// trail here, since nothing was ever actually downloaded or split by this
/// fixture. Conditional on the row not already being fully inert, for the same
/// `updated_at`-stability reason as [`normalize_listing_fields`].
fn reset_fixture_lifecycle_state(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET
             download_started_at = NULL, downloaded_at = NULL,
             downloaded_extension = NULL, download_errors_json = '[]',
             split_started_at = NULL, split_at = NULL, split_errors_json = '[]',
             tracks_present = NULL, tracks_liked = NULL,
             archive_started_at = NULL, archived_at = NULL, archive_errors_json = '[]',
             auto_split_timestamps_json = NULL, user_split_timestamps_json = NULL,
             media_duration = NULL
         WHERE id = ?1 AND (
             download_started_at IS NOT NULL OR downloaded_at IS NOT NULL OR
             downloaded_extension IS NOT NULL OR download_errors_json IS NOT '[]' OR
             split_started_at IS NOT NULL OR split_at IS NOT NULL OR
             split_errors_json IS NOT '[]' OR tracks_present IS NOT NULL OR
             tracks_liked IS NOT NULL OR
             archive_started_at IS NOT NULL OR archived_at IS NOT NULL OR
             archive_errors_json IS NOT '[]' OR auto_split_timestamps_json IS NOT NULL OR
             user_split_timestamps_json IS NOT NULL OR media_duration IS NOT NULL
         )",
        params![id],
    )?;
    Ok(())
}

/// Seed input for `SeedContext::seed_listing` / `test.seed_listing`. A
/// missing field takes the listed default; an explicit JSON `null` seeds
/// `None` — for `concert_date`/`teaser`, whose defaults are `Some(...)`,
/// that means an explicit `null` produces a real SQL NULL, distinct from
/// omitting the field.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeedListing {
    pub source_url: Option<String>,
    pub title: Option<String>,
    pub concert_date: Option<String>,
    pub teaser: Option<String>,
}

impl Default for SeedListing {
    fn default() -> Self {
        Self {
            source_url: None,
            title: None,
            concert_date: Some(DEFAULT_CONCERT_DATE.to_string()),
            teaser: Some("Test listing teaser".to_string()),
        }
    }
}

/// Seed input for `SeedContext::seed_scraped_concert` /
/// `test.seed_scraped_concert`. `set_list: null` (or omitted) takes the
/// generated three-track default; pass `set_list: []` for an empty list.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeedScrapedConcert {
    pub source_url: Option<String>,
    pub title: Option<String>,
    pub concert_date: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub set_list: Option<Vec<String>>,
}

impl Default for SeedScrapedConcert {
    fn default() -> Self {
        Self {
            source_url: None,
            title: None,
            concert_date: Some(DEFAULT_CONCERT_DATE.to_string()),
            artist: None,
            album: None,
            set_list: None,
        }
    }
}

/// Seed input for `SeedContext::seed_lifecycle_concert` /
/// `test.seed_lifecycle_concert`. `downloaded`/`split` default to `false`
/// (an inert fixture); `split: true` implies `downloaded: true` regardless of
/// the `downloaded` field, matching the real download-then-split lifecycle.
///
/// `tracks_present` defaults to `None` (column stays `NULL`, matching a
/// concert that has never been split). When set, it is written verbatim via
/// `db::split_timestamps::set_tracks_present` with no validation against
/// `set_list`'s length — the web handlers already tolerate a short
/// `tracks_present` array (`.get(idx).unwrap_or(false)`), so permissive
/// seeding lets tests exercise those defensive paths too.
///
/// `tracks_liked` has the same optional/null semantics and is written
/// verbatim when set. It exists for black-box media-info cases that need to
/// observe liked metadata without first driving the `/like` endpoint.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeedLifecycleConcert {
    pub source_url: Option<String>,
    pub title: Option<String>,
    pub concert_date: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub set_list: Option<Vec<String>>,
    pub downloaded: bool,
    pub split: bool,
    pub auto_timestamps: Option<Vec<concert_types::SongTimestamp>>,
    pub user_timestamps: Option<Vec<concert_types::SongTimestamp>>,
    pub media_duration: Option<f64>,
    pub tracks_present: Option<Vec<bool>>,
    pub tracks_liked: Option<Vec<bool>>,
}

impl Default for SeedLifecycleConcert {
    fn default() -> Self {
        Self {
            source_url: None,
            title: None,
            concert_date: Some(DEFAULT_CONCERT_DATE.to_string()),
            artist: None,
            album: None,
            set_list: None,
            downloaded: false,
            split: false,
            auto_timestamps: None,
            user_timestamps: None,
            media_duration: None,
            tracks_present: None,
            tracks_liked: None,
        }
    }
}

/// Whether `seed_media_concert`'s source file is a sentinel (existence/
/// extension checks only) or genuinely playable audio generated with
/// `ffmpeg` (for routes whose public behavior depends on real `ffprobe`
/// output, e.g. the split-timestamps POST happy path). Defaults to
/// `Sentinel` — real audio is deliberately opt-in since generating it shells
/// out and is slower.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFileKind {
    #[default]
    Sentinel,
    RealAudio,
}

/// Seed input for `SeedContext::seed_media_concert` /
/// `test.seed_media_concert`. It starts from the lifecycle fixture shape and
/// can write dummy media files under the configured workdir. Every field
/// beyond the flattened lifecycle names a domain artifact (a preview image,
/// interlude files derived from a timestamp gap, a legacy on-disk
/// `timestamps.json`, or genuinely playable source audio) rather than a raw
/// path or bytes, so this stays a scenario-seed vocabulary rather than a
/// generic filesystem-mutation API. Written files are sentinel bytes — not
/// valid audio/video and must not be used for ffprobe-backed paths — unless
/// `source_file_kind: real_audio` is set, which is the one supported way to
/// get a real ffprobe-readable file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeedMediaConcert {
    #[serde(flatten)]
    pub lifecycle: SeedLifecycleConcert,
    pub source_file: bool,
    pub source_file_extension: Option<String>,
    pub source_file_kind: SourceFileKind,
    pub track_files: Option<Vec<usize>>,
    pub track_file_extension: Option<String>,
    /// Write a sentinel `preview.jpg` in the concert directory.
    pub preview_image: bool,
    /// Write sentinel interlude files for every gap `derive_interludes` finds
    /// between the seeded user (falling back to auto) timestamps and
    /// `media_duration` — both must be present on the seed request.
    pub interlude_files: bool,
    /// Write an on-disk `timestamps.json` in the legacy `ConcertInfo` shape
    /// (predating the DB's `auto_split_timestamps_json` column), generated
    /// from the set list via the same deterministic fake-analysis timestamps
    /// the Job Driver uses. Conflicts with `auto_timestamps`: a genuinely
    /// legacy concert has no auto column by definition.
    pub legacy_timestamps_json: bool,
}

/// Seed input for `SeedContext::seed_album_null_concert` /
/// `test.seed_album_null_concert`. Unlike every other seed, this
/// deliberately leaves `album` as SQL NULL — `SeedLifecycleConcert`/
/// `SeedMediaConcert` cannot produce that shape because `update_metadata`
/// requires a real `album: String`, and `album: null` on those seeds means
/// "generate a default" rather than "store NULL".
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeedAlbumNullConcert {
    pub source_url: Option<String>,
    pub title: Option<String>,
    pub set_list: Option<Vec<String>>,
    pub tracks_present: Option<Vec<bool>>,
    pub tracks_liked: Option<Vec<bool>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connection::open_in_memory;
    use crate::db::tests::events_for;

    fn ctx(conn: &Connection) -> SeedContext<'_> {
        SeedContext::new(conn)
    }

    #[test]
    fn seed_listing_defaults_are_unique_across_calls() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let a = seeds.seed_listing(SeedListing::default()).unwrap();
        let b = seeds.seed_listing(SeedListing::default()).unwrap();
        assert_ne!(a.source_url, b.source_url);
        assert_ne!(a.title, b.title);
        assert_eq!(a.concert_date.as_deref(), Some(DEFAULT_CONCERT_DATE));
        assert_eq!(a.teaser.as_deref(), Some("Test listing teaser"));
    }

    #[test]
    fn seed_listing_explicit_fields_override_defaults() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_listing(SeedListing {
                source_url: Some("https://npr.org/c/explicit".to_string()),
                title: Some("Explicit Title".to_string()),
                concert_date: Some("2024-01-01".to_string()),
                teaser: Some("Explicit teaser".to_string()),
            })
            .unwrap();
        assert_eq!(concert.source_url, "https://npr.org/c/explicit");
        assert_eq!(concert.title, "Explicit Title");
        assert_eq!(concert.concert_date.as_deref(), Some("2024-01-01"));
        assert_eq!(concert.teaser.as_deref(), Some("Explicit teaser"));
    }

    #[test]
    fn seed_listing_null_seeds_none_on_fresh_row() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_listing(SeedListing {
                concert_date: None,
                teaser: None,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(concert.concert_date, None);
        assert_eq!(concert.teaser, None);
    }

    #[test]
    fn seed_listing_reseed_keeps_id_stable() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let url = "https://npr.org/c/reseed".to_string();
        let first = seeds
            .seed_listing(SeedListing {
                source_url: Some(url.clone()),
                ..Default::default()
            })
            .unwrap();
        let second = seeds
            .seed_listing(SeedListing {
                source_url: Some(url),
                title: Some("Updated Title".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(second.title, "Updated Title");
    }

    #[test]
    fn seed_listing_reseed_with_null_clears_previously_set_fields() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let url = "https://npr.org/c/clear".to_string();
        seeds
            .seed_listing(SeedListing {
                source_url: Some(url.clone()),
                concert_date: Some("2024-05-01".to_string()),
                teaser: Some("Has a teaser".to_string()),
                ..Default::default()
            })
            .unwrap();
        let reseeded = seeds
            .seed_listing(SeedListing {
                source_url: Some(url),
                concert_date: None,
                teaser: None,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            reseeded.concert_date, None,
            "explicit null on reseed must clear the previously-set concert_date, \
             not preserve it via upsert_listing's COALESCE"
        );
        assert_eq!(
            reseeded.teaser, None,
            "explicit null on reseed must clear the previously-set teaser"
        );
    }

    #[test]
    fn seed_scraped_concert_defaults_set_list_and_resets_teaser() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_scraped_concert(SeedScrapedConcert::default())
            .unwrap();
        assert_eq!(concert.set_list.len(), 3);
        assert_eq!(concert.teaser, None);
        assert!(concert.artist.is_some());
        assert!(concert.album.is_some());
        assert!(concert.metadata_scraped_at.is_some());
    }

    #[test]
    fn seed_scraped_concert_empty_set_list_is_explicit() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_scraped_concert(SeedScrapedConcert {
                set_list: Some(vec![]),
                ..Default::default()
            })
            .unwrap();
        assert!(concert.set_list.is_empty());
    }

    #[test]
    fn seed_scraped_concert_resets_stale_lifecycle_state_without_delete_events() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let url = "https://npr.org/c/stale-scrape".to_string();
        let downloaded = seeds
            .seed_lifecycle_concert(SeedLifecycleConcert {
                source_url: Some(url.clone()),
                split: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(downloaded.split_status(), crate::model::SplitStatus::Split);

        let rescraped = seeds
            .seed_scraped_concert(SeedScrapedConcert {
                source_url: Some(url),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            rescraped.download_status(),
            crate::model::DownloadStatus::NotDownloaded
        );
        assert_eq!(
            rescraped.split_status(),
            crate::model::SplitStatus::NotSplit
        );

        let events = events_for(&conn, rescraped.id);
        assert!(
            !events.iter().any(|(event, _)| event == "download_delete"
                || event == "split_delete"),
            "reset must not emit delete events for state that was never a real user delete: {events:?}"
        );
    }

    #[test]
    fn seed_lifecycle_concert_downloaded_and_split_marks() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_lifecycle_concert(SeedLifecycleConcert {
                split: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            concert.download_status(),
            crate::model::DownloadStatus::Downloaded
        );
        assert_eq!(concert.split_status(), crate::model::SplitStatus::Split);
    }

    #[test]
    fn seed_lifecycle_concert_reuse_resets_to_inert_defaults() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let url = "https://npr.org/c/lifecycle-reuse".to_string();
        let first = seeds
            .seed_lifecycle_concert(SeedLifecycleConcert {
                source_url: Some(url.clone()),
                split: true,
                media_duration: Some(120.0),
                auto_timestamps: Some(vec![]),
                ..Default::default()
            })
            .unwrap();
        // Liked-track state comes from a real product action (not a seed
        // param), so it can go stale on a reused source_url the same way
        // download/split/timestamp state can.
        split_timestamps::set_tracks_liked(&conn, first.id, &[true, false]).unwrap();

        let reseeded = seeds
            .seed_lifecycle_concert(SeedLifecycleConcert {
                source_url: Some(url),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            reseeded.download_status(),
            crate::model::DownloadStatus::NotDownloaded,
            "reused source_url must reset to inert defaults, not keep prior downloaded state"
        );
        assert_eq!(reseeded.split_status(), crate::model::SplitStatus::NotSplit);
        assert_eq!(reseeded.media_duration, None);
        assert!(
            reseeded.tracks_liked.is_empty(),
            "reused source_url must not keep a prior seed's liked-track state"
        );
        let stored = split_timestamps::get_split_timestamps(&conn, reseeded.id).unwrap();
        assert_eq!(stored.auto, None);
        assert_eq!(stored.user, None);
    }

    #[test]
    fn seed_lifecycle_concert_tracks_present_persists_exact_value() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_lifecycle_concert(SeedLifecycleConcert {
                tracks_present: Some(vec![true, false]),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(concert.tracks_present, vec![true, false]);
    }

    #[test]
    fn seed_lifecycle_concert_tracks_liked_persists_exact_value() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_lifecycle_concert(SeedLifecycleConcert {
                tracks_liked: Some(vec![false, true]),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(concert.tracks_liked, vec![false, true]);
    }

    #[test]
    fn seed_lifecycle_concert_tracks_present_reseed_without_field_clears_it() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let url = "https://npr.org/c/tracks-present-reseed".to_string();
        seeds
            .seed_lifecycle_concert(SeedLifecycleConcert {
                source_url: Some(url.clone()),
                tracks_present: Some(vec![true, false]),
                ..Default::default()
            })
            .unwrap();

        // Reseed the same source_url with tracks_present genuinely omitted
        // from the JSON (deserialized default is None, per the struct's
        // Default) — must reset to inert, not keep the prior seed's array.
        let omitted: SeedLifecycleConcert = serde_json::from_value(serde_json::json!({
            "source_url": url,
        }))
        .unwrap();
        assert_eq!(omitted.tracks_present, None);
        let reseeded = seeds.seed_lifecycle_concert(omitted).unwrap();
        assert!(
            reseeded.tracks_present.is_empty(),
            "omitting tracks_present on reseed must clear a prior seed's array, \
             not preserve it"
        );
    }

    #[test]
    fn seed_lifecycle_concert_tracks_present_reseed_with_explicit_null_clears_it() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let url = "https://npr.org/c/tracks-present-null-reseed".to_string();
        seeds
            .seed_lifecycle_concert(SeedLifecycleConcert {
                source_url: Some(url.clone()),
                tracks_present: Some(vec![true, true, false]),
                ..Default::default()
            })
            .unwrap();

        let cleared: SeedLifecycleConcert = serde_json::from_value(serde_json::json!({
            "source_url": url,
            "tracks_present": null,
        }))
        .unwrap();
        let reseeded = seeds.seed_lifecycle_concert(cleared).unwrap();
        assert!(
            reseeded.tracks_present.is_empty(),
            "explicit null on reseed must clear a prior seed's tracks_present array"
        );
    }

    #[test]
    fn seed_lifecycle_concert_timestamps_and_media_duration() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let ts = concert_types::SongTimestamp {
            title: "Song One".to_string(),
            start_time: 0.0,
            end_time: 10.0,
            duration: 10.0,
        };
        let concert = seeds
            .seed_lifecycle_concert(SeedLifecycleConcert {
                auto_timestamps: Some(vec![ts.clone()]),
                user_timestamps: Some(vec![ts]),
                media_duration: Some(42.0),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(concert.media_duration, Some(42.0));
        let stored = split_timestamps::get_split_timestamps(&conn, concert.id).unwrap();
        assert_eq!(stored.auto.map(|v| v.len()), Some(1));
        assert_eq!(stored.user.map(|v| v.len()), Some(1));
    }

    #[test]
    fn seed_media_concert_writes_selected_dummy_track_files() {
        let conn = open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_media_concert(
                workdir.path(),
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        album: Some("Media Fixture Album".to_string()),
                        set_list: Some(vec![
                            "Song A".to_string(),
                            "Song B".to_string(),
                            "Song C".to_string(),
                        ]),
                        ..Default::default()
                    },
                    track_files: Some(vec![0, 2]),
                    ..Default::default()
                },
            )
            .unwrap();

        let dir = crate::model::concert_dir(workdir.path(), concert.album.as_deref().unwrap());
        assert!(dir.join("Song A.mp3").exists());
        assert!(!dir.join("Song B.mp3").exists());
        assert!(dir.join("Song C.mp3").exists());
    }

    #[test]
    fn seed_media_concert_can_write_source_file() {
        let conn = open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_media_concert(
                workdir.path(),
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        album: Some("Source Fixture: Album".to_string()),
                        ..Default::default()
                    },
                    source_file: true,
                    source_file_extension: Some("mp4".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();

        let dir = crate::model::concert_dir(workdir.path(), concert.album.as_deref().unwrap());
        assert!(dir.join("Source Fixture Album.mp4").exists());
    }

    #[test]
    fn seed_media_concert_rejects_out_of_range_track_file_index() {
        let conn = open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let seeds = ctx(&conn);
        let err = seeds
            .seed_media_concert(
                workdir.path(),
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        set_list: Some(vec!["Only Song".to_string()]),
                        ..Default::default()
                    },
                    track_files: Some(vec![1]),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("out of range"),
            "unexpected error: {err}"
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM concerts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "invalid media seed must not write a DB row");
    }

    // ---------- SeedMediaConcert: preview_image / legacy_timestamps_json / interlude_files ----------

    #[test]
    fn seed_media_concert_can_write_preview_image() {
        let conn = open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_media_concert(
                workdir.path(),
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        album: Some("Preview Fixture Album".to_string()),
                        ..Default::default()
                    },
                    preview_image: true,
                    ..Default::default()
                },
            )
            .unwrap();

        let dir = crate::model::concert_dir(workdir.path(), concert.album.as_deref().unwrap());
        assert!(dir.join("preview.jpg").exists());
    }

    #[test]
    fn seed_media_concert_legacy_timestamps_json_is_readable_by_the_splitter_backfill() {
        let conn = open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_media_concert(
                workdir.path(),
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        album: Some("Legacy Timestamps Album".to_string()),
                        set_list: Some(vec!["Old A".to_string(), "Old B".to_string()]),
                        ..Default::default()
                    },
                    legacy_timestamps_json: true,
                    ..Default::default()
                },
            )
            .unwrap();

        let dir = crate::model::concert_dir(workdir.path(), concert.album.as_deref().unwrap());
        assert!(dir.join("timestamps.json").exists());
        let timestamps = crate::jobs::split::read_analysis_timestamps(&dir).unwrap();
        assert_eq!(timestamps.len(), 2);
        assert_eq!(timestamps[0].title, "Old A");
    }

    #[test]
    fn seed_media_concert_legacy_timestamps_json_conflicts_with_auto_timestamps() {
        let conn = open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let seeds = ctx(&conn);
        let err = seeds
            .seed_media_concert(
                workdir.path(),
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        set_list: Some(vec!["A".to_string()]),
                        auto_timestamps: Some(vec![]),
                        ..Default::default()
                    },
                    legacy_timestamps_json: true,
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("conflicts with auto_timestamps"));
    }

    #[test]
    fn seed_media_concert_interlude_files_writes_sentinels_for_gaps() {
        let conn = open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let seeds = ctx(&conn);
        let ts = vec![
            concert_types::SongTimestamp {
                title: "Song A".to_string(),
                start_time: 0.0,
                end_time: 55.0,
                duration: 55.0,
            },
            concert_types::SongTimestamp {
                title: "Song B".to_string(),
                start_time: 60.0,
                end_time: 115.0,
                duration: 55.0,
            },
        ];
        let concert = seeds
            .seed_media_concert(
                workdir.path(),
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        album: Some("Interlude Fixture Album".to_string()),
                        set_list: Some(vec!["Song A".to_string(), "Song B".to_string()]),
                        user_timestamps: Some(ts),
                        media_duration: Some(115.0),
                        ..Default::default()
                    },
                    interlude_files: true,
                    ..Default::default()
                },
            )
            .unwrap();

        let dir = crate::model::concert_dir(workdir.path(), concert.album.as_deref().unwrap());
        assert!(
            dir.join("interlude_01.m4a").exists(),
            "the 55s-60s gap should produce interlude_01"
        );
    }

    #[test]
    fn seed_media_concert_interlude_files_requires_timestamps() {
        let conn = open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let seeds = ctx(&conn);
        let err = seeds
            .seed_media_concert(
                workdir.path(),
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        media_duration: Some(115.0),
                        ..Default::default()
                    },
                    interlude_files: true,
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(err
            .to_string()
            .contains("requires user_timestamps or auto_timestamps"));
    }

    #[test]
    fn seed_media_concert_interlude_files_requires_media_duration() {
        let conn = open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let seeds = ctx(&conn);
        let err = seeds
            .seed_media_concert(
                workdir.path(),
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        user_timestamps: Some(vec![]),
                        ..Default::default()
                    },
                    interlude_files: true,
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("requires media_duration"));
    }

    // ---------- SeedMediaConcert: source_file_kind: real_audio ----------

    /// Runs ffmpeg to generate a tiny real audio file; skips (returns) when
    /// ffmpeg isn't installed, matching the project's existing
    /// `create_test_audio_sync`-style skip convention (see `scan.rs`).
    #[tokio::test]
    async fn seed_media_concert_real_audio_produces_a_file_ffprobe_accepts() {
        let conn = open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds.seed_media_concert(
            workdir.path(),
            SeedMediaConcert {
                lifecycle: SeedLifecycleConcert {
                    album: Some("Real Audio Fixture Album".to_string()),
                    ..Default::default()
                },
                source_file: true,
                source_file_kind: SourceFileKind::RealAudio,
                ..Default::default()
            },
        );
        let concert = match concert {
            Ok(c) => c,
            // Only skip when ffmpeg genuinely isn't on PATH (the spawn
            // itself failed) — matching on this narrower "spawning ffmpeg"
            // message, not any error that happens to mention "ffmpeg",
            // keeps a real generation failure (wrong container, bad args,
            // ffmpeg present but erroring) a hard test failure instead of a
            // silently-skipped false pass.
            Err(e) if e.to_string().contains("spawning ffmpeg") => {
                eprintln!("skipping: ffmpeg not available ({e})");
                return;
            }
            Err(e) => panic!("unexpected error: {e}"),
        };

        let dir = crate::model::concert_dir(workdir.path(), concert.album.as_deref().unwrap());
        let source = dir.join("Real Audio Fixture Album.m4a");
        assert!(source.exists());
        // Prove ffprobe (the real product code path, not just file existence)
        // accepts this file and reports the ~5s duration the seed generates —
        // this is the whole point of `real_audio` over a sentinel byte file.
        let duration = crate::split_timestamps::probe_media_duration(&source)
            .await
            .expect("ffprobe must accept the generated real-audio fixture");
        assert!(
            (duration - 5.0).abs() < 1.0,
            "expected ~5s duration, got {duration}"
        );
    }

    #[test]
    fn seed_media_concert_real_audio_requires_source_file_true() {
        let conn = open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let seeds = ctx(&conn);
        let err = seeds
            .seed_media_concert(
                workdir.path(),
                SeedMediaConcert {
                    source_file_kind: SourceFileKind::RealAudio,
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("requires source_file: true"));
    }

    #[test]
    fn seed_media_concert_real_audio_rejects_non_m4a_extension() {
        let conn = open_in_memory().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        let seeds = ctx(&conn);
        let err = seeds
            .seed_media_concert(
                workdir.path(),
                SeedMediaConcert {
                    source_file: true,
                    source_file_extension: Some("mp4".to_string()),
                    source_file_kind: SourceFileKind::RealAudio,
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("only supports the m4a container"));
    }

    // ---------- SeedAlbumNullConcert ----------

    #[test]
    fn seed_album_null_concert_leaves_album_null_but_persists_track_state() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_album_null_concert(SeedAlbumNullConcert {
                set_list: Some(vec!["Song A".to_string()]),
                tracks_present: Some(vec![true]),
                tracks_liked: Some(vec![true]),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(concert.album, None);
        assert_eq!(concert.set_list, vec!["Song A".to_string()]);
        assert_eq!(concert.tracks_present, vec![true]);
        assert_eq!(concert.tracks_liked, vec![true]);
    }

    #[test]
    fn seed_album_null_concert_defaults_are_unique_and_inert() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_album_null_concert(SeedAlbumNullConcert::default())
            .unwrap();

        assert_eq!(concert.album, None);
        assert_eq!(concert.set_list.len(), 3);
        assert!(concert.tracks_present.is_empty());
        assert_eq!(
            concert.download_status(),
            crate::model::DownloadStatus::NotDownloaded
        );
    }

    #[test]
    fn json_omitted_field_uses_default_and_explicit_null_uses_none() {
        let listing: SeedListing = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(listing.concert_date.as_deref(), Some(DEFAULT_CONCERT_DATE));
        assert_eq!(listing.teaser.as_deref(), Some("Test listing teaser"));

        let listing: SeedListing =
            serde_json::from_value(serde_json::json!({"concert_date": null, "teaser": null}))
                .unwrap();
        assert_eq!(listing.concert_date, None);
        assert_eq!(listing.teaser, None);
    }

    #[test]
    fn json_rejects_unknown_fields() {
        let result: Result<SeedListing, _> =
            serde_json::from_value(serde_json::json!({"bogus_field": 1}));
        assert!(result.is_err());
    }

    #[test]
    fn fixture_ids_shared_across_contexts_stay_unique() {
        let conn = open_in_memory().unwrap();
        let ids = FixtureIds::default();
        let a = SeedContext::with_ids(&conn, ids.clone())
            .seed_listing(SeedListing::default())
            .unwrap();
        let b = SeedContext::with_ids(&conn, ids)
            .seed_listing(SeedListing::default())
            .unwrap();
        assert_ne!(a.source_url, b.source_url);
    }
}
