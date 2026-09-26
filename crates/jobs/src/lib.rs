//! Background job system: priority queue, worker pool, progress,
//! cancellation, retry with backoff and error state.
//!
//! Jobs are CPU/process bound (FFmpeg, analysis), so plain OS threads are the
//! right tool; async would add nothing here. The UI polls [`JobSystem::snapshot`]
//! on a timer instead of receiving a callback per progress tick, which
//! naturally coalesces high-frequency updates.

use kadr_core::CancelToken;
use parking_lot::{Condvar, Mutex};
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub type JobId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    Background = 0,
    Normal = 1,
    /// Work the user is actively waiting for (export, visible thumbnails).
    High = 2,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed(String),
    Cancelled,
}

impl JobState {
    pub fn is_finished(&self) -> bool {
        matches!(self, JobState::Done | JobState::Failed(_) | JobState::Cancelled)
    }
}

#[derive(Debug)]
pub enum JobError {
    /// Transient (e.g. file locked, network) — retried with backoff.
    Retryable(String),
    Fatal(String),
    Cancelled,
}

impl<E: std::fmt::Display> From<E> for JobError {
    fn from(e: E) -> Self {
        JobError::Fatal(e.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct JobSpec {
    pub title: String,
    /// Grouping for UI, e.g. "thumbnails", "audio", "export".
    pub category: &'static str,
    pub priority: Priority,
    pub max_retries: u32,
}

impl JobSpec {
    pub fn new(title: impl Into<String>, category: &'static str) -> Self {
        JobSpec { title: title.into(), category, priority: Priority::Normal, max_retries: 0 }
    }
    pub fn priority(mut self, p: Priority) -> Self {
        self.priority = p;
        self
    }
    pub fn retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }
}

#[derive(Clone, Debug)]
pub struct JobInfo {
    pub id: JobId,
    pub title: String,
    pub category: &'static str,
    pub priority: Priority,
    pub state: JobState,
    pub progress: f32,
    pub attempts: u32,
}

/// Handed to the job body.
pub struct JobCtx {
    pub id: JobId,
    pub cancel: CancelToken,
    inner: Arc<Inner>,
}

impl JobCtx {
    pub fn progress(&self, p: f32) {
        if let Some(j) = self.inner.jobs.lock().get_mut(&self.id) {
            j.info.progress = p.clamp(0.0, 1.0);
        }
    }
    pub fn check_cancelled(&self) -> Result<(), JobError> {
        if self.cancel.is_cancelled() { Err(JobError::Cancelled) } else { Ok(()) }
    }
}

type Work = Arc<dyn Fn(&JobCtx) -> Result<(), JobError> + Send + Sync>;

struct Entry {
    info: JobInfo,
    cancel: CancelToken,
    work: Work,
    max_retries: u32,
}

#[derive(PartialEq, Eq)]
struct Queued {
    priority: Priority,
    not_before: Instant,
    seq: u64,
    id: JobId,
}

impl Ord for Queued {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        // Highest priority first, then FIFO (lower seq first).
        self.priority.cmp(&o.priority).then_with(|| o.seq.cmp(&self.seq))
    }
}
impl PartialOrd for Queued {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

struct Inner {
    queue: Mutex<BinaryHeap<Queued>>,
    cv: Condvar,
    jobs: Mutex<HashMap<JobId, Entry>>,
    next_id: Mutex<(JobId, u64)>,
    shutdown: std::sync::atomic::AtomicBool,
}

#[derive(Clone)]
pub struct JobSystem {
    inner: Arc<Inner>,
}

impl JobSystem {
    /// Starts `workers` threads (at least 1).
    pub fn new(workers: usize) -> Self {
        let inner = Arc::new(Inner {
            queue: Mutex::new(BinaryHeap::new()),
            cv: Condvar::new(),
            jobs: Mutex::new(HashMap::new()),
            next_id: Mutex::new((1, 0)),
            shutdown: false.into(),
        });
        for i in 0..workers.max(1) {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name(format!("kadr-job-{i}"))
                .spawn(move || worker(inner))
                .expect("spawn worker");
        }
        JobSystem { inner }
    }

