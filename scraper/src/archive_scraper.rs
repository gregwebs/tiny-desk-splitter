use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConcertListing {
    pub title: String,
    pub url: String,
    pub date: String,
    pub teaser: String,
}

/// Returns the last day of a given month/year.
pub fn get_last_day_of_month(year: i32, month: u32) -> u32 {
    let first_day_of_next_month = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1).unwrap()
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1).unwrap()
    };
    first_day_of_next_month.pred_opt().unwrap().day()
}

/// Parse NPR archive page HTML to extract concert listings. Pure function, no network calls.
pub fn parse_archive_html(html: &str) -> Vec<ConcertListing> {
    let document = Html::parse_document(html);
    let article_selector = Selector::parse("article.item").unwrap();
    let title_selector = Selector::parse(".title a").unwrap();
    let time_selector = Selector::parse("time").unwrap();
    let teaser_selector = Selector::parse(".teaser").unwrap();

    let mut concerts = Vec::new();

    for article in document.select(&article_selector) {
        let title_element = article.select(&title_selector).next();
        let time_element = article.select(&time_selector).next();
        let teaser_element = article.select(&teaser_selector).next();

        if let (Some(title_el), Some(time_el), Some(teaser_el)) =
            (title_element, time_element, teaser_element)
        {
            let title = title_el.text().collect::<String>().trim().to_string();
            let url = title_el.value().attr("href").unwrap_or("").to_string();
            let date_attr = time_el.value().attr("datetime").unwrap_or("").to_string();

            let full_teaser_text = teaser_el.text().collect::<String>().trim().to_string();
            let date_text = time_el.text().collect::<String>().trim().to_string();

            let mut clean_teaser = full_teaser_text;
            if !date_text.is_empty() {
                clean_teaser = clean_teaser
                    .replace(&date_text, "")
                    .trim_start_matches(|c: char| c.is_whitespace() || c == '\u{2022}')
                    .to_string();
            }

            concerts.push(ConcertListing {
                title,
                url,
                date: date_attr,
                teaser: clean_teaser,
            });
        }
    }

    concerts
}

/// Fetch concert listings for a given year/month from the NPR archive.
/// If day is None, uses the last day of the month.
pub fn fetch_archive_month(year: i32, month: u32, day: Option<u32>) -> Result<Vec<ConcertListing>> {
    let day_value = day.unwrap_or_else(|| get_last_day_of_month(year, month));

    let url = format!(
        "https://www.npr.org/series/tiny-desk-concerts/archive?date={:02}-{:02}-{}",
        month, day_value, year
    );

    let client = crate::http_client();
    let response = client.get(&url).send().context("Failed to send request")?;
    let html = response.text().context("Failed to get response text")?;

    Ok(parse_archive_html(&html))
}
