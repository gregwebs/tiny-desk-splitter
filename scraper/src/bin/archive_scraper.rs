use anyhow::Result;
use std::fs;
use tiny_desk_scraper::{fetch_archive_month, get_last_day_of_month};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 {
        eprintln!("Usage: archive_scraper <YEAR> <MONTH> [DAY]");
        eprintln!("Example: archive_scraper 2024 01");
        std::process::exit(1);
    }

    let year: i32 = args[1].parse().expect("Year must be a number");
    let month: u32 = args[2].parse().expect("Month must be 1-12");
    let day: Option<u32> = args.get(3).map(|d| d.parse().expect("Day must be a number"));

    let day_value = day.unwrap_or_else(|| get_last_day_of_month(year, month));
    println!(
        "Fetching Tiny Desk archive for {}/{:02}/{:02}...",
        year, month, day_value
    );

    let concerts = fetch_archive_month(year, month, day)?;
    println!("Found {} Tiny Desk Concerts", concerts.len());

    for (i, c) in concerts.iter().enumerate() {
        println!("{}. {} ({})", i + 1, c.title, c.date);
        if !c.teaser.is_empty() {
            let truncated = if c.teaser.len() > 100 {
                &c.teaser[..100]
            } else {
                &c.teaser
            };
            println!("   {}", truncated);
        }
        println!("   URL: {}", c.url);
        println!("{}", "-".repeat(50));
    }

    let output_file = match day {
        Some(d) => format!("listing_{}_{}_{:02}.json", year, month, d),
        None => format!("listing_{}_{:02}.json", year, month),
    };

    if !concerts.is_empty() {
        let json = serde_json::to_string_pretty(&concerts)?;
        fs::write(&output_file, json)?;
        println!("\nListings saved to {}", output_file);
    }

    Ok(())
}
