### Task 5: Job store

One file, `src/git/jobs.rs`, and nothing else. It is the concurrency core of the git
subsystem: job identity, per-repo mutual exclusion, the `request_id` retry contract,
progress throttling, and in-memory retention. It has **no `git2` import** — this is a
primitive over `FnOnce`, and the fact that it never names libgit2, axum, tokio or the
filesystem is what makes 21 tests run in under a millisecond with **zero `sleep` calls**.

Three decisions drive the whole design and every one of them is load-bearing:

1. **One flat `std::sync::Mutex`.** Not `tokio::sync`: the only writer of `Progress` is
   a libgit2 callback running on a blocking thread with no runtime context. It must
   never `.await`, so an async mutex there is a bug waiting to be written.
2. **Per-repo exclusion is a `BTreeMap` entry**, not a mutex per repo. No allocation, no
   lifetime question, no poisoning from a panicking job, and two different repos are
   parallel for free because they are two different keys.
3. **The clock is injected** (`NowFn`, from task 3). Retention has three time-based rules
   and progress has one; testing any of them against the wall clock would mean sleeping.

Everything below was compiled and run against `serde 1`, `serde_json 1` and `rand 0.9`
with the task-3 API stubbed exactly as task 3 lands it. `cargo test`, `cargo fmt --check`
and `cargo clippy --all-targets -- -D warnings` are green on the finished file as
written. Do not re-wrap the code — rustfmt will split it straight back.

**Files:**
- Create: `src/git/jobs.rs` (~1550 lines finished, 21 tests)
- Modify: `src/git/mod.rs` — one `pub mod jobs;` line inserted after `pub mod error;`

**Interfaces:**

- Consumes (all from Task 3; nothing from Task 0, 2 or 4 — this task can run in
  parallel with Task 4):

  ```rust
  // src/git/error.rs
  pub enum AbortReason { None, Timeout, Shutdown }
  impl AbortReason { pub fn as_u8(self) -> u8; pub fn from_u8(v: u8) -> AbortReason; }
  pub struct GitError { /* code, message, repo_id, path, field, git2 */ }  // Debug + Clone
  impl GitError {
      pub fn code(&self) -> GitErrorCode;
      pub fn internal(message: impl Into<String>) -> GitError;
      pub fn repo_busy(id: &str, op: &str) -> GitError;
      pub fn job_not_found(raw: &str) -> GitError;
      pub fn shutting_down() -> GitError;
  }
  impl serde::Serialize for GitError;      // hand-written, job-error shape
  impl GitErrorCode { pub fn as_str(self) -> &'static str; }

  // src/git/util.rs
  pub type NowFn = std::sync::Arc<dyn Fn() -> u64 + Send + Sync>;
  pub fn system_now() -> NowFn;
  ```

  Plus the two crate-level allows task 3 put at the top of `src/git/mod.rs`
  (`#![allow(dead_code)]`, `#![allow(clippy::result_large_err)]`) — both are required
  here too: this file publishes a dozen items whose only consumers are tasks 6 and 9,
  and `admit` returns `Result<_, GitError>`.

  Pre-existing dependencies, unchanged: `serde` (derive), `serde_json`, `rand 0.9`.

