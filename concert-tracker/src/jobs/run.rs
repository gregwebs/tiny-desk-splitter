//! The Job Run engine: turns a validated [`JobRequest`] into an admitted,
//! race-safe Job Run with exactly one terminal outcome and, on any
//! unsuccessful outcome (setup failure, execution failure, panic, or user
//! cancellation), Failed Job history. See `docs/jobs.md` for the state
//! diagram and the invariants this module upholds.
//!
//! Download, split, and archive all route through this engine.
//! `recover_failed` (below) additionally lets restart/shutdown recovery reuse
//! its terminal-commit behavior without an in-process registry reservation.

use std::any::Any;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use anyhow::{Context as _, Result};
use rusqlite::Connection;

use crate::db;
use crate::jobs::{JobKey, JobRegistry, JobRunFuture, JobStepOutcome, TerminalGate};

/// Shared with `crate::lifecycle::cancel_job` (which calls [`cancel`] below)
/// so both record the same wording for a user-initiated cancellation.
pub const CANCELLED_BY_USER: &str = "cancelled by user";

/// The one small job-request interface every Job Run implementor provides.
/// Each method maps to one phase of the Job Request → Job Run → terminal
/// outcome lifecycle from issue #124:
///
/// - [`validate`](Self::validate): pre-acceptance. `Err` is a synchronous
///   rejection — no lifecycle change, no event, no Failed Job.
/// - [`try_mark_started`](Self::try_mark_started): the persistent
///   acceptance transition. Must be the *last fallible* admission step —
///   see [`submit`].
/// - [`setup`](Self::setup) / [`execute`](Self::execute): race-safe
///   post-acceptance preparation and the actual work.
/// - [`gather_success_facts`](Self::gather_success_facts) /
///   [`commit_success`](Self::commit_success): success is split into an
///   FS-only fact-gathering step (runs before the DB mutex is taken — the
///   working dir can be a slow mount) and a DB-only commit step (runs
///   inside the terminal transaction).
/// - [`record_failure`](Self::record_failure): the DB-only failure commit,
///   covering setup failure, execution failure, panic, and cancellation.
pub trait JobCancellation {
    fn key(&self) -> JobKey;
    fn job_name(&self) -> &'static str;
    fn record_failure(&self, conn: &Connection, error: &str) -> Result<()>;
    fn has_stale_in_progress(&self, conn: &Connection) -> Result<bool>;
}

pub trait JobRequest: JobCancellation + Send + Sync + 'static {
    /// Built pre-acceptance by [`validate`](Self::validate).
    type Input: Send + 'static;
    /// Built post-acceptance by [`setup`](Self::setup).
    type Setup: Send + 'static;
    /// Facts gathered (FS only) after success, before the terminal commit.
    type Facts: Send + 'static;

    /// Pre-acceptance validation and input construction.
    fn validate(&self, conn: &Connection) -> Result<Self::Input>;

    /// The persistent started-transition. `Ok(false)` means someone else
    /// already won admission (or the concert isn't eligible) — treated the
    /// same as an occupied registry reservation.
    fn try_mark_started(&self, conn: &Connection) -> Result<bool>;

    /// Race-safe post-acceptance setup, run without holding the DB mutex.
    fn setup(&self, input: Self::Input) -> Result<Self::Setup>;

    fn execute<'a>(
        &'a self,
        setup: &'a Self::Setup,
        log_file: Option<&'a Path>,
    ) -> JobRunFuture<'a, JobStepOutcome>;

    /// FS/fact gathering for success. No `Connection` — this runs *before*
    /// the DB mutex is taken, so a stall on a slow working dir can't freeze
    /// every handler waiting on that mutex.
    fn gather_success_facts(&self, setup: &Self::Setup) -> Result<Self::Facts>;

    /// DB-only success commit; runs inside the terminal transaction.
    fn commit_success(&self, conn: &Connection, facts: Self::Facts) -> Result<()>;

    /// Directory for this Job Run's log file, used by `finish_as_failure` to
    /// persist a failed run's captured output. Default: no log file.
    fn log_dir(&self) -> Option<PathBuf> {
        None
    }

    /// Start any Job Requests queued behind this Job Run now that success
    /// is persisted (e.g. a queued split after a download). Default: no
    /// dependents. Runs after the terminal commit and before the registry
    /// slot is released.
    fn spawn_dependents(&self, db: Arc<Mutex<Connection>>, registry: Arc<JobRegistry>) {
        let _ = (db, registry);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Accepted,
    AlreadyRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    CancelledRunning,
    DroppedQueued,
    MarkedStaleFailed,
    NoSuchActiveJob,
}

