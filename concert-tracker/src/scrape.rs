use anyhow::Result;
use rusqlite::Connection;
use tiny_desk_scraper::{fetch_html, parse_concert_info, ConcertInfo};

use crate::db::{self, MetadataUpdate, NewListing};
use crate::model::Musician;

/// Fetch a concert URL, parse metadata, and upsert into the database.
pub fn scrape_url(conn: &Connection, url: &str) -> Result<()> {
    let html = fetch_html(url)?;
    let info = parse_concert_info(&html, url)?;
    apply_concert_info(conn, &info)
}

/// Upsert a parsed ConcertInfo into the database, converting Song structs to plain strings.
pub fn apply_concert_info(conn: &Connection, info: &ConcertInfo) -> Result<()> {
    db::upsert_listing(
        conn,
        &NewListing {
            source_url: info.source.clone(),
            title: info.album.clone(),
            concert_date: info.date.clone(),
            teaser: None,
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
