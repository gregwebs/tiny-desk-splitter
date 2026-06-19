//! Prints the JSON API's OpenAPI 3.1 spec to stdout, exactly as served at
//! `/api-docs/openapi.json` (see `concert_tracker::web::built_api_doc`).
//!
//! This exists so the TypeScript build can generate types
//! (`frontend/src/generated/openapi.d.ts`, via `openapi-typescript`) offline
//! and reproducibly, without a running `concert-web` server or a database.
//! See `just openapi-types`.

use anyhow::{Context, Result};

fn main() -> Result<()> {
    let doc = concert_tracker::web::built_api_doc();
    let json = doc
        .to_pretty_json()
        .context("failed to serialize OpenAPI doc")?;
    println!("{json}");
    Ok(())
}