    pub fn submit<F>(&self, spec: JobSpec, work: F) -> JobId
    where
        F: Fn(&JobCtx) -> Result<(), JobError> + Send + Sync + 'static,
    {
        let (id, seq) = {
            let mut n = self.inner.next_id.lock();
            let v = *n;
            *n = (v.0 + 1, v.1 + 1);
            v
        };
        let info = JobInfo {
            id,
            title: spec.title,
            category: spec.category,
            priority: spec.priority,
            state: JobState::Queued,
            progress: 0.0,
            attempts: 0,
        };
        tracing::debug!(id, title = %info.title, "job queued");
        self.inner
            .jobs
            .lock()
            .insert(id, Entry { info, cancel: CancelToken::new(), work: Arc::new(work), max_retries: spec.max_retries });
        self.enqueue(id, spec.priority, seq, Instant::now());
        id
    }

    fn enqueue(&self, id: JobId, priority: Priority, seq: u64, not_before: Instant) {
        self.inner.queue.lock().push(Queued { priority, not_before, seq, id });
        self.inner.cv.notify_one();
    }

    pub fn cancel(&self, id: JobId) {
        if let Some(e) = self.inner.jobs.lock().get_mut(&id) {
            e.cancel.cancel();
            if e.info.state == JobState::Queued {
                e.info.state = JobState::Cancelled;
            }
        }
    }

    /// Re-queues a failed or cancelled job.
    pub fn retry(&self, id: JobId) -> bool {
        let prio = {
            let mut jobs = self.inner.jobs.lock();
            let Some(e) = jobs.get_mut(&id) else { return false };
            if !matches!(e.info.state, JobState::Failed(_) | JobState::Cancelled) {
                return false;
            }
            e.info.state = JobState::Queued;
            e.info.progress = 0.0;
            // A manual retry gets a fresh automatic-retry budget.
            e.info.attempts = 0;
            e.cancel = CancelToken::new();
            e.info.priority
        };
        let seq = {
            let mut n = self.inner.next_id.lock();
            n.1 += 1;
            n.1
        };
        self.enqueue(id, prio, seq, Instant::now());
        true
    }

    pub fn info(&self, id: JobId) -> Option<JobInfo> {
        self.inner.jobs.lock().get(&id).map(|e| e.info.clone())
    }

    /// All jobs, newest first.
    pub fn snapshot(&self) -> Vec<JobInfo> {
        let mut v: Vec<JobInfo> = self.inner.jobs.lock().values().map(|e| e.info.clone()).collect();
        v.sort_by_key(|j| std::cmp::Reverse(j.id));
        v
    }

    /// Drops finished successful jobs from the list.
    pub fn clear_finished(&self) {
        self.inner.jobs.lock().retain(|_, e| e.info.state != JobState::Done && e.info.state != JobState::Cancelled);
    }

    pub fn active_count(&self) -> usize {
        self.inner.jobs.lock().values().filter(|e| !e.info.state.is_finished()).count()
    }

    /// Blocks until `id` finishes (tests / shutdown).
    pub fn wait(&self, id: JobId, timeout: Duration) -> Option<JobState> {
        let deadline = Instant::now() + timeout;
        loop {
            let s = self.info(id)?.state;
            if s.is_finished() {
                return Some(s);
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shutdown.store(true, std::sync::atomic::Ordering::Release);
    }
}

fn worker(inner: Arc<Inner>) {
    loop {
        let q = {
            let mut queue = inner.queue.lock();
            loop {
                if inner.shutdown.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                let now = Instant::now();
                match queue.peek() {
                    Some(top) if top.not_before <= now => break queue.pop().unwrap(),
                    Some(top) => {
                        let wait = top.not_before - now;
                        inner.cv.wait_for(&mut queue, wait);
                    }
                    None => {
                        inner.cv.wait_for(&mut queue, Duration::from_millis(500));
                    }
                }
            }
        };
        let (work, cancel) = {
            let mut jobs = inner.jobs.lock();
            let Some(e) = jobs.get_mut(&q.id) else { continue };
            if e.info.state != JobState::Queued {
                continue; // cancelled while queued
            }
            e.info.state = JobState::Running;
            e.info.attempts += 1;
            (e.work.clone(), e.cancel.clone())
        };
        let ctx = JobCtx { id: q.id, cancel: cancel.clone(), inner: inner.clone() };
        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&ctx)))
            .unwrap_or_else(|_| Err(JobError::Fatal("job panicked".into())));

