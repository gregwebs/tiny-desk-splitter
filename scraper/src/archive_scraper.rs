// Export the archive_scraper functionality
pub fn scrape_archive(year: &str, month: &str, day: Option<&str>) -> anyhow::Result<()> {
    crate::bin::archive_scraper::scrape_archive(year, month, day)
}
