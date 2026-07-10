/// Current UTC time in the `datetime('now')` space format
/// (`2026-06-09 20:33:05`) that the concerts-table timestamp columns use.
/// Prefer this over ad-hoc chrono formatting: the codebase already suffers a
/// two-format hazard (see `backfill_audit_timestamps`), so Rust-side writers
/// of concerts columns should match the SQL default format.
pub fn now_string() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}
