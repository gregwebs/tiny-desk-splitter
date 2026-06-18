# OpenAPI/Swagger docs for the JSON API

## Motivation

The JSON API (playlists CRUD, playback/media-info, split-timestamps) had no
machine-readable contract — consumers had to read handler source to know
request/response shapes. HTML/htmx routes don't need this (they're not a
public contract), but the JSON endpoints do.

## What changed

Added `utoipa` + `utoipa-swagger-ui` and annotated the 21 JSON-API handlers in
`concert-tracker/src/web/handlers.rs` with `#[utoipa::path(...)]`, declaring
method, path, params, request/response bodies. Request/response types got
`#[derive(ToSchema)]` (`handlers.rs`, `concert-tracker/src/split_timestamps.rs`,
`concert-types/src/lib.rs::SongTimestamp`). Two ad-hoc `json!` response shapes
in the prepare/split-status handlers were replaced with typed enums
(`SplitStartStatus`, `SplitStartResponse`) so the documented schema matches the
wire format exactly.

A new `concert-tracker/src/web/openapi.rs` defines `ApiDoc` via
`#[derive(OpenApi)]`, listing all 21 paths plus the schema/tag set. Wired into
the router (`concert-tracker/src/web/mod.rs`) via `SwaggerUi::new("/swagger-ui")
.url("/api-docs/openapi.json", ApiDoc::openapi())`, mounted unconditionally
(not gated by `--dev`) since this is a single-user self-hosted tool and the
docs are worth having in prod.

HTML/htmx routes are intentionally not documented — `openapi.rs`'s module doc
comment calls this out explicitly.

## Verification

- `cargo test -p concert-tracker` — `openapi::tests` (`builds_and_serializes_without_panicking`,
  `documents_all_expected_json_paths`, `documents_representative_schemas`) plus
  full existing suite pass.
- `just lint` clean.
- New `e2e/openapi.spec.js`: `/api-docs/openapi.json` is a well-formed 3.1 spec
  with the expected paths; `/swagger-ui` renders the three tag groups; a live
  "Try it out" → "Execute" round trip on `GET /api/playlists` returns 200.

## Files changed

- `concert-tracker/Cargo.toml`, `concert-types/Cargo.toml` — added `utoipa`
  (+`chrono`, `axum_extras` features), `utoipa-swagger-ui`
- `concert-tracker/src/web/handlers.rs` — `#[utoipa::path]` on 21 handlers,
  `ToSchema` derives, `SplitStartStatus`/`SplitStartResponse` typed responses
- `concert-tracker/src/web/openapi.rs` — new, `ApiDoc`
- `concert-tracker/src/web/mod.rs` — mount `SwaggerUi` + `/api-docs/openapi.json`
- `concert-tracker/src/split_timestamps.rs`, `concert-types/src/lib.rs` —
  `ToSchema` derives on `TimestampPayload`/`TimestampPayloadSong`/`SongTimestamp`
- `e2e/openapi.spec.js` — new
