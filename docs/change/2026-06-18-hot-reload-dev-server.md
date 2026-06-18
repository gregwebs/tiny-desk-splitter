# Frontend hot-reload for the dev server

## Motivation

Every frontend edit required a full `cargo build`: HTML/CSS are askama
templates compiled into the binary at compile time (CSS lives inline in
`templates/layout.html`), and the four JS files (`player.js`, `splitter.js`,
`playlists.js`, `htmx.min.js`) were embedded via `include_str!`. There was no
watch tooling — the server had to be started by hand with `cargo run --bin
concert-web` and manually restarted after every change.

## What changed

A new `--dev` CLI flag on `concert-web`
(`concert-tracker/src/bin/concert_web.rs`) enables two things via
`web::router_with_opts` (`concert-tracker/src/web/mod.rs`):

1. **Browser auto-refresh**: a `tower_livereload::LiveReloadLayer` is added to
   the router, which injects a script that reconnects and refreshes the page
   whenever the server process restarts.
2. **Instant JS edits**: `/static/*.js` is served from disk
   (`ServeDir::new(CARGO_MANIFEST_DIR/static)`) instead of the
   `include_str!`-embedded copies, so JS edits don't need a recompile. The
   static dir's existence is checked at request-routing time with a
   `tracing::error!` if missing (a 404-everything failure mode otherwise).

`pub fn router(state)` is unchanged in signature — it now just delegates to
`router_with_opts(state, RouterOpts::default())` — so all ~75 existing
`router(state)` call sites in `tests/web_integration.rs`, and `concert-web`
without `--dev`, are unaffected and still embed JS with no livereload layer.

A new `just dev` target wraps `cargo-watch`, watching
`concert-tracker/{src,templates,static}` and running `concert-web --dev` on
change:

| Edit                                   | Recompile? | What happens |
|-----------------------------------------|------------|---------------|
| `templates/*.html` (incl. inline CSS)   | yes (askama) | cargo-watch rebuilds + restarts → browser refresh |
| `src/**/*.rs`                           | yes        | cargo-watch rebuilds + restarts → browser refresh |
| `static/*.js`                           | no         | cargo finds nothing to compile, just restarts the binary → browser refresh; fresh JS read from disk |

CSS is **not** instant — it's inline in `templates/layout.html`, an askama
template, so it rides the same recompile path as HTML.

A new regression test, `prod_router_serves_embedded_js_without_livereload`
(`concert-tracker/tests/web_integration.rs`), drives the production path
(`router(state)`) and asserts `/static/player.js` still returns the
`include_str!`-embedded content with `application/javascript`, and that a
full-page response contains no livereload marker — pinning the dev/prod
divergence so it can't silently leak into production.

## Verification

- `cargo test -p concert-tracker --test web_integration` — full suite
  including the new prod-invariant test passes.
- `just lint` clean.
- Manual: `cargo install cargo-watch`, then `just dev --db <scratch>.db
  --workdir <scratch-dir> --port 3001` against a copy of `concerts.db` (the
  real db and workdir were never touched). Confirmed: editing `static/player.js`
  refreshes the browser with no recompile in the cargo-watch log; editing a
  template or the inline CSS in `layout.html` triggers a recompile + restart +
  refresh; `cargo run --bin concert-web` (no `--dev`) still serves the embedded
  JS and injects no livereload script.

## Files changed

- `concert-tracker/Cargo.toml` — added `tower-livereload = "0.10"`
- `concert-tracker/src/web/mod.rs` — `RouterOpts`, `router_with_opts`,
  `static_js_router`; `router` now delegates with `RouterOpts::default()`
- `concert-tracker/src/bin/concert_web.rs` — `--dev` flag, switched to
  `router_with_opts`
- `concert-tracker/tests/web_integration.rs` — added
  `prod_router_serves_embedded_js_without_livereload`
- `justfile` — added `dev` recipe
- `README.md` — "Hot-reload dev server" section under Development