- Produces (tasks 6, 7, 8, 9 rely on exactly these):

  ```rust
  pub const MAX_JOB_RECORDS: usize = 50;
  pub const JOB_TTL_SECS: u64 = 3_600;
  pub const JOB_MIN_AGE_SECS: u64 = 60;
  pub const PROGRESS_THROTTLE_MS: u64 = 100;

  pub struct JobId(String);                // Debug Clone PartialEq Eq PartialOrd Ord Hash Serialize(transparent)
  impl JobId {
      pub fn new(host_instance: &str, seq: u64) -> JobId;
      pub fn as_str(&self) -> &str;
      pub fn instance(&self) -> Option<&str>;
  }
  impl std::fmt::Display for JobId;

  pub enum JobOp { Init, Clone, Pull, Push, Sync, Commit, Branch, Reset }
  impl JobOp { pub fn as_str(self) -> &'static str; pub fn is_mutating(self) -> bool; }
  pub enum JobState { Running, Succeeded, Failed }
  impl JobState { pub fn as_str(self) -> &'static str; pub fn parse(s: &str) -> Option<JobState>; }
  pub enum Phase { Preparing, Cloning, Staging, Committing, Fetching, Merging,
                   CheckingOut, Pushing, Finalizing }
  impl Phase { pub fn as_str(self) -> &'static str; }
  pub struct Progress { pub received_objects: u32, pub total_objects: u32,
                        pub indexed_objects: u32, pub received_bytes: u64, pub percent: u8 }

  pub struct AbortFlag(std::sync::atomic::AtomicU8);
  impl AbortFlag {
      pub fn new() -> AbortFlag;
      pub fn abort(&self, reason: AbortReason);   // first non-None writer wins
      pub fn is_aborted(&self) -> bool;
      pub fn reason(&self) -> AbortReason;
  }
  impl Default for AbortFlag;

  pub struct JobSlot {
      pub id: JobId, pub repo_id: String, pub op: JobOp,
      pub request_id: Option<String>, pub created_at_ms: u64,
      pub abort: std::sync::Arc<AbortFlag>,
      pub stalled: std::sync::Arc<std::sync::atomic::AtomicBool>,
      // private: `live: Mutex<JobLive>` and `now: NowFn`
  }
  impl JobSlot {
      pub fn begin(&self);
      pub fn set_phase(&self, phase: Phase);
      pub fn phase(&self) -> Phase;
      pub fn set_progress(&self, progress: Progress) -> bool;
      pub fn succeed(&self, result: &serde_json::Value);
      pub fn fail(&self, error: &GitError);
      pub fn finish_if_running(&self, error: GitError);
      pub fn state(&self) -> JobState;
      pub fn is_terminal(&self) -> bool;
      pub fn started_at_ms(&self) -> u64;
      pub fn finished_at_ms(&self) -> Option<u64>;
      pub fn mark_stalled(&self);
      pub fn view(&self, host_instance: &str) -> JobView;
      pub fn as_ref_view(&self) -> JobRef;
  }
  pub struct JobView { /* 14 pub fields, see Step 18 */ }   // Debug Clone Serialize
  pub struct JobRef  { /* 6 pub fields, see Step 18 */ }    // Debug Clone Serialize
  pub struct JobFilter { pub repo_id: Option<String>, pub state: Option<JobState>,
                         pub limit: usize }                 // Debug Clone Default

  pub enum Admission {
      Started(std::sync::Arc<JobSlot>, RepoLease),
      Replay(std::sync::Arc<JobSlot>),
      Busy(std::sync::Arc<JobSlot>),
  }
  impl Admission { pub fn slot(&self) -> &std::sync::Arc<JobSlot>; }

  pub struct RepoLease;                    // no public constructor
  impl RepoLease { pub fn job(&self) -> &std::sync::Arc<JobSlot>; }
  impl Drop for RepoLease;

  pub struct JobStore;
  impl JobStore {
      pub fn new(host_instance: String) -> std::sync::Arc<JobStore>;
      pub fn with_clock(host_instance: String, now: NowFn) -> std::sync::Arc<JobStore>;
      pub fn host_instance(&self) -> &str;
      pub fn admit(self: &std::sync::Arc<Self>, repo_id: &str, op: JobOp,
                   request_id: Option<&str>) -> Result<Admission, GitError>;
      pub fn get(&self, id: &JobId) -> Option<std::sync::Arc<JobSlot>>;
      pub fn lookup(&self, raw: &str) -> Result<std::sync::Arc<JobSlot>, GitError>;
      pub fn list(&self, filter: &JobFilter) -> Vec<std::sync::Arc<JobSlot>>;
      pub fn busy(&self, repo_id: &str) -> Option<std::sync::Arc<JobSlot>>;
      pub fn live_jobs(&self) -> Vec<std::sync::Arc<JobSlot>>;
      pub fn set_draining(&self, draining: bool);
      pub fn draining(&self) -> bool;
      pub fn abort_all(&self, reason: AbortReason);
      pub fn forget_repo(&self, repo_id: &str);
  }
  pub fn random_host_instance() -> String;   // 8 lowercase hex chars
  ```

  Four semantics later tasks must not re-litigate, all decided here:
  - **`JobOp::is_mutating`** is `!matches!(self, Push | Commit)` — "can change the files
    in the working tree". `push` publishes commits that already exist; `commit` only
    writes the index and moves HEAD. Task 9 keys child-restart decisions off this.
  - **`JobFilter::limit == 0` means unlimited.** `JobFilter::default()` therefore returns
    everything; `api.rs` always passes a real cap (`DEFAULT_JOB_LIST_LIMIT`).
  - **`JobStore::forget_repo` drops terminal records only.** A live job keeps its lease
    and its record; `DELETE` on a busy repo is a 409 for exactly that reason.
  - **`JobSlot` and `Admission` deliberately have no `Debug`.** A job record holds a
    `GitError`, and `Debug` is one careless `{:?}` away from a log line nobody audited.
    Tests use `let ... else` instead of `expect_err`/`unwrap_err`.

