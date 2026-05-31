use anyhow::{Context, Result};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use tiny_desk_scraper::{
    fetch_bytes, fetch_html, parse_concert_info, save_concert_info, ConcertInfo,
};

use crate::db::{self, MetadataUpdate, NewListing};
use crate::model::{concert_dir, sanitize_album, Musician};

/// Maximum width (px) of a generated listing thumbnail. The source preview is
/// resized down to this width preserving aspect ratio; smaller sources are left
/// as-is (no upscaling).
const THUMBNAIL_MAX_WIDTH: u32 = 480;
/// JPEG quality (0-100) used when encoding thumbnails.
const THUMBNAIL_JPEG_QUALITY: u8 = 80;

/// Outcome of [`ensure_thumbnail`], so callers (scrape + backfill CLI) can log
/// or tally without re-deriving the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbOutcome {
    /// A thumbnail was generated and written.
    Created,
    /// A thumbnail already existed; nothing was done.
    AlreadyPresent,
    /// No source image was available (preview missing on disk and no URL to
    /// fetch from), so no thumbnail could be made.
    SourceMissing,
}

/// Fetch a concert URL, parse metadata, upsert into the database, and ensure
/// the preview image and its listing thumbnail are saved. Image work is
/// best-effort: failures are logged but do not fail the overall scrape.
pub fn scrape_url(conn: &Connection, url: &str, working_dir: &Path) -> Result<()> {
    let info = fetch_concert_info(url)?;
    apply_concert_info(conn, &info)?;
    ensure_and_log_thumbnail(working_dir, &info.album, info.preview_image_url.as_deref());
    Ok(())
}

/// Fetch a concert page, parse its metadata, and persist the info JSON to disk.
/// Does **no** database or image work, so callers can run this network/disk step
/// outside any DB lock and apply the result separately (see the sync handler,
/// which scrapes many concerts without holding the connection mutex across the
/// network calls).
pub fn fetch_concert_info(url: &str) -> Result<ConcertInfo> {
    let html = fetch_html(url)?;
    let info = parse_concert_info(&html, url)?;
    save_concert_info(&info)?;
    Ok(info)
}

/// Ensure the listing thumbnail for `album` exists (deriving it from the preview
/// image, fetching it if `image_url` is given) and log the outcome. Best-effort:
/// any failure is logged, never returned — image work must not fail an overall
/// scrape. Shared by [`scrape_url`] and the sync path.
pub fn ensure_and_log_thumbnail(working_dir: &Path, album: &str, image_url: Option<&str>) {
    match ensure_thumbnail(working_dir, album, image_url) {
        Ok(ThumbOutcome::Created) => {
            tracing::info!("saved preview image + thumbnail for {}", album)
        }
        Ok(ThumbOutcome::AlreadyPresent) => {
            tracing::debug!("thumbnail already present for {}", album)
        }
        Ok(ThumbOutcome::SourceMissing) => {
            tracing::debug!("no preview image available for {}", album)
        }
        Err(e) => tracing::warn!("failed to save thumbnail for {}: {}", album, e),
    }
}

/// Path where a concert's full-size preview image lives on disk. This lives
/// inside the concert directory and is moved to the archive location when the
/// concert is archived.
pub fn preview_image_path(working_dir: &Path, album: &str) -> PathBuf {
    concert_dir(working_dir, album).join("preview.jpg")
}

/// Path where a concert's listing thumbnail lives on disk. Kept in a flat
/// `thumbnails/` directory *outside* the concert directory so it is never moved
/// to the archive — the listing keeps working even when the archive (e.g. a
/// NAS) is offline.
pub fn thumbnail_path(working_dir: &Path, album: &str) -> PathBuf {
    working_dir
        .join("thumbnails")
        .join(format!("{}.jpg", sanitize_album(album)))
}

/// Ensure a listing thumbnail exists for `album`, deriving it from the preview
/// image. The thumbnail check is independent of the preview check:
///
/// - thumbnail present                          → [`ThumbOutcome::AlreadyPresent`]
///   (checked first, so an offline/NAS-down run is a fast no-op).
/// - preview present on disk                    → re-encode it into a thumbnail.
/// - preview missing but `image_url` given      → fetch it, write `preview.jpg`,
///   then make the thumbnail.
/// - preview missing and no `image_url`         → [`ThumbOutcome::SourceMissing`].
pub fn ensure_thumbnail(
    working_dir: &Path,
    album: &str,
    image_url: Option<&str>,
) -> Result<ThumbOutcome> {
    let thumb_dest = thumbnail_path(working_dir, album);
    if thumb_dest.exists() {
        return Ok(ThumbOutcome::AlreadyPresent);
    }

    let preview_dest = preview_image_path(working_dir, album);
    let bytes = if preview_dest.exists() {
        fs::read(&preview_dest)
            .with_context(|| format!("read preview {}", preview_dest.display()))?
    } else if let Some(url) = image_url {
        let bytes = fetch_bytes(url)?;
        write_file(&preview_dest, &bytes)
            .with_context(|| format!("write preview to {}", preview_dest.display()))?;
        bytes
    } else {
        return Ok(ThumbOutcome::SourceMissing);
    };

    save_thumbnail(&bytes, &thumb_dest)?;
    Ok(ThumbOutcome::Created)
}

/// Decode `bytes`, resize down to [`THUMBNAIL_MAX_WIDTH`] preserving aspect
/// ratio (never upscaling), and write a JPEG to `dest`.
fn save_thumbnail(bytes: &[u8], dest: &Path) -> Result<()> {
    let img = image::load_from_memory(bytes).context("decode preview image")?;
    let resized = if img.width() > THUMBNAIL_MAX_WIDTH {
        img.resize(THUMBNAIL_MAX_WIDTH, u32::MAX, FilterType::Lanczos3)
    } else {
        img
    };
    let mut buf = Vec::new();
    JpegEncoder::new_with_quality(&mut buf, THUMBNAIL_JPEG_QUALITY)
        .encode_image(&resized)
        .context("encode thumbnail JPEG")?;
    write_file(dest, &buf).with_context(|| format!("write thumbnail to {}", dest.display()))?;
    Ok(())
}

