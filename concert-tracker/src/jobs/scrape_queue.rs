//! Serial background metadata-scrape worker.
//!
//! `Sync` only upserts the archive listing (fast); the per-concert page scrape
//! (which produces `metadata_scraped_at`, `preview.jpg` and the listing
//! thumbnail) is handed to this queue and processed **one concert at a time** by
//! a single long-lived consumer task. Serializing is deliberate: it avoids
//! hammering NPR / getting IP-blocked. Listing cards for queued concerts render a
//! "loading…" placeholder and poll `/concerts/:id/status` until their thumbnail
//! is ready (see `web::handlers` + `templates/row.html`).
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
        match db::get_concert(&conn, req.concert_id) {
            Ok(c) if c.metadata_scraped_at.is_some() => {
                tracing::info!(target: LOG_TARGET, "concert {} already scraped; skipping", req.concert_id);
                return;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(target: LOG_TARGET, "scrape skip-check failed for concert {}: {}", req.concert_id, e);
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
            return;
        }
    };

    // Brief DB lock for the metadata write only.
    {
        let conn = db.lock().unwrap();
        if let Err(e) = scrape::apply_concert_info(&conn, &info) {
            tracing::warn!(target: LOG_TARGET, "background scrape apply failed for concert {}: {}", req.concert_id, e);
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

    fn dummy_db() -> Arc<Mutex<Connection>> {
        Arc::new(Mutex::new(db::open_in_memory().unwrap()))
    }

    /// Poll `cond` until true, failing the test if it never becomes true.
    async fn wait_until<F: Fn() -> bool>(cond: F) {
        for _ in 0..200 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition not met in time");
    }

    // Multi-thread runtime: the tests block on std `recv` to synchronize with the
    // item, which would starve the spawned worker on a current-thread runtime.
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
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();

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
        let got = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got, 2);

        // Pending cleared for both (panicked one included).
        wait_until(|| !q.is_pending(1) && !q.is_pending(2)).await;
    }
}
