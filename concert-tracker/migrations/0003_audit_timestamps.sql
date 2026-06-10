-- Audit timestamps: keep updated_at (and jobs' inserted_at/updated_at) current
-- automatically so none of the many UPDATE statements in the Rust code need to
-- set them by hand.
--
-- The AFTER UPDATE triggers carry a `WHEN NEW.updated_at IS OLD.updated_at`
-- guard so they fire only for ordinary updates (which never touch updated_at),
-- and stay quiet when a statement sets updated_at explicitly. That makes the
-- one place that does set it explicitly -- backfill_audit_timestamps -- safe
-- regardless of whether the triggers already exist, so the historical value is
-- never overwritten with now().
--
-- Recursion is not a concern either: recursive_triggers defaults to OFF
-- (configure() only sets journal_mode and foreign_keys), so the trigger body's
-- own `UPDATE ... SET updated_at` cannot re-fire any trigger.

-- concerts: inserted_at is set on INSERT by the column default; updated_at is
-- owned entirely by these triggers.
CREATE TRIGGER IF NOT EXISTS concerts_set_updated_at_insert
AFTER INSERT ON concerts
BEGIN
    UPDATE concerts SET updated_at = datetime('now') WHERE id = NEW.id;
END;

CREATE TRIGGER IF NOT EXISTS concerts_set_updated_at_update
AFTER UPDATE ON concerts
WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE concerts SET updated_at = datetime('now') WHERE id = NEW.id;
END;

-- jobs: rows are inserted once (insert_failed_job) and not updated today; the
-- AFTER UPDATE trigger is included for consistency/future use.
CREATE TRIGGER IF NOT EXISTS jobs_set_timestamps_insert
AFTER INSERT ON jobs
BEGIN
    UPDATE jobs SET inserted_at = datetime('now'), updated_at = datetime('now')
    WHERE id = NEW.id;
END;

CREATE TRIGGER IF NOT EXISTS jobs_set_updated_at_update
AFTER UPDATE ON jobs
WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE jobs SET updated_at = datetime('now') WHERE id = NEW.id;
END;

-- settings: the single row is created during migration (not at runtime), so no
-- INSERT trigger is needed; backfill populates inserted_at/updated_at for it.
CREATE TRIGGER IF NOT EXISTS settings_set_updated_at_update
AFTER UPDATE ON settings
WHEN NEW.updated_at IS OLD.updated_at
BEGIN
    UPDATE settings SET updated_at = datetime('now') WHERE id = NEW.id;
END;