---

- [ ] **Step 1: Create the file with its charter, its constants, and the first failing test**

Create `src/git/jobs.rs`. The import block is the *finished* one: several names are
unused until later steps and rustc will warn about them, which is expected and
disappears as the file fills in. Step 61 proves the finished file is warning-free.

```rust
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

use crate::git::error::{AbortReason, GitError};
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

#[cfg(test)]
mod tests {
    use super::*;

    const INSTANCE: &str = "7c1e93aa";

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
}
```

Then register the module. `src/git/mod.rs` currently ends its module list with the three
lines task 3 left:

```rust
pub mod error;
pub mod secret;
pub mod util;
```

Insert one line so the list stays alphabetical:

```rust
pub mod error;
pub mod jobs;
pub mod secret;
pub mod util;
```

(Task 4 lands `pub mod registry;` and `pub mod state;` in the same list; the two edits
touch different lines and do not collide.)

- [ ] **Step 2: Run the test and watch it fail**

This crate is a binary — there is no `--lib` target, and `src/bin/gen_icons.rs` is a
second binary — so unit tests run in the `chrome-host-app` bin target.

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: FAIL — compile errors, two distinct ones:

```
error[E0433]: failed to resolve: use of undeclared type `JobId`
  --> src/git/jobs.rs:46:20
   |
46 |         assert_eq!(JobId::new(INSTANCE, 43).as_str(), "job_7c1e93aa_00002b");
   |                    ^^^^^ use of undeclared type `JobId`

error[E0425]: cannot find function `random_host_instance` in this scope
```

- [ ] **Step 3: Implement `JobId` and `random_host_instance`**

Insert between the constants and the `#[cfg(test)]` line:

```rust
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
```

and immediately above the `#[cfg(test)]` line:

```rust
/// Eight lowercase hex chars, minted once per host launch.
pub fn random_host_instance() -> String {
    let mut rng = rand::rng();
    format!("{:08x}", rng.random::<u32>())
}
```

- [ ] **Step 4: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: PASS — `test result: ok. 2 passed`. Warnings about unused imports
(`AbortReason`, `GitError`, `NowFn`, `system_now`, `BTreeMap`, `VecDeque`, the atomics,
`Mutex`, `MutexGuard`) are expected at this point.

- [ ] **Step 5: Commit**

```bash
git add src/git/jobs.rs src/git/mod.rs
git commit -m "feat(git): job ids and the per-launch host instance"
```

---

- [ ] **Step 6: Write the failing test for the wire vocabulary**

Add to `mod tests`, after `random_host_instance_is_eight_lowercase_hex_chars`. Every
string here is a published contract value that a `[server]` child matches on, so the
test pins `as_str()` and the serde rename together — two angles on one behaviour.

```rust
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
```

