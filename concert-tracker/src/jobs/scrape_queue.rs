//! Serial background metadata-scrape worker.
//!
//! `Sync` only upserts the archive listing (fast); the per-concert page scrape
//! (which produces `metadata_scraped_at`, `preview.jpg` and the listing
//! thumbnail) is handed to this queue and processed **one concert at a time** by
//! a single long-lived consumer task. Serializing is deliberate: it avoids
//! hammering NPR / getting IP-blocked. Listing cards for queued concerts render a
//! "loading…" placeholder and poll `/concerts/:id/status` until their thumbnail
//! is ready (see `web::handlers` + `templates/concert_card.html`).
//!
//! The `pending` set is the source of truth for "is this card still loading"; it
//! is in-memory, so a restart loses it (a re-sync re-enqueues — see the plan).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use tokio::sync::mpsc;

use crate::db;
use crate::scrape;

/// A concert queued for a background metadata scrape.
pub struct ScrapeRequest {
    pub concert_id: i64,
    pub source_url: String,
}

/// The per-item unit of work. Synchronous (it uses `reqwest::blocking` and a
/// brief DB lock) so the worker runs it inside `spawn_blocking`. Injectable so
/// tests can drive the worker without network (mirrors `JobConfig::test`).
pub type ScrapeItemFn = Arc<dyn Fn(&Arc<Mutex<Connection>>, &Path, &ScrapeRequest) + Send + Sync>;

const LOG_TARGET: &str = "concert_tracker::jobs::scrape";

/// `jobs.name` value used for background metadata-scrape failures. Shared with the
/// Jobs page filter/label so the literal does not drift across files.
pub const SCRAPE_JOB_NAME: &str = "scrape";

/// Append a failed-job row for a scrape failure so it shows up on the Jobs page.
/// Best-effort: a failed insert is logged, never propagated — it must not mask
/// the original scrape error. Caller must hold the DB lock (`conn`).
fn record_scrape_failure(conn: &Connection, concert_id: i64, err: &anyhow::Error) {
    if let Err(e) =
        db::failed_jobs::insert_failed_job(conn, concert_id, SCRAPE_JOB_NAME, &format!("{:#}", err))
    {
        tracing::warn!(
            target: LOG_TARGET,
            "failed to record scrape failure for concert {}: {}",
            concert_id, e
        );
    }
}

/// Handle to the serial scrape worker. Cloneable; all clones share the same
/// channel and `pending` set.
#[derive(Clone)]
pub struct ScrapeQueue {
    tx: mpsc::UnboundedSender<ScrapeRequest>,
    /// Concert ids that are queued or in-flight. Drives the listing "loading…"
    /// state and the per-row poll.
    pending: Arc<Mutex<HashSet<i64>>>,
}

impl ScrapeQueue {
    /// Start the worker with the production scrape item (fetch → apply → thumbnail).
    pub fn start(db: Arc<Mutex<Connection>>, working_dir: PathBuf) -> Self {
        Self::start_with(db, working_dir, Arc::new(scrape_item))
    }

    /// Start the worker with an injected item fn (tests).
    pub fn start_with(
        db: Arc<Mutex<Connection>>,
        working_dir: PathBuf,
        item_fn: ScrapeItemFn,
    ) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<ScrapeRequest>();
        let pending: Arc<Mutex<HashSet<i64>>> = Arc::new(Mutex::new(HashSet::new()));
        let pending_worker = pending.clone();

        // Single consumer task: process one item at a time. Fire-and-forget — no
        // JoinHandle is kept, and `rx.recv()` returning `None` (all senders
        // dropped) ends it cleanly. An in-flight scrape during shutdown is
        // best-effort and idempotent.
        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let id = req.concert_id;
                tracing::info!(target: LOG_TARGET, "scrape worker picked up concert {}", id);

                let db = db.clone();
                let wd = working_dir.clone();
                let f = item_fn.clone();
                // Awaiting here is what makes the queue serial: the next item is
                // not pulled until this one finishes.
                let res = tokio::task::spawn_blocking(move || f(&db, &wd, &req)).await;
                if let Err(e) = res {
                    // Item panicked (e.g. a poisoned DB lock). Log and carry on —
                    // never let it kill the consumer task, which would wedge the
                    // whole queue and leave every card stuck "loading…".
                    tracing::warn!(target: LOG_TARGET, "scrape item panicked for concert {}: {}", id, e);
                }

