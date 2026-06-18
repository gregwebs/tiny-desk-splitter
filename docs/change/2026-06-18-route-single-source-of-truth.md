# Single source of truth for routes: adopt utoipa-axum

## Motivation

Route URL paths were hand-maintained in two places, two syntaxes: the axum
router chain in `concert-tracker/src/web/mod.rs` (colon-style `:id`), and the
`#[utoipa::path(path = "...")]` annotations on the 21 JSON-API handlers plus
the `paths(...)` list in `ApiDoc` (brace-style `{id}`). Adding or changing a
JSON endpoint meant editing the path string in 2-3 places by hand.

## What changed

Added `utoipa-axum = "=0.1.3"` (pinned exactly: 0.1.4+ requires axum ^0.8,
this project is on axum 0.7.9 — revisit the pin together with any future axum
0.8 migration). `router_with_opts` now builds on `utoipa_axum::router::OpenApiRouter`
instead of plain `axum::Router`:

- Annotated JSON handlers register via `.routes(routes!(handler))` (or
  `.routes(routes!(h1, h2, ...))` for handlers sharing one path with different
  verbs) — the `#[utoipa::path]` attribute on the handler is now the single
  source for both the axum route and the OpenAPI doc entry.
- Un-annotated HTML/htmx handlers (~30) are unchanged — plain `.route(path,
  get(handler))`, since they were never duplicated.
- The chain ends in `.split_for_parts()` → `(axum::Router<AppState>,
  utoipa::openapi::OpenApi)`; everything downstream (static JS router merge,
  `SwaggerUi` mount, static-file `nest_service`s, tracing/error layers,
  `.with_state()`) runs on the resulting plain `axum::Router`, not inside the
  `OpenApiRouter` chain — `OpenApiRouter::merge` only accepts another
  `OpenApiRouter`, so merging those there wouldn't compile.

The route table is now built by `pub(crate) fn api_router() -> (Router<AppState>,
utoipa::openapi::OpenApi)`, called by `router_with_opts` and also, via a
`#[cfg(test)] built_api_doc()` accessor, by the test that used to call
`ApiDoc::openapi()` directly — paths only exist once `routes!` has actually
run, so the old `paths(...)` list on `ApiDoc` was removed (it's now seeded
with `info`/`tags`/`components` only) and the `EXPECTED_PATHS` test
(`concert-tracker/src/web/openapi.rs`) reads the built router's doc instead.
That test's role shifted: it now catches "added a handler, forgot to wire
`.routes(routes!(...))` into `api_router`" (which would drop the route
entirely) rather than "forgot a `paths()` entry".

## Verification

- `cargo build` / `cargo test -p concert-tracker` (404+72+4 tests, including
  the updated `documents_all_expected_json_paths`) / `just lint` all clean.
- Captured `/api-docs/openapi.json` before and after on an isolated test
  server (separate port, test db copied from `concerts.db`, separate
  workdir — real db never touched): paths and per-path HTTP methods are
  byte-for-byte identical (17 paths, 20 operations).
- Swept representative un-annotated HTML/htmx routes (`/`, `/concerts/:id`,
  `/concerts/:id/tracks`, `/jobs`, `/settings`, `/playlists`, all four
  `/static/*.js` routes) plus JSON routes with path params — all 200 (or an
  app-level 404 with a non-empty body, confirmed distinct from axum's empty-
  body unmatched-route 404). `/swagger-ui` still renders.
- Reviewed by the engineering-lead agent.

## Files changed

- `concert-tracker/Cargo.toml` — add `utoipa-axum = "=0.1.3"`
- `concert-tracker/src/web/mod.rs` — `api_router()`, `built_api_doc()`,
  `router_with_opts` rewrite around `OpenApiRouter`/`routes!`/`split_for_parts`
- `concert-tracker/src/web/openapi.rs` — drop `paths(...)`; test reads
  `built_api_doc()` instead of `ApiDoc::openapi()`
- `concert-tracker/src/web/handlers.rs` — unchanged (the existing
  `#[utoipa::path]` annotations are now the single source)
