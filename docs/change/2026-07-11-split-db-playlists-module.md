# Split SQLite Persistence: Move Playlist Persistence Behind a Domain Module

Implements [#65](https://github.com/gregwebs/tiny-desk-splitter/issues/65), the
second and final *migrate* step of the wider `db.rs` domain split
([#69](https://github.com/gregwebs/tiny-desk-splitter/issues/69)), following
[#63](https://github.com/gregwebs/tiny-desk-splitter/issues/63) (the *expand*
step) and [#64](https://github.com/gregwebs/tiny-desk-splitter/issues/64) (the
first *migrate* step). Behavior-preserving code motion only: no schema, SQL,
transaction-scope, validation-rule, or error-message changes.

## Scope

Moved playlist persistence out of `concert-tracker/src/db/mod.rs` into a new
`db::playlists` module:

- `PlaylistError` (+ `Display`/`Error`/`From` impls) and `PlaylistMembership`.
- Row mapping: `playlist_from_row`, `RawPlaylistItem`, `raw_playlist_item_from_row`,
  `raw_to_playlist_item`, `membership_from_row` (all private).
- CRUD: `create_playlist`, `get_playlist`, `list_playlists`,
  `find_playlist_by_name`, `update_playlist`, `delete_playlist`.
- Items: `list_playlist_items`, `add_playlist_item` (validate-then-insert in
  one `unchecked_transaction`), `remove_playlist_item`,
  `reorder_playlist_items` (validate-then-renumber in one `transaction`).
- Validation helpers (private): `playlist_exists`, `concert_set_list_len`.
- Cycle detection: `would_create_cycle` (stays `pub` — no external callers
  today, but privatizing it now would prune a public path before #68, the
  ticket that owns facade/API contraction).
- Membership lookups: `playlists_containing_track`,
  `playlists_containing_concert`, `playlists_nesting_playlist`.

`track_durations` also moved, but to `db::split_timestamps` rather than
`db::playlists`: it derives per-track durations purely from
`get_split_timestamps`, touches no playlist tables, and the #69 final module
map scopes `db::playlists` to CRUD/items/membership/cycle validation only.
Moving it also retires the "load-bearing facade" note `db/mod.rs` carried
since #64 (`track_durations` previously resolved `get_split_timestamps`
through the facade's `pub use` rather than a direct import — it now calls it
directly, in the same module). #65 was the last migrate-step ticket, so
leaving `track_durations` in `mod.rs` would have stranded it there with no
remaining ticket to move it before #68's contraction.

The temporary compatibility facade added in #63 was extended with `pub use`
re-exports for every playlist item and for `track_durations`, so all existing
`db::...` call sites (`playlist.rs`, `web/handlers.rs`) keep compiling
unchanged. Caller migration to domain paths is #66/#67; facade removal is #68.

## Module map after this step

```text
db/
├── mod.rs               facade + shared test helpers only (all domains moved)
├── connection.rs         migrations, open/open_in_memory, pragmas          (#63)
├── settings.rs           Theme, Settings, settings reads/writes            (#63)
├── time.rs               now_string                                        (#63)
├── concerts.rs            NewListing, MetadataUpdate, concert reads/writes  (#64)
├── lifecycle.rs           download/split/archive transitions                (#64)
├── split_timestamps.rs    StoredSplitTimestamps, tracks, media duration,
│                          track_durations                                   (#64, #65)
├── sync.rs                synced-month persistence                          (#64)
├── failed_jobs.rs          FailedJob                                        (#64)
└── playlists.rs            PlaylistError, PlaylistMembership, playlist
                            CRUD/items/reorder/cycle validation               (#65)
```

`db/mod.rs` now contains only the module-dependency-direction doc comment,
`pub mod` declarations, the temporary facade, and the shared `pub(crate)`
test helpers (`events_for`, `listing`, `seed`, `seed_with_album`) used across
every domain's test module.

## Test moves

All 14 playlist unit tests moved intact into `db::playlists::tests`, along
with the private `seed_concert` and (implicitly, via test bodies) track-list
helpers they need. These tests use variable URLs/titles/set-lists per case,
so they keep their own `seed_concert` rather than reusing the shared
`db::tests::seed`/`seed_with_album` (which seed one fixed concert shape) —
matching the pattern already established for other domains' tests. Concert
setup calls (`upsert_listing`, `get_concert_by_url`, `update_metadata`,
`MetadataUpdate`) are imported directly from `crate::db::concerts`, not
through the top-level facade, so the moved tests don't pick up a dependency
on paths #68 removes.

`track_durations_prefers_user_then_auto` moved to
`db::split_timestamps::tests`, reseated on the existing `seed_with_album`
helper (2-track set list) with a local `ts` constructor, since it only needs
a concert with a set list to attach timestamps to — matching the pattern of
the module's other tests.

## State changes

None. This is a pure module reorganization; all SQL, transaction scopes,
validation rules, and error variants/messages are unchanged.

## Verification

- Baseline before starting: `cargo test -p concert-tracker db::` — 127 tests
  pass (including 14 playlist tests and `track_durations_prefers_user_then_auto`,
  all then under `db::tests::*`).
- After code motion: `cargo test -p concert-tracker db::` — still 127 tests,
  moved tests now reporting under `db::playlists::tests::*` and
  `db::split_timestamps::tests::track_durations_prefers_user_then_auto`.
- `cargo test -p concert-tracker` — 479 lib tests + 76 integration tests, all
  passing (same counts as after #64).
- `cargo check --workspace` — passes.
- `just lint` — passes with no warnings.
- Codex adversarial review of the plan: approved, no blockers. Findings
  folded into the plan before implementation (import the concert-setup test
  helpers directly from `db::concerts` rather than through the facade).
- Codex adversarial review of the implementation: confirmed the playlist SQL
  strings, validation branches, transaction scopes, and error messages are
  unchanged from the pre-move code; confirmed the facade re-export list is
  complete against every playlist item that was public before; confirmed the
  test relocation is semantically correct. One finding (missing this change
  doc) addressed by adding it.
- Manual: started `concert-web` on a spare port with a fresh `--db` and
  `--workdir`; exercised playlist creation, adding track/concert/nested
  items, reordering, the membership sidebar, and invalid-input cases
  (empty name, missing concert/playlist, out-of-range track, cycle, mismatched
  reorder set) confirming the same 404/422/500 mapping as before.
