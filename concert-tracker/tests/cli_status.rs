//! Compatibility coverage for `concert-db want` and `concert-db ignore`, the CLI
//! adapters over `Concerts::set_status`.
//!
//! The interface stores a *desired* Concert Status, so those verbs cannot flip
//! the user's choice on a retry. The CLI's own surface is deliberately still a
//! toggle — running `want` on a wanted concert clears the wanted flag, because
//! that is what those verbs have always done and what the HTMX badge's ✕ does
//! too. This file pins that translation, and the mutual exclusivity the
//! interface enforces underneath it.
//!
//! The commands run as a real process against a real database file, because the
//! defects worth catching are what a person reads on stdout, in the exit status,
//! and in the stored status afterwards.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use concert_tracker::db::{self, concerts::NewListing};
use concert_tracker::model::ConcertStatus;

fn seeded_database(dir: &Path) -> (PathBuf, PathBuf, i64) {
    let db_path = dir.join("concerts.db");
    let workdir = dir.join("work");
    std::fs::create_dir(&workdir).unwrap();

    let conn = db::connection::open(&db_path).unwrap();
    let source_url = "https://www.npr.org/2024/01/15/cli-status-compat".to_string();
    db::concerts::upsert_listing(
        &conn,
        &NewListing {
            source_url: source_url.clone(),
            title: "CLI Status Compatibility".to_string(),
            concert_date: Some("2024-01-15".to_string()),
            teaser: None,
        },
    )
    .unwrap();
    let id = db::concerts::get_concert_by_url(&conn, &source_url)
        .unwrap()
        .expect("the seeded listing is readable")
        .id;
    drop(conn);

    (db_path, workdir, id)
}

/// Run one status verb in a child process, exactly as a person would.
fn run(db_path: &Path, workdir: &Path, verb: &str, id: i64) -> Output {
    Command::new(env!("CARGO_BIN_EXE_concert-db"))
        .arg("--db")
        .arg(db_path)
        .arg("--workdir")
        .arg(workdir)
        .arg(verb)
        .arg(id.to_string())
        .output()
        .expect("concert-db runs")
}

/// Run one status verb and return what it printed on stdout.
fn toggled(db_path: &Path, workdir: &Path, verb: &str, id: i64) -> String {
    let output = run(db_path, workdir, verb, id);
    assert!(
        output.status.success(),
        "concert-db {verb} {id} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("stdout is UTF-8")
        .trim_end()
        .to_string()
}

/// The status stored for the concert, with the two stored intents kept apart so
/// a stale flag cannot hide behind the derived status.
fn stored(db_path: &Path, id: i64) -> (ConcertStatus, bool, bool) {
    let conn = db::connection::open(db_path).unwrap();
    let concert = db::concerts::get_concert(&conn, id).unwrap();
    (concert.concert_status(), concert.wanted, concert.ignored)
}

#[test]
fn want_and_ignore_translate_a_toggle_into_the_desired_status() {
    let dir = tempfile::tempdir().unwrap();
    let (db_path, workdir, id) = seeded_database(dir.path());

    assert_eq!(
        toggled(&db_path, &workdir, "want", id),
        format!("Toggled wanted for concert {id}")
    );
    assert_eq!(stored(&db_path, id), (ConcertStatus::Wanted, true, false));

    // A second invocation toggles back: the CLI's behaviour is unchanged, even
    // though the interface itself only ever stores a desired status.
    assert_eq!(
        toggled(&db_path, &workdir, "want", id),
        format!("Toggled wanted for concert {id}")
    );
    assert_eq!(
        stored(&db_path, id),
        (ConcertStatus::Available, false, false)
    );

    assert_eq!(
        toggled(&db_path, &workdir, "ignore", id),
        format!("Toggled ignored for concert {id}")
    );
    assert_eq!(stored(&db_path, id), (ConcertStatus::Ignored, false, true));
}

/// The one invariant the CLI cannot bypass: a direct swap between Wanted and
/// Ignored leaves exactly the requested flag set. Both the web badge and the CLI
/// call the same interface, so neither can produce a concert that is both.
#[test]
fn switching_between_wanted_and_ignored_leaves_one_status_stored() {
    let dir = tempfile::tempdir().unwrap();
    let (db_path, workdir, id) = seeded_database(dir.path());

    toggled(&db_path, &workdir, "want", id);
    assert_eq!(stored(&db_path, id), (ConcertStatus::Wanted, true, false));

    toggled(&db_path, &workdir, "ignore", id);
    assert_eq!(stored(&db_path, id), (ConcertStatus::Ignored, false, true));

    toggled(&db_path, &workdir, "want", id);
    assert_eq!(stored(&db_path, id), (ConcertStatus::Wanted, true, false));

    toggled(&db_path, &workdir, "ignore", id);
    assert_eq!(stored(&db_path, id), (ConcertStatus::Ignored, false, true));
}

/// A concert that does not exist is a rejection, not a crash and not a silent
/// success: the process reports it and exits non-zero, so a script built on these
/// verbs notices.
#[test]
fn a_missing_concert_fails_without_storing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let (db_path, workdir, id) = seeded_database(dir.path());
    let missing = id + 100;

    for verb in ["want", "ignore"] {
        let output = run(&db_path, &workdir, verb, missing);
        assert!(
            !output.status.success(),
            "concert-db {verb} {missing} must fail for a missing concert"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("concert {missing} not found")),
            "stderr must name the missing concert, got: {stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "a failed toggle must not print a success line"
        );
    }

    // The seeded concert is untouched by the failed attempts.
    assert_eq!(
        stored(&db_path, id),
        (ConcertStatus::Available, false, false)
    );
}