        let mut requeue = None;
        {
            let mut jobs = inner.jobs.lock();
            let Some(e) = jobs.get_mut(&q.id) else { continue };
            e.info.state = match result {
                Ok(()) => {
                    e.info.progress = 1.0;
                    tracing::debug!(id = q.id, title = %e.info.title, ms = started.elapsed().as_millis() as u64, "job done");
                    JobState::Done
                }
                Err(_) if cancel.is_cancelled() => JobState::Cancelled,
                Err(JobError::Cancelled) => JobState::Cancelled,
                Err(JobError::Retryable(msg)) if e.info.attempts <= e.max_retries => {
                    // Exponential backoff: 0.5 s, 1 s, 2 s, …
                    let backoff = Duration::from_millis(500 << (e.info.attempts - 1).min(6));
                    tracing::info!(id = q.id, %msg, ?backoff, "job failed, retrying");
                    requeue = Some(Instant::now() + backoff);
                    JobState::Queued
                }
                Err(JobError::Retryable(msg) | JobError::Fatal(msg)) => {
                    tracing::warn!(id = q.id, title = %e.info.title, error = %msg, "job failed");
                    JobState::Failed(msg)
                }
            };
        }
        if let Some(at) = requeue {
            inner.queue.lock().push(Queued { priority: q.priority, not_before: at, seq: q.seq, id: q.id });
            inner.cv.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn runs_reports_progress_and_completes() {
        let js = JobSystem::new(2);
        let id = js.submit(JobSpec::new("t", "test"), |ctx| {
            ctx.progress(0.5);
            Ok(())
        });
        assert_eq!(js.wait(id, Duration::from_secs(5)), Some(JobState::Done));
        assert_eq!(js.info(id).unwrap().progress, 1.0);
    }

    #[test]
    fn higher_priority_runs_first() {
        let js = JobSystem::new(1);
        let order = Arc::new(Mutex::new(vec![]));
        // Block the single worker so the rest queue up.
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let g = gate.clone();
        js.submit(JobSpec::new("gate", "t"), move |_| {
            let mut open = g.0.lock();
            while !*open {
                g.1.wait(&mut open);
            }
            Ok(())
        });
        std::thread::sleep(Duration::from_millis(50));
        let mut ids = vec![];
        for (name, p) in [("low", Priority::Background), ("high", Priority::High), ("mid", Priority::Normal)] {
            let o = order.clone();
            ids.push(js.submit(JobSpec::new(name, "t").priority(p), move |_| {
                o.lock().push(name);
                Ok(())
            }));
        }
        *gate.0.lock() = true;
        gate.1.notify_all();
        for id in ids {
            js.wait(id, Duration::from_secs(5));
        }
        assert_eq!(*order.lock(), vec!["high", "mid", "low"]);
    }

    #[test]
    fn retryable_errors_retry_then_fail() {
        let js = JobSystem::new(1);
        let n = Arc::new(AtomicU32::new(0));
        let n2 = n.clone();
        let id = js.submit(JobSpec::new("flaky", "t").retries(2), move |_| {
            n2.fetch_add(1, Ordering::SeqCst);
            Err(JobError::Retryable("busy".into()))
        });
        assert_eq!(js.wait(id, Duration::from_secs(10)), Some(JobState::Failed("busy".into())));
        assert_eq!(n.load(Ordering::SeqCst), 3, "1 try + 2 retries");
        // Manual retry re-runs it.
        assert!(js.retry(id));
        js.wait(id, Duration::from_secs(10));
        assert_eq!(n.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn cancellation_and_panics() {
        let js = JobSystem::new(1);
        let id = js.submit(JobSpec::new("long", "t"), |ctx| {
            for _ in 0..1000 {
                ctx.check_cancelled()?;
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        });
        std::thread::sleep(Duration::from_millis(30));
        js.cancel(id);
        assert_eq!(js.wait(id, Duration::from_secs(5)), Some(JobState::Cancelled));

        let id = js.submit(JobSpec::new("boom", "t"), |_| panic!("bug"));
        assert!(matches!(js.wait(id, Duration::from_secs(5)), Some(JobState::Failed(_))));
    }
}
