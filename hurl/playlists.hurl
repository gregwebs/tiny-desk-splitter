# Playlist API and HTML page cases — migrated from
# concert-tracker/tests/web_integration.rs (playlist_api_crud_and_resolution,
# playlist_api_validation_status_codes, playlists_html_pages_render,
# playlist_detail_page_unknown_id_is_404). See
# docs/change/2026-07-12-playlist-hurl-migration.md for the migration
# rationale, including why the `data-playlist-id` and nav-link markup
# assertions from the Rust originals were dropped here (that coverage now
# lives in e2e/playlists.spec.js's drag-drop reorder test).
#
# Playlists are a distinct feature area from the rest of this suite, but they
# still share one server/DB per `just test-hurl` run with every other .hurl
# file. Every assertion below is scoped to the playlist(s)/concert(s) this
# file itself creates — nothing here asserts a bare collection length.

# ── A. CRUD + resolution ────────────────────────────────────────────────────

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "source_url": "https://npr.org/c/playlist-crud-hurl",
    "title": "Playlist CRUD Hurl",
    "concert_date": "2024-08-01",
    "artist": "Playlist Artist",
    "album": "Playlist Crud Album",
    "set_list": ["t0", "t1"],
    "auto_timestamps": [
      {"title": "t0", "start_time": 0.0, "end_time": 55.0, "duration": 55.0},
      {"title": "t1", "start_time": 60.0, "end_time": 115.0, "duration": 55.0}
    ]
  }
}
HTTP 200
[Captures]
crud_cid: jsonpath "$.result.id"

POST {{base_url}}/api/playlists
Content-Type: application/json
{"name": "Hurl CRUD Mix"}
HTTP 200
[Captures]
crud_pid: jsonpath "$.id"

# Add a track item, then a whole-concert item.
POST {{base_url}}/api/playlists/{{crud_pid}}/items
Content-Type: application/json
{"type": "track", "concert_id": {{crud_cid}}, "track_index": 0}
HTTP 200

POST {{base_url}}/api/playlists/{{crud_pid}}/items
Content-Type: application/json
{"type": "concert", "concert_id": {{crud_cid}}}
HTTP 200

# Detail: 2 raw items flatten to 3 resolved tracks (t0 + [t0, t1]).
GET {{base_url}}/api/playlists/{{crud_pid}}
HTTP 200
[Asserts]
jsonpath "$.items" count == 2
jsonpath "$.resolved_tracks" count == 3
jsonpath "$.resolved_tracks[0].title" == "t0"

# List page payload carries the summary, scoped to this playlist's id — never
# assert the collection's overall length, other files seed playlists too.
GET {{base_url}}/api/playlists
HTTP 200
[Asserts]
jsonpath "$[?(@.playlist.id=={{crud_pid}})].summary.track_count" == 3

# Membership of a track — response includes item_id for sidebar remove.
GET {{base_url}}/api/concerts/{{crud_cid}}/tracks/0/playlists
HTTP 200
[Asserts]
jsonpath "$[?(@.id=={{crud_pid}})]" exists
[Captures]
crud_track_item_id: jsonpath "$[?(@.id=={{crud_pid}})].item_id"

# Confirm the round-trip: remove via item_id, membership disappears.
DELETE {{base_url}}/api/playlists/{{crud_pid}}/items/{{crud_track_item_id}}
HTTP 204

GET {{base_url}}/api/concerts/{{crud_cid}}/tracks/0/playlists
HTTP 200
[Asserts]
jsonpath "$[?(@.id=={{crud_pid}})]" not exists

# Re-add so the rest of the scenario (reorder / delete) still has an item.
POST {{base_url}}/api/playlists/{{crud_pid}}/items
Content-Type: application/json
{"type": "track", "concert_id": {{crud_cid}}, "track_index": 0}
HTTP 200

# Re-fetch detail after the remove/re-add above so item ids are current.
GET {{base_url}}/api/playlists/{{crud_pid}}
HTTP 200
[Captures]
crud_item0_id: jsonpath "$.items[0].id"
crud_item1_id: jsonpath "$.items[1].id"

# Reorder the two items (reverse), then remove the first (post-reorder).
POST {{base_url}}/api/playlists/{{crud_pid}}/items/reorder
Content-Type: application/json
{"item_ids": [{{crud_item1_id}}, {{crud_item0_id}}]}
HTTP 204

