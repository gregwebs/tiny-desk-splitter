use anyhow::{Context, Result};
use reqwest::blocking::Client;
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use std::fs;

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
    pub album: Option<String>,
    pub description: Option<String>,
    pub set_list: Vec<Song>,
    pub musicians: Vec<Musician>,
}

pub fn fetch_html(url: &str) -> Result<String> {
    let client = Client::new();
    let response = client.get(url).send().context("Failed to send request")?;
    response.text().context("Failed to get response text")
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

    let artist_name = title.split(':').next().unwrap_or("").trim().to_string();

    if artist_name.is_empty() {
        return Err(anyhow::anyhow!("Title is empty"));
    }

    // Extract story title
    let story_title_selector = Selector::parse(".storytitle h1").unwrap();
    let story_title = document
        .select(&story_title_selector)
        .next()
        .map(|element| element.text().collect::<String>().trim().to_string());

    if story_title.is_none() {
        return Err(anyhow::anyhow!("No story title found"));
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
    };

    Ok(concert_info)
}

pub fn extract_content(document: &Html) -> Result<(Option<String>, Vec<Song>, Vec<Musician>)> {
    let storytext_selector = Selector::parse("#storytext").unwrap();
    let p_selector = Selector::parse("p").unwrap();

    let mut description = None;
    let mut set_list = Vec::new();
    let mut musicians = Vec::new();

    if let Some(storytext) = document.select(&storytext_selector).next() {
        let paragraphs: Vec<_> = storytext.select(&p_selector).collect();

        // Get description from first paragraphs until SET LIST or MUSICIANS
        let mut desc_text = String::new();
        let mut description_done = false;

        for p in &paragraphs {
            let text: String = p.text().collect::<String>();
            let upper_text = text.trim().to_uppercase();

            if upper_text == "SET LIST" || upper_text == "MUSICIANS" {
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
        set_list = extract_set_list(paragraphs.as_slice())?;

        // Extract musicians
        musicians = extract_musicians(paragraphs.as_slice())?;
    }

    Ok((description, set_list, musicians))
}

pub fn extract_set_list(paragraphs: &[ElementRef]) -> Result<Vec<Song>> {
    let li_selector = Selector::parse("li").unwrap();
    let mut set_list = Vec::new();

    for p in paragraphs {
        let text: String = p.text().collect::<String>().to_uppercase();

        if text.contains("SET LIST") {
            println!("found set list paragraph");
            // Find the next sibling that is a UL element
            let mut next_element = p.next_sibling();
            while let Some(element) = next_element {
                if let Some(el) = element.value().as_element() {
                    if el.name() == "ul" {
                        let ul_element = ElementRef::wrap(element).unwrap();
                        for li in ul_element.select(&li_selector) {
                            let song_text = li
                                .text()
                                .collect::<String>()
                                .trim()
                                .trim_start_matches(|c| c == '"' || c == '\'')
                                .trim_end_matches(|c| c == '"' || c == '\'')
                                .to_string();

                            set_list.push(Song { title: song_text });
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

    let musicians_para_find = paragraphs
        .iter()
        .find(|p| return p.text().collect::<String>().trim().to_uppercase() == "MUSICIANS");
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
                for musician_instrument in musician_text.split(';') {
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
                        return Err(anyhow::anyhow!(
                            "Did not understand musician instrument list: {}",
                            parts.join(", ")
                        ));
                    }
                }
                return Ok(musicians);
            }
        }
        next_element = element.next_sibling();
    }

    Ok(musicians)
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

    let output_file_name = format!("{}_info.json", sanitized_artist_name);

    // Write to file as JSON
    let json =
        serde_json::to_string_pretty(&concert_info).context("Failed to serialize concert info")?;

    fs::write(&output_file_name, json).context("Failed to write JSON file")?;

    Ok(output_file_name)
}

pub fn scrape_data(url: &str) -> Result<()> {
    println!("Navigating to {}...", url);

    let html = fetch_html(url)?;
    let concert_info = parse_concert_info(&html, url)?;

    println!("Artist: {}", concert_info.artist);

    if let Some(title) = &concert_info.album {
        println!("Story Title: {}", title);
    }

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
