-- Playlists: user-curated, ordered collections that can contain individual
-- tracks, whole concerts, and other playlists ("live references" — a concert or
-- nested-playlist item is expanded to its CURRENT tracks at read/play time, so
-- later edits to the source propagate automatically).

CREATE TABLE IF NOT EXISTS playlists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    description TEXT,
    inserted_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT
);

-- One row = one ordered item in a playlist. Exactly one of three shapes,
-- enforced by the trailing CHECK:
--   track    : item_type='track',    concert_id set, track_index set
--   concert  : item_type='concert',  concert_id set, track_index NULL
--   playlist : item_type='playlist', child_playlist_id set
--
-- ON DELETE CASCADE keeps live references valid: deleting a concert removes its
-- track/concert items everywhere; deleting a playlist removes it and every item
-- that nests it. (PRAGMA foreign_keys=ON is set in db::configure.)
--
-- track_index is a position into the concert's set_list JSON, which can change
-- length on re-scrape. The CHECK cannot range-check it; add-time validation and
-- the read-time expander (src/playlist.rs) handle out-of-range indices instead.
CREATE TABLE IF NOT EXISTS playlist_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    item_type TEXT NOT NULL CHECK (item_type IN ('track','concert','playlist')),
    concert_id INTEGER REFERENCES concerts(id) ON DELETE CASCADE,
    track_index INTEGER,
    child_playlist_id INTEGER REFERENCES playlists(id) ON DELETE CASCADE,
    inserted_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT,
    CHECK (
        (item_type='track'    AND concert_id IS NOT NULL AND track_index IS NOT NULL AND child_playlist_id IS NULL) OR
        (item_type='concert'  AND concert_id IS NOT NULL AND track_index IS NULL     AND child_playlist_id IS NULL) OR
        (item_type='playlist' AND child_playlist_id IS NOT NULL AND concert_id IS NULL AND track_index IS NULL)
    )
);

CREATE INDEX IF NOT EXISTS idx_playlist_items_playlist ON playlist_items(playlist_id, position);
CREATE INDEX IF NOT EXISTS idx_playlist_items_concert ON playlist_items(concert_id);
CREATE INDEX IF NOT EXISTS idx_playlist_items_child ON playlist_items(child_playlist_id);

-- Audit triggers on the playlists parent only (mirrors 0003). The item table is
-- intentionally left without an AFTER UPDATE trigger: a reorder UPDATEs many
-- rows and nothing consumes per-item updated_at; its inserted_at default stays.
-- recursive_triggers is OFF (db::configure), so the trigger body's own UPDATE
-- cannot re-fire it.
CREATE TRIGGER IF NOT EXISTS playlists_set_updated_at_insert
AFTER INSERT ON playlists
BEGIN
    UPDATE playlists SET updated_at = datetime('now') WHERE id = NEW.id;
END;

CREATE TRIGGER IF NOT EXISTS playlists_set_updated_at_update
AFTER UPDATE ON playlists
WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE playlists SET updated_at = datetime('now') WHERE id = NEW.id;
END;
