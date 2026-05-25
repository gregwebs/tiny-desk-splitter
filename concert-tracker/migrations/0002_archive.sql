CREATE TABLE IF NOT EXISTS settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    archive_location TEXT
);
INSERT OR IGNORE INTO settings (id) VALUES (1);

ALTER TABLE concerts ADD COLUMN archive_started_at TEXT;
ALTER TABLE concerts ADD COLUMN archived_at TEXT;
ALTER TABLE concerts ADD COLUMN archive_errors_json TEXT NOT NULL DEFAULT '[]';
