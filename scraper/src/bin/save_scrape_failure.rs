use anyhow::{Context, Result};
use reqwest::blocking::Client;
use std::env;
use std::fs;
use std::path::Path;
use tiny_desk_scraper::parse_concert_info;

fn main() -> Result<()> {
    // Get URL from command line arguments
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        eprintln!("Please provide a URL and a test name");
        eprintln!("Usage: cargo run --bin save_scrape_failure <URL> <test_name>");
        std::process::exit(1);
    }

    let url = &args[1];
    let test_name = &args[2];

    println!("Fetching HTML from {}...", url);

    // Create HTTP client
    let client = Client::new();

    // Fetch the page
    let response = client.get(url).send().context("Failed to send request")?;
    let html = response.text().context("Failed to get response text")?;

    // Create failures directory if it doesn't exist
    let failures_dir = Path::new("src/tests/fixtures/failures");
    fs::create_dir_all(failures_dir).context("Failed to create failures directory")?;

    // Save the HTML for testing
    let file_path = failures_dir.join(format!("{}.html", test_name));
    fs::write(&file_path, &html).context("Failed to write HTML file")?;

    println!(
        "Saved HTML to {} for regression testing",
        file_path.display()
    );

    // Attempt to parse with the scraper to validate the failure
    println!("\nAttempting to parse with the scraper to confirm failure:");

    // Using scraper directly from the main binary
    use scraper::Html;

    // Let's manually check for common failure patterns
    let document = Html::parse_document(&html);
    let title_selector = scraper::Selector::parse("title").unwrap();
    let has_title = document.select(&title_selector).next().is_some();

    let story_title_selector = scraper::Selector::parse(".storytitle h1").unwrap();
    let has_story_title = document.select(&story_title_selector).next().is_some();

    let date_selector = scraper::Selector::parse(".dateblock time").unwrap();
    let has_date = document.select(&date_selector).next().is_some();

    let setlist_selector = scraper::Selector::parse("p").unwrap();
    let has_setlist = document.select(&setlist_selector).any(|p| {
        p.text()
            .collect::<String>()
            .to_uppercase()
            .contains("SET LIST")
    });

    let musicians_selector = scraper::Selector::parse("p").unwrap();
    let has_musicians = document.select(&musicians_selector).any(|p| {
        p.text()
            .collect::<String>()
            .to_uppercase()
            .contains("MUSICIANS")
    });

    println!("HTML analysis results:");
    println!("  - Has title: {}", has_title);
    println!("  - Has story title: {}", has_story_title);
    println!("  - Has date: {}", has_date);
    println!("  - Has set list section: {}", has_setlist);
    println!("  - Has musicians section: {}", has_musicians);

    // Now try to parse it with the actual parser
    match parse_concert_info(&html, url) {
        Ok(_) => {
            println!("⚠️ Parse succeeded! This may not be a failure case.");
        }
        Err(e) => {
            println!("✅ Parse failed with error: {}", e);

            if !has_title || !has_story_title || !has_date || !has_setlist || !has_musicians {
                println!("   Missing required HTML elements - structural issue");
            } else {
                println!("   Has all required elements - likely a content parsing issue");
            }

            println!("\nThis test case has been saved and will be included in regression tests.");
        }
    }

    Ok(())
}
