# Playlist Hurl migration

Third Hurl migration slice: moved the playlist API/HTML bucket that
`docs/change/2026-07-12-state-only-hurl-migration.md` and `hurl/README.md`
flagged as "a reasonable target for a future migration slice" into
`hurl/playlists.hurl`.

## Changes

- Added `hurl/playlists.hurl` covering the four playlist tests previously in
  `concert-tracker/tests/web_integration.rs`: CRUD + resolution (create
  playlist, add track/concert items, detail resolution, list-page summary,
  track membership round-trip, reorder, delete), validation status codes
  (empty name, out-of-range track index, nesting-cycle 422, unknown-id 404),
  HTML page rendering (list + detail), and the unknown-detail-page 404.
- No new Test Control API surface was needed — `test.seed_lifecycle_concert`
  (added in the prior slice) already reproduces the old Rust fixture setup
  (`set_list` + `auto_timestamps`).
- Every assertion in the new file is scoped to the playlist/concert ids the
  scenario itself created (via `{{captured_id}}` in jsonpath filters like
  `$[?(@.playlist.id=={{crud_pid}})].summary.track_count`), never to the
  bare length of a shared collection — `hurl/*.hurl` files share one server
  and DB for the whole `just test-hurl` run.
- **Coverage change, not a 1:1 port**: the deleted Rust tests also asserted
  two markup-internal details — the detail page's `data-playlist-id="{pid}"`
  attribute and the nav's `href="/playlists"` link. Both were dropped from
  the Hurl port as implementation details rather than user-visible behavior.
  `data-playlist-id` is load-bearing (the reorder JS in
  `concert-tracker/static/playlists.js` reads it before POSTing), so this is
  safe only because `e2e/playlists.spec.js`'s drag-drop reorder test already
  drives that attribute through the real DOM and reorder API. Verified by
  running that spec as part of this change (see below); if it's ever pruned,
  this attribute assertion needs to move somewhere else first.
- Removed the four migrated tests and their now-dead helpers
  (`seed_playlist_concert`, `delete_req`, `get_html`) from
  `concert-tracker/tests/web_integration.rs`. `post_body_json`/`get_json`
  stayed — other remaining tests still use them.
- Updated `hurl/README.md`: removed the playlist bucket from "why the
  remaining tests are still Rust-only", updated the count (48 → 44), added
  the playlist slice to the migrated list, and fixed a stale "not yet part
  of CI" line (Hurl has been a blocking CI step since the prior slice).

## Verification

- `cargo check -p concert-tracker --features test-control`
- `node scripts/hurl-test.js --glob 'hurl/playlists.hurl'` — new file alone
- `just test-hurl` — all five `.hurl` files together against one shared
  server/DB, proving the new file's scoped assertions hold regardless of
  what earlier files seeded
- `cargo nextest run -p concert-tracker --test web_integration` — 44 tests,
  all passing
- `just lint` — clean, confirms no dead code left behind by the helper
  removal
- `npx playwright test e2e/playlists.spec.js` — covers the dropped
  `data-playlist-id` assertion via the real DOM