- [ ] **Step 7: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app git::jobs::tests::the_wire_vocabulary_is_fixed`

Expected: FAIL — compile errors:

```
error[E0433]: failed to resolve: use of undeclared type `JobOp`
error[E0433]: failed to resolve: use of undeclared type `JobState`
error[E0433]: failed to resolve: use of undeclared type `Phase`
```

- [ ] **Step 8: Implement `JobOp`, `JobState` and `Phase`**

Insert after `impl std::fmt::Display for JobId`:

```rust
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
```

- [ ] **Step 9: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: PASS — `test result: ok. 3 passed`.

- [ ] **Step 10: Commit**

```bash
git add src/git/jobs.rs
git commit -m "feat(git): job op, state and phase vocabularies"
```

---

- [ ] **Step 11: Write the failing test for the abort flag**

Add to `mod tests`:

```rust
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
```

- [ ] **Step 12: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app git::jobs::tests::abort_flag_keeps_the_first_reason`

Expected: FAIL —

```
error[E0433]: failed to resolve: use of undeclared type `AbortFlag`
  |
  |         let flag = AbortFlag::new();
  |                    ^^^^^^^^^ use of undeclared type `AbortFlag`
```

- [ ] **Step 13: Implement `AbortFlag`**

Insert after `impl Phase`:

```rust
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
```

The `Default` impl is not optional: without it `clippy::new_without_default` fails the
build in Step 61. It is written as a call to `new()` rather than a `#[derive(Default)]`
so it keeps stating "the initial value is `AbortReason::None`", not "zero".

- [ ] **Step 14: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: PASS — `test result: ok. 4 passed`.

- [ ] **Step 15: Commit**

```bash
git add src/git/jobs.rs
git commit -m "feat(git): abort flag carrying the reason, first writer wins"
```

---

- [ ] **Step 16: Write the failing tests for the job slot lifecycle**

Three tests: the running→terminal walk, the failed variant, and the serialized wire
shape. They are three angles on one behaviour — what a slot exposes about itself — and
between them they pin the whole state machine (`result.is_some() ⟺ Succeeded`, terminal
states never mutate, `started_at_ms` is never null).

Add the clock helper and `test_slot` at the **top** of `mod tests`, immediately after
`const INSTANCE`, then the three tests at the bottom:

```rust
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
```

```rust
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
```

- [ ] **Step 17: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: FAIL — compile errors:

```
error[E0433]: failed to resolve: use of undeclared type `JobSlot`
  |
  |         Arc::new(JobSlot::new(
  |                  ^^^^^^^ use of undeclared type `JobSlot`
```

- [ ] **Step 18: Implement `Progress`, `JobSlot` and the two views**

Insert after `impl Default for AbortFlag` (before the `random_host_instance` function).
`set_progress` is deliberately absent — it lands with its own test in Step 23.

```rust
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
```

Note the `stalled` load in `view`/`as_ref_view` happens **before** the mutex is taken.
It is an independent atomic; reading it inside the guard would only widen the critical
section.

- [ ] **Step 19: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: PASS — `test result: ok. 7 passed`.

- [ ] **Step 20: Commit**

```bash
git add src/git/jobs.rs
git commit -m "feat(git): job slot state machine and its two serialized views"
```

---

- [ ] **Step 21: Write the failing test for progress throttling**

Add to `mod tests`:

```rust
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
```

- [ ] **Step 22: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app git::jobs::tests::progress_is_throttled_but_percent_changes_always_land`

Expected: FAIL —

```
error[E0599]: no method named `set_progress` found for struct `JobSlot` in the current scope
  |
  |         assert!(slot.set_progress(p(1, 1)), "the first write always lands");
  |                      ^^^^^^^^^^^^ method not found in `JobSlot`
```

- [ ] **Step 23: Implement `set_progress`**

Add one field to `JobLive`, after `error`:

```rust
    error: Option<GitError>,
    last_progress_write_ms: u64,
}
```

Initialise it in `JobSlot::new`, after `error: None,`:

```rust
                error: None,
                last_progress_write_ms: 0,
            }),
```

And add the method to `impl JobSlot`, between `phase()` and `succeed()`:

```rust
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
```

The first call always writes because `live.progress` is `None` and `None != Some(0)`,
so `same_percent` is false however the transfer starts.

- [ ] **Step 24: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: PASS — `test result: ok. 8 passed`.

- [ ] **Step 25: Commit**

```bash
git add src/git/jobs.rs
git commit -m "feat(git): throttle progress writes from the libgit2 transfer callback"
```

---

- [ ] **Step 26: Write the failing tests for per-repo exclusion and the RAII lease**

This is the step that introduces the gate. Add to `mod tests` — the `use` lines go
directly under `use super::*;`, the helpers after `test_slot`, the tests at the bottom:

```rust
    use std::sync::mpsc;
    use std::sync::Condvar;
    use std::thread;
