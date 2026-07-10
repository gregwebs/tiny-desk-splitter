use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    System,
    Light,
    Dark,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    pub fn parse(s: &str) -> Result<Theme> {
        match s {
            "system" => Ok(Theme::System),
            "light" => Ok(Theme::Light),
            "dark" => Ok(Theme::Dark),
            other => Err(anyhow!("unknown theme: {other}")),
        }
    }

    /// True for an explicit user choice — used by templates to decide
    /// whether to render the `data-theme` attribute on `<html>`.
    /// `System` produces no attribute so `prefers-color-scheme` wins.
    pub fn is_explicit(self) -> bool {
        !matches!(self, Theme::System)
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub archive_location: Option<String>,
    pub theme: Theme,
}

pub fn get_settings(conn: &Connection) -> Result<Settings> {
    conn.query_row(
        "SELECT archive_location, theme FROM settings WHERE id = 1",
        [],
        |row| {
            let archive_location: Option<String> = row.get(0)?;
            let theme_str: String = row.get(1)?;
            Ok((archive_location, theme_str))
        },
    )
    .context("Failed to read settings")
    .map(|(archive_location, theme_str)| Settings {
        archive_location,
        theme: Theme::parse(&theme_str).unwrap_or(Theme::System),
    })
}

pub fn update_archive_location(conn: &Connection, location: &str) -> Result<()> {
    let value = if location.trim().is_empty() {
        None
    } else {
        Some(location.trim())
    };
    conn.execute(
        "UPDATE settings SET archive_location = ?1 WHERE id = 1",
        params![value],
    )
    .context("Failed to update archive location")?;
    Ok(())
}

pub fn update_theme(conn: &Connection, theme: Theme) -> Result<()> {
    tracing::debug!("update_theme: {}", theme.as_str());
    conn.execute(
        "UPDATE settings SET theme = ?1 WHERE id = 1",
        params![theme.as_str()],
    )
    .context("Failed to update theme")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connection::open_in_memory;

    #[test]
    fn settings_roundtrip() {
        let conn = open_in_memory().unwrap();
        let s = get_settings(&conn).unwrap();
        assert!(s.archive_location.is_none());
        assert_eq!(s.theme, Theme::System);

        update_archive_location(&conn, "/nas/media/music").unwrap();
        let s = get_settings(&conn).unwrap();
        assert_eq!(s.archive_location.as_deref(), Some("/nas/media/music"));

        update_archive_location(&conn, "").unwrap();
        let s = get_settings(&conn).unwrap();
        assert!(s.archive_location.is_none());

        update_theme(&conn, Theme::Dark).unwrap();
        assert_eq!(get_settings(&conn).unwrap().theme, Theme::Dark);
        update_theme(&conn, Theme::Light).unwrap();
        assert_eq!(get_settings(&conn).unwrap().theme, Theme::Light);
        update_theme(&conn, Theme::System).unwrap();
        assert_eq!(get_settings(&conn).unwrap().theme, Theme::System);
    }

    #[test]
    fn settings_trims_whitespace() {
        let conn = open_in_memory().unwrap();
        update_archive_location(&conn, "  /nas/media  ").unwrap();
        assert_eq!(
            get_settings(&conn).unwrap().archive_location.as_deref(),
            Some("/nas/media")
        );
    }

    #[test]
    fn theme_parse_rejects_garbage() {
        assert!(Theme::parse("blue").is_err());
        assert!(Theme::parse("").is_err());
        assert!(Theme::parse("Dark").is_err()); // case-sensitive
    }

    #[test]
    fn theme_parse_accepts_all_variants() {
        assert_eq!(Theme::parse("system").unwrap(), Theme::System);
        assert_eq!(Theme::parse("light").unwrap(), Theme::Light);
        assert_eq!(Theme::parse("dark").unwrap(), Theme::Dark);
    }

    #[test]
    fn get_settings_falls_back_to_system_on_unknown_theme() {
        // Defensive: if the DB somehow holds a value the enum doesn't know
        // (e.g. a future variant on an older binary), don't fail the page render.
        // The CHECK constraint normally prevents this, so drop it for this test.
        let conn = open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE settings_tmp (id INTEGER PRIMARY KEY CHECK (id = 1), \
             archive_location TEXT, theme TEXT NOT NULL DEFAULT 'system'); \
             INSERT INTO settings_tmp (id, theme) VALUES (1, 'solarized'); \
             DROP TABLE settings; ALTER TABLE settings_tmp RENAME TO settings;",
        )
        .unwrap();
        assert_eq!(get_settings(&conn).unwrap().theme, Theme::System);
    }

    #[test]
    fn update_theme_bumps_settings_updated_at() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "UPDATE settings SET updated_at = '2000-01-01T00:00:00Z' WHERE id = 1",
            [],
        )
        .unwrap();
        update_theme(&conn, Theme::Dark).unwrap();
        let updated: String = conn
            .query_row("SELECT updated_at FROM settings WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_ne!(updated, "2000-01-01T00:00:00Z");
    }
}
