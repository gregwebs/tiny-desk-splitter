use anyhow::{Context, Result};
use regex::Regex;
use reqwest::blocking::Client;
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use std::fs;
use std::sync::OnceLock;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Musician {
    pub name: String,
    pub instruments: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Song {
    pub title: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ConcertInfo {
    pub artist: String,
    pub source: String,
    pub show: String,
    pub date: Option<String>,
    pub album: String,
    pub description: Option<String>,
    pub set_list: Vec<Song>,
    pub musicians: Vec<Musician>,
    #[serde(default)]
    pub preview_image_url: Option<String>,
    #[serde(default)]
    pub teaser: Option<String>,
}

pub fn fetch_html(url: &str) -> Result<String> {
    let client = Client::new();
    let response = client.get(url).send().context("Failed to send request")?;
    response.text().context("Failed to get response text")
}

/// Fetch a URL as raw bytes (e.g. for images).
pub fn fetch_bytes(url: &str) -> Result<Vec<u8>> {
    let client = Client::new();
    let response = client.get(url).send().context("Failed to send request")?;
    let response = response.error_for_status().context("HTTP error status")?;
    let bytes = response.bytes().context("Failed to read response bytes")?;
    Ok(bytes.to_vec())
}

fn first_split(s: &str, char: char) -> String {
    return s.split(char).next().unwrap_or("").trim().to_string();
}

/// Lowercase a string and strip all whitespace. Used to compare artist names
/// that differ only in spacing — e.g. NPR's `<title>` says "Kes the Band"
/// while the on-page `<h1>` says "KestheBand".
fn normalize_for_match(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

pub fn parse_concert_info(html: &str, source_url: &str) -> Result<ConcertInfo> {
    let document = Html::parse_document(html);

    // Extract the artist name from the title
    let title_selector = Selector::parse("title").unwrap();
    let title: String = document
        .select(&title_selector)
        .next()
        .map(|element| element.text().collect())
        .unwrap_or_default();

    let mut artist_name = first_split(&title, ':');

    if artist_name.is_empty() {
        return Err(anyhow::anyhow!("Title is empty"));
    }

    // Extract story title
    let story_title_selector = Selector::parse(".storytitle h1").unwrap();
    let story_title_may = document
        .select(&story_title_selector)
        .next()
        .map(|element| element.text().collect::<String>().trim().to_string());

    let story_title = match story_title_may {
        None => return Err(anyhow::anyhow!("No story title found")),
        Some(st) => st,
    };

    if !normalize_for_match(&story_title).contains(&normalize_for_match(&artist_name)) {
        if artist_name.to_lowercase() == "video"
            || artist_name.ends_with("The Tiny Desk")
            || story_title.to_lowercase().contains("tiny desk concert")
        {
            artist_name = story_title
                .split(":")
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        } else {
            return Err(anyhow::anyhow!(
                "mismatch between artist '{}' and story title '{}'",
                artist_name,
                story_title
            ));
        }
    }

    // Extract date
    let date_selector = Selector::parse(".dateblock time").unwrap();
    let date = document
        .select(&date_selector)
        .next()
        .and_then(|element| element.value().attr("datetime"))
        .map(|date_str| date_str.to_string());

    if date.is_none() {
        return Err(anyhow::anyhow!("No date found"));
    }

    // Extract description, set list, and musicians
    let (description, set_list, musicians) = extract_content(&document)?;

    if set_list.is_empty() {
        return Err(anyhow::anyhow!("No set list found"));
    }

    if musicians.is_empty() {
        return Err(anyhow::anyhow!("No musicians list found"));
    }

    let preview_image_url = extract_preview_image_url(&document);
    let teaser = extract_og_description(&document);

    // Create JSON structure
    let concert_info = ConcertInfo {
        artist: artist_name,
        source: source_url.to_string(),
        show: "Tiny Desk Concerts".to_string(),
        date,
        album: story_title,
        description,
        set_list,
        musicians,
        preview_image_url,
        teaser,
    };

    Ok(concert_info)
}

/// Extract the video preview thumbnail URL from a NPR Tiny Desk page.
///
/// Prefers the JWPlayer `div.jw-preview` `background-image` style when it's in
/// the served HTML, then falls back to the `og:image` meta tag (which NPR
/// always ships in the static markup and points at the same thumbnail). The
/// `jw-preview` div is normally JS-rendered, so the fallback is the common
/// path in practice — but we keep both since either one is acceptable.
pub fn extract_preview_image_url(document: &Html) -> Option<String> {
    extract_jw_preview_url(document).or_else(|| extract_og_image_url(document))
}

fn extract_jw_preview_url(document: &Html) -> Option<String> {
    static URL_RE: OnceLock<Regex> = OnceLock::new();
    let re = URL_RE.get_or_init(|| {
        Regex::new(r#"background-image\s*:\s*url\(\s*['"]?([^'")]+)['"]?\s*\)"#).unwrap()
    });

    let selector = Selector::parse("div.jw-preview").ok()?;
    let element = document.select(&selector).next()?;
    let style = element.value().attr("style")?;
    let caps = re.captures(style)?;
    Some(caps.get(1)?.as_str().to_string())
}

fn extract_og_image_url(document: &Html) -> Option<String> {
    let selector = Selector::parse(r#"meta[property="og:image"]"#).ok()?;
    let element = document.select(&selector).next()?;
    let content = element.value().attr("content")?;
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

pub fn extract_og_description(document: &Html) -> Option<String> {
    let selector = Selector::parse(r#"meta[property="og:description"]"#).ok()?;
    let element = document.select(&selector).next()?;
    let content = element.value().attr("content")?;
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

pub fn extract_teaser_from_html(html: &str) -> Option<String> {
    let document = Html::parse_document(html);
    extract_og_description(&document)
}

pub fn extract_content(document: &Html) -> Result<(Option<String>, Vec<Song>, Vec<Musician>)> {
    let storytext_selector = Selector::parse("#storytext").unwrap();
    let p_selector = Selector::parse("p").unwrap();
    let h3_selector = Selector::parse("h3").unwrap();

    let mut description = None;
    let mut set_list = Vec::new();
    let mut musicians = Vec::new();

    if let Some(storytext) = document.select(&storytext_selector).next() {
        let mut headings: Vec<_> = storytext.select(&p_selector).collect();
        let mut h3s: Vec<_> = storytext.select(&h3_selector).collect();
        headings.append(&mut h3s);

        // Get description from first paragraphs until SET LIST or MUSICIANS
        let mut desc_text = String::new();
        let mut description_done = false;

        for p in &headings {
            let text: String = p.text().collect::<String>();
            let upper_text = text.trim().to_uppercase();

            if upper_text == "SET LIST" || upper_text == "MUSICIANS" || upper_text == "MUSICIAN" {
                description_done = true;
                continue;
            }

            if !description_done {
                if !desc_text.is_empty() {
                    desc_text.push_str("\n\n");
                }
                desc_text.push_str(&text);
            }
        }

        if !desc_text.is_empty() {
            description = Some(desc_text);
        }

        // Extract set list
        set_list = extract_set_list(headings.as_slice())?;

        // Extract musicians
        musicians = extract_musicians(headings.as_slice())?;
    }

    Ok((description, set_list, musicians))
}

pub fn extract_set_list(paragraphs: &[ElementRef]) -> Result<Vec<Song>> {
    let li_selector = Selector::parse("li").unwrap();
    let mut set_list = Vec::new();

    for p in paragraphs {
        let text: String = p.text().collect::<String>().to_uppercase();

        if text.contains("SET LIST") {
            // println!("found set list paragraph");
            // Find the next sibling that is a UL element
            let mut next_element = p.next_sibling();
            while let Some(element) = next_element {
                if let Some(el) = element.value().as_element() {
                    if el.name() == "ul" {
                        let ul_element = ElementRef::wrap(element).unwrap();
                        for li in ul_element.select(&li_selector) {
                            let mut song_text = li.text().collect::<String>().trim().to_string();

                            if let Some(start) = song_text.chars().nth(0) {
                                if start == '"' || start == '\'' {
                                    song_text = song_text[1..]
                                        .trim_end_matches(|c| c == '"' || c == '\'')
                                        .to_string();
                                }
                                set_list.push(Song { title: song_text });
                            }
                        }
                        break;
                    }
                }
                next_element = element.next_sibling();
            }
            break; // Only exit loop once we've found and processed the SET LIST section
        }
    }

    Ok(set_list)
}

pub fn extract_musicians(paragraphs: &[ElementRef]) -> Result<Vec<Musician>> {
    let li_selector = Selector::parse("li").unwrap();
    let mut musicians = Vec::new();

    let musicians_para_find = paragraphs.iter().find(|p| -> bool {
        let mtext = p.text().collect::<String>().trim().to_uppercase();
        return mtext == "MUSICIANS" || mtext == "MUSICIAN";
    });
    let p = match musicians_para_find {
        None => return Err(anyhow::anyhow!("musicians text not found on page")),
        Some(p) => p,
    };
    println!("found musicians paragraph");

    // Find the next sibling that is a UL element
    let mut next_element = p.next_sibling();
    while let Some(element) = next_element {
        if let Some(el) = element.value().as_element() {
            // println!("sibling {}: {}", el.name(), ElementRef::wrap(element).unwrap().text().collect::<String>());
            if el.name() == "ul" {
                println!("found musicians ul list");
                let ul_element = ElementRef::wrap(element).unwrap();
                for li in ul_element.select(&li_selector) {
                    let musician_text = li
                        .text()
                        .collect::<String>()
                        .trim()
                        .trim_start_matches(|c| c == '"' || c == '\'')
                        .trim_end_matches(|c| c == '"' || c == '\'')
                        .to_string();

                    // Parse musician name and instruments
                    let parts: Vec<&str> = musician_text.split(':').collect();
                    if parts.len() == 2 {
                        let name = parts[0].trim().to_string();
                        let instruments =
                            parts[1].split(',').map(|s| s.trim().to_string()).collect();

                        musicians.push(Musician { name, instruments });
                    } else {
                        musicians.push(Musician {
                            name: musician_text,
                            instruments: Vec::new(),
                        });
                    }
                }
                return Ok(musicians);
            } else if el.name() == "p" {
                println!("found musicians p list");
                let musician_text = ElementRef::wrap(element)
                    .unwrap()
                    .text()
                    .collect::<String>();
                for musician_instrument_orig in musician_text.trim().split(';') {
                    let musician_instrument = musician_instrument_orig.trim();
                    if musician_instrument.trim() == "" {
                        continue;
                    }
                    println!("musician_instrument {}", musician_instrument);
                    let parts: Vec<&str> =
                        strip_suffix(musician_instrument, ")").split("(").collect();
                    if parts.len() == 2 {
                        let instruments =
                            parts[1].split(',').map(|s| s.trim().to_string()).collect();
                        musicians.push(Musician {
                            name: parts[0].to_string(),
                            instruments,
                        });
                    } else {
                        let parts: Vec<&str> = musician_instrument.split(": ").collect();
                        if parts.len() == 2 {
                            let instruments =
                                parts[1].split(',').map(|s| s.trim().to_string()).collect();
                            musicians.push(Musician {
                                name: parts[0].to_string(),
                                instruments,
                            });
                        } else {
                            return Err(anyhow::anyhow!(
                                "Did not understand musician instrument list: {} from {}",
                                parts.join(", "),
                                musician_instrument,
                            ));
                        }
                    }
                }
                return Ok(musicians);
            }
        }
        next_element = element.next_sibling();
    }

    Ok(musicians)
}

/// Sanitize an album name the same way concert-tracker does (strip colons
/// only). Kept inline to avoid pulling concert-tracker as a build dep.
fn sanitize_album_for_dir(album: &str) -> String {
    album.replace(':', "")
}

pub fn save_concert_info(concert_info: &ConcertInfo) -> Result<String> {
    // Create output filename based on artist name
    let sanitized_artist_name = concert_info
        .artist
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .replace(" ", "_")
        .to_lowercase();

    if sanitized_artist_name.is_empty() {
        return Err(anyhow::anyhow!("Artist name is empty"));
    }

    // Place metadata inside `concerts/<sanitized-album>/<artist-slug>.json`
    // so the JSON lives alongside the mp4, preview, and split tracks.
    let concert_dir =
        std::path::Path::new("concerts").join(sanitize_album_for_dir(&concert_info.album));
    fs::create_dir_all(&concert_dir).with_context(|| {
        format!(
            "Failed to create concert directory {}",
            concert_dir.display()
        )
    })?;
    let output_file = concert_dir.join(format!("{}.json", sanitized_artist_name));

    let json =
        serde_json::to_string_pretty(&concert_info).context("Failed to serialize concert info")?;

    fs::write(&output_file, json)
        .with_context(|| format!("Failed to write JSON file {}", output_file.display()))?;

    Ok(output_file.to_string_lossy().into_owned())
}

pub fn scrape_data(url: &str) -> Result<()> {
    println!("Navigating to {}...", url);

    let html = fetch_html(url)?;
    let concert_info = parse_concert_info(&html, url)?;

    println!("Artist: {}", concert_info.artist);
    println!("Story Title: {}", &concert_info.album);

    if let Some(date_str) = &concert_info.date {
        println!("Date: {}", date_str);
    }

    if !concert_info.set_list.is_empty() {
        println!("\nSet list:");
        for (i, song) in concert_info.set_list.iter().enumerate() {
            println!("{}. {}", i, song.title);
        }
    }

    if !concert_info.musicians.is_empty() {
        println!("\nMusicians:");
        for (idx, musician) in concert_info.musicians.iter().enumerate() {
            println!("{}. {}", idx + 1, musician.name);
            if !musician.instruments.is_empty() {
                println!("   Instruments: {}", musician.instruments.join(", "));
            }
        }
    }

    let output_file_name = save_concert_info(&concert_info)?;
    println!("\nInformation saved to {}", output_file_name);

    Ok(())
}

fn strip_suffix<'a>(s: &'a str, suffix: &str) -> &'a str {
    if let Some(stripped) = s.strip_suffix(&suffix) {
        return stripped;
    } else {
        return s;
    }
}