/// Submit a Job Request: reserve admission, validate, persist the started
/// transition, then spawn the Job Run.
///
/// `try_mark_started` must remain the last fallible admission step: it
/// emits the `*_started` event immediately and there is no way to un-emit
/// it, so nothing after it may fail synchronously. `tokio::spawn` and
/// `JobReservation::activate` are infallible, which is what makes that
/// true — the spawned task itself is parked on the reservation's
/// `ActivationSignal` until `activate` attaches its handle, so it can't
/// reach a terminal outcome (and thus `registry.release`) before the slot
/// is fully set up.
pub async fn submit<R: JobRequest>(
    db: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    request: R,
) -> Result<Admission> {
    let request = Arc::new(request);
    let key = request.key();

    let Some((reservation, signal)) = registry.try_reserve(key.clone()) else {
        return Ok(Admission::AlreadyRunning);
    };

    let input = {
        let conn = db.lock().unwrap();
        request.validate(&conn)
    };
    let input = match input {
        Ok(input) => input,
        Err(e) => {
            drop(reservation); // rollback: no lifecycle change was made
            return Err(e);
        }
    };

    let started = {
        let conn = db.lock().unwrap();
        request.try_mark_started(&conn)
    };
    let started = match started {
        Ok(started) => started,
        Err(e) => {
            drop(reservation); // rollback: the UPDATE did not commit
            return Err(e);
        }
    };
    if !started {
        drop(reservation);
        return Ok(Admission::AlreadyRunning);
    }

    let terminal = reservation.terminal_gate();
    let run_request = request.clone();
    let run_db = db.clone();
    let run_registry = registry.clone();
    let handle = tokio::spawn(async move {
        signal.wait().await;
        run(run_db, run_registry, run_request, terminal, input).await;
    });
    reservation.activate(handle);

    Ok(Admission::Accepted)
}

/// Cancel the Job Run (or queued/stale request) named by `request.key()`.
///
/// Claims the terminal gate before writing anything: if the run has already
/// claimed it (mid-commit or committed), cancellation writes nothing and
/// reports [`CancelOutcome::NoSuchActiveJob`] rather than risk a second
/// terminal outcome for the same Job Run. A Reserved-but-not-yet-accepted
/// slot (admission still in progress) has no terminal gate exposed by the
/// registry, so it falls through to the same "nothing to cancel yet" path.
pub fn cancel<R: JobCancellation>(
    conn: &Connection,
    registry: &Arc<JobRegistry>,
    request: &R,
) -> Result<CancelOutcome> {
    let key = request.key();

    if let Some(terminal) = registry.terminal_gate(&key) {
        if !terminal.claim() {
            return Ok(CancelOutcome::NoSuchActiveJob);
        }
        // Won the gate: the run task, once it reaches its own claim, will
        // find the gate already taken and do nothing — so from here on we
        // exclusively own this Job Run's terminal outcome.
        registry.drop_dependency_edges(&key);
        let commit = commit_failure_tx(conn, request, &key, CANCELLED_BY_USER);
        registry.abort_and_release(&key);
        commit.context("Failed to commit cancelled terminal")?;
        return Ok(CancelOutcome::CancelledRunning);
    }

    let dropped_queued = registry.drop_dependency_edges(&key);
    if dropped_queued {
        return Ok(CancelOutcome::DroppedQueued);
    }
    if request.has_stale_in_progress(conn)? {
        request.record_failure(conn, CANCELLED_BY_USER)?;
        return Ok(CancelOutcome::MarkedStaleFailed);
    }
    Ok(CancelOutcome::NoSuchActiveJob)
}