/// Write `bytes` to `dest`, creating the parent directory if needed.
fn write_file(dest: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).context("create directory")?;
    }
    fs::write(dest, bytes)?;
    Ok(())
}

/// Upsert a parsed ConcertInfo into the database, converting Song structs to plain strings.
pub fn apply_concert_info(conn: &Connection, info: &ConcertInfo) -> Result<()> {
    db::upsert_listing(
        conn,
        &NewListing {
            source_url: info.source.clone(),
            title: info.album.clone(),
            concert_date: info.date.clone(),
            teaser: info.teaser.clone(),
        },
    )?;

    let concert = db::get_concert_by_url(conn, &info.source)?
        .ok_or_else(|| anyhow::anyhow!("Concert not found after upsert"))?;

    let set_list: Vec<String> = info.set_list.iter().map(|s| s.title.clone()).collect();
    let musicians: Vec<Musician> = info
        .musicians
        .iter()
        .map(|m| Musician {
            name: m.name.clone(),
            instruments: m.instruments.clone(),
        })
        .collect();

    db::update_metadata(
        conn,
        concert.id,
        &MetadataUpdate {
            artist: info.artist.clone(),
            album: info.album.clone(),
            description: info.description.clone(),
            set_list,
            musicians,
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_image_path_uses_fixed_name() {
        let p = preview_image_path(Path::new("/wd"), "Some Album: Tiny Desk Concert");
        assert_eq!(
            p,
            PathBuf::from("/wd/concerts/Some Album Tiny Desk Concert/preview.jpg")
        );
    }

    #[test]
    fn preview_image_path_handles_plain_album() {
        let p = preview_image_path(Path::new("/wd"), "Plain");
        assert_eq!(p, PathBuf::from("/wd/concerts/Plain/preview.jpg"));
    }

    #[test]
    fn thumbnail_path_uses_flat_sanitized_name() {
        let p = thumbnail_path(Path::new("/wd"), "Some Album: Tiny Desk Concert");
        assert_eq!(
            p,
            PathBuf::from("/wd/thumbnails/Some Album Tiny Desk Concert.jpg")
        );
    }

    #[test]
    fn thumbnail_path_handles_plain_album() {
        let p = thumbnail_path(Path::new("/wd"), "Plain");
        assert_eq!(p, PathBuf::from("/wd/thumbnails/Plain.jpg"));
    }

    /// Encode a solid-color RGB image of the given size to in-memory JPEG bytes.
    fn make_jpeg(width: u32, height: u32) -> Vec<u8> {
        let img = image::DynamicImage::new_rgb8(width, height);
        let mut buf = Vec::new();
        JpegEncoder::new_with_quality(&mut buf, 90)
            .encode_image(&img)
            .unwrap();
        buf
    }

    #[test]
    fn save_thumbnail_resizes_down_preserving_aspect() {
        let bytes = make_jpeg(1600, 900);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("thumb.jpg");
        save_thumbnail(&bytes, &dest).unwrap();
        let out = image::load_from_memory(&fs::read(&dest).unwrap()).unwrap();
        assert_eq!(out.width(), THUMBNAIL_MAX_WIDTH);
        assert_eq!(out.height(), 270); // 900 * 480 / 1600, aspect preserved
    }

    #[test]
    fn save_thumbnail_does_not_upscale_small_source() {
        let bytes = make_jpeg(320, 180);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("thumb.jpg");
        save_thumbnail(&bytes, &dest).unwrap();
        let out = image::load_from_memory(&fs::read(&dest).unwrap()).unwrap();
        assert_eq!(out.width(), 320);
        assert_eq!(out.height(), 180);
    }

    #[test]
    fn ensure_thumbnail_creates_from_existing_preview() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Some Album: Tiny Desk Concert";
        let preview = preview_image_path(dir.path(), album);
        write_file(&preview, &make_jpeg(1600, 900)).unwrap();

        let outcome = ensure_thumbnail(dir.path(), album, None).unwrap();
        assert_eq!(outcome, ThumbOutcome::Created);
        assert!(thumbnail_path(dir.path(), album).exists());
    }

    #[test]
    fn ensure_thumbnail_noop_when_thumbnail_present() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Plain";
        // Thumbnail already exists; preview is deliberately absent (NAS-down case).
        write_file(&thumbnail_path(dir.path(), album), &make_jpeg(480, 270)).unwrap();
        assert!(!preview_image_path(dir.path(), album).exists());

        let outcome = ensure_thumbnail(dir.path(), album, None).unwrap();
        assert_eq!(outcome, ThumbOutcome::AlreadyPresent);
    }

    #[test]
    fn ensure_thumbnail_source_missing_without_preview_or_url() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = ensure_thumbnail(dir.path(), "Plain", None).unwrap();
        assert_eq!(outcome, ThumbOutcome::SourceMissing);
        assert!(!thumbnail_path(dir.path(), "Plain").exists());
    }

    #[test]
    fn ensure_and_log_thumbnail_source_missing_is_noop() {
        // No preview on disk and no URL: must return cleanly (best-effort) and
        // write nothing — no panic, no thumbnail.
        let dir = tempfile::tempdir().unwrap();
        ensure_and_log_thumbnail(dir.path(), "Plain", None);
        assert!(!thumbnail_path(dir.path(), "Plain").exists());
    }
}
