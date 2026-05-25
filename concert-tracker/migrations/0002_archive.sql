CREATE TABLE IF NOT EXISTS settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    archive_location TEXT
);
INSERT OR IGNORE INTO settings (id) VALUES (1);
