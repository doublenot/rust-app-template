//! The job engine: identity, per-repo exclusion, progress, retention.
//!
//! One flat `std::sync::Mutex` guards everything — the job map, the per-repo busy
//! index, the `request_id` index and the terminal queue. `std::sync` and not
//! `tokio::sync` because the only writer of `Progress` is a libgit2 callback running
//! on a blocking thread with no runtime context: it must never `.await`, and a
//! `tokio::sync::Mutex` there would be a bug waiting to happen.
//!
//! Per-repo mutual exclusion is an entry in a `BTreeMap`, not a mutex per repo: no
//! allocation, no lifetime question, no poisoning from a panicking job, and two
//! different repos are parallel for free because they are two different keys.
//!
//! Lock ordering, the one rule this file must never break: `JobStore::inner` may be
//! held while a `JobSlot`'s `live` mutex is taken (retention reads `finished_at_ms`),
//! never the other way round. `succeed`/`fail` touch only `live`; `RepoLease::drop`
//! finishes the slot *before* it takes `inner`.
//!
//! Nothing here names `git2`, `axum`, `tokio` or the filesystem. This is a
//! concurrency primitive over `FnOnce`, and its tests contain zero `sleep` calls.

use crate::git::error::{AbortReason, GitError, GitErrorCode};
use crate::git::util::{system_now, NowFn};
use rand::Rng;
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Terminal job records kept in memory, per host launch.
pub const MAX_JOB_RECORDS: usize = 50;
/// A terminal record older than this is dropped even when the store is under the cap.
pub const JOB_TTL_SECS: u64 = 3_600;
/// A terminal record younger than this is never dropped to make room for a newer one.
pub const JOB_MIN_AGE_SECS: u64 = 60;
/// Minimum gap between two progress writes that do not move `percent`.
pub const PROGRESS_THROTTLE_MS: u64 = 100;

/// `job_<host_instance>_<seq:06x>`, e.g. `job_7c1e93aa_00002b`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct JobId(String);

