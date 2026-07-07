# Sandbox auto-scrape test proxy stabilization

## Problem

`detail_page_auto_scrape_failure_still_renders` intentionally exercises the real
detail-page auto-scrape path against `127.0.0.1:1` so the scrape fails quickly and
the listing-only detail page still renders. In sandboxed macOS environments,
reqwest's default system proxy detection can panic before the direct connection
refusal is observed.

## Change

`concert-tracker/tests/web_integration.rs` now initializes the scraper crate's
process-wide proxy mode to `ProxyMode::None` from web integration test setup. The
production CLI defaults are unchanged: `concert-web` and `concert-db` still select
system proxy behavior unless proxy flags override it.

## State transition

| State | Trigger | Proxy mode | Expected result |
|---|---|---|---|
| Listing-only concert | Detail page opened in web integration test | `None` | Direct scrape attempt to `127.0.0.1:1` fails with connection refusal |
| Scrape failed | Handler catches scrape error | `None` | Detail page renders listing data |
| Listing-only concert | Test rereads database | `None` | `metadata_scraped_at` remains `NULL` so the next view can retry |

## Verification

- `cargo test -p concert-tracker --test web_integration detail_page_auto_scrape_failure_still_renders -- --nocapture`
- `cargo test -p concert-tracker --test web_integration -- --nocapture`
- `cargo test -p tiny-desk-scraper http_tests -- --nocapture`
- `cargo check -p concert-tracker`
- `just lint` passed `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings`, then failed in the existing shellcheck step on `scripts/github/gh-app-token.sh:20` (`SC2034`, unused `script_dir`).
