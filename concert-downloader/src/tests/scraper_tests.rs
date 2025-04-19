use super::fixtures;
use super::save_failed_html;
use crate::scraper::parse_concert_info;
use anyhow::Result;

// Test successful parsing of a sample concert
#[test]
fn test_sample_concert_parsing() {
    // Load the sample HTML directly
    let html = fixtures::load_html_fixture("sample_concert");
    let result = parse_concert_info(&html, "https://example.com/test");

    // For debugging purposes, save the HTML if parsing fails
    if let Err(e) = &result {
        println!("Error: {}", e);
        save_failed_html(&html, "sample_concert_test").unwrap();
    }

    // Assert that parsing succeeds
    assert!(
        result.is_ok(),
        "Failed to parse sample concert: {:?}",
        result.err()
    );

    // Get the parsed concert info
    let concert_info = result.unwrap();

    // Verify the extracted information
    assert_eq!(concert_info.artist, "Test Artist");
    assert_eq!(
        concert_info.album,
        Some("Test Artist: Tiny Desk Concert".to_string())
    );
    assert_eq!(concert_info.date, Some("2023-01-01".to_string()));
    assert_eq!(concert_info.show, "Tiny Desk Concerts");

    // Verify set list
    assert_eq!(concert_info.set_list.len(), 3);
    assert_eq!(concert_info.set_list[0].title, "Test Song 1");
    assert_eq!(concert_info.set_list[1].title, "Test Song 2");
    assert_eq!(concert_info.set_list[2].title, "Test Song 3");

    // Verify musicians
    assert_eq!(concert_info.musicians.len(), 3);
    assert_eq!(concert_info.musicians[0].name, "Test Artist");
    assert_eq!(
        concert_info.musicians[0].instruments,
        vec!["vocals", "guitar"]
    );
    assert_eq!(concert_info.musicians[1].name, "Test Bassist");
    assert_eq!(concert_info.musicians[1].instruments, vec!["bass"]);
    assert_eq!(concert_info.musicians[2].name, "Test Drummer");
    assert_eq!(concert_info.musicians[2].instruments, vec!["drums"]);
}

// Regression tests - load failing pages from the failures directory
#[test]
fn test_regression_failures() -> Result<()> {
    // This function will dynamically find and test all saved failure cases
    // It's designed to grow as more failing HTML pages are captured

    use std::fs;
    use std::path::Path;

    let failures_dir = Path::new("src/tests/fixtures/failures");
    if !failures_dir.exists() {
        return Err(anyhow::anyhow!("failure fixtures not found"));
    }

    println!("Loading regression tests");
    let entries = fs::read_dir(failures_dir)?;
    let mut failures: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map_or(false, |ext| ext == "html") {
            let filename = path.file_stem().unwrap().to_string_lossy();
            println!("Testing regression case: {}", filename);

            // Try to parse the HTML
            if let Some(html) = fixtures::load_failure_html(&filename) {
                let result = parse_concert_info(&html, "https://example.com/regression_test");

                // Check if we've fixed the issue
                if result.is_ok() {
                    println!("✅ Previously failing case now passes: {}", filename);
                } else {
                    failures.push(
                        format!("❌ Still failing: {} - {}", filename, result.err().unwrap())
                            .to_string(),
                    );
                }
            }
        }
    }
    if failures.len() > 0 {
        return Err(anyhow::anyhow!(failures.join("\n")));
    }

    Ok(())
}

// Test edge cases
#[test]
fn test_missing_title() {
    let html = r#"
    <html>
    <head><title></title></head>
    <body></body>
    </html>
    "#;

    let result = parse_concert_info(html, "https://example.com/test");
    assert!(result.is_err());
    assert!(result.err().unwrap().to_string().contains("Title is empty"));
}

#[test]
fn test_missing_story_title() {
    let html = r#"
    <html>
    <head><title>Some Artist: Tiny Desk Concert</title></head>
    <body></body>
    </html>
    "#;

    let result = parse_concert_info(html, "https://example.com/test");
    assert!(result.is_err());
    assert!(result
        .err()
        .unwrap()
        .to_string()
        .contains("No story title found"));
}

#[test]
fn test_missing_date() {
    let html = r#"
    <html>
    <head><title>Some Artist: Tiny Desk Concert</title></head>
    <body>
        <div class="storytitle"><h1>Some Concert</h1></div>
    </body>
    </html>
    "#;

    let result = parse_concert_info(html, "https://example.com/test");
    assert!(result.is_err());
    assert!(result.err().unwrap().to_string().contains("No date found"));
}

#[test]
fn test_missing_set_list() {
    let html = r#"
    <html>
    <head><title>Some Artist: Tiny Desk Concert</title></head>
    <body>
        <div class="storytitle"><h1>Some Concert</h1></div>
        <div class="dateblock"><time datetime="2023-01-01">Jan 1, 2023</time></div>
        <div id="storytext">
            <p>Description</p>
            <p>MUSICIANS</p>
            <ul>
                <li>Test Artist: vocals</li>
            </ul>
        </div>
    </body>
    </html>
    "#;

    let result = parse_concert_info(html, "https://example.com/test");
    assert!(result.is_err());
    assert!(result
        .err()
        .unwrap()
        .to_string()
        .contains("No set list found"));
}

#[test]
fn test_missing_musicians() {
    let html = r#"
    <html>
    <head><title>Some Artist: Tiny Desk Concert</title></head>
    <body>
        <div class="storytitle"><h1>Some Concert</h1></div>
        <div class="dateblock"><time datetime="2023-01-01">Jan 1, 2023</time></div>
        <div id="storytext">
            <p>Description</p>
            <p>SET LIST</p>
            <ul>
                <li>Test Song</li>
            </ul>
        </div>
    </body>
    </html>
    "#;

    let result = parse_concert_info(html, "https://example.com/test");
    assert!(result.is_err());
    // Check that the error is related to musicians being missing
    let error_msg = result.err().unwrap().to_string();
    println!("Error message: {}", error_msg);
    assert!(
        error_msg.contains("musicians"),
        "expected contains musicians: {}",
        error_msg
    );
}