impl JobId {
    /// `seq` is zero-padded to six hex digits and deliberately **not** truncated if it
    /// outgrows them: an id must stay unique long past the point where it stops being
    /// pretty.
    pub fn new(host_instance: &str, seq: u64) -> JobId {
        JobId(format!("job_{host_instance}_{seq:06x}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The middle field, or `None` when the string is not shaped like one of our ids.
    pub fn instance(&self) -> Option<&str> {
        let (instance, seq) = self.0.strip_prefix("job_")?.split_once('_')?;
        if instance.is_empty() || seq.is_empty() {
            return None;
        }
        Some(instance)
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobOp {
    Init,
    Clone,
    Pull,
    Push,
    Sync,
    Commit,
    Branch,
    Reset,
}

impl JobOp {
    pub fn as_str(self) -> &'static str {
        match self {
            JobOp::Init => "init",
            JobOp::Clone => "clone",
            JobOp::Pull => "pull",
            JobOp::Push => "push",
            JobOp::Sync => "sync",
            JobOp::Commit => "commit",
            JobOp::Branch => "branch",
            JobOp::Reset => "reset",
        }
    }

    /// True when the op can change the files in the working tree — i.e. when a child
    /// process reading out of that tree may be looking at stale bytes afterwards.
    /// `push` and `commit` are the two verbs that leave the checked-out files
    /// byte-identical: one publishes commits that already exist, the other only writes
    /// the index and moves HEAD.
    pub fn is_mutating(self) -> bool {
        !matches!(self, JobOp::Push | JobOp::Commit)
    }
}

/// Three states, not four. There is no `Queued`: a job either exists and is running or
/// was never created, so a queued job is unrepresentable rather than merely unused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Succeeded,
    Failed,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Running => "running",
            JobState::Succeeded => "succeeded",
            JobState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<JobState> {
        match s {
            "running" => Some(JobState::Running),
            "succeeded" => Some(JobState::Succeeded),
            "failed" => Some(JobState::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Cloning,
    Staging,
    Committing,
    Fetching,
    Merging,
    CheckingOut,
    Pushing,
    Finalizing,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Preparing => "preparing",
            Phase::Cloning => "cloning",
            Phase::Staging => "staging",
            Phase::Committing => "committing",
            Phase::Fetching => "fetching",
            Phase::Merging => "merging",
            Phase::CheckingOut => "checking_out",
            Phase::Pushing => "pushing",
            Phase::Finalizing => "finalizing",
        }
    }
}

/// An `AbortReason` in an atomic, so the libgit2 callbacks can read it without a lock.
pub struct AbortFlag(AtomicU8);

impl AbortFlag {
    pub fn new() -> AbortFlag {
        AbortFlag(AtomicU8::new(AbortReason::None.as_u8()))
    }

    /// First writer wins. The watchdog and the quit path race by construction, and the
    /// reason the job actually stopped for is the one that arrived first — that is what
    /// `error::classify` turns into `timeout` versus `canceled`. `None` is not a reason
    /// and never clears an abort.
    pub fn abort(&self, reason: AbortReason) {
        if reason == AbortReason::None {
            return;
        }
        let _ = self.0.compare_exchange(
            AbortReason::None.as_u8(),
            reason.as_u8(),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub fn is_aborted(&self) -> bool {
        self.reason() != AbortReason::None
    }

    pub fn reason(&self) -> AbortReason {
        AbortReason::from_u8(self.0.load(Ordering::SeqCst))
    }
}

impl Default for AbortFlag {
    fn default() -> AbortFlag {
        AbortFlag::new()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub received_objects: u32,
    pub total_objects: u32,
    pub indexed_objects: u32,
    pub received_bytes: u64,
    pub percent: u8,
}

/// Identity and the two lock-free flags, plus everything mutable behind one mutex.
pub struct JobSlot {
    pub id: JobId,
    pub repo_id: String,
    pub op: JobOp,
    pub request_id: Option<String>,
    pub created_at_ms: u64,
    /// Read by the git2 transfer callbacks on the blocking thread.
    pub abort: Arc<AbortFlag>,
    /// Set by the watchdog when the deadline passes but the transfer is still moving.
    pub stalled: Arc<AtomicBool>,
    live: Mutex<JobLive>,
    /// Injected so retention and progress throttling are testable without sleeping.
    now: NowFn,
}

struct JobLive {
    state: JobState,
    phase: Phase,
    progress: Option<Progress>,
    started_at_ms: u64,
    finished_at_ms: Option<u64>,
    result: Option<serde_json::Value>,
    error: Option<GitError>,
    last_progress_write_ms: u64,
}

impl JobSlot {
    fn new(
        id: JobId,
        repo_id: String,
        op: JobOp,
        request_id: Option<String>,
        now: NowFn,
    ) -> JobSlot {
        let created_at_ms = now();
        JobSlot {
            id,
            repo_id,
            op,
            request_id,
            created_at_ms,
            abort: Arc::new(AbortFlag::new()),
            stalled: Arc::new(AtomicBool::new(false)),
            live: Mutex::new(JobLive {
                state: JobState::Running,
                phase: Phase::Preparing,
                progress: None,
                // Seeded from `created_at_ms` and overwritten by `begin`, so a
                // serialized job never carries a null start time.
                started_at_ms: created_at_ms,
                finished_at_ms: None,
                result: None,
                error: None,
                last_progress_write_ms: 0,
            }),
            now,
        }
    }

    /// Recover from poisoning instead of propagating it: every critical section under
    /// this mutex is a handful of field writes with no caller code in it, so a poisoned
    /// lock cannot mean a half-written `JobLive`. Propagating would turn one dead job
    /// into a dead host.
    fn lock(&self) -> MutexGuard<'_, JobLive> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The blocking worker actually started. Under `spawn_blocking` pool pressure this
    /// can be measurably later than admission, which is exactly why both timestamps
    /// exist.
    pub fn begin(&self) {
        let now = (self.now)();
        let mut live = self.lock();
        if live.state != JobState::Running {
            return;
        }
        live.started_at_ms = now;
    }

    pub fn set_phase(&self, phase: Phase) {
        let mut live = self.lock();
        if live.state != JobState::Running {
            return;
        }
        live.phase = phase;
    }

    pub fn phase(&self) -> Phase {
        self.lock().phase
    }

    /// Writes only when `PROGRESS_THROTTLE_MS` has elapsed or `percent` changed.
    /// Returns whether it wrote. A 200k-object fetch calls this 200k times; without the
    /// throttle that is 200k acquisitions of a mutex the HTTP layer also wants.
    pub fn set_progress(&self, progress: Progress) -> bool {
        let now = (self.now)();
        let mut live = self.lock();
        if live.state != JobState::Running {
            return false;
        }
        let same_percent = live.progress.map(|p| p.percent) == Some(progress.percent);
        if same_percent && now.saturating_sub(live.last_progress_write_ms) < PROGRESS_THROTTLE_MS {
            return false;
        }
        live.progress = Some(progress);
        live.last_progress_write_ms = now;
        true
    }

    pub fn succeed(&self, result: &serde_json::Value) {
        self.finish(JobState::Succeeded, Some(result.clone()), None);
    }

    pub fn fail(&self, error: &GitError) {
        self.finish(JobState::Failed, None, Some(error.clone()));
    }

    /// Only `RepoLease::drop` calls this, and only a panicking worker can reach it with
    /// a slot that is still running.
    pub fn finish_if_running(&self, error: GitError) {
        self.finish(JobState::Failed, None, Some(error));
    }

    fn finish(&self, state: JobState, result: Option<serde_json::Value>, error: Option<GitError>) {
        let now = (self.now)();
        let mut live = self.lock();
        // First terminal write wins. `succeed`/`fail` run inside the worker and the
        // lease's `Drop` runs microseconds later on the same thread, so the second
        // call is the normal case, not an error.
        if live.state != JobState::Running {
            return;
        }
        live.state = state;
        live.finished_at_ms = Some(now);
        live.result = result;
        live.error = error;
        // `phase` is deliberately left alone: "failed while fetching" is the single
        // most useful thing a job record can tell a caller, and overwriting it with
        // `finalizing` would erase it.
    }

    pub fn state(&self) -> JobState {
        self.lock().state
    }

    pub fn is_terminal(&self) -> bool {
        self.state() != JobState::Running
    }

    pub fn started_at_ms(&self) -> u64 {
        self.lock().started_at_ms
    }

    pub fn finished_at_ms(&self) -> Option<u64> {
        self.lock().finished_at_ms
    }

    pub fn mark_stalled(&self) {
        self.stalled.store(true, Ordering::SeqCst);
    }

    pub fn view(&self, host_instance: &str) -> JobView {
        let stalled = self.stalled.load(Ordering::SeqCst);
        let live = self.lock();
        JobView {
            id: self.id.clone(),
            host_instance: host_instance.to_string(),
            repo_id: self.repo_id.clone(),
            op: self.op,
            state: live.state,
            phase: live.phase,
            stalled,
            progress: live.progress,
            request_id: self.request_id.clone(),
            created_at_ms: self.created_at_ms,
            started_at_ms: live.started_at_ms,
            finished_at_ms: live.finished_at_ms,
            result: live.result.clone(),
            error: live.error.clone(),
        }
    }

    pub fn as_ref_view(&self) -> JobRef {
        let stalled = self.stalled.load(Ordering::SeqCst);
        let live = self.lock();
        JobRef {
            id: self.id.clone(),
            op: self.op,
            state: live.state,
            stalled,
            started_at_ms: live.started_at_ms,
            request_id: self.request_id.clone(),
        }
    }
}

/// The full job body: `{"job": JobView}`.
#[derive(Debug, Clone, Serialize)]
pub struct JobView {
    pub id: JobId,
    pub host_instance: String,
    pub repo_id: String,
    pub op: JobOp,
    pub state: JobState,
    pub phase: Phase,
    pub stalled: bool,
    pub progress: Option<Progress>,
    pub request_id: Option<String>,
    pub created_at_ms: u64,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub result: Option<serde_json::Value>,
    pub error: Option<GitError>,
}

/// The trimmed form embedded in `error.job` and `status.busy_job`.
#[derive(Debug, Clone, Serialize)]
pub struct JobRef {
    pub id: JobId,
    pub op: JobOp,
    pub state: JobState,
    pub stalled: bool,
    pub started_at_ms: u64,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct JobFilter {
    pub repo_id: Option<String>,
    pub state: Option<JobState>,
    /// `0` means unlimited; the HTTP layer always passes a real cap.
    pub limit: usize,
}

pub enum Admission {
    /// The caller must run the op. The repo is released when the lease drops.
    Started(Arc<JobSlot>, RepoLease),
    /// A matching `request_id` already ran or is running: 200, not 202.
    Replay(Arc<JobSlot>),
    /// Someone else holds the repo: 409 with this job embedded.
    Busy(Arc<JobSlot>),
}

impl Admission {
    pub fn slot(&self) -> &Arc<JobSlot> {
        match self {
            Admission::Started(slot, _) | Admission::Replay(slot) | Admission::Busy(slot) => slot,
        }
    }
}

/// Exclusive hold on one repo. There is no public constructor: the only way to get one
/// is `JobStore::admit`, and the only thing you can do with it is drop it.
pub struct RepoLease {
    store: Arc<JobStore>,
    repo_id: String,
    job: Arc<JobSlot>,
}

impl RepoLease {
    pub fn job(&self) -> &Arc<JobSlot> {
        &self.job
    }
}

impl Drop for RepoLease {
    fn drop(&mut self) {
        // A non-terminal slot here means the worker closure panicked and is unwinding:
        // every normal path calls `succeed` or `fail` first, so recording `internal` is
        // a fact about what happened, not a guess.
        self.job
            .finish_if_running(GitError::internal("git worker terminated unexpectedly"));
        let now = (self.store.now)();
        let mut inner = self.store.lock();
        // Only clear the entry if it is still ours: `forget_repo` may have dropped it,
        // and clearing a *different* holder's entry — another job's, or a `RepoHold`'s —
        // would break exclusion. `Inner::drop_job` applies the same test to
        // `by_request`; the two are one rule about two indexes.
        if matches!(inner.busy.get(&self.repo_id), Some(BusyBy::Job(id)) if *id == self.job.id) {
            inner.busy.remove(&self.repo_id);
        }
        inner.terminal.push_back(self.job.id.clone());
        inner.evict(now);
    }
}

/// The host itself holds the repo: a `PUT` or `DELETE` is rewriting the registry entry
/// and, on `DELETE`, removing the tree.
///
/// Not `repo_busy`, which embeds the running job in `error.job` and which `api.rs`
/// documents a client as keying off: a maintenance hold has no job to embed, and
/// inventing a job-shaped thing to point at would put an unpollable record with no
/// progress and no result into `GET /api/git/jobs`. `RepoLocked` is 409/retryable, which
/// is honest here — the hold is released before the call that took it returns.
///
/// Lives here rather than beside `error::repo_busy` only to keep this change set's file
/// sets disjoint; it belongs there once the three groups have landed.
fn repo_locked(repo_id: &str) -> GitError {
    GitError::new(
        GitErrorCode::RepoLocked,
        format!("repo {repo_id:?} is locked while its definition or tree is being changed"),
    )
    .with_repo(repo_id)
}

/// Exclusive hold on one repo taken by the host itself rather than by a job.
///
/// `put_repo` and `delete_repo` have to exclude jobs for the length of a whole call:
/// `delete_repo` drops the registry entry and then the tree, and libgit2 writing into
/// that directory while `remove_dir_all` walks it is a half-deleted repository. Sampling
/// `busy()` only proves the repo was idle a moment ago; this occupies the same map entry
/// `admit` takes, so the two are decided by one acquisition of `inner`.
#[must_use = "the repo is released when this is dropped; bind it for the whole call"]
pub struct RepoHold {
    store: Arc<JobStore>,
    repo_id: String,
}

impl Drop for RepoHold {
    fn drop(&mut self) {
        let mut inner = self.store.lock();
        // Same ownership guard as `RepoLease::drop`, defensively: nothing today can
        // replace a hold with another holder, and this is what keeps a future path that
        // can from having this drop clear a job's lock.
        if matches!(inner.busy.get(&self.repo_id), Some(BusyBy::Maintenance)) {
            inner.busy.remove(&self.repo_id);
        }
    }
}

pub struct JobStore {
    inner: Mutex<Inner>,
    instance: String,
    seq: AtomicU64,
    now: NowFn,
    draining: AtomicBool,
}

/// Who holds a repo. Two arms of one value rather than two maps: `busy` is THE lock, and
/// a reader that pattern-matches on it cannot forget that the host itself takes repos
/// too — a second `maintenance` set could grow a hole in the exclusion with nobody
/// noticing, whereas changing this map's value type makes every missed reader a compile
/// error.
enum BusyBy {
    /// A running job. `Admission::Busy` hands it back and `status.busy_job` reports it.
    Job(JobId),
    /// `put_repo`/`delete_repo`: the host rewriting the registry entry, and on DELETE
    /// removing the tree. There is no job to embed, which is why an admission blocked by
    /// one is an `Err(repo_locked)` and not an `Admission`.
    Maintenance,
}

#[derive(Default)]
struct Inner {
    jobs: BTreeMap<JobId, Arc<JobSlot>>,
    /// Oldest first. Live jobs are never in here, which is what makes rule 3 free.
    terminal: VecDeque<JobId>,
    /// repo_id -> whoever holds it. THE LOCK.
    busy: BTreeMap<String, BusyBy>,
    by_request: BTreeMap<(String, String), JobId>,
}

impl Inner {
    /// How long ago a record finished, or `None` when the slot is already gone.
    fn terminal_age_ms(&self, id: &JobId, now: u64) -> Option<u64> {
        let slot = self.jobs.get(id)?;
        Some(now.saturating_sub(slot.finished_at_ms().unwrap_or(slot.created_at_ms)))
    }

    /// Retention. Runs on every admission, every list, and every lookup.
    ///
    /// Rules 3 and 4 are why this is not a plain `while len > cap { pop_front() }`: a
    /// live job is never in `terminal` at all, and a terminal job younger than
    /// `JOB_MIN_AGE_SECS` is skipped even when we are over the cap — otherwise a burst
    /// of auto-syncs could evict a record between a client's 202 and its first poll.
    fn evict(&mut self, now: u64) {
        let ttl_ms = JOB_TTL_SECS.saturating_mul(1_000);
        let min_age_ms = JOB_MIN_AGE_SECS.saturating_mul(1_000);

        let expired: Vec<JobId> = self
            .terminal
            .iter()
            .filter(|id| {
                self.terminal_age_ms(id, now)
                    .is_none_or(|age| age >= ttl_ms)
            })
            .cloned()
            .collect();
        for id in expired {
            self.drop_job(&id);
        }

        while self.terminal.len() > MAX_JOB_RECORDS {
            let oldest_evictable = self
                .terminal
                .iter()
                .find(|id| {
                    self.terminal_age_ms(id, now)
                        .is_none_or(|age| age >= min_age_ms)
                })
                .cloned();
            let Some(id) = oldest_evictable else {
                // Everything left is younger than the floor: rule 4 beats rule 2 and
                // the store is allowed to run over the cap until they age out.
                break;
            };
            self.drop_job(&id);
        }
    }

    fn drop_job(&mut self, id: &JobId) {
        self.terminal.retain(|q| q != id);
        if let Some(slot) = self.jobs.remove(id) {
            // Rule 5: the id becomes reusable the moment its record goes — but only
            // while the index still names *this* job. `admit` re-points the key at a
            // fresh job when a failed op is retried under the same `request_id`, and the
            // superseded record stays here until retention takes it, so evicting it must
            // not delete the successor's entry: that would answer the next matching
            // retry with the 409 the replay-before-busy rule promises is impossible.
            // Same ownership test `RepoLease::drop` applies to `busy`, for the same
            // reason — the two are one rule about two indexes, not a coincidence.
            //
            // Sound only because `jobs` is keyed by `JobId` and `slot.id == *id`; a
            // future change that files a slot under some other key must compare against
            // `Some(&slot.id)` instead.
            if let Some(rid) = slot.request_id.clone() {
                let key = (slot.repo_id.clone(), rid);
                if self.by_request.get(&key) == Some(id) {
                    self.by_request.remove(&key);
                }
            }
        }
    }
}

impl JobStore {
    pub fn new(host_instance: String) -> Arc<JobStore> {
        JobStore::with_clock(host_instance, system_now())
    }

    pub fn with_clock(host_instance: String, now: NowFn) -> Arc<JobStore> {
        Arc::new(JobStore {
            inner: Mutex::new(Inner::default()),
            instance: host_instance,
            // Sequences start at 1 so `job_<inst>_000000` never appears and an id read
            // out of a log is obviously one-based.
            seq: AtomicU64::new(1),
            now,
            draining: AtomicBool::new(false),
        })
    }

    /// See `JobSlot::lock` — same reasoning, same recovery.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn host_instance(&self) -> &str {
        &self.instance
    }

    /// Admission, all of it inside one lock.
    ///
    /// `&Arc<Self>` rather than `&self` because the `RepoLease` this hands out owns an
    /// `Arc<JobStore>` — it has to outlive the borrow.
    pub fn admit(
        self: &Arc<Self>,
        repo_id: &str,
        op: JobOp,
        request_id: Option<&str>,
    ) -> Result<Admission, GitError> {
        let now = (self.now)();
        let mut inner = self.lock();

        // Inside the lock with the busy check on purpose: a `set_draining` racing an
        // `admit` must not be able to let one last job through the gate.
        if self.draining.load(Ordering::SeqCst) {
            return Err(GitError::shutting_down());
        }

        // GC first, so a replay can never resolve against a record this very call is
        // about to evict.
        inner.evict(now);

        // Replay BEFORE busy. A matching `request_id` can never come back as 409,
        // which is what makes "my request timed out, is it safe to retry?"
        // answerable: a 409 always means *someone else* holds the repo.
        if let Some(rid) = request_id {
            let key = (repo_id.to_string(), rid.to_string());
            let previous = inner
                .by_request
                .get(&key)
                .and_then(|id| inner.jobs.get(id))
                .cloned();
            match previous {
                Some(slot) if slot.state() != JobState::Failed => {
                    // Running: this is the dropped-response case. Succeeded: the effect
                    // already happened. Either way, the same job, 200 rather than 202.
                    return Ok(Admission::Replay(slot));
                }
                // Retrying a *failed* op under the same id must work — it is the
                // natural client behaviour, and a mechanism that locks it out is worse
                // than no mechanism. Consume the entry and fall through to a fresh job.
                _ => {
                    inner.by_request.remove(&key);
                }
            }
        }

        // Busy check. The map entry is the lock, not the job's state: `after_job` still
        // has work to do after a slot goes terminal, and a second libgit2 operation on
        // the same working tree would corrupt the index.
        let busy_id = match inner.busy.get(repo_id) {
            Some(BusyBy::Job(busy_id)) => Some(busy_id.clone()),
            // A `RepoHold` has no job to hand back, so this is the one refusal here that
            // is an `Err` rather than an `Admission`: the repo is mid-PUT or mid-DELETE
            // and the honest answer is "try again", which is what `repo_locked` means.
            Some(BusyBy::Maintenance) => return Err(repo_locked(repo_id)),
            None => None,
        };
        if let Some(busy_id) = busy_id {
            match inner.jobs.get(&busy_id).cloned() {
                Some(busy) => return Ok(Admission::Busy(busy)),
                // A busy entry with no slot behind it is not a lock; heal it.
                None => {
                    inner.busy.remove(repo_id);
                }
            }
        }

        // Insert and mark busy.
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let slot = Arc::new(JobSlot::new(
            JobId::new(&self.instance, seq),
            repo_id.to_string(),
            op,
            request_id.map(str::to_string),
            self.now.clone(),
        ));
        inner.jobs.insert(slot.id.clone(), slot.clone());
        inner
            .busy
            .insert(repo_id.to_string(), BusyBy::Job(slot.id.clone()));
        if let Some(rid) = request_id {
            inner
                .by_request
                .insert((repo_id.to_string(), rid.to_string()), slot.id.clone());
        }
        drop(inner);

        let lease = RepoLease {
            store: self.clone(),
            repo_id: repo_id.to_string(),
            job: slot.clone(),
        };
        Ok(Admission::Started(slot, lease))
    }

    /// Take a repo for the host itself. `repo_busy` when a job holds it — the same 409 a
    /// second job gets, naming the op the caller can see running — and `repo_locked`
    /// when another maintenance call already has it.
    ///
    /// Deliberately not a `JobOp` admitted through `admit`: a DELETE has no progress, no
    /// result and nothing to poll, and a job record for it would outlive the repo it
    /// names. It reads and writes the same map entry under the same one acquisition of
    /// `inner`, which is the whole of the exclusion.
    ///
    /// No `draining` check, unlike `admit`: it would turn DELETE into 503 during quit for
    /// no benefit. DELETE is idempotent, and exclusion against the quit-syncs already
    /// comes from their leases.
    ///
    /// `&Arc<Self>` for `admit`'s reason: the `RepoHold` owns an `Arc<JobStore>`.
    pub fn hold_repo(self: &Arc<Self>, repo_id: &str) -> Result<RepoHold, GitError> {
        let mut inner = self.lock();
        let busy_op = match inner.busy.get(repo_id) {
            Some(BusyBy::Job(id)) => inner.jobs.get(id).map(|slot| slot.op),
            Some(BusyBy::Maintenance) => return Err(repo_locked(repo_id)),
            None => None,
        };
        if let Some(op) = busy_op {
            return Err(GitError::repo_busy(repo_id, op.as_str()));
        }
        // Falling through overwrites a `Job` entry with no slot behind it: that is not a
        // lock, and `admit` heals it the same way.
        inner.busy.insert(repo_id.to_string(), BusyBy::Maintenance);
        drop(inner);
        Ok(RepoHold {
            store: self.clone(),
            repo_id: repo_id.to_string(),
        })
    }

    pub fn get(&self, id: &JobId) -> Option<Arc<JobSlot>> {
        self.lock().jobs.get(id).cloned()
    }

    /// Parse and resolve a raw id from the wire.
    ///
    /// An id whose instance prefix is not ours is rejected without touching the map: a
    /// child that survived a host restart must never be answered about a *different*
    /// job that happens to share a sequence number.
    ///
    /// GCs before it answers, for the same reason `list` does: without that,
    /// `GET /api/git/jobs/{id}` would hand back a record that `GET /api/git/jobs`
    /// already reports as gone, and the two endpoints would disagree about what exists.
    pub fn lookup(&self, raw: &str) -> Result<Arc<JobSlot>, GitError> {
        let id = JobId(raw.to_string());
        if id.instance() != Some(self.instance.as_str()) {
            return Err(GitError::job_not_found(raw));
        }
        let now = (self.now)();
        let mut inner = self.lock();
        inner.evict(now);
        inner
            .jobs
            .get(&id)
            .cloned()
            .ok_or_else(|| GitError::job_not_found(raw))
    }

    /// Newest first. GCs before it reads, so a list and a poll agree about what exists.
    pub fn list(&self, filter: &JobFilter) -> Vec<Arc<JobSlot>> {
        let now = (self.now)();
        let mut inner = self.lock();
        inner.evict(now);
        let mut out: Vec<Arc<JobSlot>> = inner
            .jobs
            .values()
            .filter(|s| {
                filter
                    .repo_id
                    .as_deref()
                    .is_none_or(|repo_id| s.repo_id == repo_id)
            })
            .filter(|s| filter.state.is_none_or(|state| s.state() == state))
            .cloned()
            .collect();
        drop(inner);
        // Two jobs admitted in the same millisecond tie on `created_at_ms` (and always
        // tie under an injected clock), so the sequence embedded in the id breaks it.
        out.sort_by(|a, b| {
            b.created_at_ms
                .cmp(&a.created_at_ms)
                .then_with(|| b.id.cmp(&a.id))
        });
        if filter.limit > 0 {
            out.truncate(filter.limit);
        }
        out
    }

    pub fn busy(&self, repo_id: &str) -> Option<Arc<JobSlot>> {
        let inner = self.lock();
        match inner.busy.get(repo_id)? {
            BusyBy::Job(id) => inner.jobs.get(id).cloned(),
            // A hold is not a job: `status.busy_job` and `error.job` are job references
            // and there is nothing to point them at while the host owns the repo. Which
            // is exactly why nothing may use this to decide whether a repo is free —
            // see `hold_repo`.
            BusyBy::Maintenance => None,
        }
    }

    pub fn live_jobs(&self) -> Vec<Arc<JobSlot>> {
        self.lock()
            .jobs
            .values()
            .filter(|s| !s.is_terminal())
            .cloned()
            .collect()
    }

    pub fn set_draining(&self, draining: bool) {
        self.draining.store(draining, Ordering::SeqCst);
    }

    pub fn draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    pub fn abort_all(&self, reason: AbortReason) {
        for slot in self.live_jobs() {
            slot.abort.abort(reason);
        }
    }

    /// Drop every *terminal* record for a repo, on DELETE.
    ///
    /// A live job is left alone: its lease still owns the busy entry, and taking that
    /// away would let a second worker into the same working tree. DELETE on a busy repo
    /// is a 409 for exactly this reason.
    pub fn forget_repo(&self, repo_id: &str) {
        let mut inner = self.lock();
        let doomed: Vec<JobId> = inner
            .terminal
            .iter()
            .filter(|id| {
                inner
                    .jobs
                    .get(*id)
                    .is_some_and(|slot| slot.repo_id == repo_id)
            })
            .cloned()
            .collect();
        for id in doomed {
            inner.drop_job(&id);
        }
    }
}

/// Eight lowercase hex chars, minted once per host launch.
pub fn random_host_instance() -> String {
    let mut rng = rand::rng();
    format!("{:08x}", rng.random::<u32>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::Condvar;
    use std::thread;

    const INSTANCE: &str = "7c1e93aa";

    /// A clock the test moves by hand. With one of these installed nothing in this file
    /// reads the wall clock, which is what makes every retention and throttling
    /// assertion exact instead of timing-dependent.
    #[derive(Clone)]
    struct TestClock(Arc<Mutex<u64>>);

    impl TestClock {
        fn new(start_ms: u64) -> TestClock {
            TestClock(Arc::new(Mutex::new(start_ms)))
        }

        fn now_fn(&self) -> NowFn {
            let cell = self.0.clone();
            Arc::new(move || *cell.lock().expect("test clock"))
        }

        fn advance(&self, ms: u64) {
            *self.0.lock().expect("test clock") += ms;
        }

        fn set(&self, ms: u64) {
            *self.0.lock().expect("test clock") = ms;
        }
    }

    fn test_slot(clock: &TestClock) -> Arc<JobSlot> {
        Arc::new(JobSlot::new(
            JobId::new(INSTANCE, 1),
            "notes".to_string(),
            JobOp::Sync,
            None,
            clock.now_fn(),
        ))
    }

    /// A rendezvous between the test thread and a worker thread holding a `RepoLease`.
    ///
    /// Every wait below is a condvar or a channel block that the other side wakes, so
    /// no test in this file sleeps: they are deterministic under any scheduler and cost
    /// no wall-clock time.
    struct Gate {
        entered: Mutex<usize>,
        cv: Condvar,
        tx: mpsc::Sender<()>,
        rx: Mutex<mpsc::Receiver<()>>,
    }

    impl Gate {
        fn new() -> Arc<Gate> {
            let (tx, rx) = mpsc::channel();
            Arc::new(Gate {
                entered: Mutex::new(0),
                cv: Condvar::new(),
                tx,
                rx: Mutex::new(rx),
            })
        }

        /// Worker side: announce arrival, then park until the test hands out a token.
        ///
        /// The counter is bumped *before* parking, so `wait_entered(2)` can return with
        /// the second worker still queued on the receiver mutex — which is the state
        /// the tests actually assert on: both jobs admitted, both leases held.
        fn enter(&self) {
            *self.entered.lock().expect("gate count") += 1;
            self.cv.notify_all();
            let rx = self.rx.lock().expect("gate rx");
            rx.recv().expect("gate token");
        }

        /// Test side: block until `n` workers have reached the gate.
        fn wait_entered(&self, n: usize) {
            let mut count = self.entered.lock().expect("gate count");
            while *count < n {
                count = self.cv.wait(count).expect("gate wait");
            }
        }

        /// Test side: let `n` parked workers through.
        fn release(&self, n: usize) {
            for _ in 0..n {
                self.tx.send(()).expect("gate send");
            }
        }
    }

    #[derive(Clone, Copy)]
    enum Finish {
        Succeed,
        Fail,
        Panic,
    }

    fn store(clock: &TestClock) -> Arc<JobStore> {
        JobStore::with_clock(INSTANCE.to_string(), clock.now_fn())
    }

    /// Admit a job, assert it started, and run it on a worker thread that parks in the
    /// gate. The lease is moved into the thread, so the repo stays busy for exactly as
    /// long as the worker is parked.
    fn start_gated(
        store: &Arc<JobStore>,
        gate: &Arc<Gate>,
        repo: &str,
        op: JobOp,
        request_id: Option<&str>,
        finish: Finish,
    ) -> (JobId, thread::JoinHandle<()>) {
        let Admission::Started(slot, lease) = store.admit(repo, op, request_id).expect("admit")
        else {
            panic!("expected Started for {repo}");
        };
        let id = slot.id.clone();
        assert_eq!(
            lease.job().id,
            id,
            "the lease holds the repo for exactly this job"
        );
        let gate = gate.clone();
        let handle = thread::spawn(move || {
            let _lease = lease;
            slot.begin();
            gate.enter();
            match finish {
                Finish::Succeed => slot.succeed(&serde_json::json!({"outcome": "up_to_date"})),
                Finish::Fail => slot.fail(&GitError::internal("boom")),
                // The default panic hook prints this to stderr; that output is expected
                // and is not a test failure.
                Finish::Panic => panic!("worker exploded"),
            }
        });
        (id, handle)
    }

    /// Admit and finish a job inline, dropping the lease immediately. No thread and no
    /// gate: retention only cares about terminal records.
    fn finish_now(store: &Arc<JobStore>, repo: &str, request_id: Option<&str>) -> JobId {
        let Admission::Started(slot, lease) =
            store.admit(repo, JobOp::Sync, request_id).expect("admit")
        else {
            panic!("expected Started for {repo}");
        };
        slot.succeed(&serde_json::json!({"outcome": "up_to_date"}));
        drop(lease);
        slot.id.clone()
    }

    /// The same for a job that *fails*: the failed record is the one the retry contract
    /// is written for, and the one retention later evicts out from under its successor.
    fn fail_now(store: &Arc<JobStore>, repo: &str, request_id: Option<&str>) -> JobId {
        let Admission::Started(slot, lease) =
            store.admit(repo, JobOp::Sync, request_id).expect("admit")
        else {
            panic!("expected Started for {repo}");
        };
        slot.fail(&GitError::internal("boom"));
        drop(lease);
        slot.id.clone()
    }

    fn ids(slots: &[Arc<JobSlot>]) -> Vec<JobId> {
        slots.iter().map(|s| s.id.clone()).collect()
    }

    #[test]
    fn job_id_is_the_documented_format() {
        assert_eq!(JobId::new(INSTANCE, 43).as_str(), "job_7c1e93aa_00002b");
        assert_eq!(JobId::new(INSTANCE, 1).to_string(), "job_7c1e93aa_000001");
        // Six digits is padding, not a cap.
        assert_eq!(
            JobId::new(INSTANCE, 0x123_4567).as_str(),
            "job_7c1e93aa_1234567"
        );
        assert_eq!(JobId::new(INSTANCE, 43).instance(), Some(INSTANCE));
        assert_eq!(JobId("nonsense".to_string()).instance(), None);
        assert_eq!(JobId("job_7c1e93aa".to_string()).instance(), None);
        assert_eq!(JobId("job__000001".to_string()).instance(), None);
    }

    #[test]
    fn random_host_instance_is_eight_lowercase_hex_chars() {
        let instance = random_host_instance();
        assert_eq!(instance.len(), 8);
        assert!(instance
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
        assert_eq!(
            JobId::new(&instance, 1).instance(),
            Some(instance.as_str()),
            "whatever this mints must round-trip through JobId"
        );
    }

    #[test]
    fn the_wire_vocabulary_is_fixed() {
        for (op, s) in [
            (JobOp::Init, "init"),
            (JobOp::Clone, "clone"),
            (JobOp::Pull, "pull"),
            (JobOp::Push, "push"),
            (JobOp::Sync, "sync"),
            (JobOp::Commit, "commit"),
            (JobOp::Branch, "branch"),
            (JobOp::Reset, "reset"),
        ] {
            assert_eq!(op.as_str(), s);
            assert_eq!(serde_json::to_value(op).expect("op"), s);
        }
        for (state, s) in [
            (JobState::Running, "running"),
            (JobState::Succeeded, "succeeded"),
            (JobState::Failed, "failed"),
        ] {
            assert_eq!(state.as_str(), s);
            assert_eq!(JobState::parse(s), Some(state));
            assert_eq!(serde_json::to_value(state).expect("state"), s);
        }
        assert_eq!(JobState::parse("Running"), None);
        for (phase, s) in [
            (Phase::Preparing, "preparing"),
            (Phase::Cloning, "cloning"),
            (Phase::Staging, "staging"),
            (Phase::Committing, "committing"),
            (Phase::Fetching, "fetching"),
            (Phase::Merging, "merging"),
            (Phase::CheckingOut, "checking_out"),
            (Phase::Pushing, "pushing"),
            (Phase::Finalizing, "finalizing"),
        ] {
            assert_eq!(phase.as_str(), s);
            assert_eq!(serde_json::to_value(phase).expect("phase"), s);
        }
        // Only these two leave the checked-out files byte-identical.
        assert!(!JobOp::Push.is_mutating());
        assert!(!JobOp::Commit.is_mutating());
        assert!(JobOp::Sync.is_mutating());
        assert!(JobOp::Reset.is_mutating());
    }

    #[test]
    fn abort_flag_keeps_the_first_reason() {
        let flag = AbortFlag::new();
        assert!(!flag.is_aborted());
        assert_eq!(flag.reason(), AbortReason::None);

        flag.abort(AbortReason::Timeout);
        flag.abort(AbortReason::Shutdown);
        assert!(flag.is_aborted());
        assert_eq!(
            flag.reason(),
            AbortReason::Timeout,
            "the watchdog fired first, so the job ends as `timeout`, not `canceled`"
        );

        flag.abort(AbortReason::None);
        assert_eq!(flag.reason(), AbortReason::Timeout);
    }

    #[test]
    fn a_terminal_slot_never_mutates_again() {
        let clock = TestClock::new(1_000);
        let slot = test_slot(&clock);
        assert_eq!(slot.state(), JobState::Running);
        assert_eq!(
            slot.started_at_ms(),
            1_000,
            "seeded from created_at_ms so it is never null"
        );
        assert_eq!(slot.finished_at_ms(), None);

        clock.set(2_000);
        slot.begin();
        slot.set_phase(Phase::Fetching);
        assert_eq!(slot.started_at_ms(), 2_000);
        assert_eq!(slot.phase(), Phase::Fetching);

        clock.set(3_000);
        slot.succeed(&serde_json::json!({"outcome": "up_to_date"}));
        assert!(slot.is_terminal());
        assert_eq!(slot.state(), JobState::Succeeded);
        assert_eq!(slot.finished_at_ms(), Some(3_000));

        // The lease's Drop path runs on every job, including the ones that finished
        // cleanly, so it has to be a no-op here.
        clock.set(4_000);
        slot.finish_if_running(GitError::internal("late"));
        slot.begin();
        slot.set_phase(Phase::Cloning);

        let view = slot.view(INSTANCE);
        assert_eq!(view.state, JobState::Succeeded);
        assert_eq!(view.finished_at_ms, Some(3_000));
        assert_eq!(view.started_at_ms, 2_000);
        assert_eq!(
            view.phase,
            Phase::Fetching,
            "the last live phase is diagnostic and is kept"
        );
        assert!(view.error.is_none());
        assert!(view.result.is_some(), "result.is_some() <=> Succeeded");
    }

    #[test]
    fn a_failed_slot_carries_an_error_and_no_result() {
        let clock = TestClock::new(1_000);
        let slot = test_slot(&clock);
        clock.set(5_000);
        slot.fail(&GitError::internal("boom"));

        let view = slot.view(INSTANCE);
        assert_eq!(view.state, JobState::Failed);
        assert_eq!(view.finished_at_ms, Some(5_000));
        assert!(view.result.is_none(), "result.is_some() <=> Succeeded");
        assert_eq!(view.error.expect("error").code().as_str(), "internal");
    }

    #[test]
    fn views_carry_the_wire_shape() {
        let clock = TestClock::new(1_000);
        let slot = Arc::new(JobSlot::new(
            JobId::new(INSTANCE, 43),
            "notes".to_string(),
            JobOp::Sync,
            Some("boot-1".to_string()),
            clock.now_fn(),
        ));
        slot.mark_stalled();

        let body = serde_json::to_value(slot.view(INSTANCE)).expect("serialize");
        assert_eq!(body["id"], "job_7c1e93aa_00002b");
        assert_eq!(body["host_instance"], INSTANCE);
        assert_eq!(body["repo_id"], "notes");
        assert_eq!(body["op"], "sync");
        assert_eq!(body["state"], "running");
        assert_eq!(body["phase"], "preparing");
        assert_eq!(body["stalled"], true);
        assert_eq!(body["request_id"], "boot-1");
        assert_eq!(body["created_at_ms"], 1_000);
        assert_eq!(body["started_at_ms"], 1_000);
        assert!(body["progress"].is_null());
        assert!(body["finished_at_ms"].is_null());

        let embedded = slot.as_ref_view();
        assert_eq!(embedded.id.as_str(), "job_7c1e93aa_00002b");
        assert_eq!(embedded.op, JobOp::Sync);
        assert_eq!(embedded.state, JobState::Running);
        assert!(embedded.stalled);
        assert_eq!(embedded.started_at_ms, 1_000);
        assert_eq!(embedded.request_id.as_deref(), Some("boot-1"));
    }

    #[test]
    fn progress_is_throttled_but_percent_changes_always_land() {
        let clock = TestClock::new(1_000);
        let slot = test_slot(&clock);
        let p = |percent: u8, received: u32| Progress {
            received_objects: received,
            total_objects: 100,
            indexed_objects: received,
            received_bytes: u64::from(received) * 1_024,
            percent,
        };

        assert!(slot.set_progress(p(1, 1)), "the first write always lands");
        assert!(
            !slot.set_progress(p(1, 2)),
            "same percent inside the window is dropped"
        );
        assert!(
            slot.set_progress(p(2, 3)),
            "a percent change is never throttled"
        );
        assert!(!slot.set_progress(p(2, 4)));

        clock.advance(PROGRESS_THROTTLE_MS);
        assert!(slot.set_progress(p(2, 5)), "the window reopened");

        let progress = slot.view(INSTANCE).progress.expect("progress");
        assert_eq!(progress.received_objects, 5);
        assert_eq!(progress.percent, 2);

        // A terminal slot never mutates, progress included.
        slot.succeed(&serde_json::json!({"outcome": "up_to_date"}));
        clock.advance(PROGRESS_THROTTLE_MS * 10);
        assert!(!slot.set_progress(p(100, 100)));
        assert_eq!(
            slot.view(INSTANCE).progress.expect("progress").percent,
            2,
            "the last live progress is what the record keeps"
        );
    }

    #[test]
    fn second_op_on_a_busy_repo_is_refused_with_the_running_job() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let gate = Gate::new();
        let (a_id, worker) =
            start_gated(&store, &gate, "notes", JobOp::Sync, None, Finish::Succeed);
        gate.wait_entered(1);

        let refused = store.admit("notes", JobOp::Pull, None).expect("admit");
        assert_eq!(refused.slot().id, a_id);
        let Admission::Busy(busy) = refused else {
            panic!("expected Busy while the lease is held");
        };
        assert_eq!(busy.id, a_id);
        assert_eq!(busy.op, JobOp::Sync);
        assert_eq!(busy.state(), JobState::Running);
        assert_eq!(store.busy("notes").expect("busy").id, a_id);

        // The 409 body api.rs will build out of it.
        let err = GitError::repo_busy("notes", busy.op.as_str());
        assert_eq!(err.code().as_str(), "repo_busy");
        assert_eq!(busy.as_ref_view().id, a_id);

        gate.release(1);
        worker.join().expect("worker");

        assert_eq!(store.get(&a_id).expect("a").state(), JobState::Succeeded);
        assert!(store.busy("notes").is_none());
        let Admission::Started(_, lease) = store.admit("notes", JobOp::Pull, None).expect("admit")
        else {
            panic!("the repo is admissible again once the lease drops");
        };
        drop(lease);
    }

    #[test]
    fn different_repos_run_in_parallel() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let gate = Gate::new();
        let (a_id, wa) = start_gated(&store, &gate, "alpha", JobOp::Sync, None, Finish::Succeed);
        let (b_id, wb) = start_gated(&store, &gate, "beta", JobOp::Sync, None, Finish::Succeed);

        // Both report entered before either is released: the busy map keys on repo_id,
        // so two repos are never serialised against each other.
        gate.wait_entered(2);
        assert_eq!(store.live_jobs().len(), 2);
        assert_eq!(store.busy("alpha").expect("alpha").id, a_id);
        assert_eq!(store.busy("beta").expect("beta").id, b_id);

        gate.release(2);
        wa.join().expect("alpha worker");
        wb.join().expect("beta worker");
        assert!(store.busy("alpha").is_none());
        assert!(store.busy("beta").is_none());
        assert!(store.live_jobs().is_empty());
    }

    #[test]
    fn lease_is_released_when_the_worker_panics() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let gate = Gate::new();
        let (a_id, worker) = start_gated(&store, &gate, "notes", JobOp::Sync, None, Finish::Panic);
        gate.wait_entered(1);
        gate.release(1);
        assert!(worker.join().is_err(), "the worker panicked");

        let slot = store.get(&a_id).expect("the record survives the panic");
        assert_eq!(slot.state(), JobState::Failed);
        assert_eq!(
            slot.view(INSTANCE)
                .error
                .expect("Drop recorded an error")
                .code()
                .as_str(),
            "internal"
        );
        assert!(store.busy("notes").is_none());
        let Admission::Started(_, lease) = store.admit("notes", JobOp::Sync, None).expect("admit")
        else {
            panic!("the repo must be admissible again");
        };
        drop(lease);
    }

    #[test]
    fn replay_precedes_busy() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let gate = Gate::new();
        let (a_id, worker) = start_gated(
            &store,
            &gate,
            "notes",
            JobOp::Sync,
            Some("r1"),
            Finish::Succeed,
        );
        gate.wait_entered(1);

        let Admission::Busy(other) = store
            .admit("notes", JobOp::Sync, Some("r2"))
            .expect("admit")
        else {
            panic!("a different request_id sees the repo lock");
        };
        assert_eq!(other.id, a_id);

        let Admission::Replay(same) = store
            .admit("notes", JobOp::Sync, Some("r1"))
            .expect("admit")
        else {
            panic!("the same request_id must replay, never 409");
        };
        assert_eq!(same.id, a_id);

        gate.release(1);
        worker.join().expect("worker");

        // Succeeded replays too: the effect already happened.
        let Admission::Replay(done) = store
            .admit("notes", JobOp::Sync, Some("r1"))
            .expect("admit")
        else {
            panic!("a succeeded job replays");
        };
        assert_eq!(done.id, a_id);
    }

    #[test]
    fn replay_of_a_failed_job_admits_a_fresh_one() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let gate = Gate::new();
        let (a_id, worker) = start_gated(
            &store,
            &gate,
            "notes",
            JobOp::Sync,
            Some("r1"),
            Finish::Fail,
        );
        gate.wait_entered(1);
        gate.release(1);
        worker.join().expect("worker");
        assert_eq!(store.get(&a_id).expect("a").state(), JobState::Failed);

        let Admission::Started(fresh, lease) = store
            .admit("notes", JobOp::Sync, Some("r1"))
            .expect("admit")
        else {
            panic!("retrying a failed op under the same request_id must work");
        };
        assert_ne!(fresh.id, a_id);
        drop(lease);
    }

    #[test]
    fn lookup_rejects_a_foreign_host_instance() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let id = finish_now(&store, "notes", None);
        assert_eq!(store.lookup(id.as_str()).expect("own id").id, id);

        // Same sequence number, a different launch. Answering would tell a surviving
        // child about a job that is not the one it asked about.
        let foreign = id.as_str().replace(INSTANCE, "deadbeef");
        let Err(err) = store.lookup(&foreign) else {
            panic!("a foreign host_instance must not resolve");
        };
        assert_eq!(err.code().as_str(), "job_not_found");
        assert!(store.lookup("nonsense").is_err());
        assert!(store.lookup("job_7c1e93aa_ffffff").is_err());
    }

    /// `lookup` must GC before it answers, or `GET /api/git/jobs/{id}` resurrects a
    /// record `GET /api/git/jobs` already reports as gone.
    ///
    /// The order here is load-bearing: `lookup` is called BEFORE any `list`, because
    /// `list` evicts too — checking `list` first would make this pass whether or not
    /// `lookup` GCs, which is exactly the vacuous test this is written to avoid.
    #[test]
    fn lookup_evicts_before_it_answers() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let id = finish_now(&store, "notes", None);
        assert!(store.lookup(id.as_str()).is_ok());

        clock.advance(JOB_TTL_SECS * 1_000);
        let Err(err) = store.lookup(id.as_str()) else {
            panic!("an expired record must not resolve through lookup");
        };
        assert_eq!(err.code().as_str(), "job_not_found");
        assert!(
            store.list(&JobFilter::default()).is_empty(),
            "and the two endpoints agree"
        );
    }

    #[test]
    fn list_is_newest_first_and_honours_the_filter() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let a = finish_now(&store, "alpha", None);
        clock.advance(1_000);
        let b = finish_now(&store, "beta", None);
        clock.advance(1_000);
        let gate = Gate::new();
        let (c, worker) = start_gated(&store, &gate, "alpha", JobOp::Pull, None, Finish::Succeed);
        gate.wait_entered(1);

        assert_eq!(
            ids(&store.list(&JobFilter::default())),
            vec![c.clone(), b.clone(), a.clone()]
        );
        assert_eq!(
            ids(&store.list(&JobFilter {
                repo_id: Some("alpha".to_string()),
                ..JobFilter::default()
            })),
            vec![c.clone(), a.clone()]
        );
        assert_eq!(
            ids(&store.list(&JobFilter {
                state: Some(JobState::Running),
                ..JobFilter::default()
            })),
            vec![c.clone()]
        );
        assert_eq!(
            ids(&store.list(&JobFilter {
                limit: 1,
                ..JobFilter::default()
            })),
            vec![c.clone()]
        );

        gate.release(1);
        worker.join().expect("worker");
    }

    #[test]
    fn eviction_respects_the_cap_the_live_job_and_the_age_floor() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);