DELETE {{base_url}}/api/playlists/{{crud_pid}}/items/{{crud_item1_id}}
HTTP 204

GET {{base_url}}/api/playlists/{{crud_pid}}
HTTP 200
[Asserts]
jsonpath "$.items" count == 1

# Delete the playlist; it then 404s.
DELETE {{base_url}}/api/playlists/{{crud_pid}}
HTTP 204

GET {{base_url}}/api/playlists/{{crud_pid}}
HTTP 404

# ── B. Validation status codes ──────────────────────────────────────────────

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "source_url": "https://npr.org/c/playlist-validation-hurl",
    "title": "Playlist Validation Hurl",
    "concert_date": "2024-08-02",
    "artist": "Playlist Artist",
    "album": "Playlist Validation Album",
    "set_list": ["t0"]
  }
}
HTTP 200
[Captures]
val_cid: jsonpath "$.result.id"

# Empty name -> 422.
POST {{base_url}}/api/playlists
Content-Type: application/json
{"name": "   "}
HTTP 422

POST {{base_url}}/api/playlists
Content-Type: application/json
{"name": "Hurl Validation P"}
HTTP 200
[Captures]
val_p_id: jsonpath "$.id"

# Out-of-range track index -> 422.
POST {{base_url}}/api/playlists/{{val_p_id}}/items
Content-Type: application/json
{"type": "track", "concert_id": {{val_cid}}, "track_index": 9}
HTTP 422

# Cycle: P2 nests P, then P nesting P2 -> 422.
POST {{base_url}}/api/playlists
Content-Type: application/json
{"name": "Hurl Validation P2"}
HTTP 200
[Captures]
val_p2_id: jsonpath "$.id"

POST {{base_url}}/api/playlists/{{val_p2_id}}/items
Content-Type: application/json
{"type": "playlist", "child_playlist_id": {{val_p_id}}}
HTTP 200

POST {{base_url}}/api/playlists/{{val_p_id}}/items
Content-Type: application/json
{"type": "playlist", "child_playlist_id": {{val_p2_id}}}
HTTP 422

# Unknown playlist -> 404.
GET {{base_url}}/api/playlists/4242424242
HTTP 404

# ── C. HTML pages ────────────────────────────────────────────────────────────
#
# Only user-visible content is asserted here (name, track title, links). The
# Rust original also asserted the `data-playlist-id` attribute and the nav
# `href="/playlists"` link; both are markup internals now covered by
# e2e/playlists.spec.js instead of duplicated here — see the docs/change note
# at the top of this file.

POST {{test_control_url}}
Content-Type: application/json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "source_url": "https://npr.org/c/playlist-html-hurl",
    "title": "Playlist HTML Hurl",
    "concert_date": "2024-08-03",
    "artist": "Playlist Artist",
    "album": "Playlist HTML Album",
    "set_list": ["Song Zero", "Song One"],
    "auto_timestamps": [
      {"title": "Song Zero", "start_time": 0.0, "end_time": 55.0, "duration": 55.0},
      {"title": "Song One", "start_time": 60.0, "end_time": 115.0, "duration": 55.0}
    ]
  }
}
HTTP 200
[Captures]
html_cid: jsonpath "$.result.id"

POST {{base_url}}/api/playlists
Content-Type: application/json
{"name": "My Hurl Mix"}
HTTP 200
[Captures]
html_pid: jsonpath "$.id"

POST {{base_url}}/api/playlists/{{html_pid}}/items
Content-Type: application/json
{"type": "track", "concert_id": {{html_cid}}, "track_index": 1}
HTTP 200

POST {{base_url}}/api/playlists/{{html_pid}}/items
Content-Type: application/json
{"type": "concert", "concert_id": {{html_cid}}}
HTTP 200

# List page shows the playlist name and links to its detail page.
GET {{base_url}}/playlists
HTTP 200
[Asserts]
body contains "My Hurl Mix"
body contains "/playlists/{{html_pid}}"

# Detail page shows the playlist name and a known track title.
GET {{base_url}}/playlists/{{html_pid}}
HTTP 200
[Asserts]
body contains "My Hurl Mix"
body contains "Song One"

# ── D. Unknown detail page ──────────────────────────────────────────────────

GET {{base_url}}/playlists/999999
HTTP 404
