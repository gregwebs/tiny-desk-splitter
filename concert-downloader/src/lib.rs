// Export the scraper module
pub mod scraper;

// Re-export tests for integration testing
#[cfg(test)]
pub mod tests;

// Re-export key types and functions for easier access
pub use crate::scraper::{
    extract_content, extract_musicians, extract_set_list, fetch_html, parse_concert_info,
    save_concert_info, scrape_data, ConcertInfo, Musician, Song,
};