                // Unconditional: success, item-error, or panic. This is the
                // load-bearing invariant for "the card stops polling".
                pending_worker.lock().unwrap().remove(&id);
            }
            tracing::debug!(target: LOG_TARGET, "scrape worker channel closed; exiting");
        });

        ScrapeQueue { tx, pending }
    }

    /// Queue `concert_id` for a background scrape. Returns `false` (a normal
    /// no-op) if it was already queued/in-flight, or if the worker is gone.
    /// Dedupe is a single critical section: check-and-insert under one lock.
    pub fn enqueue(&self, concert_id: i64, source_url: String) -> bool {
        {
            let mut set = self.pending.lock().unwrap();
            if !set.insert(concert_id) {
                return false; // already queued or in-flight
            }
        }
        if self
            .tx
            .send(ScrapeRequest {
                concert_id,
                source_url,
            })
            .is_err()
        {
            // Worker gone: don't leave a phantom pending id that polls forever.
            self.pending.lock().unwrap().remove(&concert_id);
            tracing::warn!(target: LOG_TARGET, "scrape queue closed; dropped concert {}", concert_id);
            return false;
        }
        tracing::info!(target: LOG_TARGET, "enqueued concert {} for background scrape", concert_id);
        true
    }

    /// Whether `concert_id` is currently queued or being scraped. Used by the
    /// renderer to show the "loading…" placeholder and keep the row polling.
    pub fn is_pending(&self, concert_id: i64) -> bool {
        self.pending.lock().unwrap().contains(&concert_id)
    }
}

