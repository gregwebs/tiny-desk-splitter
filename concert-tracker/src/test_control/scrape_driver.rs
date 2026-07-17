//! Test Control Scrape Driver — deterministic timing control for the
//! background metadata-scrape queue ([`crate::jobs::scrape_queue`]).
//!
//! The scrape queue is not a download/split/open job: it has its own
//! in-memory `pending` set and an injectable [`ScrapeItemFn`], not a
//! [`crate::jobs::JobRunner`]. This driver plugs into that seam the same way
//! [`crate::test_control::job_driver::TestControlJobRunner`] plugs into
//! `JobRunner` — Hurl scenarios configure a concert's scrape to block, then
//! release it with deterministic metadata + thumbnail writes, instead of
//! driving a real network fetch. See
//! docs/change/2026-07-17-scrape-driver-hurl-migration.md.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use rusqlite::Connection;
use serde::Deserialize;

use crate::jobs::scrape_queue::{ScrapeItemFn, ScrapeRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrapeOutcome {
    Succeed,
    Block,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScrapeObservation {
    pub started: u32,
    pub completed: u32,
    pub blocked: u32,
    pub released: u32,
}

/// Holds the release channel for a scrape currently parked in
/// [`ScrapeDriver::run_item`]'s `Block` branch. `run_item` runs
/// synchronously inside `spawn_blocking` (see [`ScrapeItemFn`]), so this is a
/// `std::sync::mpsc` channel, not a `tokio::sync::oneshot` one.
struct BlockedScrape {
    tx: std::sync::mpsc::Sender<()>,
}

/// Plans and blocked senders behind one lock (see [`ScrapeDriver::state`]'s
/// docs for why they must not be two independent mutexes).
#[derive(Default)]
struct DriverState {
    /// Per-concert plan. Absent = [`ScrapeOutcome::Succeed`] — unlike the Job
    /// Driver there is no process-wide default plan: nothing in this slice
    /// needs one, and omitting it avoids one more piece of state that could
    /// leak between Hurl files sharing a process (see hurl/README.md).
    plans: HashMap<i64, ScrapeOutcome>,
    blocked: HashMap<i64, BlockedScrape>,
}

/// Test Control's scrape-timing configuration and observations. Shared (via
/// `Arc`) between the injected [`ScrapeItemFn`] built by [`scrape_item_fn`]
/// and the Test Control RPC methods that configure/inspect it.
#[derive(Default)]
pub struct ScrapeDriver {
    /// Plans and blocked senders share one lock so that "read the plan, then
    /// (if blocked) register a release sender" is one atomic step relative
    /// to [`Self::reset`] — see `run_item` and `reset`'s docs for the race
    /// this closes (adversarial review finding: with two independent locks,
    /// a `reset` landing in the gap between those two reads would clear a
    /// blocked map that does not yet contain the entry `run_item` is about
    /// to insert, permanently stranding that scrape).
    state: Mutex<DriverState>,
    observations: Mutex<HashMap<i64, ScrapeObservation>>,
}

impl ScrapeDriver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `concert_id`'s plan. There is no "unset" — a concert's plan simply
    /// stays `Succeed` until this is called for it.
    pub fn set_plan(&self, concert_id: i64, outcome: ScrapeOutcome) {
        self.state.lock().unwrap().plans.insert(concert_id, outcome);
    }

    /// Test-only: `run_item` inlines this same lookup as part of its atomic
    /// "read plan, then register if blocked" step (see `Self::state`'s
    /// docs), so this standalone accessor exists only for tests that check
    /// plan resolution without exercising a full `run_item` call.
    #[cfg(test)]
    fn resolve_plan(&self, concert_id: i64) -> ScrapeOutcome {
        self.state
            .lock()
            .unwrap()
            .plans
            .get(&concert_id)
            .copied()
            .unwrap_or(ScrapeOutcome::Succeed)
    }

    fn bump(&self, concert_id: i64, f: impl FnOnce(&mut ScrapeObservation)) {
        let mut obs = self.observations.lock().unwrap();
        f(obs.entry(concert_id).or_default());
    }

    pub fn observation(&self, concert_id: i64) -> ScrapeObservation {
        self.observations
            .lock()
            .unwrap()
            .get(&concert_id)
            .copied()
            .unwrap_or_default()
    }

    /// Release a scrape currently blocked for `concert_id`. Errors if none is
    /// blocked there — Hurl scenarios must poll
    /// `test.assert_scrape_observation` for `blocked=1` before releasing
    /// rather than racing the item's registration (same protocol as the Job
    /// Driver; see docs/change/2026-07-15-job-driver-plan.md's "Blocked-step
    /// release protocol").
    pub fn release(&self, concert_id: i64) -> anyhow::Result<()> {
        let entry = self.state.lock().unwrap().blocked.remove(&concert_id);
        match entry {
            Some(BlockedScrape { tx }) => {
                // The receiver side only errors if `run_item` already exited
                // some other way; nothing further to do here in that case.
                let _ = tx.send(());
                Ok(())
            }
            None => anyhow::bail!(
                "no blocked scrape for concert {concert_id}; poll \
                 test.assert_scrape_observation for blocked=1 before releasing"
            ),
        }
    }

    /// Clear plans, blocked senders, and observations. Does **not** silently
    /// strand a blocked scrape: dropping its sender wakes the parked
    /// `run_item` call, which then returns without writing anything (its
    /// `Err` branch below) — the scrape queue's own unconditional
    /// `pending.remove` still clears the loading card shortly after.
    /// Clearing `plans`/`blocked` under the same lock `run_item` uses to
    /// register a new blocked sender (see [`Self::state`]) means this can
    /// never race a fresh block into permanent limbo: either `reset` runs
    /// first (the block that follows starts against fresh empty state) or
    /// the block's sender is registered first (`reset` then drops it,
    /// resolving it immediately) — there is no interleaving that drops an
    /// entry `reset` never got a chance to see.
    ///
    /// This is best-effort, not a quiescence boundary for the queue itself:
    /// a request still in the channel (queued but not yet picked up) is
    /// unaffected by `reset` and still runs afterward, and a just-released
    /// item's `pending.remove` can land after `reset` returns. That mirrors
    /// the scrape/job caveat already documented on
    /// `test_control::reset_test_data`, and the Hurl suite never calls
    /// `/test/reset` mid-run (see hurl/README.md).
    pub fn reset(&self) {
        {
            let mut state = self.state.lock().unwrap();
            state.plans.clear();
            state.blocked.clear();
        }
        self.observations.lock().unwrap().clear();
    }

    /// The [`ScrapeItemFn`] body. Synchronous — the scrape queue worker
    /// already runs this inside `spawn_blocking`, so parking on
    /// `std::sync::mpsc::Receiver::recv` here is safe (mirrors
    /// `scrape_queue`'s own unit tests, which use the same pattern).
    fn run_item(&self, db: &Arc<Mutex<Connection>>, working_dir: &Path, req: &ScrapeRequest) {
        let concert_id = req.concert_id;
        self.bump(concert_id, |o| o.started += 1);

        // Read the plan and (if blocked) register the release sender in one
        // locked step — see `Self::state`'s docs for why this must not be
        // two separate lock acquisitions.
        let rx = {
            let mut state = self.state.lock().unwrap();
            let plan = state
                .plans
                .get(&concert_id)
                .copied()
                .unwrap_or(ScrapeOutcome::Succeed);
            if plan == ScrapeOutcome::Block {
                let (tx, rx) = std::sync::mpsc::channel();
                state.blocked.insert(concert_id, BlockedScrape { tx });
                Some(rx)
            } else {
                None
            }
        };

        if let Some(rx) = rx {
            // Bumping `blocked` outside the lock is fine: a Hurl scenario
            // polling `assert_scrape_observation` for `blocked=1` only needs
            // the map entry to already exist by the time it calls
            // `scrape_release`, which `state.blocked.insert` above already
            // guarantees regardless of exactly when this counter update is
            // observed. A `reset` racing this exact window (registered, not
            // yet bumped) can leave a stale `blocked=1` in `observations`
            // even though nothing is actually blocked anymore — cosmetic
            // only (no hang, no wrong write), and the same
            // not-a-quiescence-boundary territory `reset`'s docs already
            // cover; not worth more locking to close given the Hurl suite
            // never resets mid-run.
            self.bump(concert_id, |o| o.blocked += 1);
            match rx.recv() {
                Ok(()) => self.bump(concert_id, |o| o.released += 1),
                Err(_recv_error) => {
                    // The sender was dropped without a release (test.reset
                    // ran while this scrape was blocked) — return without
                    // writing anything rather than hang or panic.
                    return;
                }
            }
        }

        match write_scraped_fixture(db, working_dir, concert_id) {
            Ok(()) => self.bump(concert_id, |o| o.completed += 1),
            Err(e) => {
                // Best-effort, like the production `scrape_item`: a fixture
                // write failure must not panic the worker thread and wedge
                // the whole queue.
                tracing::warn!(
                    "test-control scrape fixture write failed for concert {}: {:#}",
                    concert_id,
                    e
                );
            }
        }
    }
}

/// Build a [`ScrapeItemFn`] backed by `driver`, for
/// [`crate::jobs::scrape_queue::ScrapeQueue::start_with`].
pub fn scrape_item_fn(driver: Arc<ScrapeDriver>) -> ScrapeItemFn {
    Arc::new(move |db, working_dir, req| driver.run_item(db, working_dir, req))
}

/// Deterministic artist/album/thumbnail written on a successful (or
/// released-as-succeed) scrape. `update_metadata` sets `metadata_scraped_at`,
/// which is what flips the card from "loading…" to a thumbnail — see
/// `Concert::thumbnail_url_from_db`.
///
/// Writes the thumbnail *before* committing metadata, deliberately the
/// opposite order from how the fields read above: if the thumbnail write
/// fails (e.g. an unwritable workdir), `metadata_scraped_at` must stay unset
/// so a later scrape attempt can retry — matching this repo's own retry
/// contract for a real failed scrape (see
/// `detail_page_auto_scrape_failure_still_renders` in
/// concert-tracker/tests/web_integration.rs). Writing metadata first would
/// have let a filesystem failure permanently mark the concert "scraped" with
/// no thumbnail ever created (adversarial review finding).
fn write_scraped_fixture(
    db: &Arc<Mutex<Connection>>,
    working_dir: &Path,
    concert_id: i64,
) -> anyhow::Result<()> {
    let album = format!("Scrape Driver Album {concert_id}");
    write_thumbnail_jpeg(working_dir, &album)?;
    let conn = db.lock().unwrap();
    crate::db::concerts::update_metadata(
        &conn,
        concert_id,
        &crate::db::concerts::MetadataUpdate {
            artist: format!("Scrape Driver Artist {concert_id}"),
            album: album.clone(),
            description: None,
            set_list: vec![],
            musicians: vec![],
        },
    )?;
    Ok(())
}

/// Write a tiny but *valid* JPEG (deliberately not
/// [`crate::db::seeds::SENTINEL_BYTES`]) to the thumbnail path. The card's
/// `<img onerror="this.style.display='none'">` (templates/concert_card.html)
/// would silently hide a non-image file, so a text sentinel could pass a
/// `200` Hurl assertion on `GET /thumbnails/...` while the browser shows
/// nothing (adversarial review finding).
fn write_thumbnail_jpeg(working_dir: &Path, album: &str) -> anyhow::Result<()> {
    let dest = crate::scrape::thumbnail_path(working_dir, album);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).context("create thumbnails directory")?;
    }
    let img = image::RgbImage::from_pixel(1, 1, image::Rgb([200, 100, 50]));
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 80)
        .encode_image(&img)
        .context("encode test-control thumbnail JPEG")?;
    std::fs::write(&dest, buf).with_context(|| format!("write thumbnail to {}", dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::concerts::{get_concert, upsert_listing, NewListing};
    use std::time::Duration;

    fn dummy_db() -> Arc<Mutex<Connection>> {
        Arc::new(Mutex::new(crate::db::connection::open_in_memory().unwrap()))
    }

    /// Seed a bare listing row (no metadata yet) so `write_scraped_fixture`
    /// has a row to update, and return its id.
    fn seeded_concert(db: &Arc<Mutex<Connection>>, url: &str) -> i64 {
        let conn = db.lock().unwrap();
        upsert_listing(
            &conn,
            &NewListing {
                source_url: url.to_string(),
                title: "Seed".to_string(),
                concert_date: Some("2026-05-01".to_string()),
                teaser: None,
            },
        )
        .unwrap();
        crate::db::concerts::get_concert_by_url(&conn, url)
            .unwrap()
            .unwrap()
            .id
    }

    fn request(concert_id: i64) -> ScrapeRequest {
        ScrapeRequest {
            concert_id,
            source_url: "https://npr.org/c/scrape-driver-test".to_string(),
        }
    }

    /// Poll `cond` until true, failing the test if it never becomes true.
    fn wait_until<F: Fn() -> bool>(cond: F) {
        for _ in 0..200 {
            if cond() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("condition not met in time");
    }

    #[test]
    fn default_plan_writes_metadata_and_a_decodable_thumbnail() {
        let tmp = tempfile::tempdir().unwrap();
        let db = dummy_db();
        let id = seeded_concert(&db, "https://npr.org/c/default-plan");
        let driver = ScrapeDriver::new();

        driver.run_item(&db, tmp.path(), &request(id));

        let concert = {
            let conn = db.lock().unwrap();
            get_concert(&conn, id).unwrap()
        };
        assert!(concert.metadata_scraped_at.is_some());
        assert_eq!(concert.album.as_deref(), Some("Scrape Driver Album 1"));

        let thumb = crate::scrape::thumbnail_path(tmp.path(), "Scrape Driver Album 1");
        let bytes = std::fs::read(&thumb).expect("thumbnail file written");
        let img = image::load_from_memory(&bytes).expect("thumbnail is a decodable image");
        assert_eq!((img.width(), img.height()), (1, 1));

        let obs = driver.observation(id);
        assert_eq!(obs.started, 1);
        assert_eq!(obs.completed, 1);
        assert_eq!(obs.blocked, 0);
    }

    #[test]
    fn block_then_release_completes_and_bumps_observations() {
        let tmp = tempfile::tempdir().unwrap();
        let db = dummy_db();
        let id = seeded_concert(&db, "https://npr.org/c/block-release");
        let driver = Arc::new(ScrapeDriver::new());
        driver.set_plan(id, ScrapeOutcome::Block);

        let driver_for_thread = driver.clone();
        let db_for_thread = db.clone();
        let tmp_path = tmp.path().to_path_buf();
        let handle = std::thread::spawn(move || {
            driver_for_thread.run_item(&db_for_thread, &tmp_path, &request(id))
        });

        wait_until(|| driver.observation(id).blocked == 1);
        driver.release(id).unwrap();
        handle.join().unwrap();

        let obs = driver.observation(id);
        assert_eq!(obs.released, 1);
        assert_eq!(obs.completed, 1);
        let concert = {
            let conn = db.lock().unwrap();
            get_concert(&conn, id).unwrap()
        };
        assert!(concert.metadata_scraped_at.is_some());
    }

    #[test]
    fn release_without_a_blocked_scrape_errors() {
        let driver = ScrapeDriver::new();
        let err = driver.release(1).unwrap_err();
        assert!(err.to_string().contains("no blocked scrape"));
    }

    #[test]
    fn reset_unblocks_a_parked_scrape_without_writing_fixtures() {
        let tmp = tempfile::tempdir().unwrap();
        let db = dummy_db();
        let id = seeded_concert(&db, "https://npr.org/c/reset-while-blocked");
        let driver = Arc::new(ScrapeDriver::new());
        driver.set_plan(id, ScrapeOutcome::Block);

        let driver_for_thread = driver.clone();
        let db_for_thread = db.clone();
        let tmp_path = tmp.path().to_path_buf();
        let handle = std::thread::spawn(move || {
            driver_for_thread.run_item(&db_for_thread, &tmp_path, &request(id))
        });

        wait_until(|| driver.observation(id).blocked == 1);
        driver.reset();
        handle
            .join()
            .expect("reset must unblock the parked scrape promptly, not hang");

        // reset() clears observations too, so check the state it leaves
        // behind rather than re-reading the (now-cleared) observation.
        let concert = {
            let conn = db.lock().unwrap();
            get_concert(&conn, id).unwrap()
        };
        assert!(
            concert.metadata_scraped_at.is_none(),
            "a reset-cancelled block must not write scrape fixtures"
        );
        let thumb = crate::scrape::thumbnail_path(tmp.path(), "Scrape Driver Album 1");
        assert!(!thumb.exists());
    }

    #[test]
    fn set_plan_is_per_concert() {
        let driver = ScrapeDriver::new();
        driver.set_plan(1, ScrapeOutcome::Block);
        assert_eq!(driver.resolve_plan(1), ScrapeOutcome::Block);
        assert_eq!(
            driver.resolve_plan(2),
            ScrapeOutcome::Succeed,
            "a plan set for one concert must not affect another"
        );
    }

    /// Regression test for an adversarial review finding: when plans and
    /// blocked senders lived behind two independent locks, a `reset()`
    /// landing between "read the plan as `Block`" and "insert the release
    /// sender" would clear a blocked map that did not yet contain the entry
    /// `run_item` was about to insert — permanently stranding that scrape
    /// (its `pending` id would never clear, and the card would poll
    /// forever). `ScrapeDriver::state` now holds both under one lock so that
    /// window cannot exist; race many overlapping block+reset pairs with no
    /// synchronization between them and assert every one finishes within a
    /// timeout, covering interleavings a single deterministic test can't
    /// reliably target.
    #[test]
    fn reset_racing_a_fresh_block_never_permanently_strands_it() {
        for i in 0..50 {
            let tmp = tempfile::tempdir().unwrap();
            let db = dummy_db();
            let id = seeded_concert(&db, &format!("https://npr.org/c/race-{i}"));
            let driver = Arc::new(ScrapeDriver::new());
            driver.set_plan(id, ScrapeOutcome::Block);

            let driver_for_thread = driver.clone();
            let db_for_thread = db.clone();
            let tmp_path = tmp.path().to_path_buf();
            let handle = std::thread::spawn(move || {
                driver_for_thread.run_item(&db_for_thread, &tmp_path, &request(id))
            });

            // Deliberately no synchronization with the spawned thread here —
            // the fix must make every interleaving safe, not just the
            // "reset after registration" one `reset_unblocks_a_parked_scrape_
            // without_writing_fixtures` exercises deterministically above.
            driver.reset();

            let start = std::time::Instant::now();
            while !handle.is_finished() {
                assert!(
                    start.elapsed() < Duration::from_secs(2),
                    "run_item permanently hung on iteration {i}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            handle.join().unwrap();
        }
    }

    /// Regression test for an adversarial review finding: writing metadata
    /// before the thumbnail meant a thumbnail-write failure (e.g. an
    /// unwritable workdir) would still leave `metadata_scraped_at` set,
    /// permanently marking the concert "scraped" with no thumbnail ever
    /// created. `write_scraped_fixture` now writes the thumbnail first.
    #[cfg(unix)]
    #[test]
    fn thumbnail_write_failure_leaves_metadata_unset_so_a_later_scrape_can_retry() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let db = dummy_db();
        let id = seeded_concert(&db, "https://npr.org/c/thumbnail-write-fail");
        let driver = ScrapeDriver::new();

        // Pre-create the thumbnails directory with no permissions so
        // std::fs::write into it fails deterministically, instead of
        // relying on a real disk-full condition.
        let thumbnails_dir = tmp.path().join("thumbnails");
        std::fs::create_dir_all(&thumbnails_dir).unwrap();
        std::fs::set_permissions(&thumbnails_dir, std::fs::Permissions::from_mode(0o000)).unwrap();

        driver.run_item(&db, tmp.path(), &request(id));

        // Restore permissions so the tempdir's own Drop cleanup can succeed.
        std::fs::set_permissions(&thumbnails_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let concert = {
            let conn = db.lock().unwrap();
            get_concert(&conn, id).unwrap()
        };
        assert!(
            concert.metadata_scraped_at.is_none(),
            "a thumbnail write failure must not mark the concert scraped — a later \
             scrape attempt needs metadata_scraped_at to still be None to retry"
        );
        assert_eq!(driver.observation(id).completed, 0);
    }
}