/// Convert a stale accepted Job Run (found at process start or during
/// graceful shutdown) into a Failed Job in one transaction — lifecycle
/// failure columns, event, and Failed Job row — without an in-process
/// registry reservation or terminal gate. Used by restart/shutdown recovery
/// in `crate::lifecycle::fail_in_progress_jobs`.
///
/// Safe without a gate because of *where* it runs, not because contention is
/// checked here: at startup it runs before the `JobRegistry` exists (so there
/// is no run task to race), and at shutdown it runs after `JobRegistry::cancel_all`
/// has already aborted and released every slot (so no gate remains to claim).
/// `*_started_at` is therefore the sole recovery coordination signal — callers
/// select only rows where it is still set — and `fail_in_progress_jobs` holds
/// the db mutex across its whole select-and-commit loop so no run task can
/// interleave within it. See `docs/jobs.md`'s Recovery section for the one
/// pre-existing residual: an aborted archive run's detached `spawn_blocking`
/// thread cannot itself be cancelled, so in principle it could still commit a
/// success after this function's caller releases the mutex.
pub fn recover_failed<R: JobCancellation>(
    conn: &Connection,
    request: &R,
    reason: &str,
) -> Result<()> {
    commit_failure_tx(conn, request, &request.key(), reason).map(|_job_id| ())
}

async fn run<R: JobRequest>(
    db: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    request: Arc<R>,
    terminal: Arc<TerminalGate>,
    input: R::Input,
) {
    let key = request.key();
    let log_dir = request.log_dir();
    let temp_file = log_dir.as_ref().and_then(|dir| {
        match std::fs::create_dir_all(dir).and_then(|_| tempfile::NamedTempFile::new_in(dir)) {
            Ok(f) => Some(f),
            Err(e) => {
                tracing::warn!(?key, "failed to create job log temp file: {}", e);
                None
            }
        }
    });
    let temp_path = temp_file.as_ref().map(|f| f.path().to_path_buf());

    let outcome = run_setup_and_execute(request.as_ref(), input, temp_path.as_deref()).await;

    // Success facts are FS-only and gathered before the DB mutex is taken
    // (and before claiming the gate) so a slow working dir can't freeze
    // every handler behind that mutex.
    let facts_or_error: std::result::Result<R::Facts, String> = match outcome {
        Ok((setup, JobStepOutcome::Succeeded)) => request
            .gather_success_facts(&setup)
            .map_err(|e| format!("{e:#}")),
        Ok((_setup, JobStepOutcome::Failed { message })) => Err(message),
        Err(message) => Err(message),
    };

    if !terminal.claim() {
        // Cancellation already won the gate; it owns the terminal write,
        // the Failed Job row, and releasing the slot. Nothing to do here —
        // an in-flight abort of our own handle lands harmlessly.
        return;
    }

    match facts_or_error {
        Ok(facts) => {
            let commit = {
                let conn = db.lock().unwrap();
                commit_success_tx(&conn, request.as_ref(), facts)
            };
            match commit {
                Ok(()) => {
                    tracing::info!(?key, "{} succeeded", request.job_name());
                    drop(temp_file);
                    request.spawn_dependents(db.clone(), registry.clone());
                    registry.release(&key);
                }
                Err(e) => {
                    tracing::warn!(
                        ?key,
                        "{} success persistence failed: {:#}",
                        request.job_name(),
                        e
                    );
                    finish_as_failure(
                        &db,
                        &registry,
                        request.as_ref(),
                        &key,
                        &format!("{e:#}"),
                        temp_file,
                        log_dir.as_deref(),
                    );
                }
            }
        }
        Err(message) => {
            tracing::warn!(?key, "{} failed: {}", request.job_name(), message);
            finish_as_failure(
                &db,
                &registry,
                request.as_ref(),
                &key,
                &message,
                temp_file,
                log_dir.as_deref(),
            );
        }
    }
}

