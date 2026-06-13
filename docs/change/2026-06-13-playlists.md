# Playlists — Phase 1: data model + JSON API

## Problem / goal

Users want to curate their own ordered collections that can mix **individual
tracks**, **whole concerts**, and **other playlists** (nested), then play them and
see, anywhere a song appears, which playlists it belongs to. This is a large
feature spanning the data model, a new page, the player bar, and the sidebar.

It is being delivered in two passes. **This change (Phase 1) is backend-only**:
the schema, the typed data-access layer, the playlist→tracks expansion logic, and
a JSON HTTP API — all with tests. **No templates or JavaScript change yet.** The
frontend (nav, playlists page, add-to-playlist autocomplete, playlist playback,
sidebar membership + nested queue) is Phase 2, built on top of this API.

Two product decisions shaped the model:

- **Live reference.** A concert or nested-playlist item is *not* copied; it is
  expanded to its **current** tracks every time the playlist is read or played, so
  later edits to the source propagate automatically.
- **Total time = sum of known durations + a count of unknowns** (e.g. "42:10 +
  3 unknown"). Per-track durations come from existing split timestamps; we do not
  ffprobe.

## Data model (`migrations/0004_playlists.sql`)

```
        playlists                         playlist_items
   ┌──────────────────┐            ┌──────────────────────────────┐
   │ id  (PK)         │◀───────────│ playlist_id  (FK, CASCADE)   │
   │ name             │            │ position                     │
   │ description      │            │ item_type  track|concert|... │
   │ inserted_at      │            │ concert_id        (FK,CASCADE)──▶ concerts
   │ updated_at       │◀───────────│ child_playlist_id (FK,CASCADE)│  (self-ref)
   └──────────────────┘            │ track_index                  │
                                   └──────────────────────────────┘
```

A `playlist_items` row is exactly one of three shapes, enforced by a CHECK:

| item_type  | concert_id | track_index | child_playlist_id | expands to            |
|------------|:----------:|:-----------:|:-----------------:|-----------------------|
| `track`    |     set    |     set     |        NULL       | one track             |
| `concert`  |     set    |    NULL     |        NULL       | all of a concert's tracks |
| `playlist` |    NULL    |    NULL     |        set        | another playlist (recursively) |

- **`ON DELETE CASCADE`** (FKs; `PRAGMA foreign_keys=ON` already set) keeps live
  references valid: deleting a concert removes its track/concert items everywhere;
  deleting a playlist removes it and every item nesting it. No orphans.
- **`updated_at` trigger on `playlists` only** (mirrors `0003`). `playlist_items`
  deliberately has no AFTER-UPDATE trigger — a reorder rewrites many rows and
  nothing consumes a per-item `updated_at`.
- **`track_index` is positional** into the concert's `set_list` JSON, which can
  change on re-scrape. The CHECK can't range-check it, so it is handled in code
  (below). A re-scrape that *reorders* a set list silently changes what a `track`
  item resolves to — an inherent, accepted property of positional live references.

## Expansion (`src/playlist.rs`)

`expand_playlist(conn, id) -> Vec<ResolvedTrack>` walks the items in `position`
order and flattens them to concrete tracks:

```
 track    ─▶ resolve_track   (title from set_list[idx], duration from timestamps,
                              available from tracks_present[idx])
 concert  ─▶ resolve_concert (all tracks, via model::list_all_tracks_from_db)
 playlist ─▶ expand_inner    (recurse)
```

Defensive properties, each covered by a test:

- **Out-of-range / shrunk `track_index` is skipped, never an error** — one stale
  reference can't poison the whole playlist GET (just a `tracing::warn!`).
- **Cycle guard**: a `HashSet` of the playlist ids on the current recursion path;
  a nested playlist already on the path is skipped and warned. This bounds even a
  cycle that predates the add-time check.

`summarize_playlist` folds the resolved tracks into `{ track_count,
known_duration_secs, unknown_count, first_track }` for the list page.

## Data-access layer (`src/db.rs`)

Typed functions following the existing `create_*/get_*/list_*` conventions:
`create_playlist`, `get_playlist`, `list_playlists`, `update_playlist`,
`delete_playlist`, `find_playlist_by_name`, `list_playlist_items`,
`add_playlist_item`, `remove_playlist_item`, `reorder_playlist_items`,
`would_create_cycle`, `track_durations`, and the three membership queries
`playlists_containing_track` / `_concert` / `playlists_nesting_playlist`.

Validation is surfaced through a dedicated `PlaylistError { NotFound, Invalid,
Db }` so the web layer can map each case to the right status:

- `add_playlist_item` validates the reference (concert exists, `track_index` in
  range against the *current* set list, nested playlist exists and isn't a
  self/cycle) before inserting; appends at `MAX(position)+1`.
- `reorder_playlist_items` runs in a transaction and rejects an id set that
  doesn't exactly match the playlist's items.
- Items are never renumbered on delete (positional gaps are harmless).

No event-log rows are written for playlist mutations: the `events` table is
`concert_id NOT NULL`-scoped and playlist history isn't required.

## JSON API (`/api/...`)

Mounted under `/api` to keep the machine/JSON surface distinct from the htmx HTML
the rest of the app serves, and to avoid colliding with the Phase-2 HTML pages at
`/playlists` and `/playlists/:id`.

| Method & path | Body | Returns |
|---|---|---|
| `POST /api/playlists` | `{name, description?}` | `{id}` |
| `GET /api/playlists` | – | `[{playlist, summary}]` |
| `GET /api/playlists/:id` | – | `{playlist, items, resolved_tracks}` |
| `PATCH /api/playlists/:id` | `{name?, description?}` | `204` |
| `DELETE /api/playlists/:id` | – | `204` |
| `POST /api/playlists/:id/items` | `{type, concert_id?, track_index?, child_playlist_id?}` | `{item_id}` |
| `DELETE /api/playlists/:id/items/:item_id` | – | `204` |
| `POST /api/playlists/:id/items/reorder` | `{item_ids:[…]}` | `204` |
| `GET /api/concerts/:id/tracks/:idx/playlists` | – | `[Playlist]` (membership) |
| `GET /api/concerts/:id/playlists` | – | `[Playlist]` |
| `GET /api/playlists/:id/nested-in` | – | `[Playlist]` |

Status mapping (`AppError`): missing playlist → **404**; validation failure (empty
name, out-of-range/missing reference, cycle, malformed item) → **422**; unexpected
→ **500**.

`GET /api/playlists/:id` returns *both* the raw `items` (the editable references)
and the flattened `resolved_tracks` (each with `concert_id`, `track_index`,
`title`, `duration`, `available`) so Phase-2 playback and the sidebar have what
they need without a second round-trip.

`PATCH` is partial: an omitted field is left unchanged. Note that sending
`description: null` does **not** clear the description (it keeps the current
value) — clearing a description back to NULL is not supported in this phase.

## Tests

- `src/db.rs` unit tests: CRUD, add of each item kind, ordering, reference
  validation, self/cycle rejection, remove + harmless gaps, reorder
  renumber/validation, cascade delete, `track_durations` user-over-auto
  preference, membership queries.
- `src/playlist.rs` unit tests: track+concert+nested flattening order,
  **out-of-range-after-shrink is skipped (not an error)**, summary known-sum +
  unknown-count, empty playlist.
- `tests/web_integration.rs`: full JSON round-trip (create → add items → resolved
  GET → list summary → membership → reorder → remove → delete → 404) and the
  validation status codes (empty name, OOB index, cycle → 422; unknown id → 404).

All pass. (Two unrelated pre-existing `web_integration` failures —
`play_button_visible_after_successful_split`,
`detail_page_auto_scrape_failure_still_renders` — predate this change, confirmed
on a clean tree.)

## Phase 2 (frontend) — not yet implemented

Built later on this API: nav reshuffle (Playlists where Jobs is; Jobs beside ⚙);
`/playlists` and `/playlists/:id` HTML pages (track count, total time, first
track); a hover "+" on tracks and concert titles opening the sidebar autocomplete
(`GET /api/playlists`) that shows current memberships and a create-new option;
`player.js` playlist playback with multi-concert auto-advance; the playing
playlist's title in the bar beside the queue button (click → open sidebar); and
the sidebar nested queue that hides already-played items.
