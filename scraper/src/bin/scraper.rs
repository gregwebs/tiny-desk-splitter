use anyhow::Result;
use tiny_desk_scraper::scrape_data;

fn main() -> Result<()> {
    // Get URL from command line arguments
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Please provide a URL as an argument");
        eprintln!("Usage: cargo run --bin scraper <URL>");
        std::process::exit(1);
    }

    let url = &args[1];
    scrape_data(url)?;

    Ok(())
}
