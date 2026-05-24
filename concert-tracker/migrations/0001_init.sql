CREATE TABLE IF NOT EXISTS concerts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_url TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    concert_date TEXT,
    teaser TEXT,
    artist TEXT,
    album TEXT,
    description TEXT,
    set_list_json TEXT,
    musicians_json TEXT,
    ignored INTEGER NOT NULL DEFAULT 0,
    wanted INTEGER NOT NULL DEFAULT 0,
    notes TEXT,
    download_started_at TEXT,
    downloaded_at TEXT,
    download_errors_json TEXT NOT NULL DEFAULT '[]',
    split_started_at TEXT,
    split_at TEXT,
    split_errors_json TEXT NOT NULL DEFAULT '[]',
    first_seen_at TEXT NOT NULL DEFAULT (datetime('now')),
    metadata_scraped_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_concerts_date ON concerts(concert_date DESC);
CREATE INDEX IF NOT EXISTS idx_concerts_ignored ON concerts(ignored);

CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    concert_id INTEGER NOT NULL,
    event TEXT NOT NULL,
    at TEXT NOT NULL,
    json TEXT,
    inserted_at TEXT NOT NULL DEFAULT (datetime('now')),
    updates TEXT
);
CREATE INDEX IF NOT EXISTS idx_events_concert_id ON events(concert_id);

CREATE TABLE IF NOT EXISTS synced_months (
    year INTEGER NOT NULL,
    month INTEGER NOT NULL,
    synced_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (year, month)
);

CREATE TABLE IF NOT EXISTS jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    concert_id INTEGER NOT NULL,
    name TEXT,
    failed_at TEXT,
    failure_message TEXT
);
CREATE INDEX IF NOT EXISTS idx_jobs_failed_at ON jobs(failed_at DESC);