/// Production per-item work. Best-effort: every failure is logged and returns;
/// it must never panic (panics are caught by the worker, but a clean return
/// keeps logs readable). Runs inside `spawn_blocking`.
fn scrape_item(db: &Arc<Mutex<Connection>>, working_dir: &Path, req: &ScrapeRequest) {
    // Already-scraped guard: the enqueue-time snapshot can be stale — a
    // detail-view auto-scrape, a pre-download scrape, or an overlapping re-sync
    // may have scraped this concert before the worker reached it. One cheap
    // indexed read here avoids a redundant NPR fetch.
    {
        let conn = db.lock().unwrap();
        match db::concerts::get_concert(&conn, req.concert_id) {
            Ok(c) if c.metadata_scraped_at.is_some() => {
                tracing::info!(target: LOG_TARGET, "concert {} already scraped; skipping", req.concert_id);
                return;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(target: LOG_TARGET, "scrape skip-check failed for concert {}: {}", req.concert_id, e);
                record_scrape_failure(&conn, req.concert_id, &e);
                return;
            }
        }
    }

    // Network/disk, no lock.
    let info = match scrape::fetch_concert_info(&req.source_url) {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!(
                target: LOG_TARGET,
                "background scrape fetch failed for concert {} ({}): {}",
                req.concert_id, req.source_url, e
            );
            // Re-acquire the lock just to record the failure (none held here).
            record_scrape_failure(&db.lock().unwrap(), req.concert_id, &e);
            return;
        }
    };

    // Brief DB lock for the metadata write only.
    {
        let conn = db.lock().unwrap();
        if let Err(e) = scrape::apply_concert_info(&conn, &info) {
            tracing::warn!(target: LOG_TARGET, "background scrape apply failed for concert {}: {}", req.concert_id, e);
            record_scrape_failure(&conn, req.concert_id, &e);
            return;
        }
    }

    // Network/disk, no lock. Best-effort (logs internally).
    scrape::ensure_and_log_thumbnail(working_dir, &info.album, info.preview_image_url.as_deref());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc as std_mpsc;
    use std::time::Duration;

    /// Cadence for the async poll helpers below.
    const POLL_INTERVAL: Duration = Duration::from_millis(10);
    /// Budget for `wait_until` (a cheap in-memory predicate): ~2s.
    const COND_MAX_POLLS: usize = 200;
    /// Budget for `recv_soon` (awaiting the worker's result after it has run an
    /// item): larger than `COND_MAX_POLLS` because it waits on real worker
    /// progress, not just a flag flip. ~5s.
    const RECV_MAX_POLLS: usize = 500;

    fn dummy_db() -> Arc<Mutex<Connection>> {
        Arc::new(Mutex::new(db::connection::open_in_memory().unwrap()))
    }

    /// Poll `cond` until true, failing the test if it never becomes true.
    async fn wait_until<F: Fn() -> bool>(cond: F) {
        for _ in 0..COND_MAX_POLLS {
            if cond() {
                return;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        panic!("condition not met in time");
    }

    /// Await a value from the worker's sync result channel without parking a
    /// runtime worker thread.
    ///
    /// `Receiver::recv_timeout` is a synchronous blocking call: from an async
    /// test it parks one of the runtime's worker threads for the whole wait. The
    /// worker task that must deliver the value needs those threads to make
    /// progress, so under load it can be starved past the timeout — a flaky
    /// `recv_timeout(...).unwrap()` failure. Polling `try_recv` between async
    /// `sleep`s frees the worker thread each tick so the scheduler can keep the
    /// worker running while we wait.
    async fn recv_soon<T>(rx: &std_mpsc::Receiver<T>) -> T {
        for _ in 0..RECV_MAX_POLLS {
            match rx.try_recv() {
                Ok(v) => return v,
                Err(std_mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    panic!("worker dropped the result sender before sending a value")
                }
            }
        }
        panic!("no value received from the scrape worker in time");
    }

    // Multi-thread runtime so the spawned worker keeps making progress while the
    // test awaits its result via `recv_soon`; the dedupe item also parks a
    // spawn_blocking thread until the test releases it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn enqueue_dedupes_and_clears_pending_after_completion() {
        // The item blocks until the test releases it, so we can deterministically
        // observe the in-flight pending state without sleep-based timing.
        let (release_tx, release_rx) = std_mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let (done_tx, done_rx) = std_mpsc::channel::<()>();

        let item: ScrapeItemFn = Arc::new(move |_db, _wd, _req| {
            let _ = release_rx.lock().unwrap().recv(); // block until released
            let _ = done_tx.send(());
        });

        let q = ScrapeQueue::start_with(dummy_db(), PathBuf::from("/tmp"), item);

        // Enqueue marks pending synchronously.
        assert!(q.enqueue(1, "https://example.org/1".into()));
        assert!(q.is_pending(1));
        // Dedupe: a second enqueue of the same id is a no-op and does not send.
        assert!(!q.enqueue(1, "https://example.org/1".into()));
        assert!(q.is_pending(1));

        // Release the in-flight item and wait for it to finish.
        release_tx.send(()).unwrap();
        recv_soon(&done_rx).await;

        // The worker removes the id from pending once the item returns.
        wait_until(|| !q.is_pending(1)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_survives_item_panic_and_clears_pending() {
        let (done_tx, done_rx) = std_mpsc::channel::<i64>();
        let item: ScrapeItemFn = Arc::new(move |_db, _wd, req| {
            if req.concert_id == 1 {
                panic!("boom");
            }
            let _ = done_tx.send(req.concert_id);
        });

        let q = ScrapeQueue::start_with(dummy_db(), PathBuf::from("/tmp"), item);
        assert!(q.enqueue(1, "u".into())); // panics in the worker
        assert!(q.enqueue(2, "u".into())); // must still be processed

        // The second item ran despite the first panicking → worker survived.
        let got = recv_soon(&done_rx).await;
        assert_eq!(got, 2);

        // Pending cleared for both (panicked one included).
        wait_until(|| !q.is_pending(1) && !q.is_pending(2)).await;
    }

    #[test]
    fn record_scrape_failure_appends_a_row_per_call() {
        let db = dummy_db();
        let conn = db.lock().unwrap();
        record_scrape_failure(&conn, 42, &anyhow::anyhow!("Failed to write JSON file foo"));
        record_scrape_failure(&conn, 42, &anyhow::anyhow!("second failure"));

        let failed = db::failed_jobs::list_failed_jobs(&conn, 100).unwrap();
        assert_eq!(failed.len(), 2, "each failure appends its own row");
        assert!(failed.iter().all(|j| j.name == SCRAPE_JOB_NAME));
        assert!(failed
            .iter()
            .any(|j| j.failure_message.contains("Failed to write JSON file foo")));
    }

    #[test]
    fn scrape_item_skips_already_scraped_without_recording_failure() {
        let db = dummy_db();
        let url = "https://npr.org/c/already-scraped";
        let id = {
            let conn = db.lock().unwrap();
            db::concerts::upsert_listing(
                &conn,
                &db::concerts::NewListing {
                    source_url: url.to_string(),
                    title: "X".to_string(),
                    concert_date: Some("2026-05-01".to_string()),
                    teaser: None,
                },
            )
            .unwrap();
            let c = db::concerts::get_concert_by_url(&conn, url)
                .unwrap()
                .unwrap();
            db::concerts::update_metadata(
                &conn,
                c.id,
                &db::concerts::MetadataUpdate {
                    artist: "Artist".to_string(),
                    album: "Album".to_string(),
                    description: None,
                    set_list: vec![],
                    musicians: vec![],
                },
            )
            .unwrap();
            c.id
        };

        // The already-scraped guard returns before any network/disk work, so this
        // is deterministic and offline. It must NOT record a failure.
        let req = ScrapeRequest {
            concert_id: id,
            source_url: url.to_string(),
        };
        scrape_item(&db, Path::new("/tmp"), &req);

        let conn = db.lock().unwrap();
        assert!(db::failed_jobs::list_failed_jobs(&conn, 100)
            .unwrap()
            .is_empty());
    }
}
