use crate::{get_last_day_of_month, parse_archive_html};

#[test]
fn parse_archive_html_extracts_listings() {
    let html = r#"
        <html><body>
        <article class="item">
          <div class="title"><a href="https://www.npr.org/concerts/123">Test Concert</a></div>
          <div class="teaser">
            <time datetime="2024-01-15">January 15, 2024</time>
            January 15, 2024 &#x2022; Some teaser text here
          </div>
        </article>
        </body></html>
    "#;
    let listings = parse_archive_html(html);
    assert_eq!(listings.len(), 1);
    assert_eq!(listings[0].title, "Test Concert");
    assert_eq!(listings[0].url, "https://www.npr.org/concerts/123");
    assert_eq!(listings[0].date, "2024-01-15");
    assert!(listings[0].teaser.contains("Some teaser text"));
}

#[test]
fn parse_archive_html_empty_when_no_articles() {
    let html = "<html><body><p>No concerts</p></body></html>";
    let listings = parse_archive_html(html);
    assert!(listings.is_empty());
}

#[test]
fn last_day_of_month_handles_february_and_leap_years() {
    assert_eq!(get_last_day_of_month(2024, 2), 29); // leap year
    assert_eq!(get_last_day_of_month(2023, 2), 28); // non-leap year
    assert_eq!(get_last_day_of_month(2024, 1), 31);
    assert_eq!(get_last_day_of_month(2024, 4), 30);
    assert_eq!(get_last_day_of_month(2024, 12), 31);
}
