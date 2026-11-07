//! Compatibility coverage for `concert-db list`, the CLI adapter over
//! `ConcertCatalog`.
//!
//! The CLI browses through the same catalog as the web listing, so the filter
//! meanings themselves are asserted once against the catalog
//! (`concerts::catalog::tests`) and the web listing's markup is pinned by
//! `hurl/listing_status.hurl`. What is left for this file is what neither of
//! those can state: the CLI's own command-line vocabulary, the line it prints
//! per concert, and the order it prints those lines in.
//!
//! Two meanings here belong to the CLI alone and differ from the web listing:
//!
//! - `all` — the default, and what every filter name the CLI does not know
//!   falls back to — lists every concert, Ignored ones included, while the web
//!   listing's default deliberately hides what the user ignored.
//! - The CLI has only ever filtered on Wanted, Available, and Ignored.
//!
//! The binary runs as a real process against a real database file, because the
//! defect worth catching is a change in what a person reads on stdout.

use std::path::{Path, PathBuf};
use std::process::Command;

use concert_tracker::db::{self, concerts::NewListing};
use concert_tracker::model::ConcertStatus;
use rusqlite::Connection;

const AVAILABLE: &str = "Catalog Compatibility Available";
const WANTED: &str = "Catalog Compatibility Wanted";
const IGNORED: &str = "Catalog Compatibility Ignored";
const DOWNLOADED: &str = "Catalog Compatibility Downloaded";
const SPLIT: &str = "Catalog Compatibility Split";

/// The seeded collection: one concert per status and representative lifecycle
/// the CLI's line reports, with the id each one was given. Concert dates
/// descend in the order the fields are listed, so the expected listing order is
/// the field order — newest concert date first, as the CLI has always ordered.
struct Fixtures {
    available: i64,
    wanted: i64,
    ignored: i64,
    downloaded: i64,
    split: i64,
}

/// A database file holding the seeded collection, plus the media working
/// directory the CLI is pointed at. Seeded through the persistence API rather
/// than the Test Control seeds, so this suite runs under the default features.
fn seeded_database(dir: &Path) -> (PathBuf, PathBuf, Fixtures) {
    let db_path = dir.join("concerts.db");
    let workdir = dir.join("work");
    std::fs::create_dir(&workdir).unwrap();

    let conn = db::connection::open(&db_path).unwrap();
    let fixtures = Fixtures {
        available: seed_listing(&conn, AVAILABLE, "2024-01-05"),
        wanted: seed_listing(&conn, WANTED, "2024-01-04"),
        ignored: seed_listing(&conn, IGNORED, "2024-01-03"),
        downloaded: seed_listing(&conn, DOWNLOADED, "2024-01-02"),
        split: seed_listing(&conn, SPLIT, "2024-01-01"),
    };

    db::concerts::set_concert_status(&conn, fixtures.wanted, ConcertStatus::Wanted).unwrap();
    db::concerts::set_concert_status(&conn, fixtures.ignored, ConcertStatus::Ignored).unwrap();

    for downloaded in [fixtures.downloaded, fixtures.split] {
        db::lifecycle::try_mark_download_started(&conn, downloaded).unwrap();
        db::lifecycle::mark_download_succeeded(&conn, downloaded, "mp4").unwrap();
    }
    db::lifecycle::try_mark_split_started(&conn, fixtures.split).unwrap();
    db::lifecycle::mark_split_succeeded(&conn, fixtures.split).unwrap();

    drop(conn);

    (db_path, workdir, fixtures)
}

fn seed_listing(conn: &Connection, title: &str, concert_date: &str) -> i64 {
    let source_url = format!(
        "https://www.npr.org/2024/01/15/catalog-compat-{}",
        title.replace(' ', "-").to_lowercase()
    );
    db::concerts::upsert_listing(
        conn,
        &NewListing {
            source_url: source_url.clone(),
            title: title.to_string(),
            concert_date: Some(concert_date.to_string()),
            teaser: None,
        },
    )
    .unwrap();
    db::concerts::get_concert_by_url(conn, &source_url)
        .unwrap()
        .expect("seeded listing is readable")
        .id
}