fn finish_as_failure<R: JobRequest>(
    db: &Arc<Mutex<Connection>>,
    registry: &Arc<JobRegistry>,
    request: &R,
    key: &JobKey,
    message: &str,
    temp_file: Option<tempfile::NamedTempFile>,
    log_dir: Option<&Path>,
) {
    registry.drop_dependency_edges(key);
    let job_id = {
        let conn = db.lock().unwrap();
        commit_failure_tx(&conn, request, key, message)
    };
    match job_id {
        Ok(job_id) => {
            if let (Some(tf), Some(dir)) = (temp_file, log_dir) {
                let final_path = dir.join(format!("{job_id}.log"));
                if let Err(e) = tf.persist(&final_path) {
                    tracing::warn!(
                        "failed to persist job log to {}: {}",
                        final_path.display(),
                        e
                    );
                }
            }
        }
        Err(e) => {
            tracing::error!(?key, "failed to commit failure terminal: {:#}", e);
        }
    }
    // The registry must never leak a reservation, even if the failure
    // transaction itself errored (e.g. the DB is busy or poisoned).
    registry.release(key);
}

fn commit_success_tx<R: JobRequest>(conn: &Connection, request: &R, facts: R::Facts) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .context("Failed to begin success terminal transaction")?;
    request.commit_success(&tx, facts)?;
    tx.commit().context("Failed to commit success terminal")?;
    Ok(())
}

fn commit_failure_tx<R: JobCancellation>(
    conn: &Connection,
    request: &R,
    key: &JobKey,
    message: &str,
) -> Result<i64> {
    let tx = conn
        .unchecked_transaction()
        .context("Failed to begin failure terminal transaction")?;
    request.record_failure(&tx, message)?;
    let job_id =
        db::failed_jobs::insert_failed_job(&tx, key.concert_id, request.job_name(), message)?;
    tx.commit().context("Failed to commit failure terminal")?;
    Ok(job_id)
}

