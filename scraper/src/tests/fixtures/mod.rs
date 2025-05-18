use std::fs;
use std::path::Path;

/// Load test HTML fixture by name
pub fn load_html_fixture(fixture_name: &str) -> String {
    let path = Path::new("src/tests/fixtures").join(format!("{}.html", fixture_name));
    fs::read_to_string(path)
        .unwrap_or_else(|_| panic!("Failed to load test fixture: {}", fixture_name))
}

/// Load a real failure case for regression testing
pub fn load_failure_html(failure_name: &str) -> Option<String> {
    let path = Path::new("src/tests/fixtures/failures").join(format!("{}.html", failure_name));
    fs::read_to_string(path).ok()
}