/// Run `concert-db list` in a child process and return its stdout lines, in the
/// order it printed them.
fn list(db_path: &Path, workdir: &Path, filter: Option<&str>) -> Vec<String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_concert-db"));
    command
        .arg("--db")
        .arg(db_path)
        .arg("--workdir")
        .arg(workdir)
        .arg("list");
    if let Some(filter) = filter {
        command.arg("--filter").arg(filter);
    }
    let output = command.output().expect("concert-db list runs");
    assert!(
        output.status.success(),
        "concert-db list --filter {filter:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("stdout is UTF-8")
        .lines()
        .map(str::to_string)
        .collect()
}

/// One listed line in the format the CLI has printed since before the catalog
/// existed: `[<id>] <title> | <status> | <download>+<split>`.
fn line(id: i64, title: &str, status: &str, media: &str) -> String {
    format!("[{id}] {title} | {status} | {media}")
}

fn available_line(id: i64, title: &str) -> String {
    line(id, title, "available", "not-downloaded+not-split")
}

#[test]
fn list_without_a_filter_lists_everything_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let (db_path, workdir, fixtures) = seeded_database(dir.path());

    // The CLI's `all` has always meant "list everything, newest concert first".
    // Unlike the web listing, it does not hide Ignored concerts.
    let expected = vec![
        available_line(fixtures.available, AVAILABLE),
        line(
            fixtures.wanted,
            WANTED,
            "wanted",
            "not-downloaded+not-split",
        ),
        line(
            fixtures.ignored,
            IGNORED,
            "ignored",
            "not-downloaded+not-split",
        ),
        line(
            fixtures.downloaded,
            DOWNLOADED,
            "available",
            "downloaded+not-split",
        ),
        line(fixtures.split, SPLIT, "available", "downloaded+split"),
    ];
    assert_eq!(list(&db_path, &workdir, None), expected);
    assert_eq!(list(&db_path, &workdir, Some("all")), expected);
}

#[test]
fn list_keeps_the_wanted_available_and_ignored_filter_meanings() {
    let dir = tempfile::tempdir().unwrap();
    let (db_path, workdir, fixtures) = seeded_database(dir.path());

    assert_eq!(
        list(&db_path, &workdir, Some("wanted")),
        vec![line(
            fixtures.wanted,
            WANTED,
            "wanted",
            "not-downloaded+not-split"
        )]
    );
    // Available is the absence of both Wanted and Ignored: a downloaded or
    // split concert is still Available until the user says otherwise.
    assert_eq!(
        list(&db_path, &workdir, Some("available")),
        vec![
            available_line(fixtures.available, AVAILABLE),
            line(
                fixtures.downloaded,
                DOWNLOADED,
                "available",
                "downloaded+not-split"
            ),
            line(fixtures.split, SPLIT, "available", "downloaded+split"),
        ]
    );
    assert_eq!(
        list(&db_path, &workdir, Some("ignored")),
        vec![line(
            fixtures.ignored,
            IGNORED,
            "ignored",
            "not-downloaded+not-split"
        )]
    );
}

#[test]
fn list_treats_a_filter_it_does_not_know_as_everything() {
    let dir = tempfile::tempdir().unwrap();
    let (db_path, workdir, _fixtures) = seeded_database(dir.path());

    // `downloaded`, `split`, and `archived` are web-listing filters the CLI has
    // never exposed, and no other name is known to it either. Each has always
    // fallen through to listing everything, so adding a filter to the CLI is a
    // deliberate, visible change rather than a silent one.
    let everything = list(&db_path, &workdir, None);
    assert_eq!(everything.len(), 5, "the fixture holds five concerts");
    for unknown in ["downloaded", "split", "archived", "bogus"] {
        assert_eq!(
            list(&db_path, &workdir, Some(unknown)),
            everything,
            "--filter {unknown} must list everything, as it always has"
        );
    }
}
