use anyhow::{Context, Result};
use rusqlite::{params, Connection};

#[derive(Debug, Clone)]
pub struct FailedJob {
    pub id: i64,
    pub concert_id: i64,
    pub name: String,
    pub failed_at: String,
    pub failure_message: String,
    pub title: String,
    pub artist: String,
}

pub fn list_for_concert(conn: &Connection, concert_id: i64) -> Result<Vec<FailedJob>> {
    let mut stmt = conn.prepare(
        "SELECT j.id, j.concert_id, j.name, j.failed_at, j.failure_message,
                COALESCE(c.title, 'Unknown'), COALESCE(c.artist, '')
         FROM jobs j
         LEFT JOIN concerts c ON j.concert_id = c.id
         WHERE j.concert_id = ?1
         ORDER BY j.failed_at ASC, j.id ASC",
    )?;
    let jobs = stmt
        .query_map(params![concert_id], |row| {
            Ok(FailedJob {
                id: row.get(0)?,
                concert_id: row.get(1)?,
                name: row.get(2)?,
                failed_at: row.get(3)?,
                failure_message: row.get(4)?,
                title: row.get(5)?,
                artist: row.get(6)?,
            })
        })
        .context("Failed to query Failed Jobs for concert")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to list Failed Jobs for concert")?;
    Ok(jobs)
}

pub fn insert_failed_job(
    conn: &Connection,
    concert_id: i64,
    name: &str,
    failure_message: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO jobs (concert_id, name, failed_at, failure_message)
         VALUES (?1, ?2, datetime('now'), ?3)",
        params![concert_id, name, failure_message],
    )
    .context("Failed to insert failed job")?;
    Ok(conn.last_insert_rowid())
}

pub fn list_failed_jobs(conn: &Connection, limit: usize) -> Result<Vec<FailedJob>> {
    let mut stmt = conn.prepare(
        "SELECT j.id, j.concert_id, j.name, j.failed_at, j.failure_message,
                COALESCE(c.title, 'Unknown'), COALESCE(c.artist, '')
         FROM jobs j
         LEFT JOIN concerts c ON j.concert_id = c.id
         ORDER BY j.failed_at DESC, j.id DESC
         LIMIT ?1",
    )?;
    let jobs = stmt
        .query_map(params![limit as i64], |row| {
            Ok(FailedJob {
                id: row.get(0)?,
                concert_id: row.get(1)?,
                name: row.get(2)?,
                failed_at: row.get(3)?,
                failure_message: row.get(4)?,
                title: row.get(5)?,
                artist: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to list failed jobs")?;
    Ok(jobs)
}

pub fn get_failed_job(conn: &Connection, id: i64) -> Result<FailedJob> {
    conn.query_row(
        "SELECT j.id, j.concert_id, j.name, j.failed_at, j.failure_message,
                COALESCE(c.title, 'Unknown'), COALESCE(c.artist, '')
         FROM jobs j
         LEFT JOIN concerts c ON j.concert_id = c.id
         WHERE j.id = ?1",
        params![id],
        |row| {
            Ok(FailedJob {
                id: row.get(0)?,
                concert_id: row.get(1)?,
                name: row.get(2)?,
                failed_at: row.get(3)?,
                failure_message: row.get(4)?,
                title: row.get(5)?,
                artist: row.get(6)?,
            })
        },
    )
    .context("Failed to get failed job")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connection::open_in_memory;
    use crate::db::tests::{seed, seed_with_album};

    #[test]
    fn insert_failed_job_returns_id() {
        let conn = open_in_memory().unwrap();
        let concert_id = seed(&conn);
        let job_id = insert_failed_job(&conn, concert_id, "download", "exit 1: boom").unwrap();
        assert!(job_id > 0);
    }

    #[test]
    fn list_failed_jobs_returns_in_descending_order() {
        let conn = open_in_memory().unwrap();
        let cid = seed(&conn);
        insert_failed_job(&conn, cid, "download", "error 1").unwrap();
        insert_failed_job(&conn, cid, "split", "error 2").unwrap();
        let jobs = list_failed_jobs(&conn, 100).unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].name, "split");
        assert_eq!(jobs[1].name, "download");
    }

    #[test]
    fn list_failed_jobs_respects_limit() {
        let conn = open_in_memory().unwrap();
        let cid = seed(&conn);
        for i in 0..5 {
            insert_failed_job(&conn, cid, "download", &format!("error {}", i)).unwrap();
        }
        let jobs = list_failed_jobs(&conn, 3).unwrap();
        assert_eq!(jobs.len(), 3);
    }

    #[test]
    fn list_failed_jobs_includes_concert_info() {
        let conn = open_in_memory().unwrap();
        let cid = seed_with_album(&conn);
        insert_failed_job(&conn, cid, "download", "boom").unwrap();
        let jobs = list_failed_jobs(&conn, 10).unwrap();
        assert_eq!(jobs[0].title, "Test Concert");
        assert_eq!(jobs[0].artist, "Test Artist");
    }

    #[test]
    fn list_failed_jobs_handles_deleted_concert() {
        let conn = open_in_memory().unwrap();
        insert_failed_job(&conn, 9999, "split", "orphaned").unwrap();
        let jobs = list_failed_jobs(&conn, 10).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].title, "Unknown");
        assert_eq!(jobs[0].artist, "");
    }

    #[test]
    fn get_failed_job_returns_matching_row() {
        let conn = open_in_memory().unwrap();
        let cid = seed(&conn);
        let job_id = insert_failed_job(&conn, cid, "download", "some error").unwrap();
        let job = get_failed_job(&conn, job_id).unwrap();
        assert_eq!(job.id, job_id);
        assert_eq!(job.concert_id, cid);
        assert_eq!(job.name, "download");
        assert_eq!(job.failure_message, "some error");
    }

    #[test]
    fn get_failed_job_returns_error_for_missing_id() {
        let conn = open_in_memory().unwrap();
        assert!(get_failed_job(&conn, 9999).is_err());
    }

    #[test]
    fn insert_failed_job_sets_timestamps() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        let job_id = insert_failed_job(&conn, id, "download", "boom").unwrap();
        let (inserted, updated): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT inserted_at, updated_at FROM jobs WHERE id = ?1",
                params![job_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(inserted.is_some(), "jobs.inserted_at should be set");
        assert!(updated.is_some(), "jobs.updated_at should be set");
    }
}