```

```rust
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
```

```rust
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
```

- [ ] **Step 27: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: FAIL — compile errors:

```
error[E0433]: failed to resolve: use of undeclared type `JobStore`
  |
  |         JobStore::with_clock(INSTANCE.to_string(), clock.now_fn())
  |         ^^^^^^^^ use of undeclared type `JobStore`

error[E0433]: failed to resolve: use of undeclared type `Admission`
```

- [ ] **Step 28: Implement `JobStore`, `Admission` and `RepoLease`**

Insert after the `JobRef` struct, before `random_host_instance`. No `request_id` index
and no retention yet — those land in Steps 33 and 48 with their own tests.

```rust
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
        let mut inner = self.store.lock();
        // Only clear the entry if it is still ours: `forget_repo` may have dropped it,
        // and clearing a *different* job's lock would break exclusion.
        if inner.busy.get(&self.repo_id) == Some(&self.job.id) {
            inner.busy.remove(&self.repo_id);
        }
    }
}

pub struct JobStore {
    inner: Mutex<Inner>,
    instance: String,
    seq: AtomicU64,
    now: NowFn,
}

#[derive(Default)]
struct Inner {
    jobs: BTreeMap<JobId, Arc<JobSlot>>,
    /// repo_id -> the job holding it. THE LOCK.
    busy: BTreeMap<String, JobId>,
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
        let mut inner = self.lock();

        // Busy check. The map entry is the lock, not the job's state: `after_job` still
        // has work to do after a slot goes terminal, and a second libgit2 operation on
        // the same working tree would corrupt the index.
        let busy_id = inner.busy.get(repo_id).cloned();
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
        inner.busy.insert(repo_id.to_string(), slot.id.clone());
        drop(inner);

        let lease = RepoLease {
            store: self.clone(),
            repo_id: repo_id.to_string(),
            job: slot.clone(),
        };
        Ok(Admission::Started(slot, lease))
    }

    pub fn get(&self, id: &JobId) -> Option<Arc<JobSlot>> {
        self.lock().jobs.get(id).cloned()
    }

    pub fn busy(&self, repo_id: &str) -> Option<Arc<JobSlot>> {
        let inner = self.lock();
        let id = inner.busy.get(repo_id)?;
        inner.jobs.get(id).cloned()
    }

    pub fn live_jobs(&self) -> Vec<Arc<JobSlot>> {
        self.lock()
            .jobs
            .values()
            .filter(|s| !s.is_terminal())
            .cloned()
            .collect()
    }
}
```

`admit` returns `Result` even though nothing fails yet: the `shutting_down` arm lands in
Step 53 and the signature is part of the contract from the start.

- [ ] **Step 29: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: PASS — `test result: ok. 11 passed`. `lease_is_released_when_the_worker_panics`
prints a `thread '<unnamed>' panicked at ...: worker exploded` line plus a note about
`RUST_BACKTRACE`; that is the default panic hook doing its job, not a failure.

- [ ] **Step 30: Commit**

```bash
git add src/git/jobs.rs
git commit -m "feat(git): job store with per-repo exclusion and an RAII repo lease"
```

---

- [ ] **Step 31: Write the failing tests for the `request_id` replay contract**

Two tests: replay wins over busy, and a *failed* job's id is reusable. The second is the
whole reason the index is consumed rather than kept — retrying a failed operation with
the same id is the natural client behaviour and the mechanism must not lock it out.

Add to `mod tests`:

```rust
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
```

- [ ] **Step 32: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::jobs::tests::replay`

These compile — there is no index yet, so the second `admit` for `"r1"` falls through to
the busy check.

Expected: FAIL — `replay_precedes_busy` panics:

```
thread 'git::jobs::tests::replay_precedes_busy' panicked at src/git/jobs.rs:...:
the same request_id must replay, never 409
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

test result: FAILED. 1 passed; 1 failed
```