/// Run `setup` (sync) then `execute` (async), converting a panic in either
/// into `Err` instead of unwinding into the caller. Both run on the same
/// task that the registry tracks, so aborting that task (cancellation)
/// still drops `execute`'s in-flight future immediately — e.g. triggering
/// `kill_on_drop` on a subprocess — exactly as it did before this engine
/// existed. A nested `tokio::spawn` would break that: aborting the outer
/// task wouldn't stop an inner task's subprocess.
async fn run_setup_and_execute<R: JobRequest>(
    request: &R,
    input: R::Input,
    log_file: Option<&Path>,
) -> std::result::Result<(R::Setup, JobStepOutcome), String> {
    let setup =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| request.setup(input))) {
            Ok(Ok(setup)) => setup,
            Ok(Err(e)) => return Err(format!("{e:#}")),
            Err(payload) => {
                return Err(format!(
                    "job panicked during setup: {}",
                    panic_message(&payload)
                ))
            }
        };
    match catch_unwind_future(request.execute(&setup, log_file)).await {
        Ok(step) => Ok((setup, step)),
        Err(payload) => Err(format!(
            "job panicked during execution: {}",
            panic_message(&payload)
        )),
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Poll `fut` to completion, converting a panic raised during polling into
/// `Err` instead of unwinding into the caller. Small local reimplementation
/// of `futures::FutureExt::catch_unwind` to avoid a new dependency for one
/// combinator.
fn catch_unwind_future<'a, T: 'a>(
    fut: Pin<Box<dyn Future<Output = T> + Send + 'a>>,
) -> impl Future<Output = std::result::Result<T, Box<dyn Any + Send>>> + 'a {
    let mut fut = fut;
    std::future::poll_fn(move |cx: &mut Context<'_>| {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fut.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(t)) => Poll::Ready(Ok(t)),
            Err(payload) => Poll::Ready(Err(payload)),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::seeds::SeedContext;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::oneshot as test_oneshot;

    /// A tiny, fully in-memory `JobRequest` for exercising the engine
    /// without any subprocess or filesystem dependency. Each test seeds its
    /// own concert row and controls `execute`'s outcome directly.
    struct TestRequest {
        concert_id: i64,
        column: &'static str, // "download" — reuses the concerts.download_* columns
        block: Mutex<Option<test_oneshot::Receiver<StepResult>>>,
        setup_result: Mutex<Option<Result<()>>>,
        setup_panics: bool,
        commit_success_fails: bool,
        dependents_spawned: Arc<AtomicUsize>,
    }

    enum StepResult {
        Succeed,
        Fail(String),
        Panic,
    }

    impl TestRequest {
        fn new(concert_id: i64, rx: test_oneshot::Receiver<StepResult>) -> Self {
            TestRequest {
                concert_id,
                column: "download",
                block: Mutex::new(Some(rx)),
                setup_result: Mutex::new(Some(Ok(()))),
                setup_panics: false,
                commit_success_fails: false,
                dependents_spawned: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn instant(concert_id: i64, result: StepResult) -> (Self, ()) {
            let (tx, rx) = test_oneshot::channel();
            let _ = tx.send(result);
            (Self::new(concert_id, rx), ())
        }
    }

    impl JobCancellation for TestRequest {
        fn key(&self) -> JobKey {
            JobKey {
                concert_id: self.concert_id,
                kind: crate::jobs::JobKind::Download,
            }
        }

        fn job_name(&self) -> &'static str {
            self.column
        }

        fn record_failure(&self, conn: &Connection, error: &str) -> Result<()> {
            db::lifecycle::mark_download_failed(conn, self.concert_id, error)
        }

        fn has_stale_in_progress(&self, conn: &Connection) -> Result<bool> {
            conn.query_row(
                "SELECT download_started_at IS NOT NULL FROM concerts WHERE id = ?1",
                [self.concert_id],
                |row| row.get(0),
            )
            .context("Failed to check stale state")
        }
    }

    impl JobRequest for TestRequest {
        type Input = ();
        type Setup = ();
        type Facts = ();

        fn validate(&self, conn: &Connection) -> Result<()> {
            db::concerts::get_concert(conn, self.concert_id)?;
            Ok(())
        }

        fn try_mark_started(&self, conn: &Connection) -> Result<bool> {
            db::lifecycle::try_mark_download_started(conn, self.concert_id)
        }

        fn setup(&self, _input: ()) -> Result<()> {
            if self.setup_panics {
                panic!("setup panic (test)");
            }
            self.setup_result.lock().unwrap().take().unwrap()
        }

        fn execute<'a>(
            &'a self,
            _setup: &'a (),
            _log_file: Option<&'a Path>,
        ) -> JobRunFuture<'a, JobStepOutcome> {
            let rx = self
                .block
                .lock()
                .unwrap()
                .take()
                .expect("execute called twice");
            Box::pin(async move {
                match rx.await {
                    Ok(StepResult::Succeed) => JobStepOutcome::Succeeded,
                    Ok(StepResult::Fail(message)) => JobStepOutcome::Failed { message },
                    Ok(StepResult::Panic) => panic!("execute panic (test)"),
                    Err(_) => JobStepOutcome::Failed {
                        message: "sender dropped".to_string(),
                    },
                }
            })
        }

        fn gather_success_facts(&self, _setup: &()) -> Result<()> {
            Ok(())
        }

        fn commit_success(&self, conn: &Connection, _facts: ()) -> Result<()> {
            if self.commit_success_fails {
                anyhow::bail!("commit_success blew up (test)");
            }
            db::lifecycle::mark_download_succeeded(conn, self.concert_id, "mp4")
        }

        fn spawn_dependents(&self, _db: Arc<Mutex<Connection>>, _registry: Arc<JobRegistry>) {
            self.dependents_spawned.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A request whose `validate` always rejects synchronously.
    struct RejectingRequest {
        concert_id: i64,
    }

    impl JobCancellation for RejectingRequest {
        fn key(&self) -> JobKey {
            JobKey {
                concert_id: self.concert_id,
                kind: crate::jobs::JobKind::Download,
            }
        }
        fn job_name(&self) -> &'static str {
            "download"
        }
        fn record_failure(&self, _conn: &Connection, _error: &str) -> Result<()> {
            unreachable!()
        }
        fn has_stale_in_progress(&self, _conn: &Connection) -> Result<bool> {
            unreachable!()
        }
    }

    impl JobRequest for RejectingRequest {
        type Input = ();
        type Setup = ();
        type Facts = ();

        fn validate(&self, _conn: &Connection) -> Result<()> {
            Err(anyhow::anyhow!("rejected for test"))
        }
        fn try_mark_started(&self, _conn: &Connection) -> Result<bool> {
            unreachable!("validate rejects before try_mark_started")
        }
        fn setup(&self, _input: ()) -> Result<()> {
            unreachable!()
        }
        fn execute<'a>(
            &'a self,
            _setup: &'a (),
            _log_file: Option<&'a Path>,
        ) -> JobRunFuture<'a, JobStepOutcome> {
            unreachable!()
        }
        fn gather_success_facts(&self, _setup: &()) -> Result<()> {
            unreachable!()
        }
        fn commit_success(&self, _conn: &Connection, _facts: ()) -> Result<()> {
            unreachable!()
        }
    }

    fn seeded_db() -> (Arc<Mutex<Connection>>, i64) {
        let conn = db::connection::open_in_memory().unwrap();
        let id = SeedContext::new(&conn)
            .seed_scraped_concert(db::seeds::SeedScrapedConcert {
                source_url: Some("https://npr.org/test/run".to_string()),
                title: Some("Run Engine Concert".to_string()),
                concert_date: None,
                artist: Some("Test Artist".to_string()),
                album: Some("Test Album".to_string()),
                set_list: Some(vec![]),
            })
            .unwrap()
            .id;
        (Arc::new(Mutex::new(conn)), id)
    }

    fn events_for(conn: &Connection, id: i64) -> Vec<String> {
        crate::events::list_for_concert(conn, id)
            .into_iter()
            .map(|e| e.event)
            .collect()
    }

    fn failed_job_count(conn: &Connection, id: i64) -> usize {
        db::failed_jobs::list_failed_jobs(conn, 100)
            .unwrap()
            .into_iter()
            .filter(|j| j.concert_id == id)
            .count()
    }

    #[tokio::test]
    async fn synchronous_rejection_creates_no_history() {
        let (db, id) = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        let key = JobKey {
            concert_id: id,
            kind: crate::jobs::JobKind::Download,
        };

        let before = {
            let conn = db.lock().unwrap();
            events_for(&conn, id)
        };

        let result = submit(
            db.clone(),
            registry.clone(),
            RejectingRequest { concert_id: id },
        )
        .await;
        assert!(result.is_err());

        let conn = db.lock().unwrap();
        assert_eq!(
            events_for(&conn, id),
            before,
            "rejection must emit no new event"
        );
        assert_eq!(
            failed_job_count(&conn, id),
            0,
            "rejection must not create a Failed Job"
        );
        assert!(
            db::concerts::get_concert(&conn, id)
                .unwrap()
                .download_started_at
                .is_none(),
            "rejection must not leave a started transition"
        );
        drop(conn);
        assert!(!registry.is_running(&key), "reservation must roll back");
    }

    #[tokio::test]
    async fn concurrent_submits_accept_exactly_one() {
        let (db, id) = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        let (tx, rx) = test_oneshot::channel();
        let request = TestRequest::new(id, rx);

        let a1 = submit(db.clone(), registry.clone(), request).await.unwrap();
        assert_eq!(a1, Admission::Accepted);

        // A second concurrent request for the same key must be rejected as
        // AlreadyRunning without touching lifecycle state.
        let (tx2, rx2) = test_oneshot::channel();
        let _ = tx2; // unused sender; request2's execute is never called
        let request2 = TestRequest::new(id, rx2);
        let a2 = submit(db.clone(), registry.clone(), request2)
            .await
            .unwrap();
        assert_eq!(a2, Admission::AlreadyRunning);

        let _ = tx.send(StepResult::Succeed);
        for _ in 0..100 {
            {
                let conn = db.lock().unwrap();
                if db::concerts::get_concert(&conn, id)
                    .unwrap()
                    .downloaded_at
                    .is_some()
                {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let conn = db.lock().unwrap();
        let started_events = events_for(&conn, id)
            .into_iter()
            .filter(|e| e == "download_started")
            .count();
        assert_eq!(started_events, 1, "exactly one started transition/event");
    }

    #[tokio::test]
    async fn setup_failure_produces_failed_terminal_and_failed_job() {
        let (db, id) = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        let (_tx, rx) = test_oneshot::channel();
        let request = TestRequest::new(id, rx);
        *request.setup_result.lock().unwrap() = Some(Err(anyhow::anyhow!("setup blew up")));

        assert_eq!(
            submit(db.clone(), registry.clone(), request).await.unwrap(),
            Admission::Accepted
        );

        wait_for(&db, id, |c| !c.download_errors.is_empty()).await;
        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(c.download_started_at.is_none());
        assert_eq!(c.download_errors.last().unwrap().error, "setup blew up");
        assert_eq!(failed_job_count(&conn, id), 1);
        drop(conn);
        let key = JobKey {
            concert_id: id,
            kind: crate::jobs::JobKind::Download,
        };
        assert!(!registry.is_running(&key), "slot must be released");
    }

    #[tokio::test]
    async fn execution_failure_produces_failed_terminal_and_failed_job() {
        let (db, id) = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        let (tx, rx) = test_oneshot::channel();
        let request = TestRequest::new(id, rx);

        assert_eq!(
            submit(db.clone(), registry.clone(), request).await.unwrap(),
            Admission::Accepted
        );
        let _ = tx.send(StepResult::Fail("boom".to_string()));

        wait_for(&db, id, |c| !c.download_errors.is_empty()).await;
        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert_eq!(c.download_errors.last().unwrap().error, "boom");
        assert_eq!(failed_job_count(&conn, id), 1);
    }

    #[tokio::test]
    async fn panic_in_execute_produces_failed_job() {
        let (db, id) = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        let (tx, rx) = test_oneshot::channel();
        let request = TestRequest::new(id, rx);

        assert_eq!(
            submit(db.clone(), registry.clone(), request).await.unwrap(),
            Admission::Accepted
        );
        let _ = tx.send(StepResult::Panic);

        wait_for(&db, id, |c| !c.download_errors.is_empty()).await;
        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(c.download_errors.last().unwrap().error.contains("panicked"));
        assert_eq!(failed_job_count(&conn, id), 1);
    }

    #[tokio::test]
    async fn panic_in_setup_produces_failed_job() {
        let (db, id) = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        let (_tx, rx) = test_oneshot::channel();
        let mut request = TestRequest::new(id, rx);
        request.setup_panics = true;

        assert_eq!(
            submit(db.clone(), registry.clone(), request).await.unwrap(),
            Admission::Accepted
        );

        wait_for(&db, id, |c| !c.download_errors.is_empty()).await;
        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(c
            .download_errors
            .last()
            .unwrap()
            .error
            .contains("panicked during setup"));
        assert_eq!(failed_job_count(&conn, id), 1);
    }

    #[tokio::test]
    async fn success_starts_dependents_and_releases_slot() {
        let (db, id) = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        let (tx, rx) = test_oneshot::channel();
        let request = TestRequest::new(id, rx);
        let spawned = request.dependents_spawned.clone();

        assert_eq!(
            submit(db.clone(), registry.clone(), request).await.unwrap(),
            Admission::Accepted
        );
        let _ = tx.send(StepResult::Succeed);

        wait_for(&db, id, |c| c.downloaded_at.is_some()).await;
        assert_eq!(spawned.load(Ordering::SeqCst), 1);
        let key = JobKey {
            concert_id: id,
            kind: crate::jobs::JobKind::Download,
        };
        // Poll: release happens right after spawn_dependents, in the same task.
        for _ in 0..50 {
            if !registry.is_running(&key) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !registry.is_running(&key),
            "slot must be released after success"
        );
    }

    #[tokio::test]
    async fn success_persistence_failure_produces_failed_terminal_not_success() {
        let (db, id) = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        let (tx, rx) = test_oneshot::channel();
        let mut request = TestRequest::new(id, rx);
        request.commit_success_fails = true;
        let spawned = request.dependents_spawned.clone();
        let key = request.key();

        assert_eq!(
            submit(db.clone(), registry.clone(), request).await.unwrap(),
            Admission::Accepted
        );
        let _ = tx.send(StepResult::Succeed);

        wait_for(&db, id, |c| !c.download_errors.is_empty()).await;
        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(
            c.downloaded_at.is_none(),
            "success must not be visible when persistence failed"
        );
        assert!(c.download_errors.last().unwrap().error.contains("blew up"));
        assert_eq!(
            failed_job_count(&conn, id),
            1,
            "persistence failure still produces exactly one Failed Job"
        );
        assert_eq!(
            spawned.load(Ordering::SeqCst),
            0,
            "dependents must not start when success never persisted"
        );
        drop(conn);
        assert!(!registry.is_running(&key), "slot must be released");
    }

    #[tokio::test]
    async fn cancel_while_blocked_produces_cancelled_failed_job_and_no_success() {
        let (db, id) = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        let (_tx, rx) = test_oneshot::channel(); // never sent — execute stays blocked
        let request = TestRequest::new(id, rx);
        let key = request.key();

        assert_eq!(
            submit(db.clone(), registry.clone(), request).await.unwrap(),
            Admission::Accepted
        );
        // Give the run task a moment to reach the blocked `execute` await.
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        let cancel_request = TestRequest {
            concert_id: id,
            column: "download",
            block: Mutex::new(None),
            setup_result: Mutex::new(None),
            setup_panics: false,
            commit_success_fails: false,
            dependents_spawned: Arc::new(AtomicUsize::new(0)),
        };
        let outcome = {
            let conn = db.lock().unwrap();
            cancel(&conn, &registry, &cancel_request).unwrap()
        };
        assert_eq!(outcome, CancelOutcome::CancelledRunning);

        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(c.downloaded_at.is_none(), "cancelled run must not succeed");
        assert_eq!(c.download_errors.last().unwrap().error, CANCELLED_BY_USER);
        assert_eq!(failed_job_count(&conn, id), 1);
        drop(conn);
        assert!(!registry.is_running(&key), "slot released after cancel");
    }

    #[tokio::test]
    async fn cancel_after_run_wins_the_gate_writes_nothing() {
        // The run claims the gate itself (by completing) before cancel gets
        // a chance to; cancel must then be a no-op, not a second terminal.
        let (db, id) = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        let (tx, rx) = test_oneshot::channel();
        let request = TestRequest::new(id, rx);
        let key = request.key();

        assert_eq!(
            submit(db.clone(), registry.clone(), request).await.unwrap(),
            Admission::Accepted
        );
        let _ = tx.send(StepResult::Succeed);
        wait_for(&db, id, |c| c.downloaded_at.is_some()).await;
        // Let the success path fully finish (spawn_dependents + release).
        for _ in 0..50 {
            if !registry.is_running(&key) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let cancel_request = TestRequest {
            concert_id: id,
            column: "download",
            block: Mutex::new(None),
            setup_result: Mutex::new(None),
            setup_panics: false,
            commit_success_fails: false,
            dependents_spawned: Arc::new(AtomicUsize::new(0)),
        };
        let outcome = {
            let conn = db.lock().unwrap();
            cancel(&conn, &registry, &cancel_request).unwrap()
        };
        assert_eq!(outcome, CancelOutcome::NoSuchActiveJob);

        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(
            c.downloaded_at.is_some(),
            "the successful terminal must be untouched"
        );
        assert_eq!(
            failed_job_count(&conn, id),
            0,
            "no duplicate Failed Job from cancel"
        );
    }

    #[tokio::test]
    async fn instant_completion_leaves_no_zombie_slot() {
        let (db, id) = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        let (request, ()) = TestRequest::instant(id, StepResult::Succeed);
        let key = request.key();

        assert_eq!(
            submit(db.clone(), registry.clone(), request).await.unwrap(),
            Admission::Accepted
        );

        wait_for(&db, id, |c| c.downloaded_at.is_some()).await;
        for _ in 0..50 {
            if !registry.is_running(&key) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !registry.is_running(&key),
            "an instantly-completing run must not leave a zombie slot"
        );
    }

    async fn wait_for(
        db: &Arc<Mutex<Connection>>,
        id: i64,
        check: impl Fn(&crate::model::Concert) -> bool,
    ) {
        for _ in 0..100 {
            {
                let conn = db.lock().unwrap();
                if let Ok(c) = db::concerts::get_concert(&conn, id) {
                    if check(&c) {
                        return;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("wait_for timed out");
    }
}
