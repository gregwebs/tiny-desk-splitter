# Split SQLite Persistence: Migrate Web Handlers and Tests to Domain Paths

Implements [#66](https://github.com/gregwebs/tiny-desk-splitter/issues/66), a
*migrate* step of the wider `db.rs` domain split
([#69](https://github.com/gregwebs/tiny-desk-splitter/issues/69)), parallel to
[#67](https://github.com/gregwebs/tiny-desk-splitter/issues/67) (the non-web
migration batch, already merged) and following
[#63](https://github.com/gregwebs/tiny-desk-splitter/issues/63)–[#65](https://github.com/gregwebs/tiny-desk-splitter/issues/65)
(expand + move domain persistence behind the facade). Call-path-only change:
no persistence logic, error propagation, HTTP status mapping, template data,
htmx response shapes, JSON response shapes, or transaction-scope changes; no
facade export removed.

## Scope

Pointed every web caller at its owning `db::<domain>::...` module instead of
the temporary top-level facade in `concert-tracker/src/db/mod.rs`.

3 files migrated:

- `src/web/handlers.rs` — 112 call/type-path sites across concerts, settings,
  lifecycle, playlists, and failed-jobs domains.
- `tests/web_integration.rs` — 150 call/type-path sites, plus a grouped
  `use concert_tracker::db::{get_split_timestamps, set_auto_split_timestamps,
  set_user_split_timestamps}` import split to
  `use concert_tracker::db::split_timestamps::{...}`.
- `src/bin/concert_web.rs` — app-state setup, `db::open` →
  `db::connection::open` (2 call sites).

Left untouched, as designed (owned by #68): `src/db/mod.rs` itself (the
facade block and its shared `#[cfg(test)]` helpers).

## Migration mechanics

Same approach as #67: a scripted regex substitution (word-boundary matched,
built from the exact re-export list in `src/db/mod.rs`, so e.g.
`get_concert` never matched `get_concert_opt`) qualified every facade symbol
with its owning domain module across all three files in one pass, followed by
manual review of the diff. All 27 mapped domain names are unique across
domains, so the substitution was unambiguous. One grouped `use` import (in
`tests/web_integration.rs`, not matched by the `db::name` call-site regex)
needed a manual follow-up edit to `db::split_timestamps::{...}`.

`cargo fmt` re-wrapped three lines that exceeded the column width once domain
module segments were added to call chains (e.g.
`db::settings::get_settings(&conn)?.archive_location.is_some()` in
`handlers.rs`, `db::concerts::get_concert_by_url(conn, url).unwrap().unwrap().id`
in `web_integration.rs`) — purely cosmetic, no logic touched.

## State changes

None. This is a pure call-path reorganization; all SQL, transaction scopes,
event emission, HTTP status mapping, and timestamp formats are unchanged —
verified by identical test counts before and after.

## Verification

- Baseline: `cargo test -p concert-tracker` — 479 lib tests + 76 integration
  tests, all passing (unchanged from #67's post-migration baseline).
- After migration: `cargo check -p concert-tracker --tests` and `cargo test
  -p concert-tracker` — still 479 lib + 76 integration tests passing.
- Completeness check — grep for every facade symbol name (calls and type
  paths, built from the mapping table) across `src/web/handlers.rs`,
  `src/bin/concert_web.rs`, and `tests/web_integration.rs`: zero matches.
- Final: `cargo check --workspace` passes; `just lint` passes with no
  warnings (after `cargo fmt` reflowed lines lengthened by domain
  qualification).
- Codex adversarial review of the branch diff against `main`: verdict
  approve, no material findings — mappings verified against the facade
  exports in `src/db/mod.rs`. (`cargo check` inside the review's own sandbox
  was blocked by a read-only `/tmp`; already covered by the `cargo
  test`/`cargo check` runs above.)
- Manual: built a 6-concert fixture DB with `cargo run --example
  make_test_fixture`, started `concert-web` on a spare port against that
  fixture `--db` and a fresh `--workdir`, and exercised the flows named in
  #66's acceptance criteria over HTTP:
  - concerts domain: `/`, `/concerts/:id` (200 and 404), ignore/want toggles
    (`POST /concerts/:id/ignore`, `/want`)
  - settings domain: `GET /settings`, `POST /settings` (theme persisted and
    reflected on reload)
  - lifecycle/jobs domain: `/jobs`, `/jobs/count`
  - playlists domain: create/list (`POST`/`GET /api/playlists`), add item
    (`POST /api/playlists/:id/items`), delete (`DELETE
    /api/playlists/:id`), playlist detail page
  - split_timestamps domain: `/concerts/:id/tracks`, like-track toggle
    (`POST /concerts/:id/tracks/:idx/like`)
  - openapi: `/api-docs/openapi.json` returns 200

## Next steps

#68 deletes the facade block in `src/db/mod.rs`, fixes the shared test
helpers' now-unqualified imports, and documents the final module map in
`docs/backend-persistence.md`.