`replay_of_a_failed_job_admits_a_fresh_one` passes *vacuously* at this point: with no
index, every admission is already fresh. It is the guard that Step 33's index does not
lock the retry out — the regression it catches only becomes possible once the index
exists, which is why both tests are written before it.

- [ ] **Step 33: Implement the `request_id` index and the replay check**

Add the third field to `Inner`:

```rust
#[derive(Default)]
struct Inner {
    jobs: BTreeMap<JobId, Arc<JobSlot>>,
    /// repo_id -> the job holding it. THE LOCK.
    busy: BTreeMap<String, JobId>,
    by_request: BTreeMap<(String, String), JobId>,
}
```

Insert the replay block in `admit`, between `let mut inner = self.lock();` and the busy
check:

```rust
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

```

and index the new job, replacing the `inner.busy.insert(...)` / `drop(inner);` pair at
the end of `admit` with:

```rust
        inner.busy.insert(repo_id.to_string(), slot.id.clone());
        if let Some(rid) = request_id {
            inner
                .by_request
                .insert((repo_id.to_string(), rid.to_string()), slot.id.clone());
        }
        drop(inner);
```

- [ ] **Step 34: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: PASS — `test result: ok. 13 passed`.

- [ ] **Step 35: Commit**

```bash
git add src/git/jobs.rs
git commit -m "feat(git): request_id replay index, checked before the busy gate"
```

---

- [ ] **Step 36: Write the failing test for the host-instance restart contract**

`finish_now` is the last test helper: it admits and finishes a job inline, with no thread
and no gate, for the tests that only care about terminal records.

Add it after `start_gated`, and the test at the bottom:

```rust
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
```

```rust
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
```

- [ ] **Step 37: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app git::jobs::tests::lookup_rejects_a_foreign_host_instance`

Expected: FAIL —

```
error[E0599]: no method named `lookup` found for struct `JobStore` in the current scope
  |
  |         assert_eq!(store.lookup(id.as_str()).expect("own id").id, id);
  |                          ^^^^^^ method not found in `JobStore`
```

- [ ] **Step 38: Implement `lookup`**

Add to `impl JobStore`, directly after `get`:

```rust
    /// Parse and resolve a raw id from the wire.
    ///
    /// An id whose instance prefix is not ours is rejected without touching the map: a
    /// child that survived a host restart must never be answered about a *different*
    /// job that happens to share a sequence number.
    pub fn lookup(&self, raw: &str) -> Result<Arc<JobSlot>, GitError> {
        let id = JobId(raw.to_string());
        if id.instance() != Some(self.instance.as_str()) {
            return Err(GitError::job_not_found(raw));
        }
        self.get(&id).ok_or_else(|| GitError::job_not_found(raw))
    }
```

- [ ] **Step 39: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: PASS — `test result: ok. 14 passed`.

- [ ] **Step 40: Commit**

```bash
git add src/git/jobs.rs
git commit -m "feat(git): reject job ids minted by an earlier host launch"
```

---

- [ ] **Step 41: Write the failing test for listing**

Add the `ids` helper after `finish_now`, and the test at the bottom:

```rust
    fn ids(slots: &[Arc<JobSlot>]) -> Vec<JobId> {
        slots.iter().map(|s| s.id.clone()).collect()
    }
```

```rust
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
```

- [ ] **Step 42: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app git::jobs::tests::list_is_newest_first_and_honours_the_filter`

Expected: FAIL —

```
error[E0433]: failed to resolve: use of undeclared type `JobFilter`
error[E0599]: no method named `list` found for struct `JobStore` in the current scope
```

- [ ] **Step 43: Implement `JobFilter` and `list`**

Insert `JobFilter` after the `JobRef` struct, above `pub enum Admission`:

```rust
#[derive(Debug, Clone, Default)]
pub struct JobFilter {
    pub repo_id: Option<String>,
    pub state: Option<JobState>,
    /// `0` means unlimited; the HTTP layer always passes a real cap.
    pub limit: usize,
}
```

and `list` in `impl JobStore`, between `lookup` and `busy`:

```rust
    /// Newest first.
    pub fn list(&self, filter: &JobFilter) -> Vec<Arc<JobSlot>> {
        let inner = self.lock();
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
```

