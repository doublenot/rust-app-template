### Task 9: GitService and the HTTP API

Two files carry the whole feature into the running host: `src/git/mod.rs` gains `GitService`
— the only git type the rest of the app ever names — and `src/git/api.rs` gains the axum
sub-router that the `[server]` child drives.

Five things in this task are load-bearing. Each is a bug that ships silently if you skip it:

1. **The sub-router is nested only when `[git]` is present**, and when it is absent an
   explicit `git_disabled` fallback is nested in its place. A bare 404 with an empty body is
   indistinguishable from a typo in the path; the child needs to know the *host* has no git.
2. **One `from_fn_with_state` auth layer on the nested router**, never a per-handler check.
   Seventeen routes with seventeen `if !authorized` lines is sixteen chances to forget one.
   This authenticates **GETs too**, a deliberate divergence from the unauthenticated
   `GET /api/status`.
3. **No `rt.enter()` guard anywhere in this task.** That guard exists in this codebase
   solely because `tokio::process::Command::spawn` registers a child with the runtime's
   signal driver. Git spawns no processes, and `Handle::spawn_blocking` is callable from any
   thread. There is a comment saying so at the spawn site, so a future reader does not
   "fix" it.
4. **No `git2` value escapes the `spawn_blocking` closure**, and the `Repository` stays
   alive for the whole job. Dropping a `Repository` while an `Index` is still held detaches
   the index and `write_tree()` then fails. `GitService` enforces this structurally: it
   calls `ops::run` and never opens a repository itself.
5. **The RAII lease is dropped last.** `let _lease = lease;` is the first statement in the
   closure, so it is the last local destroyed — the repo is released the instant the job is
   terminal and not before, on the panic path as well as the return path.

`GitService` takes an injected `Arc<dyn GitOps>`. That is what makes eighteen router tests
deterministic and network-free: the fake returns a canned `OpOutcome` and the tests are
tests of admission, serialization, and status codes rather than of libgit2. It follows the
precedent already in this codebase — `chrome::find_first_existing(paths, exists: impl Fn)`.

**House rules for every step below:** unit tests live in a `#[cfg(test)] mod tests` at the
bottom of the file they test; `tempfile` for filesystem tests; comments explain WHY (an
invariant, a race, an attack), never what; no `unwrap()` outside `#[cfg(test)]` unless a
panic is the correct behaviour and a comment says so; `cargo fmt` and
`cargo clippy --all-targets -- -D warnings` clean at every commit.

**This crate has no `--lib` target** (`src/main.rs` is the only entry point; `src/bin/gen_icons.rs`
is a second binary), so unit tests run in the bin target:
`cargo test --bin chrome-host-app <filter>`. Never `cargo test --lib`.

`src/git/mod.rs` already carries `#![allow(dead_code)]` and `#![allow(clippy::result_large_err)]`
from task 3; both propagate into `git::api`. Do not add a second copy.

---

**Files:**
- Modify: `src/git/mod.rs` — add `pub mod api;` to the existing `pub mod` block (keep it
  alphabetical: `api`, `creds`, `error`, `jobs`, `merge`, `ops`, `registry`, `secret`,
  `state`, `util`), add three constants, `GitOps`/`RealOps`, `contained_in`, the nine
  response types, `StartOutcome`, and `GitService`. All new tests go **inside** the
  `#[cfg(test)] mod tests` block task 7 left at the bottom.
- Create: `src/git/api.rs`
- Modify: `src/internal_server.rs:18-21` (`HostEvent`), `:36-44` (`HostState`), `:46-56`
  (`router`), `:169-175` (`authorized` visibility), `:218-257` (`spawn_state`), and the
  `mod tests` block for the end-to-end router tests.
- Modify: `src/main.rs` — one line, `git: None,` in the `HostState { … }` literal inside
  `main()` (`src/main.rs:551-562` before task 0's split; the literal moves with `main()`,
  which stays in `main.rs`). Without it the crate does not compile.
- Modify: `src/config.rs` — Step 102 only: delete the `#[allow(dead_code)]` attributes on
  `RuntimePaths::{repos_dir, registry_file, git_state_file}` and on `GitSection`, which
  task 2 landed expecting this task to remove them.
- Modify: `src/git/registry.rs` and `src/git/state.rs` — Step 102 only: delete the
  file-level `#![allow(dead_code)]` task 4 landed expecting this task to remove it.

**Interfaces:**

- **Consumes** — from `src/config.rs` (task 2):

  ```rust
  pub struct AppConfig { pub app: AppSection, pub settings: Option<SettingsSection>,
                         pub git: Option<GitSection>, /* … */ }
  impl AppConfig { pub fn from_str(s: &str) -> Result<Self, String>;
                   pub fn settings_enabled(&self) -> bool; }
  pub struct GitSection {                      // Debug + Clone, Deserialize only
      pub tray_sync: bool, pub error_dialogs: bool, pub status_api: bool,
      pub registry_writes: bool, pub default_branch: String,
      pub author_name: String, pub author_email: String,
      pub network_timeout_secs: u64, pub quit_sync_timeout_secs: u64,
      pub allow_http: bool, pub ssh_host_key_policy: SshHostKeyPolicy,
  }
  pub enum SshHostKeyPolicy { Tofu, Accept }   // Copy
  pub fn validate_branch_name(name: &str) -> bool;
  pub struct RuntimePaths { pub data_dir, pub logs_dir, pub settings_file,
                            pub repos_dir, pub registry_file, pub git_state_file, /* … */ }
  impl RuntimePaths { pub fn under(base: &Path, identifier: &str) -> Self;
                      pub fn ensure(&self) -> std::io::Result<()>; }
  ```

- **Consumes** — from `src/git/error.rs` + `src/git/util.rs` (task 3):

  ```rust
  pub struct GitError { pub code: GitErrorCode, pub message: String,
                        pub repo_id: Option<String>, pub path: Option<String>,
                        pub field: Option<String>, pub git2: Option<Git2Hint> }  // Debug + Clone
  impl GitError {
      pub fn code(&self) -> GitErrorCode;
      pub fn http_status(&self) -> axum::http::StatusCode;
      pub fn with_repo(self, repo_id: &str) -> GitError;
      pub fn internal(message: impl Into<String>) -> GitError;
      pub fn invalid_request(field: &str, message: impl Into<String>) -> GitError;
      pub fn repo_busy(id: &str, op: &str) -> GitError;
      pub fn registry_read_only() -> GitError;
      pub fn purge_failed(e: impl std::fmt::Display) -> GitError;
      pub fn path_refused(path: &std::path::Path, why: &str) -> GitError;
      pub fn status_timeout() -> GitError;
      pub fn remote_missing() -> GitError;
      pub fn confirm_required() -> GitError;
  }
  impl GitErrorCode { pub fn as_str(self) -> &'static str;
                      pub fn http_status(self) -> axum::http::StatusCode; }  // Copy + PartialEq
  impl axum::response::IntoResponse for GitError;   // §5.4 envelope WITHOUT host_instance
  impl std::fmt::Display for GitError;
  pub enum AbortReason { None, Timeout, Shutdown }
  pub fn now_ms() -> u64;
  ```

- **Consumes** — from `src/git/registry.rs` + `src/git/state.rs` (task 4):

  ```rust
  pub struct RepoDef { /* 14 pub fields; Deserialize only, never Serialize */ }
  pub enum CredentialSpec { None, Token{..}, SshKey{..} }   // Deserialize
  pub struct AuthorSpec { pub name: Option<String>, pub email: Option<String> }
  pub struct RepoView;  impl RepoView { pub fn new(def: &RepoDef, repos_dir: &Path) -> RepoView; }
  pub struct RegistryError;  pub struct Warning { pub code: &'static str, pub message: String }
  pub struct RegistryDefaults { pub default_branch: String, pub settings_enabled: bool,
                                pub allow_http: bool, pub registry_writes: bool }
  pub struct PutOutcome { pub created: bool, pub repo: RepoDef, pub warnings: Vec<Warning> }
  impl Registry {
      pub fn load(path: &Path, repos_dir: &Path, defaults: RegistryDefaults) -> Registry;
      pub fn path(&self) -> &Path;      pub fn writable(&self) -> bool;
      pub fn repos_dir(&self) -> &Path;   // the ONLY copy of the repos root, see below
      pub fn error(&self) -> Option<RegistryError>;   pub fn notes(&self) -> &[String];
      pub fn count(&self) -> usize;     pub fn ids(&self) -> Vec<String>;
      pub fn list(&self) -> Vec<RepoDef>;
      pub fn snapshot(&self, id: &str) -> Result<RepoDef, GitError>;   // repo_not_found
      pub fn auto_sync_secs(&self, id: &str) -> Option<u64>;
      pub fn put(&self, def: RepoDef) -> Result<PutOutcome, GitError>;
      pub fn remove(&self, id: &str) -> Result<RepoDef, GitError>;
  }
  pub fn validate_id(id: &str) -> Result<(), GitError>;     // invalid_repo_id
  pub struct LastSync { pub at_ms: u64, pub ok: bool, pub op: String, pub job_id: String,
                        pub outcome: Option<String>, pub head: Option<String>,
                        pub code: Option<String>, pub message: Option<String> }
  impl StateStore {
      pub fn load(path: &Path) -> StateStore;   pub fn load_error(&self) -> Option<&str>;
      pub fn fingerprint(&self, repo_id: &str) -> Option<String>;
      pub fn record_fingerprint(&self, repo_id: &str, fingerprint: &str);
      pub fn last_sync(&self, repo_id: &str) -> Option<LastSync>;
      pub fn record_last_sync(&self, repo_id: &str, last: LastSync);
      pub fn forget(&self, repo_id: &str);
  }
  ```

- **Consumes** — from `src/git/jobs.rs` (task 5):

  ```rust
  pub const MAX_JOB_RECORDS: usize = 50;  pub const JOB_TTL_SECS: u64 = 3_600;
  pub const JOB_MIN_AGE_SECS: u64 = 60;
  pub struct JobId(String);  impl std::fmt::Display for JobId;
  pub enum JobOp { Init, Clone, Pull, Push, Sync, Commit, Branch, Reset }  // Copy
  impl JobOp { pub fn as_str(self) -> &'static str; }
  pub enum JobState { Running, Succeeded, Failed }
  impl JobState { pub fn parse(s: &str) -> Option<JobState>; }
  pub struct JobSlot { pub id: JobId, pub repo_id: String, pub op: JobOp,
                       pub abort: Arc<AbortFlag>, /* … */ }
  impl JobSlot {
      pub fn begin(&self);  pub fn succeed(&self, result: &serde_json::Value);
      pub fn fail(&self, error: &GitError);  pub fn is_terminal(&self) -> bool;
      pub fn mark_stalled(&self);
      pub fn view(&self, host_instance: &str) -> JobView;
      pub fn as_ref_view(&self) -> JobRef;
  }
  pub struct JobView;  pub struct JobRef;   // both Debug + Clone + Serialize
  pub struct JobFilter { pub repo_id: Option<String>, pub state: Option<JobState>,
                         pub limit: usize }               // limit == 0 means unlimited
  pub enum Admission { Started(Arc<JobSlot>, RepoLease), Replay(Arc<JobSlot>), Busy(Arc<JobSlot>) }
  pub struct RepoLease;                                   // no public constructor
  impl JobStore {
      pub fn new(host_instance: String) -> Arc<JobStore>;
      pub fn host_instance(&self) -> &str;
      pub fn admit(self: &Arc<Self>, repo_id: &str, op: JobOp, request_id: Option<&str>)
          -> Result<Admission, GitError>;
      pub fn lookup(&self, raw: &str) -> Result<Arc<JobSlot>, GitError>;   // job_not_found
      pub fn list(&self, filter: &JobFilter) -> Vec<Arc<JobSlot>>;         // newest first
      pub fn busy(&self, repo_id: &str) -> Option<Arc<JobSlot>>;
      pub fn set_draining(&self, draining: bool);  pub fn draining(&self) -> bool;
      pub fn abort_all(&self, reason: AbortReason);  pub fn forget_repo(&self, repo_id: &str);
  }
  pub fn random_host_instance() -> String;
  ```

- **Consumes** — from `src/git/creds.rs` + `src/git/ops.rs` (tasks 6, 7, 8):

  ```rust
  pub fn resolve(per_call: Option<&CredentialSpec>, stored: Option<&CredentialSpec>,
                 remote: Option<&str>) -> Result<CredResolution, GitError>;
  pub struct CredResolution { pub cred: ResolvedCred, pub unbound: bool }
  impl HostKeyChecker { pub fn new(policy: SshHostKeyPolicy, pinned: Option<String>) -> HostKeyChecker; }
  pub struct Identity;  impl Identity { pub fn new(app_name: &str, identifier: &str) -> Identity; }
  pub struct SettingsCtx { pub schema: SettingsSection, pub settings_file: PathBuf }  // Clone
  pub struct BranchRequest { pub name: String, pub create: bool, pub from: Option<String>,
                             pub checkout: bool, pub upstream: Option<Option<String>> }
  pub struct ResetRequest { pub to: String, pub clean_untracked: bool, pub confirm: bool }
  pub struct OpRequest { /* contract §4.9, all pub */ }
  impl Default for OpRequest;  impl OpRequest { pub fn auto() -> OpRequest; pub fn manual() -> OpRequest; }
  pub struct OpScratch;  impl Default for OpScratch;      // task 6's refinement of §4.9
  pub struct OpCtx { /* every field pub, plus `pub scratch: OpScratch` */ }
  pub struct OpOutcome { /* contract §4.9, all pub; Debug + Clone + Serialize */ }
  impl OpOutcome { pub fn new(outcome: &'static str, branch: &str) -> OpOutcome; }
  pub struct MergeReport { pub kind: &'static str, pub merge_commit: Option<String>,
                           pub conflicts_resolved: Vec<String>, pub recover_hint: Option<String> }
  pub struct ReadCtx { pub tree: PathBuf, pub def: RepoDef,
                       pub last_sync: Option<LastSync>, pub busy_job: Option<JobRef> }
  pub struct RepoStatus;  pub struct BranchesResponse;    // both Serialize
  pub fn run(op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError>;
  pub fn status(ctx: &ReadCtx) -> RepoStatus;             // never fails
  pub fn branches(tree: &Path, def: &RepoDef) -> Result<BranchesResponse, GitError>;
  pub fn purge_tree(repos_root_canon: &Path, tree: &Path) -> Result<(), GitError>;
  // src/git/mod.rs, already landed by task 7 — DO NOT re-declare either of these:
  pub const CONNECT_TIMEOUT_MS: i32 = 10_000;
  pub fn init_libgit2_timeouts(network_timeout_secs: u64) -> Result<(), GitError>;
  ```

- **Consumes** — existing host code:
  `crate::internal_server::{AppStatus, HostEvent, HostState, router, start}`,
  `crate::supervisor::{open_log, LOG_MAX_BYTES}`, `axum 0.8.9`, `tokio`, `serde_json`.

- **Produces** — tasks 10, 11 and 12 rely on exactly these:

  ```rust
  // src/git/mod.rs
  pub const STATUS_READ_TIMEOUT_MS: u64 = 2_000;
  pub const QUIT_ABANDON_GRACE_MS: u64 = 2_000;
  pub const GIT_LOG_FILE: &str = "git.log";

  pub trait GitOps: Send + Sync {
      fn run(&self, op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError>;
      // Both defaulted (they forward to `ops::`), so an implementor that only cares
      // about jobs — task 10's `ScriptedOps` — writes neither. See refinement 5.
      fn status(&self, ctx: &ReadCtx) -> RepoStatus;
      fn branches(&self, tree: &Path, def: &RepoDef) -> Result<BranchesResponse, GitError>;
  }
  pub struct RealOps;
  pub fn contained_in(root_canon: &Path, candidate: &Path) -> Result<PathBuf, GitError>;

  pub struct ServiceInfo;  pub struct ServiceDefaults;  pub struct AuthorView;
  pub struct ServiceFeatures;  pub struct GitStatusSummary;  pub struct RepoStatusSummary;
  pub struct RepoWithStatus { pub repo: RepoView, pub status: RepoStatus }
  pub struct DeleteOutcome { pub deleted: bool, pub purged: bool, pub path: String }
  pub enum StartOutcome { Started(Arc<JobSlot>), Replay(Arc<JobSlot>), Busy(Arc<JobSlot>) }
  impl StartOutcome { pub fn slot(&self) -> &Arc<JobSlot>; }

  pub struct GitService;                        // the 27 methods of contract §5.10

  // src/git/api.rs
  pub fn router(state: HostState) -> axum::Router<HostState>;
  pub fn disabled_router() -> axum::Router<HostState>;
  pub struct ApiError { pub err: GitError, pub host_instance: String, pub job: Option<JobRef> }
  pub const MAX_REQUEST_ID_LEN: usize = 128;
  pub const DEFAULT_JOB_LIST_LIMIT: usize = 50;
  pub const MAX_JOB_LIST_LIMIT: usize = 200;
  pub struct RepoDefBody;  impl RepoDefBody { pub fn into_def(self, id: String) -> RepoDef; }
  pub struct InitBody;  pub struct CloneBody;  pub struct PullBody;  pub struct PushBody;
  pub struct SyncBody;  pub struct CommitBody; pub struct BranchBody; pub struct ResetBody;
  pub struct JobsQuery;  pub struct DeleteQuery;
  pub struct ReposResponse;  pub struct RepoResponse;  pub struct JobResponse;  pub struct JobsResponse;

  // src/internal_server.rs
  pub enum HostEvent { RestartRequested,
                       GitRestartChildren { repo_id: String, reason: &'static str },
                       GitFailed { repo_id: String, op: String, code: String, message: String },
                       GitConflictsResolved { repo_id: String, merge_commit: String, paths: Vec<String> } }
  pub struct HostState { /* … */ pub git: Option<Arc<crate::git::GitService>> }
  pub(crate) fn authorized(headers: &HeaderMap, state: &HostState) -> bool;
  ```

**Five deliberate refinements of the interface contract.** Each is called out so the
consistency audit sees it rather than flagging it:

1. **`api::router` takes the state: `pub fn router(state: HostState) -> Router<HostState>`.**
   Contract §5.11 writes `router()`, but `axum::middleware::from_fn_with_state(state, f)`
   takes a *state value*, and `from_fn` (the no-value form) explicitly does not support the
   `State` extractor. A no-argument constructor therefore cannot carry the one auth layer
   §5 requires. `internal_server::router` already owns a `HostState` at the nest site, so it
   passes a clone; the returned router is still `Router<HostState>` and its handlers are
   still resolved by the outer `.with_state(state)`. Verified against axum 0.8.9.
2. **`GitService.log` is `Mutex<Option<File>>`, not `Mutex<File>`.** `GitService::new` is
   required to create nothing, so the log is opened in `start()`; and a `git.log` that
   cannot be opened must not disable the git service or crash the host. `App::chrome_log`
   already sets this precedent (`Option<std::fs::File>`, `None` when it cannot be opened).
3. **`GitService.repos_root_canon` is computed by `canonical_root`, not `canonicalize`.**
   `repos/` does not exist on a first launch and `canonicalize` fails on a missing path,
   while on macOS the data dir sits under a symlinked `/var` — a non-canonical root would
   make every containment check answer `path_refused`. `canonical_root` resolves the
   deepest ancestor that does exist and re-appends the rest.
4. **`after_job` takes `&mut Result<OpOutcome, GitError>` and publishes the job itself.**
   `result.restart_requested` and the `restart_deferred` warning are fields *of* the result,
   so the restart decision has to happen before `JobSlot::succeed` is called. The spec's
   sketch publishes first and decides after, which cannot produce either field.
