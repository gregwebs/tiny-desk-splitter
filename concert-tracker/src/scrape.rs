use anyhow::{Context, Result};
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use tiny_desk_scraper::{fetch_bytes, fetch_html, parse_concert_info, ConcertInfo};

use crate::db::{self, MetadataUpdate, NewListing};
use crate::model::{concert_dir, sanitize_album, Musician};

/// Fetch a concert URL, parse metadata, upsert into the database, and save
/// the preview thumbnail into the concert's directory. Thumbnail download is
/// best-effort: failures are logged but do not fail the overall scrape.
pub fn scrape_url(conn: &Connection, url: &str, working_dir: &Path) -> Result<()> {
    let html = fetch_html(url)?;
    let info = parse_concert_info(&html, url)?;
    apply_concert_info(conn, &info)?;

    if let Some(image_url) = info.preview_image_url.as_deref() {
        let dest = preview_image_path(working_dir, &info.album);
        if dest.exists() {
            tracing::debug!("preview image already exists, skipping: {}", dest.display());
        } else if let Err(e) = save_preview_image(image_url, &dest) {
            tracing::warn!(
                "failed to save preview image for {} from {}: {}",
                info.album,
                image_url,
                e
            );
        } else {
            tracing::info!("saved preview image to {}", dest.display());
        }
    }

    Ok(())
}

/// Path where a concert's preview image lives on disk.
pub fn preview_image_path(working_dir: &Path, album: &str) -> PathBuf {
    concert_dir(working_dir, album).join(format!("{}.jpg", sanitize_album(album)))
}

fn save_preview_image(url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).context("create concert directory")?;
    }
    let bytes = fetch_bytes(url)?;
    fs::write(dest, bytes).with_context(|| format!("write preview to {}", dest.display()))?;
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
    fn preview_image_path_uses_sanitized_album() {
        let p = preview_image_path(Path::new("/wd"), "Some Album: Tiny Desk Concert");
        assert_eq!(
            p,
            PathBuf::from(
                "/wd/concerts/Some Album Tiny Desk Concert/Some Album Tiny Desk Concert.jpg"
            )
        );
    }

    #[test]
    fn preview_image_path_handles_plain_album() {
        let p = preview_image_path(Path::new("/wd"), "Plain");
        assert_eq!(p, PathBuf::from("/wd/concerts/Plain/Plain.jpg"));
    }
}