`Option::is_none_or` is stable since Rust 1.82 and this crate's `rust-version` is 1.85.

- [ ] **Step 44: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: PASS — `test result: ok. 15 passed`.

- [ ] **Step 45: Commit**

```bash
git add src/git/jobs.rs
git commit -m "feat(git): filtered, newest-first job listing"
```

---

- [ ] **Step 46: Write the failing tests for retention**

Three tests for the five retention rules: the cap plus the live-job and age-floor
exemptions; the age floor winning against a burst; and TTL expiry freeing the
`request_id`. All three use the injected clock, so none of them sleeps.

Add to `mod tests`:

```rust
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
```

- [ ] **Step 47: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::jobs`

They compile — nothing is ever evicted, so the counts are wrong.

Expected: FAIL — two of the three:

```
thread 'git::jobs::tests::eviction_respects_the_cap_the_live_job_and_the_age_floor'
panicked at src/git/jobs.rs:...:
assertion `left == right` failed
  left: 61
 right: 50

thread 'git::jobs::tests::ttl_expiry_drops_the_record_and_frees_the_request_id'
panicked at src/git/jobs.rs:...: an expired record frees its request_id
```

(`a_young_terminal_record_survives_a_burst` passes vacuously until retention exists; it
is the guard that Step 48 does not over-evict.)

- [ ] **Step 48: Implement retention**

Add the terminal queue to `Inner`:

```rust
#[derive(Default)]
struct Inner {
    jobs: BTreeMap<JobId, Arc<JobSlot>>,
    /// Oldest first. Live jobs are never in here, which is what makes rule 3 free.
    terminal: VecDeque<JobId>,
    /// repo_id -> the job holding it. THE LOCK.
    busy: BTreeMap<String, JobId>,
    by_request: BTreeMap<(String, String), JobId>,
}
```

Add `impl Inner` immediately below the struct:

```rust
impl Inner {
    /// How long ago a record finished, or `None` when the slot is already gone.
    fn terminal_age_ms(&self, id: &JobId, now: u64) -> Option<u64> {
        let slot = self.jobs.get(id)?;
        Some(now.saturating_sub(slot.finished_at_ms().unwrap_or(slot.created_at_ms)))
    }

    /// Retention. Runs on every admission and every list.
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
            // Rule 5: the id becomes reusable the moment its record goes.
            if let Some(rid) = slot.request_id.clone() {
                self.by_request.remove(&(slot.repo_id.clone(), rid));
            }
        }
    }
}
```

`terminal_age_ms` takes a `JobSlot`'s `live` mutex while `Inner`'s is held. That is the
**only** legal nesting direction in this file, and it is why `RepoLease::drop` calls
`finish_if_running` before it takes `inner`.

Wire it into the three call sites.

`RepoLease::drop` — read the clock before taking the lock, and enqueue plus evict:

```rust
        self.job
            .finish_if_running(GitError::internal("git worker terminated unexpectedly"));
        let now = (self.store.now)();
        let mut inner = self.store.lock();
        // Only clear the entry if it is still ours: `forget_repo` may have dropped it,
        // and clearing a *different* job's lock would break exclusion.
        if inner.busy.get(&self.repo_id) == Some(&self.job.id) {
            inner.busy.remove(&self.repo_id);
        }
        inner.terminal.push_back(self.job.id.clone());
        inner.evict(now);
```

`admit` — replace its first two lines so the GC runs before the replay lookup:

```rust
        let now = (self.now)();
        let mut inner = self.lock();

        // GC first, so a replay can never resolve against a record this very call is
        // about to evict.
        inner.evict(now);