5. **`GitOps` carries two defaulted read methods, `status` and `branches`.** Contract §4.12
   gives the trait `run` alone, which leaves `status_timeout` — spec §5.1 row 6, the one
   route that can answer **504** — with no way to be tested short of a genuinely wedged
   filesystem. Routing the two bounded reads through the same seam as jobs makes the
   ceiling drivable from a test that stalls one method. Both have default bodies that
   forward to `crate::git::ops::`, so every existing implementor (`RealOps`, task 10's
   `ScriptedOps`, this task's `FakeOps`/`StubOps`) is unchanged and keeps the real
   behaviour. Step 33 adds them; Step 31 and Step 87 are the tests that need them.

**One forward dependency, deliberately unreachable until task 11.** `decide_restart`'s
`out.settings_changed` term, and the `reason: "settings"` branch it selects in `after_job`,
have **no writer in this task**: `OpOutcome.settings_changed` is set only by task 11's
settings-sync cycle (task 11 Cycle D). Until that lands the branch is unreachable and
therefore untested, and `[git]`'s own tests cannot cover it — the field is always `false`.
This is not a bug and must not be "fixed" by deleting the term: writing the condition here
is what keeps the restart policy in one place instead of splitting it across two tasks.

---

## Part 1 — `src/git/mod.rs`

- [ ] **Step 1: Write the failing tests for `contained_in`**

Append inside the existing `#[cfg(test)] mod tests { … }` block at the bottom of
`src/git/mod.rs` (task 7 left it there with the `init_libgit2_timeouts` test). If it already
has `use super::*;`, do not add a second one.

```rust
    #[test]
    fn contained_in_accepts_a_child_and_refuses_an_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repos");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::create_dir_all(dir.path().join("secret")).unwrap();
        let root_canon = std::fs::canonicalize(&root).unwrap();

        let ok = contained_in(&root_canon, &root.join("notes")).expect("a child is contained");
        assert!(ok.ends_with("notes"));

        // The root itself is not "inside" the root: purging it would delete every repo.
        let err = contained_in(&root_canon, &root).expect_err("the root is not contained");
        assert_eq!(err.code(), GitErrorCode::PathRefused);

        let err = contained_in(&root_canon, &dir.path().join("secret"))
            .expect_err("a sibling is not contained");
        assert_eq!(err.code(), GitErrorCode::PathRefused);
        assert!(err.path.is_some(), "path_refused must name the path it refused");
    }

    #[test]
    fn contained_in_resolves_a_symlink_before_deciding() {
        // The one attack this function exists for: `repos/<id>` is a symlink someone
        // planted, pointing at a tree the host does not own.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repos");
        std::fs::create_dir_all(&root).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let root_canon = std::fs::canonicalize(&root).unwrap();
        let link = root.join("notes");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, &link).unwrap();
        let err = contained_in(&root_canon, &link).expect_err("a symlink out is refused");
        assert_eq!(err.code(), GitErrorCode::PathRefused);
    }

    #[test]
    fn canonical_root_resolves_the_deepest_existing_ancestor() {
        // `repos/` does not exist before `start()` runs, and on macOS the data dir sits
        // under a symlinked /var — a non-canonical root makes every containment check
        // answer path_refused.
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(canonical_root(&dir.path().join("repos")), real.join("repos"));
        assert_eq!(
            canonical_root(&dir.path().join("a").join("b")),
            real.join("a").join("b")
        );
    }
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::tests::contained_in`

Expected: FAIL — compile errors, one per missing item.

```
error[E0425]: cannot find function `contained_in` in this scope
  --> src/git/mod.rs:NN:18
   |
NN |         let ok = contained_in(&root_canon, &root.join("notes")).expect("a child is contained");
   |                  ^^^^^^^^^^^^ not found in this scope

error[E0425]: cannot find function `canonical_root` in this scope
error[E0433]: failed to resolve: use of undeclared type `GitErrorCode`
```

- [ ] **Step 3: Implement `contained_in` and `canonical_root`**

Extend the import block at the top of `src/git/mod.rs` (task 7 left `use crate::git::error::GitError;`
there) so it reads:

```rust
use crate::git::error::{GitError, GitErrorCode};
use std::path::{Path, PathBuf};
```

Then add, above the `#[cfg(test)]` line:

```rust
/// Resolve `candidate` and refuse it unless it lands strictly inside `root_canon`.
///
/// Both sides are canonical, which is the only form in which the question is
/// answerable: a symlink at `repos/<id>` pointing outside the root would otherwise pass a
/// textual `starts_with`. The root itself is refused as well — every caller is about to
/// delete or unlink something, and "inside the root" must never include the root.
pub fn contained_in(root_canon: &Path, candidate: &Path) -> Result<PathBuf, GitError> {
    let real = std::fs::canonicalize(candidate)
        .map_err(|e| GitError::path_refused(candidate, &format!("cannot be resolved: {e}")))?;
    if real == root_canon || !real.starts_with(root_canon) {
        return Err(GitError::path_refused(
            &real,
            &format!("resolves outside {}", root_canon.display()),
        ));
    }
    Ok(real)
}

/// Canonical form of a path that may not exist yet.
///
/// `repos/` is created by `GitService::start`, but `repos_root_canon` is computed in
/// `new`, and `canonicalize` fails outright on a missing path. Resolving the deepest
/// ancestor that does exist keeps the root canonical on macOS, where the data directory
/// lives under a symlinked `/var` and a textual root would refuse every purge.
fn canonical_root(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => canonical_root(parent).join(name),
        // A path with no parent is already a root; there is nothing left to resolve.
        _ => path.to_path_buf(),
    }
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::tests::c`

Expected: PASS.

```
running 3 tests
test git::tests::canonical_root_resolves_the_deepest_existing_ancestor ... ok
test git::tests::contained_in_accepts_a_child_and_refuses_an_escape ... ok
test git::tests::contained_in_resolves_a_symlink_before_deciding ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 5: Commit**

```bash
git add src/git/mod.rs
git commit -m "feat(git): path containment for the repos root

Both sides are canonicalized because that is the only form in which the
question is answerable: a symlink planted at repos/<id> passes a textual
starts_with. canonical_root exists because repos/ does not exist until
start() runs and macOS puts the data dir under a symlinked /var."
```

---

- [ ] **Step 6: Write the failing test for construction**

Append to `mod tests` in `src/git/mod.rs`:

```rust
    fn config(git: &str) -> crate::config::AppConfig {
        crate::config::AppConfig::from_str(&format!(
            "[app]\nname = \"Test App\"\nidentifier = \"com.example.test\"\n{git}"
        ))
        .expect("config parses")
    }

    /// A `GitOps` that records what it was asked to do and hands back a canned answer.
    /// Every test in this file is therefore a test of the service, never of libgit2.
    struct FakeOps {
        calls: std::sync::Mutex<Vec<(JobOp, String)>>,
        result: std::sync::Mutex<Result<OpOutcome, GitError>>,
    }

    impl FakeOps {
        fn returning(result: Result<OpOutcome, GitError>) -> Arc<FakeOps> {
            Arc::new(FakeOps {
                calls: std::sync::Mutex::new(Vec::new()),
                result: std::sync::Mutex::new(result),
            })
        }
        fn ok() -> Arc<FakeOps> {
            FakeOps::returning(Ok(OpOutcome::new("up_to_date", "main")))
        }
        fn calls(&self) -> Vec<(JobOp, String)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl GitOps for FakeOps {
        fn run(&self, op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError> {
            self.calls.lock().unwrap().push((op, ctx.def.id.clone()));
            self.result.lock().unwrap().clone()
        }
    }

    #[tokio::test]
    async fn no_git_section_means_no_service_and_no_directories() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::config::RuntimePaths::under(dir.path(), "com.example.test");
        paths.ensure().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let status = Arc::new(tokio::sync::RwLock::new(AppStatus::Starting));
        let svc = GitService::new(
            &config(""),
            &paths,
            tokio::runtime::Handle::current(),
            tx,
            status,
        );
        assert!(svc.is_none(), "[git] absent must yield no service");
        // The whole promise of the off switch: nothing on disk changes.
        assert!(!paths.repos_dir.exists());
        assert!(!paths.registry_file.exists());
        assert!(!paths.logs_dir.join(GIT_LOG_FILE).exists());
    }

    #[tokio::test]
    async fn a_git_section_yields_a_service_that_has_created_nothing_yet() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::config::RuntimePaths::under(dir.path(), "com.example.test");
        paths.ensure().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let status = Arc::new(tokio::sync::RwLock::new(AppStatus::Starting));
        let svc = GitService::new(
            &config("[git]\n"),
            &paths,
            tokio::runtime::Handle::current(),
            tx,
            status,
        )
        .expect("[git] present yields a service");
        assert_eq!(svc.host_instance().len(), 8);
        assert_eq!(svc.repo_count(), 0);
        assert!(svc.registry_writes());
        assert!(!svc.tray_sync() && !svc.error_dialogs() && !svc.status_api());
        assert_eq!(svc.tree_path("notes"), paths.repos_dir.join("notes"));
        assert_eq!(svc.log_path(), paths.logs_dir.join(GIT_LOG_FILE));
        // `new` reads; only `start` writes.
        assert!(!paths.repos_dir.exists());
    }
```

- [ ] **Step 7: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::tests::a_git_section`

Expected: FAIL — compile errors.

```
error[E0433]: failed to resolve: use of undeclared type `GitService`
error[E0433]: failed to resolve: use of undeclared type `JobOp`
error[E0405]: cannot find trait `GitOps` in this scope
error[E0425]: cannot find value `GIT_LOG_FILE` in this scope
```

- [ ] **Step 8: Implement the constants, `GitOps`, and `GitService::new`**

Add `pub mod api;` as the first line of the `pub mod` block in `src/git/mod.rs`, then extend
the import block so it reads:

```rust
use crate::config::{AppConfig, GitSection, RuntimePaths};
use crate::git::error::{GitError, GitErrorCode};
use crate::git::jobs::{
    random_host_instance, JobOp, JobStore, JOB_MIN_AGE_SECS, JOB_TTL_SECS, MAX_JOB_RECORDS,
};
use crate::git::ops::{Identity, OpCtx, OpOutcome, SettingsCtx};
use crate::git::registry::{Registry, RegistryDefaults, RegistryError};
use crate::git::state::StateStore;
use crate::internal_server::{AppStatus, HostEvent};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
```

Then add, above the `#[cfg(test)]` line:

```rust
/// A bounded status read (`GET /repos/:id/status`) gives up after this and answers
/// `status_timeout`, so a wedged filesystem cannot hang the caller's poll loop.
pub const STATUS_READ_TIMEOUT_MS: u64 = 2_000;
/// After the quit deadline every job is aborted; this is how long they get to notice
/// before the host logs them as abandoned and exits.
pub const QUIT_ABANDON_GRACE_MS: u64 = 2_000;
pub const GIT_LOG_FILE: &str = "git.log";

/// The single seam between `GitService` and libgit2.
///
/// Injected rather than called directly so the service's own tests — admission, the
/// job lifecycle, the event decisions, every HTTP status — run without a network, a
/// remote, or a working tree. `chrome::find_first_existing(paths, exists)` is the same
/// pattern already in this codebase.
pub trait GitOps: Send + Sync {
    fn run(&self, op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError>;
}

/// The production implementation: one call, no state, no decisions.
pub struct RealOps;

impl GitOps for RealOps {
    fn run(&self, op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError> {
        crate::git::ops::run(op, ctx)
    }
}

/// The git subsystem, and the only type in `src/git/` the rest of the host names.
///
/// It deliberately holds **no** `Children`, no `chrome_generation`, no
/// `server_generation`, and it never calls `rt.enter()` or `tokio::process`. Its only
/// channel into the tao event loop is `HostEvent`, which already exists and is already
/// bridged. `status` is the one exception and it is read-only: §9.7's third restart
/// condition ("the app is Ready") is unimplementable without it, and it is read with
/// `try_read` from a blocking thread, never written.
pub struct GitService {
    cfg: GitSection,
    identity: Identity,
    repos_root_canon: PathBuf,
    log_path: PathBuf,
    /// There is deliberately **no** `repos_dir` field. `Registry` is handed the repos
    /// root at `load` and owns it; a second, independently-computed copy here is exactly
    /// the drift task 4 forbids, so every path in this file goes through
    /// `self.registry.repos_dir()`.
    registry: Registry,
    state: StateStore,
    jobs: Arc<JobStore>,
    rt: tokio::runtime::Handle,
    events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
    /// `None` until `start()` opens it, and `None` forever if it cannot be opened: a
    /// log file the host cannot write must not disable git or crash the app.
    log: Mutex<Option<std::fs::File>>,
    settings: Option<SettingsCtx>,
    ops: Arc<dyn GitOps>,
    timers: Mutex<BTreeSet<String>>,
    status: Arc<tokio::sync::RwLock<AppStatus>>,
}

impl GitService {
    /// `None` when `[git]` is absent. Creates no directory and runs no libgit2 code —
    /// with the section absent the feature must be invisible on disk.
    pub fn new(
        cfg: &AppConfig,
        paths: &RuntimePaths,
        rt: tokio::runtime::Handle,
        events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
        status: Arc<tokio::sync::RwLock<AppStatus>>,
    ) -> Option<Arc<GitService>> {
        let git = cfg.git.clone()?;
        Some(Self::build(
            cfg,
            git,
            paths,
            rt,
            events,
            status,
            Arc::new(RealOps),
        ))
    }

    #[cfg(test)]
    pub fn with_ops(
        cfg: &AppConfig,
        paths: &RuntimePaths,
        rt: tokio::runtime::Handle,
        events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
        status: Arc<tokio::sync::RwLock<AppStatus>>,
        ops: Arc<dyn GitOps>,
    ) -> Arc<GitService> {
        let git = cfg
            .git
            .clone()
            .expect("with_ops is only called from tests that supply a [git] section");
        Self::build(cfg, git, paths, rt, events, status, ops)
    }

    fn build(
        cfg: &AppConfig,
        git: GitSection,
        paths: &RuntimePaths,
        rt: tokio::runtime::Handle,
        events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
        status: Arc<tokio::sync::RwLock<AppStatus>>,
        ops: Arc<dyn GitOps>,
    ) -> Arc<GitService> {
        let defaults = RegistryDefaults {
            default_branch: git.default_branch.clone(),
            settings_enabled: cfg.settings_enabled(),
            allow_http: git.allow_http,
            registry_writes: git.registry_writes,
        };
        let settings = if cfg.settings_enabled() {
            cfg.settings.clone().map(|schema| SettingsCtx {
                schema,
                settings_file: paths.settings_file.clone(),
            })
        } else {
            None
        };
        Arc::new(GitService {
            identity: Identity::new(&cfg.app.name, &cfg.app.identifier),
            registry: Registry::load(&paths.registry_file, &paths.repos_dir, defaults),
            state: StateStore::load(&paths.git_state_file),
            jobs: JobStore::new(random_host_instance()),
            repos_root_canon: canonical_root(&paths.repos_dir),
            log_path: paths.logs_dir.join(GIT_LOG_FILE),
            cfg: git,
            rt,
            events,
            log: Mutex::new(None),
            settings,
            ops,
            timers: Mutex::new(BTreeSet::new()),
            status,
        })
    }

    pub fn host_instance(&self) -> &str {
        self.jobs.host_instance()
    }

    pub fn jobs(&self) -> &Arc<JobStore> {
        &self.jobs
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn repo_count(&self) -> usize {
        self.registry.count()
    }

    pub fn tray_sync(&self) -> bool {
        self.cfg.tray_sync
    }

    pub fn error_dialogs(&self) -> bool {
        self.cfg.error_dialogs
    }

    pub fn status_api(&self) -> bool {
        self.cfg.status_api
    }

    /// One source of truth: `Registry::load` also turns this off when `repos.json`
    /// itself is unwritable, and callers must see that, not just the config key.
    pub fn registry_writes(&self) -> bool {
        self.registry.writable()
    }

    pub fn draining(&self) -> bool {
        self.jobs.draining()
    }

    pub fn auto_sync_secs(&self, id: &str) -> Option<u64> {
        self.registry.auto_sync_secs(id)
    }

    /// One source of truth for the repos root: `Registry` was handed it at `load`.
    pub fn tree_path(&self, id: &str) -> PathBuf {
        self.registry.repos_dir().join(id)
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }
}
```

Also create `src/git/api.rs` with nothing but a module comment so `pub mod api;` resolves:

```rust
//! The `/api/git` sub-router: DTOs in, `GitService` calls out, one error envelope back.
//!
//! No `git2` type crosses this file. Every route — GETs included — is authenticated by
//! the single `from_fn_with_state` layer at the bottom of `router`, so no handler can
//! forget the token.
```

- [ ] **Step 9: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::tests::`

Expected: PASS.

```
running 5 tests
test git::tests::a_git_section_yields_a_service_that_has_created_nothing_yet ... ok
test git::tests::no_git_section_means_no_service_and_no_directories ... ok
...
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 10: Commit**

```bash
git add src/git/mod.rs src/git/api.rs
git commit -m "feat(git): GitService construction and the GitOps seam

new() returns None without [git] and touches nothing on disk, which is the
whole promise of the off switch. GitOps is injected so the service's own
tests never reach libgit2 — the same shape as chrome::find_first_existing."
```

---

- [ ] **Step 11: Write the failing test for `start()` and the log**

Append to `mod tests` in `src/git/mod.rs`:

```rust
    /// Everything a service test needs, alive for the test's duration.
    struct Svc {
        _dir: tempfile::TempDir,
        paths: crate::config::RuntimePaths,
        svc: Arc<GitService>,
        events: tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
        status: Arc<tokio::sync::RwLock<AppStatus>>,
    }

    impl Svc {
        fn git_log(&self) -> String {
            std::fs::read_to_string(self.svc.log_path()).unwrap_or_default()
        }
    }

    async fn service(git: &str, ops: Arc<dyn GitOps>) -> Svc {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::config::RuntimePaths::under(dir.path(), "com.example.test");
        paths.ensure().unwrap();
        let (tx, events) = tokio::sync::mpsc::unbounded_channel();
        let status = Arc::new(tokio::sync::RwLock::new(AppStatus::Ready {
            target_url: "http://x/".to_string(),
        }));
        let svc = GitService::with_ops(
            &config(git),
            &paths,
            tokio::runtime::Handle::current(),
            tx,
            status.clone(),
            ops,
        );
        svc.start();
        Svc {
            _dir: dir,
            paths,
            svc,
            events,
            status,
        }
    }

    #[tokio::test]
    async fn start_creates_the_repos_dir_and_logs_one_startup_line() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        assert!(fx.paths.repos_dir.is_dir(), "start() owns the repos root");
        let log = fx.git_log();
        let line = log.lines().last().expect("a startup line");
        // §3.18: startup records use op=startup and job=-, so one grep finds them all.
        let fields: Vec<&str> = line.split(' ').collect();
        assert!(fields[0].parse::<u64>().is_ok(), "field 0 is epoch millis: {line}");
        assert_eq!(fields[1], "startup");
        assert_eq!(fields[2], "repo=-");
        assert_eq!(fields[3], "job=-");
        assert_eq!(fields[4], "ok");
        assert_eq!(fields[5], "code=-");
        assert!(line.contains(fx.svc.host_instance()), "{line}");
        assert!(line.contains("repos=0"), "{line}");
    }

    #[tokio::test]
    async fn start_is_idempotent_and_a_second_run_appends() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        let before = fx.git_log().lines().count();
        fx.svc.start();
        assert_eq!(fx.git_log().lines().count(), before * 2);
    }
```

- [ ] **Step 12: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::tests::start_`

Expected: FAIL — compile error.

```
error[E0599]: no method named `start` found for struct `Arc<GitService>` in the current scope
   --> src/git/mod.rs:NN:17
    |
NN  |         svc.start();
    |             ^^^^^ method not found in `Arc<GitService>`
```

- [ ] **Step 13: Implement `start`, `log`, and `log_startup`**

Add `use std::io::Write;` and `use crate::git::util::now_ms;` to the import block, then add
these methods inside `impl GitService`:

```rust
    /// Everything the service does exactly once, at host startup: open the log, take
    /// ownership of `repos/`, apply libgit2's process-global timeouts, report whatever
    /// the registry and state files had to say, clear stale index locks, and start the
    /// auto-sync timers.
    pub fn start(self: &Arc<Self>) {
        match crate::supervisor::open_log(&self.log_path, crate::supervisor::LOG_MAX_BYTES) {
            Ok(file) => *self.log_guard() = Some(file),
            // `App` owns the host log and `src/git/` is forbidden to touch it, so stderr
            // is the only sink left for "the log itself failed".
            Err(e) => eprintln!("git: cannot open {}: {e}", self.log_path.display()),
        }
        if let Err(e) = std::fs::create_dir_all(self.registry.repos_dir()) {
            self.log_startup(&format!(
                "cannot create {}: {e}",
                self.registry.repos_dir().display()
            ));
        }
        if let Err(e) = init_libgit2_timeouts(self.cfg.network_timeout_secs) {
            self.log_startup(&format!("libgit2 timeouts not applied: {e}"));
        }
        for note in self.registry.notes() {
            self.log_startup(note);
        }
        if let Some(err) = self.registry.error() {
            self.log_startup(&format!("registry {}: {}", err.code, err.message));
        }
        if let Some(err) = self.state.load_error() {
            self.log_startup(&format!("git-state.json unreadable, starting empty: {err}"));
        }
        self.recover_index_locks();
        for id in self.registry.ids() {
            self.spawn_auto_timer(&id);
        }
        self.log_startup(&format!(
            "instance={} repos={} root={}",
            self.jobs.host_instance(),
            self.registry.count(),
            self.registry.repos_dir().display()
        ));
    }

    /// A panic while a log line was being written must not silence the log for the rest
    /// of the process — the guarded value is a file handle, not an invariant.
    fn log_guard(&self) -> std::sync::MutexGuard<'_, Option<std::fs::File>> {
        self.log.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn log(&self, line: &str) {
        if let Some(file) = self.log_guard().as_mut() {
            let _ = writeln!(file, "{line}");
        }
    }

    fn log_startup(&self, message: &str) {
        self.log(&format!(
            "{} startup repo=- job=- ok code=- {}",
            now_ms(),
            one_line(message)
        ));
    }
```

And, as a free function above `impl GitService`:

```rust
/// `git.log` is one record per line, and libgit2 messages sometimes contain newlines.
/// A wrapped record is unparseable by anything reading the file, including a human.
fn one_line(message: &str) -> String {
    message.replace(['\n', '\r'], " ")
}
```

Add a placeholder for the two methods `start` calls that do not exist yet, so the file
compiles; the next two cycles replace their bodies:

```rust
    fn recover_index_locks(&self) {}

    fn spawn_auto_timer(self: &Arc<Self>, _id: &str) {}
```

- [ ] **Step 14: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::tests::start_`

Expected: PASS.

```
running 2 tests
test git::tests::start_creates_the_repos_dir_and_logs_one_startup_line ... ok
test git::tests::start_is_idempotent_and_a_second_run_appends ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 15: Commit**

```bash
git add src/git/mod.rs
git commit -m "feat(git): GitService::start and the git.log record format

The log handle is Option because a git.log that cannot be opened must not
disable git or crash the host — the same shape App::chrome_log already uses.
Every record is forced onto one line: libgit2 messages contain newlines and
a wrapped record is unparseable."
```

---

- [ ] **Step 16: Write the failing tests for stale `index.lock` recovery**

Append to `mod tests` in `src/git/mod.rs`:

```rust
    fn repo_def(id: &str, remote: Option<&str>) -> crate::git::registry::RepoDef {
        crate::git::registry::RepoDef {
            id: id.to_string(),
            remote: remote.map(str::to_string),
            remote_name: "origin".to_string(),
            branch: String::new(),
            credential: None,
            author: None,
            sync_settings: false,
            settings_path: "settings.json".to_string(),
            auto_sync_secs: None,
            sync_on_start: false,
            sync_on_quit: false,
            restart_children_on_pull: false,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn plant_index_lock(tree: &Path, age: std::time::Duration) -> PathBuf {
        let lock = tree.join(".git").join("index.lock");
        std::fs::create_dir_all(tree.join(".git")).unwrap();
        std::fs::write(&lock, b"").unwrap();
        let when = std::time::SystemTime::now() - age;
        // filetime is not a dependency; set mtime through the platform API the std
        // library already links.
        let f = std::fs::File::options().write(true).open(&lock).unwrap();
        f.set_modified(when).unwrap();
        lock
    }

    #[tokio::test]
    async fn startup_clears_a_stale_index_lock_and_keeps_a_fresh_one() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("stale", repo_def("stale", None)).unwrap();
        fx.svc.put_repo("live", repo_def("live", None)).unwrap();
        let stale = plant_index_lock(
            &fx.svc.tree_path("stale"),
            std::time::Duration::from_secs(600),
        );
        let live = plant_index_lock(&fx.svc.tree_path("live"), std::time::Duration::ZERO);

        fx.svc.start();

        assert!(!stale.exists(), "a lock older than this process is ours to clear");
        // The documented case the mtime guard exists for: someone is running `git` in
        // the tree right now, and their lock is not ours to remove.
        assert!(live.exists(), "a lock newer than this process is left alone");
        let log = fx.git_log();
        assert!(log.contains("repo=stale"), "{log}");
        assert!(log.contains("left in place"), "{log}");
    }
```

- [ ] **Step 17: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::tests::startup_clears`

Expected: FAIL — `put_repo` does not exist yet, and the recovery is a no-op.

```
error[E0599]: no method named `put_repo` found for struct `Arc<GitService>` in the current scope
```

- [ ] **Step 18: Implement `recover_index_locks` and `put_repo`/`delete_repo`**

> **Amended by the post-execution audit (F3, F8).** Both mutators originally opened with
> `if !self.registry.writable() { … }` and `if let Some(job) = self.jobs.busy(id) { … }` — a
> readability check that could not word the second refusal, and a **sample** where a lock was
> needed. `JobStore::busy` releases the mutex before it returns, so an auto-sync tick admitted in
> the gap between the sample and the purge left libgit2 writing into the directory
> `remove_dir_all` was walking; and on the `path_refused` early return, `state.forget` and
> `jobs.forget_repo` sat *below* the purge and never ran, leaving a `last_sync` row and job
> records for an id that was no longer registered. `hold_repo` (task 5, step 63) is the fix, and
> the two forgets move up to sit directly under `registry.remove`.

Add to the import block: `use crate::git::registry::{PutOutcome, RepoDef, RepoView};` and
`use crate::git::ops;`. Replace the `recover_index_locks` placeholder and add the registry
mutators inside `impl GitService`:

```rust
    /// `std::process::exit(0)` runs no destructors, so an abandoned job can leave one
    /// durable residue: `.git/index.lock`. Clearing it is safe because the host holds
    /// `app.lock` and owns `<data-dir>/repos/`.
    ///
    /// Deliberately **no** `reset --hard` and **no** `cleanup_state()`: our merge never
    /// enters `RepositoryState::Merge`, so a merge state in one of these trees was
    /// created by a human and discarding their staged resolution would be unrecoverable.
    fn recover_index_locks(&self) {
        // `start()` runs during host startup, so "now" is process start to within the
        // startup path. The guard only has to distinguish a lock left by a previous run
        // from one a `git` process created moments ago.
        let started = std::time::SystemTime::now();
        for id in self.registry.ids() {
            let tree = self.tree_path(&id);
            // A symlink planted at `repos/<id>` would point the unlink at a repository
            // the host does not own.
            let Ok(real) = contained_in(&self.repos_root_canon, &tree) else {
                continue;
            };
            let lock = real.join(".git").join("index.lock");
            let Ok(meta) = std::fs::metadata(&lock) else {
                continue;
            };
            match meta.modified() {
                Ok(mtime) if mtime < started => match std::fs::remove_file(&lock) {
                    Ok(()) => self.log_startup(&format!(
                        "repo={id} removed stale {}",
                        lock.display()
                    )),
                    Err(e) => self.log_startup(&format!(
                        "repo={id} cannot remove {}: {e}",
                        lock.display()
                    )),
                },
                _ => self.log_startup(&format!(
                    "repo={id} {} is newer than this process; left in place",
                    lock.display()
                )),
            }
        }
    }

    pub fn put_repo(self: &Arc<Self>, id: &str, body: RepoDef) -> Result<PutOutcome, GitError> {
        crate::git::registry::validate_id(id)?;
        // Writability before the hold, so a read-only host answers 403 rather than
        // taking a lock it is about to refuse to use. `ensure_writable` and not
        // `writable()`, because only the former can word the second refusal — a
        // `repos.json` the host could neither read nor quarantine (task 4).
        self.registry
            .ensure_writable()
            .map_err(|e| e.with_repo(id))?;
        // Held rather than sampled, exactly as in `delete_repo`: a job snapshotted its
        // `RepoDef` at admission, and a job admitted in the gap between a `busy()` read
        // and the write below would run the whole way through against a definition the
        // caller has already been told was replaced.
        let _hold = self.jobs.hold_repo(id)?;
        let outcome = self.registry.put(body)?;
        self.spawn_auto_timer(id);
        Ok(outcome)
    }

    pub fn repo_view(&self, id: &str) -> Result<RepoView, GitError> {
        let def = self.registry.snapshot(id)?;
        Ok(RepoView::new(&def, self.registry.repos_dir()))
    }

    pub fn list_views(&self) -> Vec<RepoView> {
        self.registry
            .list()
            .iter()
            .map(|def| RepoView::new(def, self.registry.repos_dir()))
            .collect()
    }
```

- [ ] **Step 19: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::tests::startup_clears`

Expected: PASS.

```
running 1 test
test git::tests::startup_clears_a_stale_index_lock_and_keeps_a_fresh_one ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 20: Commit**

```bash
git add src/git/mod.rs
git commit -m "feat(git): startup index.lock recovery and put_repo

The mtime guard is the whole point: a lock older than this process was left
by a crash of ours, a newer one belongs to a git someone is running by hand.
No reset --hard and no cleanup_state — our merge never leaves a merge state,
so one in a tree here is a human's and discarding it is unrecoverable.
PUT on a busy repo is refused because the running job snapshotted its RepoDef
at admission, so a replace mid-job would silently not apply."
```

---

- [ ] **Step 21: Write the failing tests for `delete_repo` and the mutator guards**

Append to `mod tests` in `src/git/mod.rs`:

```rust
    #[tokio::test]
    async fn delete_forgets_the_definition_and_optionally_purges_the_tree() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let tree = fx.svc.tree_path("notes");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("inbox.md"), b"hi").unwrap();

        let out = fx.svc.delete_repo("notes", false).expect("delete");
        assert!(out.deleted && !out.purged);
        assert!(tree.exists(), "without ?purge the tree survives");
        assert_eq!(fx.svc.repo_count(), 0);

        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let out = fx.svc.delete_repo("notes", true).expect("purge");
        assert!(out.purged);
        assert!(!tree.exists());
        assert_eq!(out.path, tree.display().to_string());
    }

    #[tokio::test]
    async fn delete_of_an_unknown_repo_is_repo_not_found() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        let err = fx.svc.delete_repo("ghost", false).expect_err("unknown id");
        assert_eq!(err.code(), GitErrorCode::RepoNotFound);
    }

    #[tokio::test]
    async fn a_read_only_registry_refuses_both_mutators() {
        let fx = service("[git]\nregistry_writes = false\n", FakeOps::ok()).await;
        let err = fx
            .svc
            .put_repo("notes", repo_def("notes", None))
            .expect_err("read-only");
        assert_eq!(err.code(), GitErrorCode::RegistryReadOnly);
        let err = fx.svc.delete_repo("notes", false).expect_err("read-only");
        assert_eq!(err.code(), GitErrorCode::RegistryReadOnly);
        assert!(!fx.svc.registry_writes());
    }

    #[tokio::test]
    async fn an_invalid_repo_id_never_reaches_the_registry() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        for bad in ["../evil", "A", "with space", ""] {
            let err = fx
                .svc
                .put_repo(bad, repo_def(bad, None))
                .expect_err("rejected");
            assert_eq!(err.code(), GitErrorCode::InvalidRepoId, "accepted {bad:?}");
        }
    }
```

- [ ] **Step 22: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::tests::delete_`

Expected: FAIL — compile error.

```
error[E0599]: no method named `delete_repo` found for struct `Arc<GitService>` in the current scope
   --> src/git/mod.rs:NN:31
    |
NN  |         let out = fx.svc.delete_repo("notes", false).expect("delete");
    |                          ^^^^^^^^^^^ method not found in `Arc<GitService>`
```

- [ ] **Step 23: Implement `delete_repo`**

Add above `impl GitService` in `src/git/mod.rs`:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct DeleteOutcome {
    pub deleted: bool,
    pub purged: bool,
    pub path: String,
}
```

and, inside `impl GitService`, immediately after `put_repo`:

```rust
    pub fn delete_repo(&self, id: &str, purge: bool) -> Result<DeleteOutcome, GitError> {
        crate::git::registry::validate_id(id)?;
        self.registry
            .ensure_writable()
            .map_err(|e| e.with_repo(id))?;
        // Held for the whole call, not sampled: `start_job` snapshots the definition and
        // resolves credentials *before* it admits, so an auto-sync tick can land in the
        // gap between a `busy()` read and the purge below and leave libgit2 writing into
        // the directory `remove_dir_all` is walking. The hold occupies the same map entry
        // `admit` takes, so the two are decided by one acquisition of the job store's
        // mutex, and `Drop` releases it on every path out of here — the refused purge
        // included. `_hold` and not `_`: the latter drops it on the spot.
        let _hold = self.jobs.hold_repo(id)?;
        let tree = self.tree_path(id);
        // Containment is checked before anything is removed: a refusal must leave both
        // the registry entry and the tree exactly as they were.
        if purge && std::fs::symlink_metadata(&tree).is_ok() {
            contained_in(&self.repos_root_canon, &tree).map_err(|e| e.with_repo(id))?;
        }
        // The definition goes first. Purging first and then failing to save would leave
        // a registered repo with no tree, which no retry can repair; this order leaves
        // at worst an orphaned directory, which a re-`PUT` reuses.
        self.registry.remove(id)?;
        // Both belong to the definition, not to the tree: once the entry is gone the id
        // is unregistered whatever the purge does, and a `last_sync` row or a job record
        // left behind by a refusal below would be inherited by the next `PUT` of the same
        // id. `forget_repo` touches only terminal records, never `busy`, so it cannot
        // clear the hold it is running inside.
        self.state.forget(id);
        self.jobs.forget_repo(id);
        let purged = if purge {
            match ops::purge_tree(&self.repos_root_canon, &tree) {
                Ok(()) => true,
                // A refusal keeps its own 403; anything else is a failed removal (500).
                Err(e) if e.code() == GitErrorCode::PathRefused => return Err(e.with_repo(id)),
                Err(e) => return Err(GitError::purge_failed(e).with_repo(id)),
            }
        } else {
            false
        };
        Ok(DeleteOutcome {
            deleted: true,
            purged,
            path: tree.display().to_string(),
        })
    }
```

- [ ] **Step 24: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::tests::`

Expected: PASS.

```
running 11 tests
test git::tests::a_read_only_registry_refuses_both_mutators ... ok
test git::tests::an_invalid_repo_id_never_reaches_the_registry ... ok
test git::tests::delete_forgets_the_definition_and_optionally_purges_the_tree ... ok
test git::tests::delete_of_an_unknown_repo_is_repo_not_found ... ok
...
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 25: Commit**

```bash
git add src/git/mod.rs
git commit -m "feat(git): delete_repo with an idempotent, order-safe purge

Containment is checked before anything is removed so a path_refused is a
true no-op. The definition is removed before the tree: the reverse order can
leave a registered repo with no tree, which no retry repairs, while this one
leaves at worst an orphaned directory that a re-PUT reuses."
```

---

- [ ] **Step 26: Write the failing tests for `service_info` and `status_summary`**

Append to `mod tests` in `src/git/mod.rs`:

```rust
    #[tokio::test]
    async fn service_info_reports_paths_defaults_and_retention() {
        let fx = service("[git]\ndefault_branch = \"trunk\"\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let info = fx.svc.service_info();
        assert_eq!(info.host_instance, fx.svc.host_instance());
        assert_eq!(info.repos_root, fx.paths.repos_dir.display().to_string());
        assert_eq!(info.registry_file, fx.paths.registry_file.display().to_string());
        assert_eq!(info.log_file, fx.svc.log_path().display().to_string());
        assert!(info.registry_writable);
        assert!(info.registry_error.is_none());
        assert_eq!(info.defaults.branch, "trunk");
        assert_eq!(info.defaults.network_timeout_secs, 120);
        // Reported so a caller can see what a commit will be signed with without
        // having to make one: [app].name and <identifier>@<hostname> are the fallbacks.
        assert_eq!(info.defaults.author.name, "Test App");
        assert!(info.defaults.author.email.starts_with("com.example.test@"));
        assert_eq!(info.job_retention, MAX_JOB_RECORDS);
        assert_eq!(info.job_ttl_secs, JOB_TTL_SECS);
        assert_eq!(info.job_min_age_secs, JOB_MIN_AGE_SECS);
        assert_eq!(info.repo_count, 1);
        assert!(!info.features.tray_sync && !info.features.error_dialogs && !info.features.status_api);
    }

    #[tokio::test]
    async fn configured_author_overrides_the_fallback() {
        let fx = service(
            "[git]\nauthor_name = \"Notes App\"\nauthor_email = \"notes@acme.dev\"\n",
            FakeOps::ok(),
        )
        .await;
        let info = fx.svc.service_info();
        assert_eq!(info.defaults.author.name, "Notes App");
        assert_eq!(info.defaults.author.email, "notes@acme.dev");
    }

    #[tokio::test]
    async fn status_summary_never_touches_the_filesystem() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        let mut def = repo_def("notes", None);
        def.auto_sync_secs = Some(300);
        fx.svc.put_repo("notes", def).unwrap();
        // Deleting the repos root would break anything that opened a repository; the
        // summary is built from the registry, the job store and the in-memory state
        // file, and `loading.html` polls it in a tight loop.
        std::fs::remove_dir_all(&fx.paths.repos_dir).unwrap();
        let summary = fx.svc.status_summary();
        assert!(summary.registry_error.is_none());
        assert_eq!(summary.repos.len(), 1);
        let repo = &summary.repos[0];
        assert_eq!(repo.id, "notes");
        assert_eq!(repo.path, fx.svc.tree_path("notes").display().to_string());
        assert_eq!(repo.auto_sync_secs, Some(300));
        assert!(repo.busy_job.is_none());
        assert!(repo.last_sync.is_none());
    }
```

- [ ] **Step 27: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::tests::service_info`

Expected: FAIL — compile error.

```
error[E0599]: no method named `service_info` found for struct `Arc<GitService>` in the current scope
   --> src/git/mod.rs:NN:29
    |
NN  |         let info = fx.svc.service_info();
    |                           ^^^^^^^^^^^^ method not found in `Arc<GitService>`
```

- [ ] **Step 28: Implement the read-only views**

Extend the import block with `use crate::git::jobs::JobRef;` and
`use crate::git::state::LastSync;`, then add above `impl GitService`:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct AuthorView {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceDefaults {
    pub branch: String,
    pub author: AuthorView,
    pub network_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceFeatures {
    pub tray_sync: bool,
    pub error_dialogs: bool,
    pub status_api: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceInfo {
    pub host_instance: String,
    pub repos_root: String,
    pub registry_file: String,
    pub log_file: String,
    pub registry_writable: bool,
    pub registry_error: Option<RegistryError>,
    pub defaults: ServiceDefaults,
    pub features: ServiceFeatures,
    pub job_retention: usize,
    pub job_ttl_secs: u64,
    pub job_min_age_secs: u64,
    pub repo_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoStatusSummary {
    pub id: String,
    pub path: String,
    pub auto_sync_secs: Option<u64>,
    pub busy_job: Option<JobRef>,
    pub last_sync: Option<LastSync>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitStatusSummary {
    pub registry_error: Option<RegistryError>,
    pub repos: Vec<RepoStatusSummary>,
}
```

and inside `impl GitService`:

```rust
    pub fn service_info(&self) -> ServiceInfo {
        ServiceInfo {
            host_instance: self.jobs.host_instance().to_string(),
            repos_root: self.registry.repos_dir().display().to_string(),
            registry_file: self.registry.path().display().to_string(),
            log_file: self.log_path.display().to_string(),
            registry_writable: self.registry.writable(),
            registry_error: self.registry.error(),
            defaults: ServiceDefaults {
                branch: self.cfg.default_branch.clone(),
                author: self.default_author(),
                network_timeout_secs: self.cfg.network_timeout_secs,
            },
            features: ServiceFeatures {
                tray_sync: self.cfg.tray_sync,
                error_dialogs: self.cfg.error_dialogs,
                status_api: self.cfg.status_api,
            },
            job_retention: MAX_JOB_RECORDS,
            job_ttl_secs: JOB_TTL_SECS,
            job_min_age_secs: JOB_MIN_AGE_SECS,
            repo_count: self.registry.count(),
        }
    }

    /// The same fallback chain `OpCtx::author_name`/`author_email` apply, reported so a
    /// caller can see what a commit will be signed with without making one. `""` is the
    /// config default rather than a choice, so it must not win over the fallback.
    fn default_author(&self) -> AuthorView {
        let name = if self.cfg.author_name.trim().is_empty() {
            self.identity.app_name.clone()
        } else {
            self.cfg.author_name.clone()
        };
        let email = if self.cfg.author_email.trim().is_empty() {
            format!("{}@{}", self.identity.identifier, self.identity.hostname)
        } else {
            self.cfg.author_email.clone()
        };
        AuthorView { name, email }
    }

    /// Built purely from the registry, the job store and the in-memory state file —
    /// zero libgit2 calls, zero filesystem access. `assets/loading.html` polls
    /// `/api/status` in a tight loop, and a `Repository::open` + `statuses()` per poll
    /// per repo would be a genuine performance bug.
    pub fn status_summary(&self) -> GitStatusSummary {
        GitStatusSummary {
            registry_error: self.registry.error(),
            repos: self
                .registry
                .list()
                .into_iter()
                .map(|def| RepoStatusSummary {
                    path: self.tree_path(&def.id).display().to_string(),
                    auto_sync_secs: def.auto_sync_secs,
                    busy_job: self.jobs.busy(&def.id).map(|job| job.as_ref_view()),
                    last_sync: self.state.last_sync(&def.id),
                    id: def.id,
                })
                .collect(),
        }
    }
```

- [ ] **Step 29: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::tests::`

Expected: PASS.

```
running 14 tests
test git::tests::configured_author_overrides_the_fallback ... ok
test git::tests::service_info_reports_paths_defaults_and_retention ... ok
test git::tests::status_summary_never_touches_the_filesystem ... ok
...
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 30: Commit**

```bash
git add src/git/mod.rs
git commit -m "feat(git): ServiceInfo and the libgit2-free status summary

status_summary reads only the registry, the job store and the in-memory
state file. loading.html polls /api/status in a tight loop, so a
Repository::open per poll per repo would be a real performance bug — the
test proves it by deleting the repos root first."
```

---

- [ ] **Step 31: Write the failing tests for the bounded reads**

Append to `mod tests` in `src/git/mod.rs`:

```rust
    #[tokio::test]
    async fn read_status_returns_the_definition_and_a_snapshot() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let both = fx.svc.read_status("notes").await.expect("status");
        assert_eq!(both.repo.id, "notes");
        assert_eq!(both.status.id, "notes");
        // Nothing has been cloned or initialised yet.
        assert!(!both.status.exists);
        assert_eq!(both.status.state, "absent");
        assert_eq!(both.status.path, fx.svc.tree_path("notes").display().to_string());
    }

    #[tokio::test]
    async fn reads_of_an_unknown_or_invalid_repo_fail_before_any_io() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        let err = fx.svc.read_status("ghost").await.expect_err("unknown");
        assert_eq!(err.code(), GitErrorCode::RepoNotFound);
        let err = fx.svc.read_branches("../evil").await.expect_err("invalid");
        assert_eq!(err.code(), GitErrorCode::InvalidRepoId);
    }

    #[tokio::test]
    async fn read_branches_reports_not_a_repository_for_an_absent_tree() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let err = fx.svc.read_branches("notes").await.expect_err("no tree");
        assert_eq!(err.code(), GitErrorCode::NoWorktree);
    }

    /// A `GitOps` whose `status` blocks until the test releases it — a stand-in for the
    /// wedged filesystem (an unresponsive NFS mount, a spinning-up external disk) that
    /// `STATUS_READ_TIMEOUT_MS` exists for. Overriding the read seam is the only way to
    /// hold a bounded read open deterministically; a `sleep` long enough to beat the
    /// ceiling on a loaded CI box would be a flaky test.
    struct StallingOps {
        gate: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl StallingOps {
        fn holding(rx: std::sync::mpsc::Receiver<()>) -> Arc<StallingOps> {
            Arc::new(StallingOps {
                gate: std::sync::Mutex::new(Some(rx)),
            })
        }
    }

    impl GitOps for StallingOps {
        fn run(&self, _op: JobOp, _ctx: &OpCtx) -> Result<OpOutcome, GitError> {
            Ok(OpOutcome::new("up_to_date", "main"))
        }

        fn status(&self, ctx: &ReadCtx) -> RepoStatus {
            if let Some(rx) = self.gate.lock().unwrap().take() {
                // Returns as soon as the test drops the sender. A blocking-pool thread
                // that is never released would hang the runtime's own shutdown.
                let _ = rx.recv();
            }
            ops::status(ctx)
        }
    }

    #[tokio::test]
    async fn read_status_gives_up_at_the_ceiling_and_answers_status_timeout() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let fx = service("[git]\n", StallingOps::holding(rx)).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();

        let err = fx
            .svc
            .read_status("notes")
            .await
            .expect_err("the ceiling has to bound the caller");
        assert_eq!(err.code(), GitErrorCode::StatusTimeout);
        assert_eq!(err.repo_id.as_deref(), Some("notes"));
        // §5.1 row 6 and §5.6: `status_timeout` is the one code that answers 504, and
        // this is the only path in the host that can produce it.
        assert_eq!(err.http_status().as_u16(), 504);

        // This test is the one place in the suite that pays the whole
        // STATUS_READ_TIMEOUT_MS; the constant it is proving is what makes it 2 s.
        drop(tx);
    }
```

- [ ] **Step 32: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::tests::read_`

Expected: FAIL — compile error.

```
error[E0599]: no method named `read_status` found for struct `Arc<GitService>` in the current scope
   --> src/git/mod.rs:NN:31
    |
NN  |         let both = fx.svc.read_status("notes").await.expect("status");
    |                           ^^^^^^^^^^^ method not found in `Arc<GitService>`

error[E0407]: method `status` is not a member of trait `GitOps`
   --> src/git/mod.rs:NN:9
    |
NN  |         fn status(&self, ctx: &ReadCtx) -> RepoStatus {
    |         ^ not a member of trait `GitOps`

error[E0412]: cannot find type `ReadCtx` in this scope
error[E0412]: cannot find type `RepoStatus` in this scope
```

- [ ] **Step 33: Implement `read_status` and `read_branches`**

Extend the import block with `use crate::git::ops::{BranchesResponse, ReadCtx, RepoStatus};`
and `use std::time::Duration;`, then add above `impl GitService`:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct RepoWithStatus {
    pub repo: RepoView,
    pub status: RepoStatus,
}
```

and inside `impl GitService`:

```rust
    /// A synchronous read, deliberately **not** taken under the per-repo lease:
    /// observing a repo while it syncs is the main reason to call this. A read taken
    /// mid-checkout is a snapshot; the job's `result` is authoritative.
    pub async fn read_status(&self, repo_id: &str) -> Result<RepoWithStatus, GitError> {
        crate::git::registry::validate_id(repo_id)?;
        let def = self.registry.snapshot(repo_id)?;
        let repo = RepoView::new(&def, &self.repos_dir);
        let ctx = ReadCtx {
            tree: self.tree_path(repo_id),
            last_sync: self.state.last_sync(repo_id),
            busy_job: self.jobs.busy(repo_id).map(|job| job.as_ref_view()),
            def,
        };
        let status = self
            .bounded(repo_id, move || ops::status(&ctx))
            .await?;
        Ok(RepoWithStatus { repo, status })
    }

    pub async fn read_branches(&self, repo_id: &str) -> Result<BranchesResponse, GitError> {
        crate::git::registry::validate_id(repo_id)?;
        let def = self.registry.snapshot(repo_id)?;
        let tree = self.tree_path(repo_id);
        self.bounded(repo_id, move || ops::branches(&tree, &def))
            .await?
    }

    /// Run one blocking read with a hard ceiling.
    ///
    /// The blocking task cannot be cancelled — `JoinHandle::abort` does not interrupt a
    /// thread already inside libgit2 — so the timeout bounds the *caller*, not the work.
    /// That is the point: a wedged filesystem must not hang the child's poll loop.
    async fn bounded<T, F>(&self, repo_id: &str, work: F) -> Result<T, GitError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let handle = self.rt.spawn_blocking(work);
        match tokio::time::timeout(Duration::from_millis(STATUS_READ_TIMEOUT_MS), handle).await {
            Ok(Ok(value)) => Ok(value),
            // A panic inside libgit2 kills the read, not the host.
            Ok(Err(e)) => Err(GitError::internal(format!("read task failed: {e}")).with_repo(repo_id)),
            Err(_) => Err(GitError::status_timeout().with_repo(repo_id)),
        }
    }
```

- [ ] **Step 34: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::tests::read`

Expected: PASS.

```
running 3 tests
test git::tests::read_branches_reports_not_a_repository_for_an_absent_tree ... ok
test git::tests::read_status_returns_the_definition_and_a_snapshot ... ok
test git::tests::reads_of_an_unknown_or_invalid_repo_fail_before_any_io ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 35: Commit**

```bash
git add src/git/mod.rs
git commit -m "feat(git): bounded, lock-free status and branch reads

Neither read takes the per-repo lease: watching a repo while it syncs is the
main reason to call them. The timeout bounds the caller and not the work,
because a thread already inside libgit2 cannot be cancelled — which is
exactly why the ceiling exists."
```

---

- [ ] **Step 36: Write the failing tests for `start_job`**

Append to `mod tests` in `src/git/mod.rs`:

```rust
    /// Blocking jobs run on the blocking pool, so a test that asserts on a result has to
    /// wait for the slot rather than for `start_job` to return. It polls rather than
    /// sleeping a fixed amount so a loaded CI box cannot make the suite flaky, and it
    /// waits for the lease as well as the slot — the repo is only free once both are.
    async fn settle(svc: &Arc<GitService>, repo_id: &str, slot: &Arc<JobSlot>) {
        for _ in 0..400 {
            if slot.is_terminal() && svc.jobs().busy(repo_id).is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("job {} never settled", slot.id);
    }

    fn merged_outcome() -> OpOutcome {
        let mut out = OpOutcome::new("merged", "main");
        out.head_before = Some("9c1f0ae".to_string());
        out.head_after = Some("b4a71dd".to_string());
        out.committed = true;
        out.pushed = true;
        out
    }

    #[tokio::test]
    async fn a_job_runs_the_injected_ops_records_last_sync_and_logs_one_line() {
        let ops = FakeOps::returning(Ok(merged_outcome()));
        let fx = service("[git]\n", ops.clone()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();

        let started = fx
            .svc
            .start_job("notes", JobOp::Sync, OpRequest::manual())
            .expect("admitted");
        let StartOutcome::Started(slot) = &started else {
            panic!("first job on an idle repo must be Started");
        };
        let slot = slot.clone();
        settle(&fx.svc, "notes", &slot).await;

        assert_eq!(ops.calls(), vec![(JobOp::Sync, "notes".to_string())]);
        assert_eq!(slot.state(), JobState::Succeeded);
        let view = slot.view(fx.svc.host_instance());
        let result = view.result.expect("a succeeded job carries its result");
        assert_eq!(result["outcome"], "merged");
        assert_eq!(result["head_after"], "b4a71dd");

        let last = fx.svc.status_summary().repos[0]
            .last_sync
            .clone()
            .expect("last_sync survives the job");
        assert!(last.ok);
        assert_eq!(last.op, "sync");
        assert_eq!(last.job_id, slot.id.to_string());
        assert_eq!(last.outcome.as_deref(), Some("merged"));
        assert_eq!(last.head.as_deref(), Some("b4a71dd"));
        assert!(last.code.is_none());

        let log = fx.git_log();
        let line = log
            .lines()
            .find(|l| l.contains("job="))
            .expect("one job record");
        let fields: Vec<&str> = line.split(' ').collect();
        assert_eq!(fields[1], "sync");
        assert_eq!(fields[2], "repo=notes");
        assert_eq!(fields[3], format!("job={}", slot.id));
        assert_eq!(fields[4], "ok");
        assert_eq!(fields[5], "code=-");
        assert!(line.contains("merged branch=main head=b4a71dd pushed"), "{line}");
    }

    #[tokio::test]
    async fn a_failed_job_records_the_code_and_the_message() {
        let ops = FakeOps::returning(Err(GitError::new(
            GitErrorCode::NetworkFailed,
            "could not connect to github.com",
        )));
        let fx = service("[git]\n", ops).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let started = fx
            .svc
            .start_job("notes", JobOp::Pull, OpRequest::manual())
            .expect("admitted");
        let slot = started.slot().clone();
        settle(&fx.svc, "notes", &slot).await;

        assert_eq!(slot.state(), JobState::Failed);
        // The failure lives in the job, never in an HTTP status.
        let view = slot.view(fx.svc.host_instance());
        assert_eq!(view.error.expect("job error").code(), GitErrorCode::NetworkFailed);
        let last = fx.svc.status_summary().repos[0].last_sync.clone().unwrap();
        assert!(!last.ok);
        assert_eq!(last.code.as_deref(), Some("network_failed"));
        let log = fx.git_log();
        assert!(log.contains("err code=network_failed"), "{log}");
    }

    #[tokio::test]
    async fn a_repeated_request_id_replays_and_the_lease_is_released() {
        // The dropped-response case: the caller never saw the 202 and asks again with
        // the same id. Replay is checked before the busy check, so it can never 409.
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let mut req = OpRequest::manual();
        req.request_id = Some("boot-1".to_string());
        let first = fx.svc.start_job("notes", JobOp::Sync, req.clone()).unwrap();
        let slot = first.slot().clone();

        // Replay comes before the busy check, so a matching request_id can never 409.
        let again = fx.svc.start_job("notes", JobOp::Sync, req).unwrap();
        let StartOutcome::Replay(replayed) = &again else {
            panic!("a repeated request_id must replay, got a different admission");
        };
        assert_eq!(replayed.id, slot.id);

        settle(&fx.svc, "notes", &slot).await;
        assert!(fx.svc.jobs().busy("notes").is_none(), "the lease is released");
    }
```

- [ ] **Step 37: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::tests::a_job_runs`

Expected: FAIL — compile errors.

```
error[E0433]: failed to resolve: use of undeclared type `StartOutcome`
error[E0433]: failed to resolve: use of undeclared type `JobState`
error[E0599]: no method named `start_job` found for struct `Arc<GitService>` in the current scope
```

- [ ] **Step 38: Implement `StartOutcome`, `start_job`, and `after_job`**

Extend the import block with
`use crate::git::creds::{self, HostKeyChecker};`,
`use crate::git::jobs::{Admission, JobId, JobSlot, JobState};`,
`use crate::git::ops::{OpRequest, OpScratch};`,
`use crate::git::registry::{AuthorSpec, Warning};` and
`use std::time::Instant;` — merging into the existing `use crate::git::...` lines rather than
duplicating them (`error[E0252]` otherwise).

Add above `impl GitService`:

```rust
/// What admission decided, without the lease.
///
/// `RepoLease` never leaves `start_job`: the spec's `Admission::Started(slot, lease)`
/// would hand a second lease to the HTTP layer, and the second drop would release a repo
/// the job still owns.
pub enum StartOutcome {
    Started(Arc<JobSlot>),
    Replay(Arc<JobSlot>),
    Busy(Arc<JobSlot>),
}

impl StartOutcome {
    pub fn slot(&self) -> &Arc<JobSlot> {
        match self {
            StartOutcome::Started(slot) | StartOutcome::Replay(slot) | StartOutcome::Busy(slot) => {
                slot
            }
        }
    }
}

/// The one-line summary of a successful job for `git.log`.
fn success_line(out: &OpOutcome) -> String {
    let mut line = format!("{} branch={}", out.outcome, out.branch);
    if let Some(head) = &out.head_after {
        line.push_str(&format!(" head={head}"));
    }
    if out.pushed {
        line.push_str(" pushed");
    }
    if out.restart_requested {
        line.push_str(" restart");
    }
    if let Some(merge) = &out.merge {
        if !merge.conflicts_resolved.is_empty() {
            // The overwritten paths are recorded here as well as in the job result and
            // the merge commit message: prefer-local is lossy and the record has to
            // outlive the job store.
            line.push_str(&format!(
                " conflicts_resolved={}",
                merge.conflicts_resolved.join(",")
            ));
        }
    }
    line
}
```

and inside `impl GitService`:

```rust
    pub fn start_job(
        self: &Arc<Self>,
        repo_id: &str,
        op: JobOp,
        req: OpRequest,
    ) -> Result<StartOutcome, GitError> {
        crate::git::registry::validate_id(repo_id)?;
        let def = self.registry.snapshot(repo_id)?;

        // Credentials are resolved *before* admission. Resolving after would mean an
        // invalid spec produced a job record that failed with "git worker terminated
        // unexpectedly" — the lease's own drop message — instead of the real reason.
        let resolution = creds::resolve(
            req.credential.as_ref(),
            def.credential.as_ref(),
            def.remote.as_deref(),
        )
        .map_err(|e| e.with_repo(repo_id))?;

        let admission = self.jobs.admit(repo_id, op, req.request_id.as_deref())?;
        let (slot, lease) = match admission {
            Admission::Replay(slot) => return Ok(StartOutcome::Replay(slot)),
            Admission::Busy(slot) => return Ok(StartOutcome::Busy(slot)),
            Admission::Started(slot, lease) => (slot, lease),
        };

        let ctx = OpCtx {
            tree: self.tree_path(repo_id),
            cred: resolution.cred,
            cred_unbound: resolution.unbound,
            identity: self.identity.clone(),
            host_key: HostKeyChecker::new(
                self.cfg.ssh_host_key_policy,
                self.state.fingerprint(repo_id),
            ),
            abort: slot.abort.clone(),
            slot: slot.clone(),
            deadline: Instant::now() + Duration::from_secs(self.cfg.network_timeout_secs),
            settings: self.settings.clone(),
            default_author: AuthorSpec {
                name: Some(self.cfg.author_name.clone()),
                email: Some(self.cfg.author_email.clone()),
            },
            scratch: OpScratch::default(),
            request: req,
            def,
        };

        self.spawn_watchdog(slot.clone());

        let this = self.clone();
        let started = slot.clone();
        // No `rt.enter()` guard here, and none is missing. That guard exists in this
        // codebase solely because `tokio::process::Command::spawn` registers a child
        // with the runtime's signal driver; git spawns no processes and
        // `Handle::spawn_blocking` is callable from any thread. Do not "fix" this.
        self.rt.spawn_blocking(move || {
            // Declared first so it is dropped last: the repo is released the instant the
            // job is terminal, and on the panic path too — `spawn_blocking` contains a
            // libgit2 panic, the lease releases via Drop, and the host keeps running.
            let _lease = lease;
            ctx.slot.begin();
            // The only git2 code in the process. Nothing `git2` escapes this closure:
            // `Repository` is opened inside `ops::run` and stays alive for the whole
            // job, because dropping it while an `Index` is held detaches the index and
            // `write_tree()` then fails.
            let mut outcome = this.ops.run(op, &ctx);
            this.after_job(op, &ctx, &mut outcome);
        });
        Ok(StartOutcome::Started(started))
    }

    /// Everything that happens once `ops::run` has returned.
    ///
    /// It publishes the job itself rather than leaving that to the caller, because
    /// `result.restart_requested` and the `restart_deferred` warning are fields *of* the
    /// result: the decision has to be made before `JobSlot::succeed` freezes it.
    fn after_job(&self, op: JobOp, ctx: &OpCtx, outcome: &mut Result<OpOutcome, GitError>) {
        let repo_id = ctx.def.id.clone();
        let job_id = ctx.slot.id.clone();

        match outcome.as_ref() {
            Ok(out) => match serde_json::to_value(out) {
                Ok(value) => ctx.slot.succeed(&value),
                Err(e) => ctx
                    .slot
                    .fail(&GitError::internal(format!("job result not serializable: {e}"))),
            },
            Err(e) => ctx.slot.fail(e),
        }

        match outcome.as_ref() {
            Ok(out) => {
                if let Some(fingerprint) = &out.learned_fingerprint {
                    // git-state.json has exactly one writer and it is this thread. The
                    // blocking worker holds no handle to the store, by construction.
                    self.state.record_fingerprint(&repo_id, fingerprint);
                }
                self.state.record_last_sync(
                    &repo_id,
                    LastSync {
                        at_ms: now_ms(),
                        ok: true,
                        op: op.as_str().to_string(),
                        job_id: job_id.to_string(),
                        outcome: Some(out.outcome.to_string()),
                        head: out.head_after.clone(),
                        code: None,
                        message: None,
                    },
                );
                self.log_job(op, &repo_id, &job_id, Ok(&success_line(out)));
            }
            Err(e) => {
                self.state.record_last_sync(
                    &repo_id,
                    LastSync {
                        at_ms: now_ms(),
                        ok: false,
                        op: op.as_str().to_string(),
                        job_id: job_id.to_string(),
                        outcome: None,
                        head: None,
                        code: Some(e.code().as_str().to_string()),
                        message: Some(e.message.clone()),
                    },
                );
                self.log_job(op, &repo_id, &job_id, Err(e));
            }
        }
    }

    fn log_job(&self, op: JobOp, repo_id: &str, job_id: &JobId, result: Result<&str, &GitError>) {
        let (state, code, message) = match result {
            Ok(message) => ("ok", "-", message),
            Err(e) => ("err", e.code().as_str(), e.message.as_str()),
        };
        self.log(&format!(
            "{} {} repo={repo_id} job={job_id} {state} code={code} {}",
            now_ms(),
            op.as_str(),
            one_line(message)
        ));
    }

    fn spawn_watchdog(self: &Arc<Self>, _slot: Arc<JobSlot>) {}
```

- [ ] **Step 39: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::tests::a_`

Expected: PASS.

```
running 5 tests
test git::tests::a_failed_job_records_the_code_and_the_message ... ok
test git::tests::a_git_section_yields_a_service_that_has_created_nothing_yet ... ok
test git::tests::a_job_runs_the_injected_ops_records_last_sync_and_logs_one_line ... ok
test git::tests::a_read_only_registry_refuses_both_mutators ... ok
test git::tests::a_repeated_request_id_replays_and_the_lease_is_released ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 40: Commit**

```bash
git add src/git/mod.rs
git commit -m "feat(git): start_job on spawn_blocking with an RAII lease

The lease is the first local in the closure so it is the last one dropped:
the repo is released the instant the job is terminal, on the panic path too.
Credentials resolve before admission so a bad spec never produces a job
record blaming the lease's own drop message. No rt.enter() guard is needed
and the comment at the spawn site says why, so nobody adds one."
```

---

- [ ] **Step 41: Write the failing tests for the admission guards**

Append to `mod tests` in `src/git/mod.rs`:

```rust
    #[tokio::test]
    async fn remote_verbs_need_a_remote_and_reset_needs_confirmation() {
        let ops = FakeOps::ok();
        let fx = service("[git]\n", ops.clone()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();

        for op in [JobOp::Clone, JobOp::Pull, JobOp::Push] {
            let err = fx
                .svc
                .start_job("notes", op, OpRequest::manual())
                .expect_err("no remote");
            assert_eq!(err.code(), GitErrorCode::RemoteMissing, "{op:?}");
            assert_eq!(err.repo_id.as_deref(), Some("notes"));
        }
        // `sync` is deliberately NOT in that loop: §9.4 says a `remote: null` repo's
        // sync inits and commits, so refusing it here would make that promise
        // unreachable. This assertion is the guard against the whole list being pasted
        // back, and it doubles as the local-only-verbs-are-unaffected case — one job at
        // a time per repo, so admitting an `Init` first would only make this one 409.
        assert!(
            matches!(
                fx.svc.start_job("notes", JobOp::Sync, OpRequest::manual()),
                Ok(StartOutcome::Started(_))
            ),
            "§9.4: a local-only sync is admitted, not refused"
        );

        let mut req = OpRequest::manual();
        req.reset = Some(crate::git::ops::ResetRequest {
            to: "head".to_string(),
            clean_untracked: true,
            confirm: false,
        });
        let err = fx
            .svc
            .start_job("notes", JobOp::Reset, req)
            .expect_err("unconfirmed reset");
        assert_eq!(err.code(), GitErrorCode::ConfirmRequired);

        // A refused request must never have reached the executor, and must never have
        // left a job record behind for a caller to poll. One call: the admitted `sync`.
        assert_eq!(ops.calls().len(), 1, "only the Sync job ran");
    }

    #[tokio::test]
    async fn a_job_for_an_unknown_or_invalid_repo_is_refused_before_admission() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        let err = fx
            .svc
            .start_job("ghost", JobOp::Sync, OpRequest::manual())
            .expect_err("unknown");
        assert_eq!(err.code(), GitErrorCode::RepoNotFound);
        let err = fx
            .svc
            .start_job("../evil", JobOp::Sync, OpRequest::manual())
            .expect_err("invalid");
        assert_eq!(err.code(), GitErrorCode::InvalidRepoId);
    }
```

- [ ] **Step 42: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::tests::remote_verbs`

Expected: FAIL — the guards do not exist, so `start_job` admits the job and returns `Ok`.

```
running 1 test
test git::tests::remote_verbs_need_a_remote_and_reset_needs_confirmation ... FAILED

failures:

---- git::tests::remote_verbs_need_a_remote_and_reset_needs_confirmation stdout ----
thread 'git::tests::…' panicked at src/git/mod.rs:NN:14:
no remote
```

- [ ] **Step 43: Implement the guards**

> **Amended by the post-execution audit (F5a).** This step's `matches!` originally listed
> `JobOp::Sync` alongside `Clone`/`Pull`/`Push`, which contradicted two documents Task 8 had
> already been implemented against: spec §5.6's admission table (`remote_missing` is
> "`clone`/`pull`/`push` on a repo with `remote: null`") and README §9.4 ("`null` means a
> local-only repo: `sync` inits and commits and reports `outcome: "committed"`"). The effect was
> that a `remote: null` repo could never sync at all — the local-only arm inside `ops::sync` was
> dead code for as long as this guard stood, and task 11's `settings_sync_events` fixture had to
> point at a fake remote to work around it. Step 41's test above asserted the defect, so it is
> corrected too and renamed `remote_verbs_…`.

Insert into `start_job`, immediately after `let def = self.registry.snapshot(repo_id)?;`:

```rust
        // Refused before admission, so a request that cannot possibly succeed never
        // creates a job record for the caller to poll. `sync` is deliberately not in
        // this list: §9.4's `remote: null` is a local-only repo whose sync stages and
        // commits (`ops::sync`'s early return), so the verb still has an arm to run.
        // The rule is that admission refuses a verb only when EVERY mode of it is
        // impossible — which is why `reset` is not here either: `to: "upstream"` needs
        // an upstream and fails in the worker, `to: "head"` needs nothing.
        if def.remote.is_none() && matches!(op, JobOp::Clone | JobOp::Pull | JobOp::Push) {
            return Err(GitError::remote_missing().with_repo(repo_id));
        }
        if op == JobOp::Reset && !req.reset.as_ref().is_some_and(|r| r.confirm) {
            // `reset` is the one verb that destroys uncommitted work, so the caller has
            // to say so in the body rather than in the URL.
            return Err(GitError::confirm_required().with_repo(repo_id));
        }
```

`JobOp` needs `PartialEq` for `op == JobOp::Reset`; it already derives it (contract §4.7).

- [ ] **Step 44: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::tests::`

Expected: PASS.

```
running 21 tests
test git::tests::a_job_for_an_unknown_or_invalid_repo_is_refused_before_admission ... ok
test git::tests::remote_verbs_need_a_remote_and_reset_needs_confirmation ... ok
...
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 45: Commit**

```bash
git add src/git/mod.rs
git commit -m "feat(git): refuse impossible jobs before admission

A clone with no remote and an unconfirmed reset cannot succeed, so they must
not produce a job record: a caller polling one would watch a job it could
have been told about synchronously."
```

---

- [ ] **Step 46: Write the failing tests for the host events**

Append to `mod tests` in `src/git/mod.rs`:

```rust
    fn next_event(rx: &mut tokio::sync::mpsc::UnboundedReceiver<HostEvent>) -> Option<HostEvent> {
        // The events are sent from the blocking thread before it returns, and `settle`
        // has already waited for that, so a non-blocking read is deterministic here.
        rx.try_recv().ok()
    }

    #[tokio::test]
    async fn git_failed_fires_on_the_transition_into_failure_and_not_again() {
        let ops = FakeOps::returning(Err(GitError::new(
            GitErrorCode::AuthFailed,
            "remote rejected the token",
        )));
        let mut fx = service("[git]\n", ops).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();

        for expected in [true, false] {
            let started = fx
                .svc
                .start_job("notes", JobOp::Commit, OpRequest::manual())
                .unwrap();
            let slot = started.slot().clone();
            settle(&fx.svc, "notes", &slot).await;
            let event = next_event(&mut fx.events);
            assert_eq!(
                event.is_some(),
                expected,
                // dialog() is modal and blocks the tao loop, so a repo whose token
                // expired with auto_sync_secs = 300 would otherwise stack a modal every
                // five minutes with the tray unreachable.
                "GitFailed must fire once per outage, not once per attempt"
            );
            if let Some(HostEvent::GitFailed { repo_id, op, code, .. }) = event {
                assert_eq!(repo_id, "notes");
                assert_eq!(op, "commit");
                assert_eq!(code, "auth_failed");
            }
        }
    }

    #[tokio::test]
    async fn conflicts_resolved_reports_the_merge_commit_and_the_paths() {
        let mut out = merged_outcome();
        out.merge = Some(crate::git::ops::MergeReport {
            kind: "merge_commit",
            merge_commit: Some("b4a71dd".to_string()),
            conflicts_resolved: vec!["notes/todo.md".to_string()],
            recover_hint: Some("git show b4a71dd^2:notes/todo.md".to_string()),
        });
        let mut fx = service("[git]\n", FakeOps::returning(Ok(out))).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let started = fx
            .svc
            .start_job("notes", JobOp::Commit, OpRequest::manual())
            .unwrap();
        settle(&fx.svc, "notes", started.slot()).await;

        let Some(HostEvent::GitConflictsResolved { repo_id, merge_commit, paths }) =
            next_event(&mut fx.events)
        else {
            panic!("a lossy success must be reported");
        };
        assert_eq!(repo_id, "notes");
        assert_eq!(merge_commit, "b4a71dd");
        assert_eq!(paths, vec!["notes/todo.md".to_string()]);
        assert!(fx.git_log().contains("conflicts_resolved=notes/todo.md"));
    }

    #[tokio::test]
    async fn a_restart_is_requested_only_when_head_moved_and_the_app_is_ready() {
        let mut fx = service("[git]\n", FakeOps::returning(Ok(merged_outcome()))).await;
        let mut def = repo_def("notes", None);
        def.restart_children_on_pull = true;
        fx.svc.put_repo("notes", def).unwrap();

        // `OpRequest::default()` leaves `restart_children` at `None`, which falls back
        // to the repo's `restart_children_on_pull`. `auto()` and `manual()` both pin it
        // to `Some(false)` — see the next test.
        let started = fx
            .svc
            .start_job("notes", JobOp::Commit, OpRequest::default())
            .unwrap();
        settle(&fx.svc, "notes", started.slot()).await;
        assert!(matches!(
            next_event(&mut fx.events),
            Some(HostEvent::GitRestartChildren { reason: "requested", .. })
        ));
        let view = started.slot().view(fx.svc.host_instance());
        assert_eq!(view.result.unwrap()["restart_requested"], true);
    }

    #[tokio::test]
    async fn a_restart_needs_both_a_moved_head_and_a_caller_that_wants_one() {
        let mut def = repo_def("notes", None);
        def.restart_children_on_pull = true;

        // `up_to_date` leaves head_before == head_after. This is what stops a
        // five-minute auto-sync from restarting Chrome 288 times a day.
        let mut fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", def.clone()).unwrap();
        let started = fx
            .svc
            .start_job("notes", JobOp::Commit, OpRequest::default())
            .unwrap();
        settle(&fx.svc, "notes", started.slot()).await;
        assert!(next_event(&mut fx.events).is_none(), "an unmoved head cannot restart");

        // A background trigger pins `restart_children` to `Some(false)`, and no config
        // key can turn it on: only an explicit HTTP call, or a real settings change,
        // may restart the user's window.
        let mut fx = service("[git]\n", FakeOps::returning(Ok(merged_outcome()))).await;
        fx.svc.put_repo("notes", def).unwrap();
        let started = fx
            .svc
            .start_job("notes", JobOp::Sync, OpRequest::auto())
            .unwrap();
        settle(&fx.svc, "notes", started.slot()).await;
        assert!(next_event(&mut fx.events).is_none(), "auto-sync never restarts");
    }

    #[tokio::test]
    async fn a_restart_before_the_app_is_ready_is_deferred_into_a_warning() {
        let mut fx = service("[git]\n", FakeOps::returning(Ok(merged_outcome()))).await;
        *fx.status.write().await = AppStatus::Starting;
        let mut def = repo_def("notes", None);
        def.restart_children_on_pull = true;
        fx.svc.put_repo("notes", def).unwrap();
        let started = fx
            .svc
            .start_job("notes", JobOp::Commit, OpRequest::default())
            .unwrap();
        settle(&fx.svc, "notes", started.slot()).await;

        assert!(next_event(&mut fx.events).is_none(), "no restart mid-startup");
        let view = started.slot().view(fx.svc.host_instance());
        let result = view.result.unwrap();
        assert_eq!(result["restart_requested"], false);
        assert_eq!(result["warnings"][0]["code"], "restart_deferred");
    }
```

- [ ] **Step 47: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::tests::git_failed_fires`

Expected: FAIL — no event is ever sent.

```
running 1 test
test git::tests::git_failed_fires_on_the_transition_into_failure_and_not_again ... FAILED

failures:

---- git::tests::git_failed_fires_on_the_transition_into_failure_and_not_again stdout ----
thread 'git::tests::…' panicked at src/git/mod.rs:NN:13:
assertion `left == right` failed: GitFailed must fire once per outage, not once per attempt
  left: false
 right: true
```

- [ ] **Step 48: Add the three `HostEvent` variants**

Replace the enum in `src/internal_server.rs:18-21`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    RestartRequested,
    /// Git wants the children restarted (a pull moved HEAD, or settings changed).
    ///
    /// This MUST travel the channel rather than calling `App::restart()` directly:
    /// `restart()` calls `kill_children()`, which calls `rt.block_on`, and `block_on`
    /// panics when called from inside a runtime worker thread. The hop is load-bearing.
    GitRestartChildren {
        repo_id: String,
        reason: &'static str,
    },
    GitFailed {
        repo_id: String,
        op: String,
        code: String,
        message: String,
    },
    /// A sync succeeded but resolved conflicts in favour of the local copy — it
    /// overwrote someone else's edit. Carries the merge commit so the consumer can
    /// de-duplicate: one data-loss event, at most one dialog, ever.
    GitConflictsResolved {
        repo_id: String,
        merge_commit: String,
        paths: Vec<String>,
    },
}
```

The existing `match` in `run()` already ends with `_ => {}`, so no event-loop arm breaks.
Task 10 adds the arms that consume these.

- [ ] **Step 49: Implement the restart decision and the event sends**

In `src/git/mod.rs`, add to `impl GitService`:

```rust
    /// §9.7: a restart needs all three of — someone asked for it, the operation really
    /// changed something, and the app is in a state where restarting is not a
    /// double-start.
    fn decide_restart(&self, repo_id: &str, ctx: &OpCtx, out: &mut OpOutcome) {
        let asked = ctx
            .request
            .restart_children
            .unwrap_or(ctx.def.restart_children_on_pull);
        // "Actually moved HEAD" is what stops a five-minute auto-sync from restarting
        // Chrome 288 times a day: `up_to_date` leaves the two heads equal.
        let moved = out.head_after.is_some() && out.head_before != out.head_after;
        if !((asked && moved) || out.settings_changed) {
            return;
        }
        if self.app_is_ready() {
            out.restart_requested = true;
            return;
        }
        // A `sync_on_start` repo can finish before `wait_healthy` does. The existing
        // `server_generation` guard makes an early restart *correct*, but the user
        // would watch the server start twice.
        out.warnings.push(Warning {
            code: "restart_deferred",
            message: format!(
                "children were not restarted for \"{repo_id}\": the app is not ready yet"
            ),
        });
    }

    /// `try_read` and never `read`: this runs on a blocking thread with no runtime
    /// context, so it must not await. A momentarily-locked status reads as not-ready,
    /// which defers the restart — the safe direction.
    fn app_is_ready(&self) -> bool {
        match self.status.try_read() {
            Ok(status) => matches!(&*status, AppStatus::Ready { .. }),
            Err(_) => false,
        }
    }
```

Then, in `after_job`, insert before the publishing `match`:

```rust
        // Read before the new record overwrites it: `GitFailed` fires on the ok -> fail
        // transition only.
        let was_ok = self.state.last_sync(&repo_id).map(|last| last.ok);
        if let Ok(out) = outcome.as_mut() {
            self.decide_restart(&repo_id, ctx, out);
        }
```

and extend the two arms of the second `match` in `after_job`. In the `Ok(out)` arm, after
`self.log_job(...)`:

```rust
                if out.restart_requested {
                    let reason = if out.settings_changed {
                        "settings"
                    } else {
                        "requested"
                    };
                    let _ = self.events.send(HostEvent::GitRestartChildren {
                        repo_id: repo_id.clone(),
                        reason,
                    });
                }
                if let Some(merge) = &out.merge {
                    if !merge.conflicts_resolved.is_empty() {
                        // A prefer-local merge that resolved a conflict has, by
                        // definition, overwritten an edit made somewhere else. Silence
                        // is the one thing not worth defending here.
                        let _ = self.events.send(HostEvent::GitConflictsResolved {
                            repo_id: repo_id.clone(),
                            merge_commit: merge.merge_commit.clone().unwrap_or_default(),
                            paths: merge.conflicts_resolved.clone(),
                        });
                    }
                }
```

and in the `Err(e)` arm, after `self.log_job(...)`:

```rust
                if was_ok != Some(false) {
                    let _ = self.events.send(HostEvent::GitFailed {
                        repo_id: repo_id.clone(),
                        op: op.as_str().to_string(),
                        code: e.code().as_str().to_string(),
                        message: e.message.clone(),
                    });
                }
```

- [ ] **Step 50: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::tests::`

Expected: PASS.

```
running 26 tests
test git::tests::a_restart_before_the_app_is_ready_is_deferred_into_a_warning ... ok
test git::tests::a_restart_is_requested_only_when_head_moved_and_the_app_is_ready ... ok
test git::tests::a_restart_needs_both_a_moved_head_and_a_caller_that_wants_one ... ok
test git::tests::conflicts_resolved_reports_the_merge_commit_and_the_paths ... ok
test git::tests::git_failed_fires_on_the_transition_into_failure_and_not_again ... ok
...
test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 51: Commit**

```bash
git add src/git/mod.rs src/internal_server.rs
git commit -m "feat(git): the three HostEvent variants and the policies behind them

All three decisions are made here, in src/git/, so the event-loop arms task
10 adds stay dumb consumers. GitFailed fires on the ok->fail transition
because dialog() is modal and a five-minute auto-sync would otherwise stack
one modal per failure with the tray unreachable. A restart needs a head that
actually moved and an app that is already Ready; otherwise it becomes a
restart_deferred warning on the job."
```

---

- [ ] **Step 52: Write the failing tests for the watchdog**

Append to `mod tests` in `src/git/mod.rs`:

```rust
    #[tokio::test]
    async fn the_watchdog_marks_a_live_job_stalled_and_trips_its_abort_flag() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let started = fx
            .svc
            .start_job("notes", JobOp::Init, OpRequest::manual())
            .unwrap();
        let slot = started.slot().clone();
        settle(&fx.svc, "notes", &slot).await;

        // A terminal job is never touched: the deadline outliving the work is the
        // normal case, and a stalled flag on a finished job would be a lie.
        assert!(!trip_watchdog(&slot));
        assert!(!slot.view(fx.svc.host_instance()).stalled);

        // Against a live job the flag goes up and the reason is Timeout, which is what
        // makes `classify` report `timeout` rather than `network_failed`.
        let live = fx.svc.jobs().admit("other", JobOp::Sync, None).expect("admitted");
        let Admission::Started(live_slot, _lease) = live else {
            panic!("an idle repo must admit");
        };
        assert!(trip_watchdog(&live_slot));
        assert!(live_slot.view(fx.svc.host_instance()).stalled);
        assert!(live_slot.abort.is_aborted());
        assert_eq!(live_slot.abort.reason(), AbortReason::Timeout);
    }
```

- [ ] **Step 53: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app git::tests::the_watchdog`

Expected: FAIL — compile errors.

```
error[E0425]: cannot find function `trip_watchdog` in this scope
error[E0433]: failed to resolve: use of undeclared type `AbortReason`
```

- [ ] **Step 54: Implement the watchdog**

Extend the import block with `use crate::git::error::AbortReason;` (merge into the existing
`use crate::git::error::{…};` line), then add above `impl GitService`:

```rust
/// The watchdog's whole policy, factored out of the timer so it is testable without a
/// two-minute sleep. Returns whether the job was still live.
///
/// The lease is deliberately **not** released: two concurrent libgit2 operations on one
/// working tree would corrupt the index. Visible-but-stuck beats silently-corrupt, which
/// is what `stalled` is for.
fn trip_watchdog(slot: &Arc<JobSlot>) -> bool {
    if slot.is_terminal() {
        return false;
    }
    slot.mark_stalled();
    slot.abort.abort(AbortReason::Timeout);
    true
}
```

and replace the `spawn_watchdog` placeholder in `impl GitService`:

```rust
    /// One timer per job. Its only job is to trip the abort flag so a wedged transfer
    /// ends as `timeout` instead of pinning a blocking thread forever.
    ///
    /// libgit2's process-global connect and idle timeouts (`init_libgit2_timeouts`) are
    /// what actually bound a black-holed *connect* — this watchdog lives in the transfer
    /// callbacks, which never fire before the transport has data.
    fn spawn_watchdog(self: &Arc<Self>, slot: Arc<JobSlot>) {
        let this = self.clone();
        let secs = self.cfg.network_timeout_secs;
        self.rt.spawn(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            if trip_watchdog(&slot) {
                this.log(&format!(
                    "{} {} repo={} job={} err code=timeout no progress for {secs}s; aborting",
                    now_ms(),
                    slot.op.as_str(),
                    slot.repo_id,
                    slot.id
                ));
            }
        });
    }
```

- [ ] **Step 55: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::tests::the_watchdog`

Expected: PASS.

```
running 1 test
test git::tests::the_watchdog_marks_a_live_job_stalled_and_trips_its_abort_flag ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 56: Commit**

```bash
git add src/git/mod.rs
git commit -m "feat(git): per-job watchdog that stalls rather than releases

The policy is a free function so it is testable without a two-minute sleep.
The lease is deliberately kept: two concurrent libgit2 operations on one
working tree corrupt the index, so visible-but-stuck beats silently-corrupt."
```

---

- [ ] **Step 57: Write the failing tests for the triggers**

Append to `mod tests` in `src/git/mod.rs`:

```rust
    #[tokio::test]
    async fn sync_all_manual_admits_one_job_per_repo_and_skips_busy_ones() {
        let ops = FakeOps::ok();
        let fx = service("[git]\n", ops.clone()).await;
        fx.svc.put_repo("a", repo_def("a", None)).unwrap();
        fx.svc.put_repo("b", repo_def("b", None)).unwrap();

        fx.svc.sync_all_manual();
        for id in ["a", "b"] {
            for _ in 0..400 {
                if fx.svc.jobs().busy(id).is_none() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        let mut ran: Vec<String> = ops.calls().into_iter().map(|(_, id)| id).collect();
        ran.sort();
        assert_eq!(ran, vec!["a".to_string(), "b".to_string()]);
        assert!(fx.git_log().contains("manual: started"), "{}", fx.git_log());
    }

    #[tokio::test]
    async fn sync_on_start_only_touches_repos_that_asked_for_it() {
        let ops = FakeOps::ok();
        let fx = service("[git]\n", ops.clone()).await;
        let mut eager = repo_def("eager", None);
        eager.sync_on_start = true;
        fx.svc.put_repo("eager", eager).unwrap();
        fx.svc.put_repo("quiet", repo_def("quiet", None)).unwrap();

        fx.svc.sync_on_start();
        for _ in 0..400 {
            if fx.svc.jobs().busy("eager").is_none() && !ops.calls().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(ops.calls(), vec![(JobOp::Sync, "eager".to_string())]);
    }

    #[tokio::test]
    async fn an_auto_sync_repo_gets_exactly_one_timer() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        let mut def = repo_def("notes", None);
        def.auto_sync_secs = Some(300);
        fx.svc.put_repo("notes", def.clone()).unwrap();
        assert_eq!(fx.svc.timer_ids(), vec!["notes".to_string()]);
        // A second PUT must not start a second ticker for the same repo.
        fx.svc.put_repo("notes", def).unwrap();
        assert_eq!(fx.svc.timer_ids(), vec!["notes".to_string()]);
        // A repo with no interval never gets one.
        fx.svc.put_repo("quiet", repo_def("quiet", None)).unwrap();
        assert_eq!(fx.svc.timer_ids(), vec!["notes".to_string()]);
    }
```

- [ ] **Step 58: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::tests::sync_all_manual`

Expected: FAIL — compile errors.

```
error[E0599]: no method named `sync_all_manual` found for struct `Arc<GitService>` in the current scope
error[E0599]: no method named `timer_ids` found for struct `Arc<GitService>` in the current scope
```

- [ ] **Step 59: Implement the triggers and the auto-sync timer**

Replace the `spawn_auto_timer` placeholder and add the trigger methods inside
`impl GitService`:

```rust
    pub fn sync_all_manual(self: &Arc<Self>) {
        for id in self.registry.ids() {
            self.trigger(&id, JobOp::Sync, OpRequest::manual(), "manual");
        }
    }

    /// Fired after `App::start_children()`, never before: the child server is already
    /// up, so a `sync_settings` pull restarts a live child rather than racing the first
    /// spawn, and a wedged network at launch cannot make the app look dead.
    pub fn sync_on_start(self: &Arc<Self>) {
        for def in self.registry.list() {
            if def.sync_on_start {
                self.trigger(&def.id, JobOp::Sync, OpRequest::auto(), "sync_on_start");
            }
        }
    }

    /// Admit a job for a background trigger and record what happened. Nothing here can
    /// block: every one of these callers is either the tao thread or a timer task.
    fn trigger(self: &Arc<Self>, id: &str, op: JobOp, req: OpRequest, why: &str) {
        let ts = now_ms();
        match self.start_job(id, op, req) {
            Ok(StartOutcome::Started(slot)) => self.log(&format!(
                "{ts} {} repo={id} job={} ok code=- {why}: started",
                op.as_str(),
                slot.id
            )),
            // A tick on a busy repo is dropped, not queued: a backlog of stale syncs is
            // never useful and would turn one slow network into an unbounded queue.
            Ok(StartOutcome::Replay(slot)) | Ok(StartOutcome::Busy(slot)) => self.log(&format!(
                "{ts} {} repo={id} job={} ok code=- {why}: skipped, already running",
                op.as_str(),
                slot.id
            )),
            Err(e) => self.log(&format!(
                "{ts} {} repo={id} job=- err code={} {why}: {}",
                op.as_str(),
                e.code().as_str(),
                one_line(&e.message)
            )),
        }
    }

    /// One task per repo with an interval, started by `start()` and re-checked on every
    /// `PUT`. The `timers` set is what stops a second `PUT` from double-spawning.
    fn spawn_auto_timer(self: &Arc<Self>, id: &str) {
        if self.auto_sync_secs(id).is_none() {
            return;
        }
        {
            let mut timers = self.timers.lock().unwrap_or_else(|p| p.into_inner());
            if !timers.insert(id.to_string()) {
                return;
            }
        }
        let this = self.clone();
        let id = id.to_string();
        self.rt.spawn(async move {
            // A sleep loop rather than `tokio::time::interval`, so the period is re-read
            // from the registry each cycle: a PUT that changes or clears
            // `auto_sync_secs` takes effect after at most one old period, and a DELETE
            // lets the task exit on its own. `MissedTickBehavior` is moot here — a sleep
            // loop cannot burst after a laptop resume.
            loop {
                let Some(secs) = this.auto_sync_secs(&id) else {
                    break;
                };
                tokio::time::sleep(Duration::from_secs(secs)).await;
                if this.draining() {
                    break;
                }
                this.trigger(&id, JobOp::Sync, OpRequest::auto(), "auto");
            }
            let mut timers = this.timers.lock().unwrap_or_else(|p| p.into_inner());
            timers.remove(&id);
        });
    }

    #[cfg(test)]
    fn timer_ids(&self) -> Vec<String> {
        self.timers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .cloned()
            .collect()
    }
```

- [ ] **Step 60: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::tests::`

Expected: PASS.

```
running 30 tests
test git::tests::an_auto_sync_repo_gets_exactly_one_timer ... ok
test git::tests::sync_all_manual_admits_one_job_per_repo_and_skips_busy_ones ... ok
test git::tests::sync_on_start_only_touches_repos_that_asked_for_it ... ok
...
test result: ok. 30 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 61: Commit**

```bash
git add src/git/mod.rs
git commit -m "feat(git): tray, startup and interval sync triggers

A sleep loop rather than tokio::time::interval so the period is re-read each
cycle: a PUT that changes auto_sync_secs takes effect after one old period
and a DELETE lets the task exit. A tick on a busy repo is dropped rather
than queued — a backlog of stale syncs turns one slow network into an
unbounded queue."
```

---

- [ ] **Step 62: Write the failing tests for `run_quit_syncs`**

Append to `mod tests` in `src/git/mod.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn quit_syncs_run_then_drain_the_store() {
        let ops = FakeOps::ok();
        let fx = service("[git]\n", ops.clone()).await;
        let mut leaving = repo_def("leaving", None);
        leaving.sync_on_quit = true;
        fx.svc.put_repo("leaving", leaving).unwrap();
        fx.svc.put_repo("staying", repo_def("staying", None)).unwrap();

        fx.svc.run_quit_syncs(Duration::from_secs(5));

        assert_eq!(ops.calls(), vec![(JobOp::Sync, "leaving".to_string())]);
        // Every later admission answers `shutting_down` (503): the window is closing
        // and a new job could only be abandoned.
        assert!(fx.svc.draining());
        let err = fx
            .svc
            .start_job("staying", JobOp::Sync, OpRequest::manual())
            .expect_err("draining");
        assert_eq!(err.code(), GitErrorCode::ShuttingDown);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_zero_quit_timeout_skips_the_syncs_but_still_drains() {
        let ops = FakeOps::ok();
        let fx = service("[git]\n", ops.clone()).await;
        let mut leaving = repo_def("leaving", None);
        leaving.sync_on_quit = true;
        fx.svc.put_repo("leaving", leaving).unwrap();

        fx.svc.run_quit_syncs(Duration::ZERO);

        assert!(ops.calls().is_empty(), "0 disables the whole step");
        assert!(fx.svc.draining());
    }
```

- [ ] **Step 63: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::tests::quit_syncs`

Expected: FAIL — compile error.

```
error[E0599]: no method named `run_quit_syncs` found for struct `Arc<GitService>` in the current scope
   --> src/git/mod.rs:NN:17
    |
NN  |         fx.svc.run_quit_syncs(Duration::from_secs(5));
    |                ^^^^^^^^^^^^^^ method not found in `Arc<GitService>`
```

- [ ] **Step 64: Implement `run_quit_syncs`**

Add to `impl GitService`:

```rust
    /// The last thing the host does, after `kill_children()` so the trees are quiescent
    /// and the final commit is a consistent snapshot.
    ///
    /// Blocks the calling thread — `App::quit()` runs on the tao thread, never inside a
    /// runtime worker, so this cannot deadlock the runtime. It polls rather than joining
    /// futures because the work is on the blocking pool and `futures::join_all` is not a
    /// dependency of this crate.
    pub fn run_quit_syncs(self: &Arc<Self>, timeout: Duration) {
        // Admission happens BEFORE draining is set: `JobStore::admit` refuses every
        // request once draining, including ours. The gap between the two is one
        // synchronous loop with no `.await` in it.
        let mut started = Vec::new();
        if !timeout.is_zero() {
            for def in self.registry.list() {
                if !def.sync_on_quit {
                    continue;
                }
                match self.start_job(&def.id, JobOp::Sync, OpRequest::manual()) {
                    Ok(StartOutcome::Started(slot)) => started.push(slot),
                    Ok(other) => self.log(&format!(
                        "{} sync repo={} job={} ok code=- quit: skipped, already running",
                        now_ms(),
                        def.id,
                        other.slot().id
                    )),
                    Err(e) => self.log(&format!(
                        "{} sync repo={} job=- err code={} quit: {}",
                        now_ms(),
                        def.id,
                        e.code().as_str(),
                        one_line(&e.message)
                    )),
                }
            }
        }
        self.jobs.set_draining(true);
        if started.is_empty() {
            return;
        }
        if !wait_until_terminal(&started, timeout) {
            self.jobs.abort_all(AbortReason::Shutdown);
            wait_until_terminal(&started, Duration::from_millis(QUIT_ABANDON_GRACE_MS));
        }
        let abandoned: Vec<String> = started
            .iter()
            .filter(|slot| !slot.is_terminal())
            .map(|slot| slot.id.to_string())
            .collect();
        if !abandoned.is_empty() {
            // `std::process::exit(0)` joins no blocking threads. libgit2 writes objects
            // and refs via write-to-temp + rename, so an abandoned job leaves a valid
            // repository — at worst missing its last operation. The ids are logged so
            // the next run's `last_sync` can be read against them.
            self.log(&format!(
                "{} shutdown repo=- job=- err code=canceled {} job(s) abandoned: {}",
                now_ms(),
                abandoned.len(),
                abandoned.join(", ")
            ));
        }
    }
```

and as a free function above `impl GitService`:

```rust
/// Poll every slot to terminal or give up. Returns whether they all finished.
fn wait_until_terminal(slots: &[Arc<JobSlot>], budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        if slots.iter().all(|slot| slot.is_terminal()) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
```

- [ ] **Step 65: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::tests::`

Expected: PASS.

```
running 32 tests
test git::tests::a_zero_quit_timeout_skips_the_syncs_but_still_drains ... ok
test git::tests::quit_syncs_run_then_drain_the_store ... ok
...
test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 66: Commit and run the full gate**

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test --bin chrome-host-app
git add src/git/mod.rs
git commit -m "feat(git): run_quit_syncs, draining, and the abandonment log

Admission runs before draining is set, the opposite of the design sketch's
order: admit() refuses everything once draining, including the quit syncs
themselves. The wait polls because the work is on the blocking pool and
join_all is not a dependency here."
```

---

## Part 2 — `src/git/api.rs` and the nested router

- [ ] **Step 67: Write the failing tests for `ApiError`**

Add a `#[cfg(test)] mod tests` block at the bottom of `src/git/api.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::error::GitErrorCode;
    use crate::git::jobs::{Admission, JobOp, JobStore};

    async fn body_of(r: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(r.into_body(), 64 * 1024)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    #[tokio::test]
    async fn the_envelope_always_names_the_host_instance() {
        let err = GitError::invalid_request("branch", "not a valid branch name")
            .with_repo("notes");
        let r = ApiError::new(err, "7c1e93aa").into_response();
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(r.headers().get("retry-after").is_none());
        let body = body_of(r).await;
        assert_eq!(body["error"]["code"], "invalid_request");
        assert_eq!(body["error"]["message"], "not a valid branch name");
        assert_eq!(body["error"]["repo_id"], "notes");
        assert_eq!(body["error"]["field"], "branch");
        assert_eq!(body["error"]["host_instance"], "7c1e93aa");
        // Absent fields vanish rather than serializing as null: a client that keys off
        // `"job" in error` must not see one on every response.
        assert!(body["error"].get("job").is_none());
        assert!(body["error"].get("path").is_none());
    }

    #[tokio::test]
    async fn only_repo_busy_embeds_the_running_job_and_sets_retry_after() {
        let store = JobStore::new("7c1e93aa".to_string());
        let Ok(Admission::Started(slot, _lease)) = store.admit("notes", JobOp::Sync, None) else {
            panic!("an idle repo must admit");
        };
        let r = ApiError::busy(&slot, "7c1e93aa").into_response();
        assert_eq!(r.status(), StatusCode::CONFLICT);
        assert_eq!(r.headers()["retry-after"], "1");
        let body = body_of(r).await;
        assert_eq!(body["error"]["code"], "repo_busy");
        assert_eq!(body["error"]["repo_id"], "notes");
        assert_eq!(body["error"]["job"]["id"], slot.id.to_string());
        assert_eq!(body["error"]["job"]["op"], "sync");
        assert_eq!(body["error"]["job"]["state"], "running");
        assert_eq!(body["error"]["job"]["stalled"], false);
    }
}
```

- [ ] **Step 68: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::api`

Expected: FAIL — compile errors.

```
error[E0433]: failed to resolve: use of undeclared type `ApiError`
error[E0433]: failed to resolve: use of undeclared type `GitError`
error[E0412]: cannot find type `Response` in this scope
```

- [ ] **Step 69: Implement `ApiError`**

Put this at the top of `src/git/api.rs`, under the module comment:

```rust
use crate::git::error::GitError;
use crate::git::jobs::{JobRef, JobSlot};
use crate::internal_server::HostState;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use std::sync::Arc;

/// The §5.4 error envelope: a `GitError` plus the two things only the service knows.
///
/// `GitError` has its own `IntoResponse` for the answers produced where no service
/// exists — the `git_disabled` fallback. Everything a live service answers goes through
/// this type instead, so `host_instance` is present on every one of them.
pub struct ApiError {
    pub err: GitError,
    pub host_instance: String,
    /// Populated for `repo_busy` and nothing else: a client keys off its presence to
    /// decide whether it can poll instead of retry.
    pub job: Option<JobRef>,
}

impl ApiError {
    pub fn new(err: GitError, host_instance: &str) -> ApiError {
        ApiError {
            err,
            host_instance: host_instance.to_string(),
            job: None,
        }
    }

    pub fn busy(slot: &Arc<JobSlot>, host_instance: &str) -> ApiError {
        ApiError {
            err: GitError::repo_busy(&slot.repo_id, slot.op.as_str()).with_repo(&slot.repo_id),
            host_instance: host_instance.to_string(),
            job: Some(slot.as_ref_view()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Hand-rolled so the field order is the envelope's order and the optional
        // fields vanish rather than serializing as null.
        struct Body<'a>(&'a ApiError);
        impl Serialize for Body<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                let e = &self.0.err;
                let len = 3
                    + usize::from(e.repo_id.is_some())
                    + usize::from(e.path.is_some())
                    + usize::from(e.field.is_some())
                    + usize::from(self.0.job.is_some());
                let mut st = s.serialize_struct("error", len)?;
                st.serialize_field("code", e.code.as_str())?;
                st.serialize_field("message", &e.message)?;
                if let Some(v) = &e.repo_id {
                    st.serialize_field("repo_id", v)?;
                }
                if let Some(v) = &e.path {
                    st.serialize_field("path", v)?;
                }
                if let Some(v) = &e.field {
                    st.serialize_field("field", v)?;
                }
                st.serialize_field("host_instance", &self.0.host_instance)?;
                if let Some(job) = &self.0.job {
                    st.serialize_field("job", job)?;
                }
                st.end()
            }
        }
        #[derive(Serialize)]
        struct Envelope<'a> {
            error: Body<'a>,
        }

        let status = self.err.http_status();
        let mut response = (status, Json(Envelope { error: Body(&self) })).into_response();
        if status == StatusCode::CONFLICT {
            // Every 409 is a "someone else holds this, come back" answer, and a client
            // without a backoff should still not hot-loop.
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                HeaderValue::from_static("1"),
            );
        }
        response
    }
}
```

- [ ] **Step 70: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::api`

Expected: PASS.

```
running 2 tests
test git::api::tests::only_repo_busy_embeds_the_running_job_and_sets_retry_after ... ok
test git::api::tests::the_envelope_always_names_the_host_instance ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 71: Commit**

```bash
git add src/git/api.rs
git commit -m "feat(git): the /api/git error envelope

Hand-rolled serializer so the field order is the envelope's and absent
fields vanish instead of serializing as null — a client that keys off
\"job\" in error must not see one on every response."
```

---

- [ ] **Step 72: Write the failing tests for the request DTOs**

Append inside `mod tests` in `src/git/api.rs`:

```rust
    fn parse<T: serde::de::DeserializeOwned>(json: &str) -> Result<T, GitError> {
        body_to::<T>(json)
    }

    #[test]
    fn a_repo_body_fills_in_the_registry_defaults_and_rejects_strays() {
        let body: RepoDefBody = parse("{}").expect("an empty body is a valid definition");
        assert_eq!(body.remote_name, "origin");
        assert_eq!(body.settings_path, "settings.json");
        assert!(body.branch.is_none());
        let def = body.into_def("notes".to_string());
        assert_eq!(def.id, "notes");
        // "" is the registry's sentinel for "use [git].default_branch", so the API
        // never has to know what the default is.
        assert!(def.branch.is_empty());
        assert_eq!(def.created_at_ms, 0);

        let err = parse::<RepoDefBody>(r#"{"bogus": 1}"#).expect_err("stray key");
        assert_eq!(err.code(), GitErrorCode::InvalidRequest);
        assert_eq!(err.field.as_deref(), Some("body"));
        assert!(err.message.contains("bogus"), "{}", err.message);
    }

    #[test]
    fn a_missing_body_is_an_empty_object_and_bad_json_is_invalid_request() {
        // `POST /init` from a shell has no body at all, and `{}` must mean the same.
        let init: InitBody = parse("").expect("no body");
        assert!(init.request_id.is_none());
        let err = parse::<InitBody>("{").expect_err("malformed");
        assert_eq!(err.code(), GitErrorCode::InvalidRequest);
    }

    #[test]
    fn sync_pushes_by_default_and_the_caller_can_turn_it_off() {
        let body: SyncBody = parse("{}").unwrap();
        assert!(body.push, "sync means sync, so push defaults to true");
        let body: SyncBody = parse(r#"{"push": false}"#).unwrap();
        assert!(!body.push);
        assert!(!body.force && !body.allow_empty);
        assert!(body.restart_children.is_none());
    }

    #[test]
    fn branch_upstream_distinguishes_absent_from_null_from_a_value() {
        // Three different intentions that JSON alone cannot tell apart without this:
        // leave it alone, unset it, set it.
        let body: BranchBody = parse(r#"{"name": "dev"}"#).unwrap();
        assert_eq!(body.upstream, None);
        assert!(body.checkout, "checkout defaults to true");
        let body: BranchBody = parse(r#"{"name": "dev", "upstream": null}"#).unwrap();
        assert_eq!(body.upstream, Some(None));
        let body: BranchBody = parse(r#"{"name": "dev", "upstream": "origin/dev"}"#).unwrap();
        assert_eq!(body.upstream, Some(Some("origin/dev".to_string())));
    }

    #[test]
    fn a_request_id_is_bounded_and_printable() {
        assert!(check_request_id(None).is_ok());
        assert!(check_request_id(Some("boot-1")).is_ok());
        let long = "x".repeat(MAX_REQUEST_ID_LEN + 1);
        let err = check_request_id(Some(&long)).expect_err("too long");
        assert_eq!(err.code(), GitErrorCode::InvalidRequest);
        assert_eq!(err.field.as_deref(), Some("request_id"));
        // A newline would break `git.log`'s one-record-per-line format.
        assert!(check_request_id(Some("boot\n1")).is_err());
        assert!(check_request_id(Some("")).is_err());
    }

    #[test]
    fn the_job_filter_is_always_capped() {
        let filter = job_filter(JobsQuery {
            repo_id: None,
            state: None,
            limit: None,
        })
        .unwrap();
        assert_eq!(filter.limit, DEFAULT_JOB_LIST_LIMIT);
        // `JobFilter::limit == 0` means unlimited in the store, so the HTTP layer must
        // never pass it through.
        let filter = job_filter(JobsQuery {
            repo_id: None,
            state: None,
            limit: Some(0),
        })
        .unwrap();
        assert_eq!(filter.limit, 1);
        let filter = job_filter(JobsQuery {
            repo_id: None,
            state: None,
            limit: Some(100_000),
        })
        .unwrap();
        assert_eq!(filter.limit, MAX_JOB_LIST_LIMIT);
        let err = job_filter(JobsQuery {
            repo_id: None,
            state: Some("sleeping".to_string()),
            limit: None,
        })
        .expect_err("unknown state");
        assert_eq!(err.field.as_deref(), Some("state"));
    }
```

- [ ] **Step 73: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::api`

Expected: FAIL — compile errors.

```
error[E0433]: failed to resolve: use of undeclared type `RepoDefBody`
error[E0425]: cannot find function `body_to` in this scope
error[E0425]: cannot find function `check_request_id` in this scope
error[E0425]: cannot find function `job_filter` in this scope
error[E0425]: cannot find value `MAX_REQUEST_ID_LEN` in this scope
```

- [ ] **Step 74: Implement the DTOs and the three validators**

Extend the import block at the top of `src/git/api.rs`:

```rust
use crate::git::jobs::{JobFilter, JobState};
use crate::git::registry::{AuthorSpec, CredentialSpec, RepoDef};
use serde::Deserialize;
```

Then add, above the `#[cfg(test)]` block:

```rust
/// Long enough for a UUID plus a prefix, short enough that a hostile child cannot make
/// the `(repo_id, request_id)` index grow without bound.
pub const MAX_REQUEST_ID_LEN: usize = 128;
pub const DEFAULT_JOB_LIST_LIMIT: usize = 50;
pub const MAX_JOB_LIST_LIMIT: usize = 200;

fn default_remote_name() -> String {
    "origin".to_string()
}

fn default_settings_path() -> String {
    "settings.json".to_string()
}

fn yes() -> bool {
    true
}

/// `Option<Option<T>>` where the outer layer is "was the key present at all".
///
/// `#[serde(default)]` gives `None` for an absent key; this function is reached only
/// when the key *is* present, so an explicit `null` becomes `Some(None)`.
fn de_double_option<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(d)?))
}

/// Parse a request body into its DTO.
///
/// The handlers take the body as a `String` rather than as `Json<T>` so that a stray
/// key, malformed JSON, a missing content-type and a missing body all come back through
/// the same envelope. `Json<T>`'s own rejection is a bare 422 with a plain-text body,
/// and §5.4 promises one envelope on every non-2xx from `/api/git/*`.
fn body_to<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, GitError> {
    let trimmed = raw.trim();
    let text = if trimmed.is_empty() { "{}" } else { trimmed };
    serde_json::from_str(text).map_err(|e| GitError::invalid_request("body", e.to_string()))
}

fn check_request_id(request_id: Option<&str>) -> Result<(), GitError> {
    let Some(value) = request_id else {
        return Ok(());
    };
    // Printable ASCII only: the id is echoed into `git.log`, which is one record per
    // line, and into a `BTreeMap` key.
    let printable = value.bytes().all(|b| (0x20..0x7f).contains(&b));
    if value.is_empty() || value.len() > MAX_REQUEST_ID_LEN || !printable {
        return Err(GitError::invalid_request(
            "request_id",
            format!("request_id must be 1 to {MAX_REQUEST_ID_LEN} printable ASCII characters"),
        ));
    }
    Ok(())
}

fn job_filter(query: JobsQuery) -> Result<JobFilter, GitError> {
    if let Some(id) = &query.repo_id {
        crate::git::registry::validate_id(id)?;
    }
    let state = match query.state.as_deref() {
        None => None,
        Some(raw) => Some(JobState::parse(raw).ok_or_else(|| {
            GitError::invalid_request(
                "state",
                format!("state must be running, succeeded or failed (got {raw:?})"),
            )
        })?),
    };
    Ok(JobFilter {
        repo_id: query.repo_id,
        state,
        // `JobFilter::limit == 0` means unlimited in the store; the HTTP layer always
        // passes a real cap so no caller can ask the host to serialize every record.
        limit: query
            .limit
            .unwrap_or(DEFAULT_JOB_LIST_LIMIT)
            .clamp(1, MAX_JOB_LIST_LIMIT),
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoDefBody {
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default = "default_remote_name")]
    pub remote_name: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub credential: Option<CredentialSpec>,
    #[serde(default)]
    pub author: Option<AuthorSpec>,
    #[serde(default)]
    pub sync_settings: bool,
    #[serde(default = "default_settings_path")]
    pub settings_path: String,
    #[serde(default)]
    pub auto_sync_secs: Option<u64>,
    #[serde(default)]
    pub sync_on_start: bool,
    #[serde(default)]
    pub sync_on_quit: bool,
    #[serde(default)]
    pub restart_children_on_pull: bool,
}

impl RepoDefBody {
    pub fn into_def(self, id: String) -> RepoDef {
        RepoDef {
            id,
            remote: self.remote,
            remote_name: self.remote_name,
            // "" is the registry's sentinel for "use [git].default_branch", and
            // `Registry::put` is the single place that resolves it.
            branch: self.branch.unwrap_or_default(),
            credential: self.credential,
            author: self.author,
            sync_settings: self.sync_settings,
            settings_path: self.settings_path,
            auto_sync_secs: self.auto_sync_secs,
            sync_on_start: self.sync_on_start,
            sync_on_quit: self.sync_on_quit,
            restart_children_on_pull: self.restart_children_on_pull,
            // 0 means "stamp me": the API never invents timestamps, and a replace keeps
            // the original `created_at_ms` because `Registry::put` owns that decision.
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitBody {
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneBody {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub credential: Option<CredentialSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullBody {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub credential: Option<CredentialSpec>,
    #[serde(default)]
    pub commit_local: bool,
    #[serde(default)]
    pub restart_children: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushBody {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub credential: Option<CredentialSpec>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncBody {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub credential: Option<CredentialSpec>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub author: Option<AuthorSpec>,
    #[serde(default)]
    pub allow_empty: bool,
    #[serde(default = "yes")]
    pub push: bool,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub restart_children: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitBody {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub author: Option<AuthorSpec>,
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    #[serde(default)]
    pub allow_empty: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchBody {
    #[serde(default)]
    pub request_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub create: bool,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default = "yes")]
    pub checkout: bool,
    #[serde(default, deserialize_with = "de_double_option")]
    pub upstream: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetBody {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub clean_untracked: bool,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Deserialize)]
pub struct JobsQuery {
    #[serde(default)]
    pub repo_id: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteQuery {
    #[serde(default)]
    pub purge: bool,
}
```

`JobsQuery` and `DeleteQuery` deliberately do **not** carry `deny_unknown_fields`: a query
string routinely picks up cache-busting parameters, and rejecting them would break callers
for no safety gain. Every JSON body does carry it — a silently-ignored `"messsage"` key is
a data-loss bug.

- [ ] **Step 75: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::api`

Expected: PASS.

```
running 8 tests
test git::api::tests::a_missing_body_is_an_empty_object_and_bad_json_is_invalid_request ... ok
test git::api::tests::a_repo_body_fills_in_the_registry_defaults_and_rejects_strays ... ok
test git::api::tests::a_request_id_is_bounded_and_printable ... ok
test git::api::tests::branch_upstream_distinguishes_absent_from_null_from_a_value ... ok
test git::api::tests::sync_pushes_by_default_and_the_caller_can_turn_it_off ... ok
test git::api::tests::the_job_filter_is_always_capped ... ok
...
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 76: Commit**

```bash
git add src/git/api.rs
git commit -m "feat(git): request DTOs, body parsing, and the query validators

Bodies are taken as String and parsed here so a stray key, malformed JSON,
a missing content-type and a missing body all answer through one envelope;
Json<T>'s own rejection is a bare 422 with a plain-text body. upstream is a
double Option because absent, null and a value are three different
intentions and JSON alone cannot tell them apart."
```

---

- [ ] **Step 77: Write the failing test for the disabled router**

Append inside `mod tests` in `src/internal_server.rs`:

```rust
    const GIT_ROUTES: [(&str, &str); 17] = [
        ("GET", "/api/git"),
        ("GET", "/api/git/repos"),
        ("PUT", "/api/git/repos/notes"),
        ("GET", "/api/git/repos/notes"),
        ("DELETE", "/api/git/repos/notes"),
        ("GET", "/api/git/repos/notes/status"),
        ("GET", "/api/git/repos/notes/branches"),
        ("POST", "/api/git/repos/notes/init"),
        ("POST", "/api/git/repos/notes/clone"),
        ("POST", "/api/git/repos/notes/pull"),
        ("POST", "/api/git/repos/notes/push"),
        ("POST", "/api/git/repos/notes/sync"),
        ("POST", "/api/git/repos/notes/commit"),
        ("POST", "/api/git/repos/notes/branch"),
        ("POST", "/api/git/repos/notes/reset"),
        ("GET", "/api/git/jobs"),
        ("GET", "/api/git/jobs/job_deadbeef_000001"),
    ];

    #[tokio::test]
    async fn every_git_route_is_git_disabled_without_the_section() {
        let (_state, port, _rx) = spawn_state(false).await;
        let client = reqwest::Client::new();
        for (method, path) in GIT_ROUTES {
            let r = client
                .request(method.parse().unwrap(), format!("http://127.0.0.1:{port}{path}"))
                .header("x-host-token", "tok123")
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 404, "{method} {path}");
            let body: serde_json::Value = r.json().await.unwrap();
            // A bare 404 with an empty body is indistinguishable from a typo in the
            // path; the child has to be able to tell that the *host* has no git.
            assert_eq!(body["error"]["code"], "git_disabled", "{method} {path}");
        }
        // The rest of the server is untouched.
        let r = reqwest::get(format!("http://127.0.0.1:{port}/api/status"))
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
```

- [ ] **Step 78: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app internal_server::tests::every_git_route`

Expected: FAIL — the paths are not routed at all, so axum answers with an empty 404 body
and the JSON decode fails.

```
running 1 test
test internal_server::tests::every_git_route_is_git_disabled_without_the_section ... FAILED

---- internal_server::tests::every_git_route_is_git_disabled_without_the_section stdout ----
thread 'internal_server::tests::…' panicked at src/internal_server.rs:NN:56:
called `Result::unwrap()` on an `Err` value: reqwest::Error { kind: Decode,
source: Error("EOF while parsing a value", line: 1, column: 0) }
```

- [ ] **Step 79: Wire `HostState.git` and nest the disabled router**

In `src/git/api.rs`, add above the `#[cfg(test)]` block:

```rust
/// The router nested at `/api/git` when `[git]` is absent.
///
/// A fallback rather than a route table, so it answers for `/api/git` itself, for every
/// sub-path, and for every method — there is no shape of request that can get a bare 404
/// out of a host that simply has no git.
pub fn disabled_router() -> axum::Router<HostState> {
    axum::Router::new().fallback(git_disabled)
}

async fn git_disabled() -> Response {
    // Deliberately a bare `GitError`: there is no service, so there is no
    // `host_instance` to name and inventing one would be a lie.
    GitError::new(
        crate::git::error::GitErrorCode::GitDisabled,
        "the host was built without a [git] section in app.toml",
    )
    .into_response()
}
```

In `src/internal_server.rs`, add the field to `HostState`:

```rust
pub struct HostState {
    pub app_name: String,
    pub token: String,
    pub status: Arc<RwLock<AppStatus>>,
    pub schema: Option<SettingsSection>,
    pub settings_file: PathBuf,
    pub events: mpsc::UnboundedSender<HostEvent>,
    pub git: Option<Arc<crate::git::GitService>>,
}
```

and nest in `router`:

```rust
pub fn router(state: HostState) -> Router {
    let router = Router::new()
        .route("/loading", get(loading))
        .route("/placeholder", get(placeholder))
        .route("/settings", get(settings_page))
        .route("/style.css", get(style))
        .route("/api/status", get(api_status))
        .route("/api/settings", post(api_settings))
        .route("/api/restart", post(api_restart));
    // Nested before `with_state` so the sub-router's handlers resolve against the same
    // state. Which of the two is nested is the on/off switch for the whole feature.
    let router = match &state.git {
        Some(_) => router.nest("/api/git", crate::git::api::router(state.clone())),
        None => router.nest("/api/git", crate::git::api::disabled_router()),
    };
    router.with_state(state)
}
```

Add `git: None,` to the `HostState { … }` literal in `spawn_state` (`src/internal_server.rs`,
after `events: tx,`) and to the one in `main()` in `src/main.rs` (after `events: host_tx,`).

`crate::git::api::router` does not exist yet — for this step only, add the minimal form
next to `disabled_router` in `src/git/api.rs` so the crate compiles; Step 84 fills in the
route table:

```rust
pub fn router(state: HostState) -> axum::Router<HostState> {
    axum::Router::new().route_layer(axum::middleware::from_fn_with_state(state, require_token))
}

/// The single gate. Every route under `/api/git` passes through it — GETs included,
/// which is a deliberate divergence from the unauthenticated `GET /api/status`: a repo
/// listing names remotes and paths, and `GET /jobs` names branches and commit ids.
///
/// `route_layer` and not `layer`, so a request for a path that does not exist is a plain
/// 404 rather than a 401 that implies the path is real.
async fn require_token(
    axum::extract::State(state): axum::extract::State<HostState>,
    headers: axum::http::HeaderMap,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if !crate::internal_server::authorized(&headers, &state) {
        let instance = state
            .git
            .as_ref()
            .map(|git| git.host_instance().to_string())
            .unwrap_or_default();
        return ApiError::new(
            GitError::new(
                crate::git::error::GitErrorCode::Unauthorized,
                "missing or invalid x-host-token",
            ),
            &instance,
        )
        .into_response();
    }
    next.run(request).await
}
```

and change `authorized`'s visibility in `src/internal_server.rs:169`:

```rust
pub(crate) fn authorized(headers: &HeaderMap, state: &HostState) -> bool {
```

- [ ] **Step 80: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app internal_server::tests`

Expected: PASS — including the six pre-existing tests, unchanged.

```
running 7 tests
test internal_server::tests::every_git_route_is_git_disabled_without_the_section ... ok
test internal_server::tests::pages_render_with_app_name ... ok
test internal_server::tests::restart_endpoint_sends_event ... ok
test internal_server::tests::settings_page_404_without_schema ... ok
test internal_server::tests::settings_post_404_with_valid_token_but_no_schema ... ok
test internal_server::tests::settings_post_requires_token_and_validates ... ok
test internal_server::tests::status_endpoint_reports_states ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 81: Commit**

```bash
git add src/git/api.rs src/internal_server.rs src/main.rs
git commit -m "feat(git): nest /api/git, or an explicit git_disabled fallback

Which router is nested is the on switch for the whole feature. The disabled
side is a fallback rather than a route table so no method or sub-path can
get a bare 404 out of a host that simply has no git. The auth layer is a
route_layer so an unknown path stays a 404 instead of a 401 that would imply
the path is real."
```

---

- [ ] **Step 82: Write the failing tests for the service and repo-list routes**

Append inside `mod tests` in `src/internal_server.rs`:

```rust
    async fn spawn_git_state_cfg(
        git_toml: &str,
        ops: Arc<dyn crate::git::GitOps>,
    ) -> (HostState, u16, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::config::RuntimePaths::under(dir.path(), "com.example.git");
        paths.ensure().unwrap();
        let cfg = AppConfig::from_str(&format!(
            "[app]\nname = \"TestApp\"\nidentifier = \"com.example.git\"\n{git_toml}"
        ))
        .unwrap();
        // The receiver is dropped on purpose: every `events.send` in the git subsystem
        // ignores a closed channel, and these tests assert over HTTP, not over events.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let status = Arc::new(tokio::sync::RwLock::new(AppStatus::Ready {
            target_url: "http://x/".to_string(),
        }));
        let git = cfg.git.as_ref().map(|_| {
            let svc = crate::git::GitService::with_ops(
                &cfg,
                &paths,
                tokio::runtime::Handle::current(),
                tx.clone(),
                status.clone(),
                ops,
            );
            svc.start();
            svc
        });
        let state = HostState {
            app_name: "TestApp".to_string(),
            token: "tok123".to_string(),
            status,
            schema: None,
            settings_file: paths.settings_file.clone(),
            events: tx,
            git,
        };
        let port = start(state.clone()).await.unwrap();
        (state, port, dir)
    }

    /// Mirrors `spawn_state`, with a git service wired in.
    async fn spawn_git_state(
        enabled: bool,
        ops: Arc<dyn crate::git::GitOps>,
    ) -> (HostState, u16, tempfile::TempDir) {
        spawn_git_state_cfg(if enabled { "[git]\n" } else { "" }, ops).await
    }

    /// A `GitOps` that answers instantly, so the HTTP tests never wait on libgit2.
    struct StubOps;

    impl crate::git::GitOps for StubOps {
        fn run(
            &self,
            _op: crate::git::jobs::JobOp,
            _ctx: &crate::git::ops::OpCtx,
        ) -> Result<crate::git::ops::OpOutcome, crate::git::error::GitError> {
            Ok(crate::git::ops::OpOutcome::new("up_to_date", "main"))
        }
    }

    fn git(port: u16, path: &str) -> String {
        format!("http://127.0.0.1:{port}/api/git{path}")
    }

    #[tokio::test]
    async fn service_info_and_the_repo_list_answer_only_with_the_token() {
        let (_state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        let client = reqwest::Client::new();

        for path in ["", "/repos"] {
            let bare = client.get(git(port, path)).send().await.unwrap();
            assert_eq!(bare.status(), 401, "{path} answered without a token");
            let body: serde_json::Value = bare.json().await.unwrap();
            assert_eq!(body["error"]["code"], "unauthorized");
        }

        let info: serde_json::Value = client
            .get(git(port, ""))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(info["host_instance"].as_str().unwrap().len(), 8);
        assert_eq!(info["job_retention"], 50);
        assert_eq!(info["repo_count"], 0);
        assert_eq!(info["registry_writable"], true);

        let repos: serde_json::Value = client
            .get(git(port, "/repos"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(repos["repos"].as_array().unwrap().len(), 0);
    }
```

- [ ] **Step 83: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app internal_server::tests::service_info_and`

Expected: FAIL — the two routes do not exist, so `route_layer` never runs and axum answers
404.

```
running 1 test
test internal_server::tests::service_info_and_the_repo_list_answer_only_with_the_token ... FAILED

---- internal_server::tests::service_info_and_the_repo_list_answer_only_with_the_token stdout ----
thread 'internal_server::tests::…' panicked at src/internal_server.rs:NN:13:
assertion `left == right` failed:  answered without a token
  left: 404
 right: 401
```

- [ ] **Step 84: Implement the first two routes and the shared handler plumbing**

In `src/git/api.rs`, extend the imports:

```rust
use crate::git::{DeleteOutcome, GitService, RepoWithStatus, StartOutcome};
use crate::git::ops::{BranchRequest, OpRequest, ResetRequest};
use crate::git::registry::{RepoView, Warning};
use axum::extract::{Path, Query, Request, State};
use axum::middleware::Next;
use axum::routing::{get, post, put};
use axum::http::HeaderMap;
```

Add the response DTOs and the two handler helpers above `disabled_router`:

```rust
#[derive(Serialize)]
pub struct ReposResponse {
    pub repos: Vec<RepoView>,
}

#[derive(Serialize)]
pub struct RepoResponse {
    pub repo: RepoView,
    pub warnings: Vec<Warning>,
}

#[derive(Serialize)]
pub struct JobResponse {
    pub job: crate::git::jobs::JobView,
}

#[derive(Serialize)]
pub struct JobsResponse {
    pub jobs: Vec<crate::git::jobs::JobView>,
}

/// `HostState.git` is `Some` for every request that reaches this router — the disabled
/// variant is nested instead. Answering `git_disabled` rather than panicking keeps that
/// guarantee honest if the wiring is ever changed.
fn service(state: &HostState) -> Result<Arc<GitService>, Response> {
    match &state.git {
        Some(git) => Ok(git.clone()),
        None => Err(git_disabled_response()),
    }
}

fn git_disabled_response() -> Response {
    GitError::new(
        crate::git::error::GitErrorCode::GitDisabled,
        "the host was built without a [git] section in app.toml",
    )
    .into_response()
}

/// Every handler funnels its errors through here, so `host_instance` is on all of them.
fn fail(git: &GitService, err: GitError) -> Response {
    ApiError::new(err, git.host_instance()).into_response()
}
```

Replace `git_disabled`'s body with a call to `git_disabled_response()`, and now that the
extractors are imported, rewrite `require_token`'s signature with the short names
(`State(state): State<HostState>`, `headers: HeaderMap`, `request: Request`, `next: Next`)
— the fully-qualified form in Step 79 existed only because nothing was imported yet.
Then replace `router` with:

```rust
pub fn router(state: HostState) -> axum::Router<HostState> {
    axum::Router::new()
        .route("/", get(service_info))
        .route("/repos", get(list_repos))
        .route_layer(axum::middleware::from_fn_with_state(state, require_token))
}

async fn service_info(State(state): State<HostState>) -> Response {
    match service(&state) {
        Ok(git) => Json(git.service_info()).into_response(),
        Err(response) => response,
    }
}

async fn list_repos(State(state): State<HostState>) -> Response {
    match service(&state) {
        Ok(git) => Json(ReposResponse {
            repos: git.list_views(),
        })
        .into_response(),
        Err(response) => response,
    }
}
```

- [ ] **Step 85: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app internal_server::tests`

Expected: PASS.

```
running 8 tests
test internal_server::tests::service_info_and_the_repo_list_answer_only_with_the_token ... ok
...
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 86: Commit**

```bash
git add src/git/api.rs src/internal_server.rs
git commit -m "feat(git): the auth layer, GET /api/git and GET /api/git/repos

One from_fn_with_state layer authenticates every route including the GETs:
a repo listing names remotes and paths and a job listing names branches and
commit ids, so /api/status's open door is not the right precedent here.
spawn_git_state mirrors spawn_state with a stubbed GitOps."
```

---

- [ ] **Step 87: Write the failing tests for the repo routes**

Append inside `mod tests` in `src/internal_server.rs`:

```rust
    fn authed() -> reqwest::Client {
        reqwest::Client::new()
    }

    async fn put_notes(port: u16, body: serde_json::Value) -> reqwest::Response {
        authed()
            .put(git(port, "/repos/notes"))
            .header("x-host-token", "tok123")
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn put_creates_then_replaces_and_never_echoes_a_secret() {
        let (_state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        let body = json!({
            "remote": "https://github.com/acme/notes.git",
            "branch": "main",
            "credential": {"kind": "token", "username": "x-access-token",
                           "token": "github_pat_SUPERSECRET"},
            "auto_sync_secs": 300
        });

        let created = put_notes(port, body.clone()).await;
        assert_eq!(created.status(), 201);
        let text = created.text().await.unwrap();
        // Blunt canary: no path from a stored token to any response body may exist.
        assert!(!text.contains("github_pat"), "leaked a token: {text}");
        let first: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(first["repo"]["id"], "notes");
        assert_eq!(first["repo"]["branch"], "main");
        assert_eq!(first["repo"]["credential"]["kind"], "token");
        assert_eq!(first["repo"]["credential"]["token_stored"], true);
        assert_eq!(first["repo"]["credential"]["bound_host"], "github.com");
        assert_eq!(first["repo"]["has_credentials"], true);
        assert_eq!(first["warnings"].as_array().unwrap().len(), 0);

        let replaced = put_notes(port, body).await;
        assert_eq!(replaced.status(), 200, "PUT is create-or-replace");
        let second: serde_json::Value = replaced.json().await.unwrap();
        assert!(
            second["repo"]["updated_at_ms"].as_u64() >= first["repo"]["updated_at_ms"].as_u64()
        );

        let err = authed()
            .put(git(port, "/repos/../evil"))
            .header("x-host-token", "tok123")
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        // axum normalises `..` out of the path, so this can only ever be a 404 or a 422
        // — never a write outside `repos/`.
        assert!(err.status() == 404 || err.status() == 422, "{}", err.status());

        for bad in ["A", &"n".repeat(200)] {
            let r = authed()
                .put(git(port, &format!("/repos/{bad}")))
                .header("x-host-token", "tok123")
                .json(&json!({}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 422, "accepted id {bad:?}");
            let body: serde_json::Value = r.json().await.unwrap();
            assert_eq!(body["error"]["code"], "invalid_repo_id");
        }
    }

    #[tokio::test]
    async fn get_and_delete_a_repo() {
        let (state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        put_notes(port, json!({})).await;

        for path in ["/repos/notes", "/repos/notes/status"] {
            let body: serde_json::Value = authed()
                .get(git(port, path))
                .header("x-host-token", "tok123")
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            // Both shapes are `{repo, status}` — §5.5's worked example, not §5.1's cell.
            assert_eq!(body["repo"]["id"], "notes", "{path}");
            assert_eq!(body["status"]["state"], "absent", "{path}");
        }

        let unknown = authed()
            .get(git(port, "/repos/ghost"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(unknown.status(), 404);
        let body: serde_json::Value = unknown.json().await.unwrap();
        assert_eq!(body["error"]["code"], "repo_not_found");
        assert!(body["error"]["host_instance"].is_string());

        let tree = state.git.as_ref().unwrap().tree_path("notes");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("inbox.md"), b"hi").unwrap();
        let deleted: serde_json::Value = authed()
            .delete(git(port, "/repos/notes?purge=true"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(deleted["deleted"], true);
        assert_eq!(deleted["purged"], true);
        assert!(!tree.exists());
    }

    #[tokio::test]
    async fn a_read_only_registry_still_serves_reads() {
        let (_state, port, _dir) =
            spawn_git_state_cfg("[git]\nregistry_writes = false\n", Arc::new(StubOps)).await;
        let r = put_notes(port, json!({})).await;
        assert_eq!(r.status(), 403);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["code"], "registry_read_only");

        let r = authed()
            .delete(git(port, "/repos/notes"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);

        // Reads are unaffected: `registry_writes = false` means the author owns
        // repos.json, not that the API is off.
        let r = authed()
            .get(git(port, "/repos"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let r = authed()
            .get(git(port, ""))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let info: serde_json::Value = r.json().await.unwrap();
        assert_eq!(info["registry_writable"], false);
    }
```

- [ ] **Step 88: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app internal_server::tests::put_creates`

Expected: FAIL — the route does not exist, so `route_layer` never runs and axum answers 404.

```
running 1 test
test internal_server::tests::put_creates_then_replaces_and_never_echoes_a_secret ... FAILED

---- internal_server::tests::put_creates_then_replaces_and_never_echoes_a_secret stdout ----
thread 'internal_server::tests::…' panicked at src/internal_server.rs:NN:9:
assertion `left == right` failed
  left: 404
 right: 201
```

- [ ] **Step 89: Implement the five repo routes**

Extend the route table in `src/git/api.rs` — every new `.route(...)` goes **before** the
`.route_layer(...)` line. A route chained after the layer is not covered by it, which is
the exact failure mode the single-layer design exists to prevent:

```rust
        .route(
            "/repos/{id}",
            put(put_repo).get(get_repo).delete(delete_repo),
        )
        .route("/repos/{id}/status", get(get_repo))
        .route("/repos/{id}/branches", get(get_branches))
```

and add the handlers:

```rust
async fn put_repo(
    State(state): State<HostState>,
    Path(id): Path<String>,
    raw: String,
) -> Response {
    let git = match service(&state) {
        Ok(git) => git,
        Err(response) => return response,
    };
    let body: RepoDefBody = match body_to(&raw) {
        Ok(body) => body,
        Err(e) => return fail(&git, e.with_repo(&id)),
    };
    let outcome = match git.put_repo(&id, body.into_def(id.clone())) {
        Ok(outcome) => outcome,
        Err(e) => return fail(&git, e),
    };
    // Re-read rather than converting the returned def: the response must show what the
    // registry actually stored, including its normalised branch and timestamps.
    let repo = match git.repo_view(&id) {
        Ok(repo) => repo,
        Err(e) => return fail(&git, e),
    };
    let status = if outcome.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    (
        status,
        Json(RepoResponse {
            repo,
            warnings: outcome.warnings,
        }),
    )
        .into_response()
}

/// Serves both `GET /repos/{id}` and `GET /repos/{id}/status`: §5.5's worked example
/// gives them the same `{repo, status}` body, and one handler cannot drift from itself.
async fn get_repo(State(state): State<HostState>, Path(id): Path<String>) -> Response {
    let git = match service(&state) {
        Ok(git) => git,
        Err(response) => return response,
    };
    match git.read_status(&id).await {
        Ok(both) => Json::<RepoWithStatus>(both).into_response(),
        Err(e) => fail(&git, e),
    }
}

async fn get_branches(State(state): State<HostState>, Path(id): Path<String>) -> Response {
    let git = match service(&state) {
        Ok(git) => git,
        Err(response) => return response,
    };
    match git.read_branches(&id).await {
        Ok(branches) => Json(branches).into_response(),
        Err(e) => fail(&git, e),
    }
}

async fn delete_repo(
    State(state): State<HostState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteQuery>,
) -> Response {
    let git = match service(&state) {
        Ok(git) => git,
        Err(response) => return response,
    };
    match git.delete_repo(&id, query.purge) {
        Ok(outcome) => Json::<DeleteOutcome>(outcome).into_response(),
        Err(e) => fail(&git, e),
    }
}
```

- [ ] **Step 90: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app internal_server::tests`

Expected: PASS.

```
running 11 tests
test internal_server::tests::a_read_only_registry_still_serves_reads ... ok
test internal_server::tests::get_and_delete_a_repo ... ok
test internal_server::tests::put_creates_then_replaces_and_never_echoes_a_secret ... ok
...
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 91: Commit**

```bash
git add src/git/api.rs src/internal_server.rs
git commit -m "feat(git): the five repository routes

PUT is one idempotent create-or-replace: no POST/PATCH split means no merge
rules to document and no way to repoint a remote while silently keeping the
stored credential. GET /repos/:id and GET /repos/:id/status share a handler
so the two bodies cannot drift. The leak canary greps the whole PUT response
for the token that went in."
```

---

- [ ] **Step 92: Write the failing tests for the eight mutating routes**

Append inside `mod tests` in `src/internal_server.rs`:

```rust
    async fn post(port: u16, path: &str, body: serde_json::Value) -> reqwest::Response {
        authed()
            .post(git(port, path))
            .header("x-host-token", "tok123")
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    /// Blocks the first job it is given until the test drops the sender. That is what
    /// makes the 409 deterministic: `admit` marks the repo busy synchronously, so the
    /// second request conflicts whether or not the blocking thread has started.
    struct GatedOps {
        gate: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl crate::git::GitOps for GatedOps {
        fn run(
            &self,
            _op: crate::git::jobs::JobOp,
            _ctx: &crate::git::ops::OpCtx,
        ) -> Result<crate::git::ops::OpOutcome, crate::git::error::GitError> {
            let gate = self.gate.lock().unwrap().take();
            if let Some(rx) = gate {
                let _ = rx.recv();
            }
            Ok(crate::git::ops::OpOutcome::new("up_to_date", "main"))
        }
    }

    #[tokio::test]
    async fn sync_returns_202_with_a_location_and_replays_a_repeated_request_id() {
        let (_state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        put_notes(port, json!({"remote": "https://github.com/acme/notes.git"})).await;

        let accepted = post(port, "/repos/notes/sync", json!({"request_id": "boot-1"})).await;
        assert_eq!(accepted.status(), 202);
        let location = accepted.headers()["location"].to_str().unwrap().to_string();
        let body: serde_json::Value = accepted.json().await.unwrap();
        let id = body["job"]["id"].as_str().unwrap().to_string();
        assert_eq!(location, format!("/api/git/jobs/{id}"));
        assert!(id.starts_with("job_"));
        assert_eq!(body["job"]["repo_id"], "notes");
        assert_eq!(body["job"]["op"], "sync");
        assert_eq!(body["job"]["request_id"], "boot-1");
        assert_eq!(body["job"]["stalled"], false);
        assert!(body["job"]["host_instance"].is_string());

        // The dropped-response case: same id, so the answer is the same job and never
        // a conflict.
        let replay = post(port, "/repos/notes/sync", json!({"request_id": "boot-1"})).await;
        assert_eq!(replay.status(), 200);
        assert!(replay.headers().get("location").is_none());
        let body: serde_json::Value = replay.json().await.unwrap();
        assert_eq!(body["job"]["id"], id);
    }

    #[tokio::test]
    async fn a_busy_repo_answers_409_with_the_running_job_embedded() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let ops = Arc::new(GatedOps {
            gate: std::sync::Mutex::new(Some(rx)),
        });
        let (_state, port, _dir) = spawn_git_state(true, ops).await;
        put_notes(port, json!({})).await;

        assert_eq!(post(port, "/repos/notes/commit", json!({})).await.status(), 202);
        let conflict = post(port, "/repos/notes/commit", json!({})).await;
        assert_eq!(conflict.status(), 409);
        assert_eq!(conflict.headers()["retry-after"], "1");
        let body: serde_json::Value = conflict.json().await.unwrap();
        assert_eq!(body["error"]["code"], "repo_busy");
        assert_eq!(body["error"]["repo_id"], "notes");
        assert_eq!(body["error"]["job"]["op"], "commit");
        assert_eq!(body["error"]["job"]["state"], "running");

        // A DELETE mid-job is the same conflict: the running job snapshotted its def.
        let conflict = authed()
            .delete(git(port, "/repos/notes"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(conflict.status(), 409);

        drop(tx); // release the blocked worker rather than leaking a pool thread
    }

    #[tokio::test]
    async fn the_mutating_routes_refuse_impossible_requests() {
        let (_state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        put_notes(port, json!({})).await;

        for verb in ["clone", "pull", "push", "sync"] {
            let r = post(port, &format!("/repos/notes/{verb}"), json!({})).await;
            assert_eq!(r.status(), 422, "{verb} with no remote");
            let body: serde_json::Value = r.json().await.unwrap();
            assert_eq!(body["error"]["code"], "remote_missing", "{verb}");
        }

        let r = post(port, "/repos/notes/reset", json!({"to": "head"})).await;
        assert_eq!(r.status(), 422);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["code"], "confirm_required");

        // create:false, checkout:false, no upstream asks for nothing at all.
        let r = post(
            port,
            "/repos/notes/branch",
            json!({"name": "dev", "checkout": false}),
        )
        .await;
        assert_eq!(r.status(), 422);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["code"], "invalid_request");

        let r = post(port, "/repos/notes/branch", json!({"name": "../evil"})).await;
        assert_eq!(r.status(), 422);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["field"], "name");

        let r = post(
            port,
            "/repos/notes/init",
            json!({"request_id": "x".repeat(200)}),
        )
        .await;
        assert_eq!(r.status(), 422);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["field"], "request_id");

        let r = post(port, "/repos/notes/commit", json!({"messsage": "typo"})).await;
        assert_eq!(r.status(), 422, "a silently-ignored key is a data-loss bug");
    }
```

- [ ] **Step 93: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app internal_server::tests::sync_returns_202`

Expected: FAIL — the route does not exist.

```
running 1 test
test internal_server::tests::sync_returns_202_with_a_location_and_replays_a_repeated_request_id ... FAILED

---- internal_server::tests::sync_returns_202_… stdout ----
thread 'internal_server::tests::…' panicked at src/internal_server.rs:NN:9:
assertion `left == right` failed
  left: 404
 right: 202
```

- [ ] **Step 94: Implement the eight mutating routes**

Extend the route table in `src/git/api.rs` — every new `.route(...)` goes **before** the
`.route_layer(...)` line. A route chained after the layer is not covered by it, which is
the exact failure mode the single-layer design exists to prevent:

```rust
        .route("/repos/{id}/init", post(post_init))
        .route("/repos/{id}/clone", post(post_clone))
        .route("/repos/{id}/pull", post(post_pull))
        .route("/repos/{id}/push", post(post_push))
        .route("/repos/{id}/sync", post(post_sync))
        .route("/repos/{id}/commit", post(post_commit))
        .route("/repos/{id}/branch", post(post_branch))
        .route("/repos/{id}/reset", post(post_reset))
```

and add the shared plumbing plus the handlers:

```rust
/// Pull the service and the parsed body out in one place, so eight handlers do not
/// repeat the same two `match`es.
fn prepare<T: serde::de::DeserializeOwned>(
    state: &HostState,
    id: &str,
    raw: &str,
) -> Result<(Arc<GitService>, T), Response> {
    let git = service(state)?;
    match body_to::<T>(raw) {
        Ok(body) => Ok((git, body)),
        Err(e) => Err(fail(&git, e.with_repo(id))),
    }
}

fn start(git: &Arc<GitService>, id: &str, op: crate::git::jobs::JobOp, req: OpRequest) -> Response {
    if let Err(e) = check_request_id(req.request_id.as_deref()) {
        return fail(git, e.with_repo(id));
    }
    match git.start_job(id, op, req) {
        Ok(outcome) => job_response(git, outcome),
        Err(e) => fail(git, e),
    }
}

fn job_response(git: &GitService, outcome: StartOutcome) -> Response {
    let (status, slot) = match &outcome {
        StartOutcome::Started(slot) => (StatusCode::ACCEPTED, slot),
        // Not 202: nothing new was started, and a caller that keys off the status code
        // must be able to tell "I created this" from "you already had it".
        StartOutcome::Replay(slot) => (StatusCode::OK, slot),
        StartOutcome::Busy(slot) => {
            return ApiError::busy(slot, git.host_instance()).into_response()
        }
    };
    let job = slot.view(git.host_instance());
    let mut response = (status, Json(JobResponse { job })).into_response();
    if status == StatusCode::ACCEPTED {
        // Built here rather than in the client, so the job id stays opaque.
        if let Ok(value) = HeaderValue::from_str(&format!("/api/git/jobs/{}", slot.id)) {
            response
                .headers_mut()
                .insert(axum::http::header::LOCATION, value);
        }
    }
    response
}

async fn post_init(State(state): State<HostState>, Path(id): Path<String>, raw: String) -> Response {
    let (git, body): (Arc<GitService>, InitBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let req = OpRequest {
        request_id: body.request_id,
        ..OpRequest::default()
    };
    start(&git, &id, crate::git::jobs::JobOp::Init, req)
}

async fn post_clone(
    State(state): State<HostState>,
    Path(id): Path<String>,
    raw: String,
) -> Response {
    let (git, body): (Arc<GitService>, CloneBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let req = OpRequest {
        request_id: body.request_id,
        credential: body.credential,
        ..OpRequest::default()
    };
    start(&git, &id, crate::git::jobs::JobOp::Clone, req)
}

async fn post_pull(State(state): State<HostState>, Path(id): Path<String>, raw: String) -> Response {
    let (git, body): (Arc<GitService>, PullBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let req = OpRequest {
        request_id: body.request_id,
        credential: body.credential,
        commit_local: body.commit_local,
        restart_children: body.restart_children,
        ..OpRequest::default()
    };
    start(&git, &id, crate::git::jobs::JobOp::Pull, req)
}

async fn post_push(State(state): State<HostState>, Path(id): Path<String>, raw: String) -> Response {
    let (git, body): (Arc<GitService>, PushBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let req = OpRequest {
        request_id: body.request_id,
        credential: body.credential,
        force: body.force,
        ..OpRequest::default()
    };
    start(&git, &id, crate::git::jobs::JobOp::Push, req)
}

async fn post_sync(State(state): State<HostState>, Path(id): Path<String>, raw: String) -> Response {
    let (git, body): (Arc<GitService>, SyncBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let req = OpRequest {
        request_id: body.request_id,
        credential: body.credential,
        message: body.message,
        author: body.author,
        allow_empty: body.allow_empty,
        push: body.push,
        force: body.force,
        restart_children: body.restart_children,
        ..OpRequest::default()
    };
    start(&git, &id, crate::git::jobs::JobOp::Sync, req)
}

async fn post_commit(
    State(state): State<HostState>,
    Path(id): Path<String>,
    raw: String,
) -> Response {
    let (git, body): (Arc<GitService>, CommitBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let req = OpRequest {
        request_id: body.request_id,
        message: body.message,
        author: body.author,
        paths: body.paths,
        allow_empty: body.allow_empty,
        ..OpRequest::default()
    };
    start(&git, &id, crate::git::jobs::JobOp::Commit, req)
}

async fn post_branch(
    State(state): State<HostState>,
    Path(id): Path<String>,
    raw: String,
) -> Response {
    let (git, body): (Arc<GitService>, BranchBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    if !crate::config::validate_branch_name(&body.name) {
        return fail(
            &git,
            GitError::invalid_request(
                "name",
                format!("{:?} is not a valid branch name", body.name),
            )
            .with_repo(&id),
        );
    }
    if !body.create && !body.checkout && body.upstream.is_none() {
        // Answering 202 here would hand back a job that provably does nothing, and the
        // caller would poll it to find out.
        return fail(
            &git,
            GitError::invalid_request(
                "create",
                "nothing to do: set create, checkout or upstream",
            )
            .with_repo(&id),
        );
    }
    let req = OpRequest {
        request_id: body.request_id,
        branch: Some(BranchRequest {
            name: body.name,
            create: body.create,
            from: body.from,
            checkout: body.checkout,
            upstream: body.upstream,
        }),
        ..OpRequest::default()
    };
    start(&git, &id, crate::git::jobs::JobOp::Branch, req)
}

async fn post_reset(
    State(state): State<HostState>,
    Path(id): Path<String>,
    raw: String,
) -> Response {
    let (git, body): (Arc<GitService>, ResetBody) = match prepare(&state, &id, &raw) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let req = OpRequest {
        request_id: body.request_id,
        reset: Some(ResetRequest {
            to: body.to.unwrap_or_else(|| "head".to_string()),
            clean_untracked: body.clean_untracked,
            confirm: body.confirm,
        }),
        ..OpRequest::default()
    };
    start(&git, &id, crate::git::jobs::JobOp::Reset, req)
}
```

- [ ] **Step 95: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app internal_server::tests`

Expected: PASS.

```
running 14 tests
test internal_server::tests::a_busy_repo_answers_409_with_the_running_job_embedded ... ok
test internal_server::tests::sync_returns_202_with_a_location_and_replays_a_repeated_request_id ... ok
test internal_server::tests::the_mutating_routes_refuse_impossible_requests ... ok
...
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 96: Commit**

```bash
git add src/git/api.rs src/internal_server.rs
git commit -m "feat(git): the eight mutating routes

202 plus Location for a new job, 200 for a replay, 409 with the running job
embedded and Retry-After for a conflict. A branch request that asks for
nothing is refused synchronously rather than handed back as a job that
provably does nothing and has to be polled to find that out."
```

---

- [ ] **Step 97: Write the failing tests for the job routes and the whole-table auth loop**

Append inside `mod tests` in `src/internal_server.rs`:

```rust
    #[tokio::test]
    async fn jobs_can_be_listed_filtered_and_fetched_by_id() {
        let (_state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        put_notes(port, json!({})).await;
        authed()
            .put(git(port, "/repos/other"))
            .header("x-host-token", "tok123")
            .json(&json!({}))
            .send()
            .await
            .unwrap();

        let first: serde_json::Value = post(port, "/repos/notes/init", json!({}))
            .await
            .json()
            .await
            .unwrap();
        let id = first["job"]["id"].as_str().unwrap().to_string();
        post(port, "/repos/other/init", json!({})).await;

        let all: serde_json::Value = authed()
            .get(git(port, "/jobs"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(all["jobs"].as_array().unwrap().len(), 2);
        // Newest first, so a caller reading the head of the list gets the latest.
        assert_eq!(all["jobs"][0]["repo_id"], "other");

        let filtered: serde_json::Value = authed()
            .get(git(port, "/jobs?repo_id=notes&limit=1"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(filtered["jobs"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["jobs"][0]["id"], id);

        let bad = authed()
            .get(git(port, "/jobs?state=sleeping"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status(), 422);

        let one: serde_json::Value = authed()
            .get(git(port, &format!("/jobs/{id}")))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(one["job"]["id"], id);
        assert_eq!(one["job"]["op"], "init");
    }

    #[tokio::test]
    async fn an_unknown_job_id_404s_and_names_the_host_instance() {
        let (state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        let r = authed()
            .get(git(port, "/jobs/job_deadbeef_000001"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["code"], "job_not_found");
        // The published recipe: a *different* host_instance means the host restarted and
        // the caller should read `last_sync` rather than retry the poll.
        assert_eq!(
            body["error"]["host_instance"],
            state.git.as_ref().unwrap().host_instance()
        );
    }

    #[tokio::test]
    async fn no_route_under_api_git_answers_without_the_token() {
        let (_state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        put_notes(port, json!({})).await;
        let client = reqwest::Client::new();
        for (method, path) in GIT_ROUTES {
            let r = client
                .request(method.parse().unwrap(), format!("http://127.0.0.1:{port}{path}"))
                .json(&json!({}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 401, "{method} {path} answered without a token");
            let body: serde_json::Value = r.json().await.unwrap();
            assert_eq!(body["error"]["code"], "unauthorized", "{method} {path}");
            // A wrong token is the same answer as no token.
            let r = client
                .request(method.parse().unwrap(), format!("http://127.0.0.1:{port}{path}"))
                .header("x-host-token", "not-the-token")
                .json(&json!({}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 401, "{method} {path} accepted a wrong token");
        }
    }
```

- [ ] **Step 98: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app internal_server::tests::jobs_can_be_listed`

Expected: FAIL — `/jobs` is not routed, so it falls through to a plain 404 and the
whole-table loop fails on the last two rows for the same reason.

```
running 1 test
test internal_server::tests::jobs_can_be_listed_filtered_and_fetched_by_id ... FAILED

---- internal_server::tests::jobs_can_be_listed_filtered_and_fetched_by_id stdout ----
thread 'internal_server::tests::…' panicked at src/internal_server.rs:NN:9:
called `Result::unwrap()` on an `Err` value: reqwest::Error { kind: Decode,
source: Error("EOF while parsing a value", line: 1, column: 0) }
```

- [ ] **Step 99: Implement the two job routes**

Extend the route table in `src/git/api.rs` — every new `.route(...)` goes **before** the
`.route_layer(...)` line. A route chained after the layer is not covered by it, which is
the exact failure mode the single-layer design exists to prevent:

```rust
        .route("/jobs", get(list_jobs))
        .route("/jobs/{job_id}", get(get_job))
```

and add the handlers:

```rust
async fn list_jobs(State(state): State<HostState>, Query(query): Query<JobsQuery>) -> Response {
    let git = match service(&state) {
        Ok(git) => git,
        Err(response) => return response,
    };
    let filter = match job_filter(query) {
        Ok(filter) => filter,
        Err(e) => return fail(&git, e),
    };
    let jobs = git
        .jobs()
        .list(&filter)
        .iter()
        .map(|slot| slot.view(git.host_instance()))
        .collect();
    Json(JobsResponse { jobs }).into_response()
}

async fn get_job(State(state): State<HostState>, Path(job_id): Path<String>) -> Response {
    let git = match service(&state) {
        Ok(git) => git,
        Err(response) => return response,
    };
    match git.jobs().lookup(&job_id) {
        // A failed job is still a 200: the failure lives in `job.error`, so a client
        // writes one `match` over `code` for both surfaces rather than two.
        Ok(slot) => Json(JobResponse {
            job: slot.view(git.host_instance()),
        })
        .into_response(),
        // `lookup` rejects a foreign instance prefix outright, so a surviving child can
        // never be answered about a different job that shares a sequence number.
        Err(e) => fail(&git, e),
    }
}
```

- [ ] **Step 100: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app internal_server::tests`

Expected: PASS.

```
running 17 tests
test internal_server::tests::an_unknown_job_id_404s_and_names_the_host_instance ... ok
test internal_server::tests::every_git_route_is_git_disabled_without_the_section ... ok
test internal_server::tests::jobs_can_be_listed_filtered_and_fetched_by_id ... ok
test internal_server::tests::no_route_under_api_git_answers_without_the_token ... ok
...
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 101: Commit**

```bash
git add src/git/api.rs src/internal_server.rs
git commit -m "feat(git): the two job routes and the whole-table auth proof

A failed job still answers 200 — the failure lives in job.error, so a client
writes one match over `code` for both the synchronous and the asynchronous
surface. The auth test loops the entire 17-row route table twice, once with
no token and once with a wrong one, so a route added later without the layer
fails the suite."
```

---

- [ ] **Step 102: Audit the invariants this task is responsible for**

Run each of these and read the output; every one is a one-line claim this task makes.

Every one of these invariants is *stated in a doc comment in the very file it constrains*
— `api.rs:3` says "No `git2` type crosses this file", `ops.rs:10` and `jobs.rs:18` name
`tokio` in order to forbid it, `creds.rs:436` explains the `.expose()` count. A bare
`grep` therefore matches its own documentation and can never report the number the audit
claims. Each command below filters out whole-line comments (`^path:NN: *//`) so it
measures code rather than prose. Measured on the task-9 tree: item 1 → no code hit
(`api.rs:3` is the doc comment), item 4 → no code hit (`ops.rs:10,281,1731` and
`jobs.rs:5,7,18` are all comments), item 5 → 4.

```bash
nc() { grep -v ':[0-9]*: *//'; }   # drop whole-line comments

# 1. No git2 type crosses api.rs.
! grep -Hn 'git2' src/git/api.rs | nc | grep . && echo "OK: api.rs names no git2"

# 2. No rt.enter() was added anywhere. The only pre-existing hits are in host.rs,
#    around tokio::process::Command::spawn. (`gate.enter()` in jobs.rs is a test
#    barrier, not a runtime guard.)
grep -rn 'rt.enter()\|\.enter()' src/ | nc || echo "none"

# 3. Nothing under src/git/ touches the host's process or window machinery.
#    `HostEvent::GitRestartChildren` is the one sanctioned channel and is expected.
! grep -rn 'tokio::process\|chrome_generation\|server_generation\|Children' src/git/ \
  | nc | grep -v 'HostEvent::GitRestartChildren' | grep . \
  && echo "OK: src/git/ holds no host machinery"

# 4. The leaf modules still name no axum and no tokio.
! grep -rn 'axum\|tokio' src/git/ops.rs src/git/merge.rs src/git/creds.rs \
  src/git/jobs.rs src/git/registry.rs src/git/state.rs src/git/secret.rs src/git/util.rs \
  | nc | grep . && echo "OK: leaf modules are runtime-free"
# (src/git/error.rs is expected to name axum: it owns http_status() and IntoResponse.)

# 5. Exactly four `.expose()` call sites in production code, all in creds::callbacks.
#    secret.rs's own unit test calls it too; that is the accessor testing itself.
grep -rn '\.expose()' src/ | nc | grep -v '^src/git/secret.rs:' | tee /dev/stderr | wc -l
```

Item 5 must print `4`; anything else means a secret escaped `creds.rs` and the audit in
§8.5 is no longer true.

- [ ] **Step 103: Run the whole gate and commit the audit fixes, if any**

```bash
cargo fmt
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --bin chrome-host-app
```

Expected: `cargo fmt --check` silent, clippy silent, and the full suite green — the
pre-existing `config`, `settings`, `supervisor`, `tray`, `chrome` and `internal_server`
tests plus the ~40 added by this task.

```
test result: ok. NNN passed; 0 failed; 1 ignored; 0 measured; 0 filtered out
```

The one ignored test is the pre-existing `chrome::tests::real_chrome_reports_version`.

If `cargo fmt` rewrote anything:

```bash
git add -A
git commit -m "style(git): cargo fmt after the api and service landing"
```

**Definition of done for this task:** `/api/git/*` answers `404 git_disabled` when `[git]`
is absent; no route answers without the token; `GET /api/status` is byte-identical to what
it was (task 10 adds the `git` key); `cargo test`, `cargo fmt --check` and
`cargo clippy --all-targets -- -D warnings` are all clean.
