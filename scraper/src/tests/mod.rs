use anyhow::Result;
use std::fs;
use std::path::Path;

pub mod fixtures;
pub mod scraper_tests;

/// Helper function to log and save failed HTML for future regression testing
pub fn save_failed_html(html: &str, test_name: &str) -> Result<()> {
    // Create tests/fixtures/failures directory if it doesn't exist
    let failures_dir = Path::new("src/tests/fixtures/failures");
    fs::create_dir_all(failures_dir)?;

    // Save the HTML for further analysis
    let file_path = failures_dir.join(format!("{}.html", test_name));
    fs::write(&file_path, html)?;

    println!("Saved failed HTML to {}", file_path.display());
    Ok(())
}