        // 60 finished jobs a minute apart, so all of them are past the age floor.
        let mut old = Vec::new();
        for i in 0..60 {
            old.push(finish_now(&store, &format!("r{i}"), None));
            clock.advance(60_000);
        }
        let gate = Gate::new();
        let (live, worker) = start_gated(&store, &gate, "live", JobOp::Sync, None, Finish::Succeed);
        gate.wait_entered(1);

        clock.advance(60_000);
        let newest = finish_now(&store, "newest", None);

        let terminal = store.list(&JobFilter {
            state: Some(JobState::Succeeded),
            ..JobFilter::default()
        });
        assert_eq!(terminal.len(), MAX_JOB_RECORDS);
        assert!(store.get(&live).is_some(), "a live job is never evicted");
        assert!(store.get(&newest).is_some(), "the newest record survives");
        assert!(store.get(&old[0]).is_none(), "the oldest was evicted");
        assert!(store.get(&old[59]).is_some());

        gate.release(1);
        worker.join().expect("worker");
    }

    #[test]
    fn a_young_terminal_record_survives_a_burst() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        // The record a client is about to poll.
        let mine = finish_now(&store, "mine", Some("r1"));
        // A burst of auto-syncs, all inside the age floor.
        for i in 0..MAX_JOB_RECORDS + 10 {
            finish_now(&store, &format!("auto{i}"), None);
        }
        assert!(
            store.get(&mine).is_some(),
            "nothing younger than JOB_MIN_AGE_SECS is evicted, cap or no cap"
        );
        assert!(
            store.list(&JobFilter::default()).len() > MAX_JOB_RECORDS,
            "the age floor is allowed to hold the store over the cap"
        );
    }

    #[test]
    fn ttl_expiry_drops_the_record_and_frees_the_request_id() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let first = finish_now(&store, "notes", Some("r1"));

        let Admission::Replay(same) = store
            .admit("notes", JobOp::Sync, Some("r1"))
            .expect("admit")
        else {
            panic!("the record is still here, so this replays");
        };
        assert_eq!(same.id, first);

        clock.advance(JOB_TTL_SECS * 1_000);
        let Admission::Started(fresh, lease) = store
            .admit("notes", JobOp::Sync, Some("r1"))
            .expect("admit")
        else {
            panic!("an expired record frees its request_id");
        };
        assert_ne!(fresh.id, first);
        assert!(store.get(&first).is_none(), "the expired record is gone");
        drop(lease);
    }

    #[test]
    fn draining_refuses_new_jobs() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        store.set_draining(true);
        assert!(store.draining());

        // `let ... else` rather than `expect_err`: neither `Admission` nor `JobSlot`
        // implements `Debug`, deliberately — a job record holds a `GitError` and is one
        // careless `{:?}` away from a log line nobody audited.
        let Err(err) = store.admit("notes", JobOp::Sync, None) else {
            panic!("draining must refuse new jobs");
        };
        assert_eq!(err.code().as_str(), "shutting_down");

        store.set_draining(false);
        let Admission::Started(_, lease) = store.admit("notes", JobOp::Sync, None).expect("admit")
        else {
            panic!("admission resumes once draining clears");
        };
        drop(lease);
    }

    #[test]
    fn abort_all_trips_only_live_jobs() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let done = finish_now(&store, "done", None);
        let gate = Gate::new();
        let (live, worker) = start_gated(&store, &gate, "live", JobOp::Sync, None, Finish::Succeed);
        gate.wait_entered(1);

        store.abort_all(AbortReason::Shutdown);
        assert_eq!(
            store.get(&live).expect("live").abort.reason(),
            AbortReason::Shutdown
        );
        assert_eq!(
            store.get(&done).expect("done").abort.reason(),
            AbortReason::None
        );

        gate.release(1);
        worker.join().expect("worker");
    }

    #[test]
    fn forget_repo_drops_terminal_records_and_leaves_a_live_job_alone() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let gone = finish_now(&store, "notes", Some("r1"));
        let kept = finish_now(&store, "other", None);
        let gate = Gate::new();
        let (live, worker) = start_gated(&store, &gate, "live", JobOp::Sync, None, Finish::Succeed);
        gate.wait_entered(1);

        store.forget_repo("notes");
        assert!(store.get(&gone).is_none());
        assert!(store.get(&kept).is_some());

        store.forget_repo("live");
        assert!(
            store.get(&live).is_some(),
            "a live job keeps its lease and its record"
        );
        assert!(store.busy("live").is_some());

        gate.release(1);
        worker.join().expect("worker");
    }

    #[test]
    fn evicting_a_superseded_record_keeps_the_live_successors_request_id() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let failed = fail_now(&store, "notes", Some("r1"));

        let Admission::Started(fresh, lease) = store
            .admit("notes", JobOp::Sync, Some("r1"))
            .expect("admit")
        else {
            panic!("retrying a failed op under the same request_id must work");
        };
        assert_ne!(fresh.id, failed);

        // Ages the superseded record past its TTL. `admit`'s own `evict` is what drops
        // it — never `drop_job` by hand — so this is the production path, and the
        // injected clock means no test in this file sleeps to reach it.
        clock.advance(JOB_TTL_SECS * 1_000);
        let Admission::Replay(same) = store
            .admit("notes", JobOp::Sync, Some("r1"))
            .expect("admit")
        else {
            panic!("the running successor must replay, never 409");
        };
        assert_eq!(same.id, fresh.id);
        assert!(store.get(&failed).is_none(), "the expired record is gone");

        fresh.succeed(&serde_json::json!({"outcome": "up_to_date"}));
        drop(lease);
        let Admission::Replay(same) = store
            .admit("notes", JobOp::Sync, Some("r1"))
            .expect("admit")
        else {
            panic!("a succeeded job replays; a second sync is the duplicate this prevents");
        };
        assert_eq!(same.id, fresh.id);
    }

    #[test]
    fn forget_repo_leaves_a_live_successors_request_id_alone() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let failed = fail_now(&store, "notes", Some("r1"));

        let Admission::Started(fresh, lease) = store
            .admit("notes", JobOp::Sync, Some("r1"))
            .expect("admit")
        else {
            panic!("retrying a failed op under the same request_id must work");
        };

        // The other `drop_job` call site. Reachable in production through the narrow
        // window in `GitService::delete_repo` between the hold and `forget_repo`, which
        // is the same window `forget_repo`'s own live-job carve-out exists for.
        store.forget_repo("notes");
        assert!(store.get(&failed).is_none(), "the failed record is gone");
        assert!(
            store.get(&fresh.id).is_some(),
            "the live job keeps its record"
        );

        let Admission::Replay(same) = store
            .admit("notes", JobOp::Sync, Some("r1"))
            .expect("admit")
        else {
            panic!("a live job keeps its request_id too, so the retry still replays");
        };
        assert_eq!(same.id, fresh.id);
        drop(lease);
    }

    #[test]
    fn a_maintenance_hold_and_a_job_exclude_each_other() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let hold = store.hold_repo("notes").expect("an idle repo can be held");

        let Err(err) = store.admit("notes", JobOp::Sync, None) else {
            panic!("a held repo admits nothing");
        };
        assert_eq!(err.code().as_str(), "repo_locked");
        // An *unknown* `request_id` falls through to the hold check like any other
        // admission. A *matching* one still replays, because a replay hands back an
        // existing record and touches neither the tree nor the registry entry.
        let Err(err) = store.admit("notes", JobOp::Sync, Some("r1")) else {
            panic!("an unknown request_id is not a way past the hold");
        };
        assert_eq!(err.code().as_str(), "repo_locked");
        // `let ... else` rather than `expect_err` for the same reason `Admission` gets
        // it: `RepoHold` has no `Debug`, and giving it one would print a repo id and a
        // whole `JobStore` into whatever formatted the panic.
        let Err(err) = store.hold_repo("notes") else {
            panic!("one holder at a time");
        };
        assert_eq!(err.code().as_str(), "repo_locked");

        // A hold is not a job: there is nothing to poll, nothing to embed in a 409, and
        // nothing for `status.busy_job` to point at.
        assert!(store.busy("notes").is_none());
        assert!(store.list(&JobFilter::default()).is_empty());
        assert!(store.live_jobs().is_empty());

        let Admission::Started(_, other) = store.admit("other", JobOp::Sync, None).expect("admit")
        else {
            panic!("one key, one lock: a different repo is untouched");
        };
        drop(other);

        drop(hold);
        let Admission::Started(_, lease) = store.admit("notes", JobOp::Sync, None).expect("admit")
        else {
            panic!("the repo is free once the hold drops");
        };
        drop(lease);
    }

    #[test]
    fn repo_locked_is_a_retryable_409_that_names_the_repo() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let _hold = store.hold_repo("notes").expect("idle");
        let Err(err) = store.hold_repo("notes") else {
            panic!("held");
        };
        assert_eq!(err.code(), GitErrorCode::RepoLocked);
        assert_eq!(err.repo_id.as_deref(), Some("notes"));
        assert_eq!(err.http_status().as_u16(), 409);
        // True, and the whole reason this is not `repo_busy`: the hold is released
        // before the call that took it returns, so "try again" is real advice.
        assert!(err.retryable());
    }

    #[test]
    fn a_hold_is_refused_while_a_job_owns_the_repo() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let gate = Gate::new();
        let (running, worker) =
            start_gated(&store, &gate, "notes", JobOp::Pull, None, Finish::Succeed);
        gate.wait_entered(1);

        // The job direction keeps answering `repo_busy`, naming the op the caller can
        // see running — collapsing it into `repo_locked` would take `error.job` away
        // from every client that keys off it.
        let Err(err) = store.hold_repo("notes") else {
            panic!("a job owns it");
        };
        assert_eq!(err.code(), GitErrorCode::RepoBusy);
        assert!(err.message.contains("pull"), "{}", err.message);
        assert_eq!(err.repo_id.as_deref(), Some("notes"));
        assert_eq!(store.busy("notes").expect("the job").id, running);

        gate.release(1);
        worker.join().expect("worker");

        let _hold = store.hold_repo("notes").expect("the lease released it");
        let Err(err) = store.admit("notes", JobOp::Sync, None) else {
            panic!("the lease's own ownership guard must not have cleared the hold");
        };
        assert_eq!(err.code(), GitErrorCode::RepoLocked);
    }

    /// The race itself, with real threads and no sleeps. Both sides are released by one
    /// barrier so neither call is merely sequenced after the other, and both park on a
    /// second barrier *before* dropping what they won, so the two outcomes overlap in
    /// time rather than taking turns.
    #[test]
    fn a_hold_and_an_admission_can_never_both_win() {
        for _ in 0..64 {
            let clock = TestClock::new(1_700_000_000_000);
            let store = store(&clock);
            let start = Arc::new(std::sync::Barrier::new(2));
            let hold_both = Arc::new(std::sync::Barrier::new(2));

            let admitter = {
                let (store, start, hold_both) = (store.clone(), start.clone(), hold_both.clone());
                thread::spawn(move || {
                    start.wait();
                    let r = store.admit("notes", JobOp::Sync, None);
                    let won = matches!(r, Ok(Admission::Started(..)));
                    hold_both.wait();
                    drop(r);
                    won
                })
            };
            let holder = {
                let (store, start, hold_both) = (store.clone(), start.clone(), hold_both.clone());
                thread::spawn(move || {
                    start.wait();
                    let r = store.hold_repo("notes");
                    let won = r.is_ok();
                    hold_both.wait();
                    drop(r);
                    won
                })
            };

            let admitted = admitter.join().expect("admitter");
            let held = holder.join().expect("holder");
            assert_ne!(
                admitted, held,
                "exactly one side owns the repo, and nothing loses to nobody"
            );
        }
    }

    #[test]
    fn forget_repo_never_clears_a_hold() {
        let clock = TestClock::new(1_700_000_000_000);
        let store = store(&clock);
        let done = finish_now(&store, "notes", Some("r1"));
        let _hold = store.hold_repo("notes").expect("idle");

        // `delete_repo` calls `forget_repo` from *inside* its own hold, so a future
        // version of it that touched `busy` would unlock the repo one line before the
        // purge — with libgit2 free to open the directory `remove_dir_all` is walking.
        store.forget_repo("notes");
        assert!(store.get(&done).is_none(), "the terminal record still goes");
        let Err(err) = store.admit("notes", JobOp::Sync, None) else {
            panic!("the hold survives forget_repo");
        };
        assert_eq!(err.code(), GitErrorCode::RepoLocked);
    }
}
