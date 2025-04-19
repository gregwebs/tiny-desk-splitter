use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate};
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Serialize, Deserialize)]
struct ConcertListing {
    title: String,
    url: String,
    date: String,
    teaser: String,
}

/// Get the last day of a month
fn get_last_day_of_month(year: i32, month: u32) -> u32 {
    // The last day of the month is the day before the first day of the next month
    let first_day_of_next_month = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1).unwrap()
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1).unwrap()
    };

    let last_day = first_day_of_next_month.pred_opt().unwrap();
    last_day.day()
}

pub fn scrape_archive(year: &str, month: &str, day: Option<&str>) -> Result<()> {
    // Parse year and month
    let year_num = year.parse::<i32>().context("Invalid year format")?;
    let month_num = month.parse::<u32>().context("Invalid month format")?;

    // If day is not provided, use the last day of the month
    let day_value = match day {
        Some(d) => d.to_string(),
        None => {
            let last_day = get_last_day_of_month(year_num, month_num);
            let day_str = format!("{:02}", last_day);
            println!("No day specified, using last day of month: {}", day_str);
            day_str
        }
    };

    // Construct the URL with date
    let url = format!(
        "https://www.npr.org/series/tiny-desk-concerts/archive?date={}-{}-{}",
        month, day_value, year
    );

    println!("Navigating to {}...", url);

    // Create HTTP client
    let client = Client::new();

    // Fetch the page
    let response = client.get(&url).send().context("Failed to send request")?;

    let html = response.text().context("Failed to get response text")?;
    let document = Html::parse_document(&html);

    // Extract concert listings
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

            // Get the datetime attribute
            let date_attr = time_el.value().attr("datetime").unwrap_or("").to_string();

            // Extract teaser text, removing the date portion
            let full_teaser_text = teaser_el.text().collect::<String>().trim().to_string();
            let date_text = time_el.text().collect::<String>().trim().to_string();

            // Remove the date text from the teaser
            let mut clean_teaser = full_teaser_text.clone();
            if !date_text.is_empty() {
                // Replace the date text and any following dots or bullets
                clean_teaser = clean_teaser
                    .replace(&date_text, "")
                    .trim_start_matches(|c: char| c.is_whitespace() || c == '•')
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

    // Format date string for display
    let display_date = match day {
        Some(d) => format!("{}/{}/{}", month, d, year),
        None => format!("{}/{}/{} (last day of month)", month, day_value, year),
    };

    println!(
        "Found {} Tiny Desk Concerts for {}",
        concerts.len(),
        display_date
    );

    // Create output filename
    let output_file_name = match day {
        Some(d) => format!("listing_{}_{}_{}.json", year, month, d),
        None => format!("listing_{}_{}.json", year, month),
    };

    // Format for console output and JSON
    if !concerts.is_empty() {
        println!("\nConcert Listings:");
        for (index, concert) in concerts.iter().enumerate() {
            println!("{}. {} ({})", index + 1, concert.title, concert.date);

            // Display the teaser, but truncate long teasers with ellipsis
            if !concert.teaser.is_empty() {
                let truncated_teaser = if concert.teaser.len() > 100 {
                    format!("{}...", &concert.teaser[..100])
                } else {
                    concert.teaser.clone()
                };
                println!("   {}", truncated_teaser);
            }

            println!("   URL: {}", concert.url);
            println!("{}", "-".repeat(50));
        }

        // Write to file as JSON
        let json = serde_json::to_string_pretty(&concerts)
            .context("Failed to serialize concert listings")?;

        fs::write(&output_file_name, json).context("Failed to write JSON file")?;

        println!("\nListings saved to {}", output_file_name);
    } else {
        println!("No Tiny Desk Concerts found for this period");
    }

    Ok(())
}

fn main() -> Result<()> {
    // Get year, month, and optional day from command line arguments
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 {
        eprintln!("Please provide year and month as arguments");
        eprintln!("Usage: cargo run --bin archive_scraper <YEAR> <MONTH> [DAY]");
        eprintln!("Example: cargo run --bin archive_scraper 2023 01");
        eprintln!("Example with day: cargo run --bin archive_scraper 2023 01 15");
        std::process::exit(1);
    }

    let year = &args[1];
    let month = &args[2];
    let day = if args.len() > 3 { Some(&args[3]) } else { None };

    // Validate year, month, and day format
    if !year.chars().all(char::is_numeric) || year.len() != 4 {
        eprintln!("Year must be in YYYY format (e.g., 2023)");
        std::process::exit(1);
    }

    if !month.chars().all(char::is_numeric)
        || month.parse::<u32>().unwrap_or(0) < 1
        || month.parse::<u32>().unwrap_or(0) > 12
    {
        eprintln!("Month must be between 1 and 12");
        std::process::exit(1);
    }

    // Ensure month has leading zero if needed for URL formatting
    let month = if month.len() == 1 {
        format!("0{}", month)
    } else {
        month.to_string()
    };

    let day = if let Some(d) = day {
        if !d.chars().all(char::is_numeric)
            || d.parse::<u32>().unwrap_or(0) < 1
            || d.parse::<u32>().unwrap_or(0) > 31
        {
            eprintln!("Day must be between 1 and 31");
            std::process::exit(1);
        }

        // Ensure day has leading zero if needed for URL formatting
        if d.len() == 1 {
            Some(format!("0{}", d))
        } else {
            Some(d.to_string())
        }
    } else {
        None
    };

    // Validate that day is valid for the given month and year
    if let Some(ref d) = day {
        let year_num = year.parse::<i32>().unwrap();
        let month_num = month.parse::<u32>().unwrap();
        let day_num = d
            .parse::<u32>()
            .unwrap_or_else(|_| d.trim_start_matches('0').parse::<u32>().unwrap());
        let last_day_of_month = get_last_day_of_month(year_num, month_num);

        if day_num < 1 || day_num > last_day_of_month {
            eprintln!(
                "Invalid day: {}. For {}/{}, days must be between 1 and {}",
                d, month, year, last_day_of_month
            );
            std::process::exit(1);
        }
    }

    scrape_archive(year, &month, day.as_deref())?;

    Ok(())
}