```

`list` — the same, so a list and a poll agree about what exists. Its opening becomes:

```rust
    /// Newest first. GCs before it reads, so a list and a poll agree about what exists.
    pub fn list(&self, filter: &JobFilter) -> Vec<Arc<JobSlot>> {
        let now = (self.now)();
        let mut inner = self.lock();
        inner.evict(now);
        let mut out: Vec<Arc<JobSlot>> = inner
```

- [ ] **Step 49: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: PASS — `test result: ok. 18 passed`.

- [ ] **Step 50: Commit**

```bash
git add src/git/jobs.rs
git commit -m "feat(git): job retention with a TTL, a cap, and a 60s eviction floor"
```

---

- [ ] **Step 51: Write the failing tests for the shutdown path**

Add to `mod tests`:

```rust
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
```

- [ ] **Step 52: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: FAIL —

```
error[E0599]: no method named `set_draining` found for struct `JobStore` in the current scope
error[E0599]: no method named `draining` found for struct `JobStore` in the current scope
error[E0599]: no method named `abort_all` found for struct `JobStore` in the current scope
```

- [ ] **Step 53: Implement draining and `abort_all`**

Add the field to `JobStore`:

```rust
pub struct JobStore {
    inner: Mutex<Inner>,
    instance: String,
    seq: AtomicU64,
    now: NowFn,
    draining: AtomicBool,
}
```

and to `with_clock`'s literal, after `now,`:

```rust
            now,
            draining: AtomicBool::new(false),
        })
```

Add the check to `admit`, between `let mut inner = self.lock();` and the `evict` call:

```rust
        let mut inner = self.lock();

        // Inside the lock with the busy check on purpose: a `set_draining` racing an
        // `admit` must not be able to let one last job through the gate.
        if self.draining.load(Ordering::SeqCst) {
            return Err(GitError::shutting_down());
        }

```

and the three methods to `impl JobStore`, after `live_jobs`:

```rust
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
```

`abort_all` collects `live_jobs()` first and then trips the flags, so `Inner`'s lock is
released before any slot is touched — the same nesting discipline as everywhere else.

- [ ] **Step 54: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: PASS — `test result: ok. 20 passed`.

- [ ] **Step 55: Commit**

```bash
git add src/git/jobs.rs
git commit -m "feat(git): draining gate and abort_all for the quit path"
```

---

- [ ] **Step 56: Write the failing test for `forget_repo`**

Add to `mod tests`:

```rust
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
```

- [ ] **Step 57: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app git::jobs::tests::forget_repo_drops_terminal_records_and_leaves_a_live_job_alone`

Expected: FAIL —

```
error[E0599]: no method named `forget_repo` found for struct `JobStore` in the current scope
  |
  |         store.forget_repo("notes");
  |               ^^^^^^^^^^^ method not found in `JobStore`
```

- [ ] **Step 58: Implement `forget_repo`**

Add to `impl JobStore`, after `abort_all` — it is the last method in the block:

```rust
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
```

- [ ] **Step 59: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::jobs`

Expected: PASS — `test result: ok. 21 passed`.

- [ ] **Step 60: Commit**

```bash
git add src/git/jobs.rs
git commit -m "feat(git): forget a repo's terminal job records on delete"
```

---

- [ ] **Step 61: Prove the file is clean and the tests really do not sleep**

Run, in order:

```bash
cargo fmt
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
grep -n "thread::sleep" src/git/jobs.rs
grep -n "git2::" src/git/jobs.rs
```

Expected:
- `cargo fmt --check` prints nothing.
- `cargo clippy --all-targets -- -D warnings` finishes with no diagnostics. The two
  crate-level allows task 3 put in `src/git/mod.rs` are what make this true for a module
  whose consumers do not exist yet.
- `cargo test` is green — 21 tests in `git::jobs`, plus every pre-existing test in the
  crate unchanged: this task touched nothing but `src/git/jobs.rs` and one line of
  `src/git/mod.rs`.
- Both `grep`s print nothing and exit 1. Zero `thread::sleep` is the promise §10.2
  makes; zero `git2::` is the module-boundary rule from §2.4. (Plain `grep sleep` and
  `grep git2` both *do* match — the doc comments say "zero `sleep` calls" and "a libgit2
  callback" — so match the call syntax, not the word.)

If `cargo fmt` reported changes, they are formatting only — re-run `cargo test` and
include them in the commit below.

- [ ] **Step 62: Commit**

```bash
git add src/git/jobs.rs
git commit -m "chore(git): fmt and clippy pass over the job store"
```

If `cargo fmt` changed nothing there is nothing to commit here; skip the commit and the
task is done at Step 60.
