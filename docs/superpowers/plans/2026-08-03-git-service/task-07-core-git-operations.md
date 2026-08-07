### Task 7: Core git operations

The eight verbs and reads that need no network and no merge: `init`, `clone`, `commit`,
`status`, `branches`, `branch`, `reset`, `purge`, plus the shared preamble (`open_tree`,
`require_branch`, `ensure_branch`, `stage_and_commit`) they all sit on. Everything lands in
`src/git/ops.rs`, which task 6 has already created with the context layer (`OpCtx`,
`OpRequest`, `OpOutcome`, `Identity`, `OpScratch`) and with a `pub fn run` that already
dispatches every `JobOp` to a stub of the same name, each returning
`Err(GitError::internal("<verb> is not implemented yet"))`. Five of those stubs are **replaced
in place** here; `run` itself is not touched. One extra function lands in `src/git/mod.rs`:
the process-global libgit2 timeout setter from §7.1.

**No `tokio`. No `axum`. No `App`.** This file is a synchronous library over libgit2; the
whole point is that it is callable from a `spawn_blocking` closure and testable without a
runtime, a server, or a socket.

Six details in here are not stylistic — each one is a bug that ships silently if you skip it:

1. **`RepositoryInitOptions::initial_head` must be pinned.** An unpinned `Repository::init`
   honours the developer's global `init.defaultBranch`, so it produces
   `refs/heads/master` on roughly half of all machines. Every later refspec is built from
   `refs/heads/<def.branch>` and silently matches nothing. The test fixture points libgit2's
   global config search path at a scratch `.gitconfig` that says
   `init.defaultBranch = master`, so *every test in this file* runs on a hostile machine and
   the pinning is proven rather than assumed.
2. **`RepoBuilder`, never the free `Repository::clone` function.** The free function takes no
   callbacks and therefore cannot authenticate. It works fine against the local-path remote
   in the fixture and fails against every real one.
3. **`git2::Signature::now`, never `repo.signature()`.** The latter is verified to fail with
   `code=NotFound class=Config "config value 'user.name' was not found"` on any machine that
   has never configured git — which is most machines this template ships to.
4. **`index.add_all(["*"], IndexAddOption::DEFAULT)`.** `DEFAULT` honours `.gitignore`;
   `IndexAddOption::FORCE` is never used anywhere in this file, because a generic executor
   that bypasses `.gitignore` eventually commits somebody's `.env`.
5. **`checkout_tree` BEFORE `set_head`**, and a hard `reset` ignores any `CheckoutBuilder`
   you hand it (libgit2 hard-codes `GIT_CHECKOUT_FORCE` in `reset.c`), so untracked cleanup
   has to be a separate `checkout_head` call.
6. **`repo.state()` is checked on both surfaces, through one function.** `require_clean_state`
   is called by `open_tree`, so every mutating verb refuses a repository a human left mid-merge,
   mid-rebase or mid-cherry-pick with `409 dirty_tree`; and by `fill_status`, so `status` reports
   the same condition as `state: "error"` with the same message. This is the other half of §6.6's
   decision that startup runs no `cleanup_state()` and no `reset --hard`: our own merge is pure
   (§7.6) and never leaves `RepositoryState::Merge`, so a `MERGE_HEAD` in one of these trees was
   put there by a human running git by hand, and discarding their staged resolution would be
   unrecoverable. `reset` is the single verb that may open such a tree — through
   `open_tree_any_state` — because `git_reset` ends in `git_repository_state_cleanup`, and the
   refusal message hands the caller that exact request body.

   > **Amended by the post-execution audit (F7).** This decision originally covered `status`
   > only, and its own wording gave the game away: "the repo reads back as `clean` while every
   > mutating verb fails for a reason the caller cannot see" describes a refusal that did not
   > exist. Nothing on the write path read `repo.state()`, so a `sync` on a wedged tree returned
   > `Ok`/`"merged"` — measured — after `add_all` staged the `<<<<<<<` markers as resolved
   > content, the commit recorded the merged branch nowhere, `cleanup_state()` deleted the
   > human's `MERGE_HEAD`, and `push` published all of it. `open_tree` is split below and
   > `fill_status`'s inlined error becomes a call to the shared function.
   >
   > Known gap, left open: `ops::clone`'s idempotent branch and `ops::init`'s `Exists` branch
   > open the repository directly rather than through `open_tree`, so they still answer
   > `up_to_date`/`initialized` on a wedged tree. Neither writes to it, so this is a consistency
   > defect rather than corruption — but `open_tree`'s doc comment claims the gate is exhaustive
   > and it is not.

Three git2 0.21 API facts the design document's sketches predate. Use these, not the sketch:

- `Reference::shorthand()` returns `Result<&str, Error>`, not `Option<&str>`.
- `Reference::symbolic_target()` returns `Result<Option<&str>, Error>`.
- **`Remote::url()` returns `Result<&str, Error>`, not `Option<&str>`.** The §7.2 sketch's
  `r.url() != Some(url)` does not compile against 0.21; it must be `r.url().ok() != Some(url)`.

---

**Files:**
- Modify: `src/git/ops.rs` — append every genuinely new item (`MAX_DIRTY_FILES`, `head_oid`,
  `open_tree`, `require_branch`, `ensure_branch`, `stage_and_commit`, the read-path types,
  `status`, `fill_status`, `dirty_summary`, `branches`, `purge_tree`) after `pub fn run`, and
  add the tests **inside** the `#[cfg(test)] mod tests` block task 6 already left at the bottom
  of the file. The five verbs `init`, `clone`, `commit`, `branch` and `reset` are **replacements
  in place** of task 6's Step 48 stubs of the same names, never appends — appending one is
  `error[E0428]: the name 'init' is defined multiple times`. `pub fn run` itself needs no edit
  at all (Step 52); the one other edit to task 6's code is trimming its dispatch scaffolding
  test in Step 10.
- Modify: `src/git/mod.rs` — add `pub const CONNECT_TIMEOUT_MS` and
  `pub fn init_libgit2_timeouts`, plus one test inside the existing `#[cfg(test)] mod tests`.

**Interfaces:**

- Consumes — from task 3 (`error.rs`, `util.rs`):

  ```rust
  pub struct GitError { pub code: GitErrorCode, pub message: String, /* … */ }   // Debug + Clone
  impl GitError {
      pub fn new(code: GitErrorCode, message: impl Into<String>) -> GitError;
      pub fn code(&self) -> GitErrorCode;
      pub fn internal(message: impl Into<String>) -> GitError;
      pub fn invalid_request(field: &str, message: impl Into<String>) -> GitError;
      pub fn io(message: impl Into<String>) -> GitError;
      pub fn no_worktree(id: &str) -> GitError;
      pub fn not_a_repository(path: &std::path::Path) -> GitError;
      pub fn path_refused(path: &std::path::Path, why: &str) -> GitError;
      pub fn remote_missing() -> GitError;
      pub fn confirm_required() -> GitError;
      pub fn detached_head() -> GitError;
      pub fn branch_mismatch(actual: &str, expected: &str) -> GitError;
      pub fn as_dirty_tree_if_conflict(self) -> GitError;   // MergeUnresolvable -> DirtyTree
  }
  pub enum GitErrorCode { /* ... */ }        // Copy + PartialEq
  impl From<git2::Error> for GitError;       // classify(e, AbortReason::None) + Git2Hint
  impl From<std::io::Error> for GitError;    // IoFailed
  pub fn now_ms() -> u64;
  pub fn utc_stamp(ms: u64) -> String;
  pub fn bytes_to_path(b: &[u8]) -> Result<std::path::PathBuf, GitError>;   // cfg unix + windows
  ```

  Relevant `classify` rows this task relies on: `(ErrorCode::Exists, _) => AlreadyExists`
  (a branch-name clash), `(ErrorCode::Conflict, _) => MergeUnresolvable` (a `safe()` checkout
  that would clobber local edits — re-labelled by `as_dirty_tree_if_conflict`),
  `(ErrorCode::UnbornBranch, _) => UnbornBranch`, `(ErrorCode::NotFound, ErrorClass::Repository)
  => NoWorktree`.

- Consumes — from task 4 (`registry.rs`, `state.rs`): `RepoDef` (every field `pub`),
  `AuthorSpec`, `LastSync`. From task 5 (`jobs.rs`): `JobOp`, `Phase`, `JobSlot` (with
  `pub abort: Arc<AbortFlag>`), `JobRef`, `JobStore`, `Admission`, `RepoLease`.
  From task 2 (`config.rs`): `SshHostKeyPolicy`.

- Consumes — from task 6 (`creds.rs`, and the context half of `ops.rs`):

  ```rust
  pub fn callbacks<'a>(ctx: &'a OpCtx) -> git2::RemoteCallbacks<'a>;
  pub enum ResolvedCred { None, Token{..}, SshPath{..}, SshMem{..} }
  impl HostKeyChecker { pub fn new(policy: SshHostKeyPolicy, pinned: Option<String>) -> HostKeyChecker; }

  pub struct Identity { pub app_name: String, pub identifier: String, pub hostname: String }
  pub struct BranchRequest { pub name: String, pub create: bool, pub from: Option<String>,
                             pub checkout: bool, pub upstream: Option<Option<String>> }
  pub struct ResetRequest { pub to: String, pub clean_untracked: bool, pub confirm: bool }
  pub struct OpRequest { /* ... */ }  impl Default for OpRequest;
  pub struct OpScratch;  impl Default for OpScratch;    // task 6's refinement of §4.9
  pub struct OpCtx { /* every field pub, per contract §4.9, plus `pub scratch: OpScratch`.
                       Task 6 folded the contract's three private Cell/RefCell fields
                       (`cred_attempts`, `cred_failure`, `push_rejections`) into that one
                       public field, whose own fields stay private, so an `OpCtx` remains
                       constructible by struct literal without a twelve-argument
                       constructor. The fixture below builds one with
                       `scratch: OpScratch::default()`; tasks 8, 9 and 11 do the same. */ }
  impl OpCtx {
      pub fn phase(&self, p: Phase);
      pub fn author_name(&self) -> String;
      pub fn author_email(&self) -> String;
      pub fn classify_err(&self, e: &git2::Error) -> GitError;
      pub fn learned_fingerprint(&self) -> Option<String>;
  }
  pub struct OpOutcome { /* all fields pub, per contract §4.9 */ }
  impl OpOutcome { pub fn new(outcome: &'static str, branch: &str) -> OpOutcome; }
  pub fn run(op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError>;
  // …and the eight stubs `run` already dispatches to, each `pub fn <verb>(_ctx: &OpCtx)`
  // returning `Err(GitError::internal("<verb> is not implemented yet"))`:
  //   init  clone  pull  push  sync  commit  branch  reset
  // This task REPLACES five of them in place. It does not append, and it does not touch `run`.
  ```

  `src/git/ops.rs` already carries this `use` block from task 6 — verify with
  `rg '^use' src/git/ops.rs`. **Task 7 adds no new `use` line**; everything else it names is
  written with a full path (`crate::git::util::bytes_to_path`, `crate::git::state::LastSync`,
  `crate::git::jobs::JobRef`, `git2::…`). If one of these is missing, add it; never add a
  duplicate, which is `error[E0252]`.

  ```rust
  use crate::git::creds::{HostKeyChecker, ResolvedCred};
  use crate::git::error::{GitError, GitErrorCode};
  use crate::git::jobs::{AbortFlag, JobOp, JobSlot, Phase};
  use crate::git::registry::{AuthorSpec, CredentialSpec, RepoDef, Warning};
  use serde::Serialize;
  use std::path::PathBuf;
  ```

  Also relied on: the two module-wide allows task 3 put at the top of `src/git/mod.rs` —
  `#![allow(dead_code)]` (this task publishes ten items whose only consumers are tasks 8 and
  9) and `#![allow(clippy::result_large_err)]` (every signature here is
  `Result<_, GitError>`).

- Produces — tasks 8, 9 and 11 rely on exactly these:

  ```rust
  // src/git/ops.rs
  pub const MAX_DIRTY_FILES: usize = 200;

  pub fn open_tree(ctx: &OpCtx) -> Result<git2::Repository, GitError>;
  pub fn open_tree_any_state(ctx: &OpCtx) -> Result<git2::Repository, GitError>;  // reset only
  pub fn require_branch(repo: &git2::Repository, branch: &str) -> Result<(), GitError>;
  pub fn ensure_branch(repo: &git2::Repository, def: &RepoDef) -> Result<(), GitError>;
  pub fn stage_and_commit(repo: &git2::Repository, ctx: &OpCtx)
      -> Result<(Option<git2::Oid>, usize), GitError>;
  pub fn init(ctx: &OpCtx) -> Result<OpOutcome, GitError>;
  pub fn clone(ctx: &OpCtx) -> Result<OpOutcome, GitError>;
  pub fn commit(ctx: &OpCtx) -> Result<OpOutcome, GitError>;
  pub fn branch(ctx: &OpCtx) -> Result<OpOutcome, GitError>;
  pub fn reset(ctx: &OpCtx) -> Result<OpOutcome, GitError>;
  pub fn status(ctx: &ReadCtx) -> RepoStatus;                 // never fails
  pub fn branches(tree: &std::path::Path, def: &RepoDef) -> Result<BranchesResponse, GitError>;
  pub fn purge_tree(repos_root_canon: &std::path::Path, tree: &std::path::Path)
      -> Result<(), GitError>;

  pub struct ReadCtx { pub tree, pub def, pub last_sync, pub busy_job }
  pub struct DirtySummary { pub modified, added, deleted, renamed, untracked: Vec<String>,
                            pub count: usize, pub truncated: bool }   // + Default
  pub struct RemoteInfo { pub name: String, pub url: Option<String> }
  pub struct AutoInfo { pub auto_sync_secs: Option<u64>, pub sync_on_start: bool,
                        pub sync_on_quit: bool }
  pub struct RepoStatus { /* contract §4.10 */ }
  pub struct BranchesResponse { pub current, pub detached, pub local, pub remote, pub upstream }

  // private, same file — task 8 may reuse it without re-declaring it
  fn head_oid(repo: &git2::Repository) -> Option<String>;

  // src/git/mod.rs
  pub const CONNECT_TIMEOUT_MS: i32 = 10_000;
  pub fn init_libgit2_timeouts(network_timeout_secs: u64) -> Result<(), GitError>;
  ```

  **Task 9 must not re-declare `CONNECT_TIMEOUT_MS` or `init_libgit2_timeouts`.** Contract §2
  and §5.10 assign both to `git/mod.rs` and to task 9; this task lands them early because the
  §7.1 reasoning is inseparable from the ops layer. Task 9's only remaining job for them is one
  call from `GitService::start`.

  Task 8 also gets, from the test module: `Fixture`, `commit_file`, `push_branch`,
  `repo_def`, `Op`, `hostile_global_config` — the merge matrix in §10.3 is written against
  this fixture and must not fork a second one.

**Two decisions this task makes where the design document contradicts itself:**

1. **Cloning into a directory that already holds a repository succeeds** with
   `outcome == "up_to_date"`. §7.4's code is explicit (`Ok(OpOutcome::already_cloned())`)
   while §10.3's prose says `already_exists`; the code wins, because a `POST /clone` retry
   after a dropped response must not turn into an error. A populated directory that is *not*
   a repository is still `not_a_repository`. §3.5 has no `already_cloned` string, so the
   outcome is `up_to_date`, and `already_exists` keeps its one real producer: a branch-name
   clash in `branch` (§7.9).
2. **`ensure_branch` returns early on an empty clone.** §7.4's sketch falls through to
   `repo.head()?`, which on a clone of an empty remote fails with `UnbornBranch` — so cloning
   a brand-new empty remote would always fail. The fix is three lines and has its own test.

---

- [ ] **Step 1: Write the failing tests — the fixture, `open_tree`, `require_branch`**

Everything below goes **inside** the `#[cfg(test)] mod tests { … }` block that task 6 left at
the bottom of `src/git/ops.rs`, appended after its existing contents. If that module already
has `use super::*;`, do not add a second one. If task 6 already defined a helper with one of
these names, reuse it rather than duplicating it.

```rust
    use crate::config::SshHostKeyPolicy;
    use crate::git::jobs::{Admission, JobStore, RepoLease};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, OnceLock};
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    /// Points libgit2's global config search path at a scratch `.gitconfig` that sets
    /// `init.defaultBranch = master`, and its system/XDG/ProgramData search paths at the
    /// same (otherwise empty) directory.
    ///
    /// Two jobs, both load-bearing. It makes every test in this file run on a machine
    /// whose git default *disagrees* with `[git].default_branch`, which is the only way
    /// to prove `RepositoryInitOptions::initial_head` is doing the work rather than the
    /// ambient default happening to agree. And it stops the developer's real
    /// `~/.gitconfig` — `init.defaultBranch`, `core.autocrlf`, `commit.gpgsign`, a global
    /// `core.excludesFile` — from deciding whether these tests pass.
    ///
    /// **Amended by the post-execution audit.** Its original SAFETY comment argued the
    /// `OnceLock` was sufficient synchronisation because "every test enters through
    /// `Fixture::*` before it touches git2". That premise was false the moment task 8 added
    /// a second test module with its own fixtures, and the resulting race is measured and
    /// recorded in the plan index. Task 8 moves this function into `merge::testkit` and
    /// makes `Fixture` call it there — a second `OnceLock` would be a second, unordered
    /// mutation of the same global.
    fn hostile_global_config() {
        // The search path is process-global and must outlive every test, so the directory
        // is deliberately never dropped: a static is not dropped at process exit. It is
        // one 30-byte file in the OS temp dir.
        static CONFIG_HOME: OnceLock<TempDir> = OnceLock::new();
        CONFIG_HOME.get_or_init(|| {
            let dir = tempfile::tempdir().expect("config tempdir");
            std::fs::write(
                dir.path().join(".gitconfig"),
                "[init]\n\tdefaultBranch = master\n",
            )
            .expect("write .gitconfig");
            // SAFETY: `set_search_path` mutates libgit2 process-global state behind no
            // lock of its own, so it is sound only if it happens-before every libgit2 call
            // in the process — including calls on the other threads cargo is running tests
            // on right now.
            //
            // `get_or_init` supplies exactly that edge, and nothing more. The thread that
            // runs this closure finishes all four writes before any other thread returns
            // from `get_or_init`, and the writes never happen again. So the ordering holds
            // for a thread if and only if that thread calls `hostile_global_config` BEFORE
            // its own first libgit2 call. It buys nothing for a thread already inside
            // libgit2 — which is why this must live in the one module every test reaches
            // git2 through, rather than in one module's private fixture. Task 8 hoists it
            // into `merge::testkit` for that reason; see the note below.
            //
            // The cost of missing it is silent rather than loud: `git_repository_is_empty`
            // compares HEAD's symbolic target against `git_repository_initialbranch`,
            // which reads `init.defaultBranch` back through this very search path. Flip
            // the path between a clone's `git_repository_init` and its emptiness check and
            // the fresh repository stops looking empty — the clone dies with "the
            // repository is not empty" in a test that never mentioned config at all.
            unsafe {
                for level in [
                    git2::ConfigLevel::Global,
                    git2::ConfigLevel::XDG,
                    git2::ConfigLevel::System,
                    git2::ConfigLevel::ProgramData,
                ] {
                    git2::opts::set_search_path(level, dir.path()).expect("set search path");
                }
            }
            dir
        });
    }

    /// A `RepoDef` with the shape `Registry::put` guarantees: non-empty branch, non-zero
    /// timestamps, `origin` as the remote name.
    fn repo_def(id: &str, branch: &str, remote: Option<String>) -> RepoDef {
        RepoDef {
            id: id.to_string(),
            remote,
            remote_name: "origin".to_string(),
            branch: branch.to_string(),
            credential: None,
            author: None,
            sync_settings: false,
            settings_path: "settings.json".to_string(),
            auto_sync_secs: None,
            sync_on_start: false,
            sync_on_quit: false,
            restart_children_on_pull: false,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    /// A bare "remote" plus a repos root, so every clone/push/diverge scenario is
    /// reproducible with no network, no credential, and no running server.
    ///
    /// Remotes are **plain absolute paths**, never `file://` URLs: libgit2 accepts the
    /// path form on every platform and it sidesteps the Windows `file:///C:/…`
    /// drive-letter question entirely.
    ///
    /// Note for task 8: any test that asserts on transfer progress or on abort must set
    /// `RepoBuilder::clone_local(git2::build::CloneLocal::None)`. The default local-clone
    /// path bypasses the transport and fires `transfer_progress` **zero** times, so such a
    /// test would otherwise assert on nothing. The local transport also does not run
    /// server hooks, so server-side push rejection cannot be unit-tested this way.
    struct Fixture {
        root: TempDir,
        repos_root: PathBuf,
        origin: PathBuf,
    }

    impl Fixture {
        fn new() -> Fixture {
            Fixture::with_origin_head("master")
        }

        fn with_origin_head(head: &str) -> Fixture {
            hostile_global_config();
            let root = tempfile::tempdir().expect("tempdir");
            let repos_root = root.path().join("repos");
            std::fs::create_dir_all(&repos_root).expect("repos root");
            let origin = root.path().join("origin.git");
            let mut opts = git2::RepositoryInitOptions::new();
            opts.bare(true).initial_head(head);
            git2::Repository::init_opts(&origin, &opts).expect("bare origin");
            Fixture {
                root,
                repos_root,
                origin,
            }
        }

        fn scratch(&self, name: &str) -> PathBuf {
            self.root.path().join(name)
        }

        fn tree(&self, id: &str) -> PathBuf {
            self.repos_root.join(id)
        }

        fn repos_root_canon(&self) -> PathBuf {
            // macOS puts temp dirs under a symlinked /var, so the containment check in
            // `purge_tree` only means anything against a canonicalized root.
            std::fs::canonicalize(&self.repos_root).expect("repos root exists")
        }

        fn remote_url(&self) -> String {
            self.origin
                .to_str()
                .expect("temp paths are utf-8")
                .to_string()
        }

        fn local_def(&self, id: &str, branch: &str) -> RepoDef {
            repo_def(id, branch, None)
        }

        fn remote_def(&self, id: &str, branch: &str) -> RepoDef {
            repo_def(id, branch, Some(self.remote_url()))
        }

        /// A throw-away clone standing in for someone else's machine.
        ///
        /// The free `git2::Repository::clone` is acceptable here and only here: a
        /// local-path remote needs no callbacks. Production code must use `RepoBuilder`
        /// (see `ops::clone`), which is the only form that can authenticate.
        fn worker(&self, name: &str) -> git2::Repository {
            git2::Repository::clone(&self.remote_url(), self.scratch(name))
                .expect("worker clone")
        }

        /// Publish `files` on `branch` in the bare origin, one commit per file.
        fn seed(&self, branch: &str, files: &[(&str, &str)]) -> git2::Repository {
            let repo = self.worker(&format!("seed-{branch}"));
            // A clone of an empty bare repo has an unborn HEAD whose name comes from the
            // remote's advertised default; pin it so the seed lands on `branch` exactly.
            if repo.is_empty().expect("is_empty") {
                repo.set_head(&format!("refs/heads/{branch}"))
                    .expect("set head");
            }
            for (name, body) in files {
                commit_file(&repo, name, body, &format!("add {name}"));
            }
            push_branch(&repo, branch);
            repo
        }

        fn op(&self, def: RepoDef, kind: JobOp, request: OpRequest) -> Op {
            let store = JobStore::new("0badcafe".to_string());
            let (slot, lease) = match store
                .admit(&def.id, kind, None)
                .expect("a fresh store is never draining")
            {
                Admission::Started(slot, lease) => (slot, lease),
                _ => panic!("a fresh store must admit its first job"),
            };
            let ctx = OpCtx {
                tree: self.tree(&def.id),
                def,
                cred: ResolvedCred::None,
                cred_unbound: false,
                request,
                identity: Identity {
                    app_name: "Test App".to_string(),
                    identifier: "com.example.test".to_string(),
                    hostname: "test-host".to_string(),
                },
                host_key: HostKeyChecker::new(SshHostKeyPolicy::Tofu, None),
                abort: slot.abort.clone(),
                slot,
                deadline: Instant::now() + Duration::from_secs(60),
                settings: None,
                default_author: AuthorSpec {
                    name: Some("Host App".to_string()),
                    email: Some("host@example.invalid".to_string()),
                },
                // Task 6's one public catch-all for the credential-attempt counter, the
                // recorded credential failure and the drained push rejections.
                scratch: OpScratch::default(),
            };
            Op { ctx, _lease: lease }
        }
    }

    /// Holds the job lease for as long as the context is alive. Dropping the lease
    /// finishes the job, and `OpCtx::phase` would then be writing phases into a terminal
    /// slot — a state the production code never produces and the tests must not either.
    struct Op {
        ctx: OpCtx,
        _lease: RepoLease,
    }

    impl std::ops::Deref for Op {
        type Target = OpCtx;
        fn deref(&self) -> &OpCtx {
            &self.ctx
        }
    }

    fn commit_file(repo: &git2::Repository, name: &str, body: &str, message: &str) -> git2::Oid {
        let workdir = repo.workdir().expect("not a bare repo");
        let path = workdir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, body).expect("write file");
        let mut index = repo.index().expect("index");
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .expect("add_all");
        index.write().expect("index write");
        let tree_oid = index.write_tree().expect("write_tree");
        let tree = repo.find_tree(tree_oid).expect("find_tree");
        let sig = git2::Signature::now("Worker", "worker@example.invalid").expect("signature");
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .expect("commit")
    }

    fn push_branch(repo: &git2::Repository, branch: &str) {
        let mut remote = repo.find_remote("origin").expect("origin remote");
        let spec = format!("refs/heads/{branch}:refs/heads/{branch}");
        remote.push(&[spec.as_str()], None).expect("push");
    }

    fn open(path: &Path) -> git2::Repository {
        git2::Repository::open(path).expect("open repository")
    }

    #[test]
    fn open_tree_refuses_a_missing_worktree() {
        let fx = Fixture::new();
        let op = fx.op(fx.local_def("notes", "main"), JobOp::Commit, OpRequest::default());
        let err = open_tree(&op).expect_err("nothing has been cloned yet");
        assert_eq!(err.code(), GitErrorCode::NoWorktree);
    }

    #[test]
    fn open_tree_repoints_a_changed_remote_url() {
        let fx = Fixture::new();
        let stale = fx.scratch("stale.git");
        let init = fx.op(
            repo_def("notes", "main", Some(stale.to_string_lossy().into_owned())),
            JobOp::Init,
            OpRequest::default(),
        );
        std::fs::create_dir_all(fx.tree("notes")).expect("mkdir");
        git2::Repository::init(fx.tree("notes")).expect("init");
        open_tree(&init).expect("first open registers the stale url");

        // The registry was edited between operations; the next op must not keep pushing
        // at the old address.
        let moved = fx.op(fx.remote_def("notes", "main"), JobOp::Commit, OpRequest::default());
        let repo = open_tree(&moved).expect("open");
        assert_eq!(
            repo.find_remote("origin").expect("origin").url().ok(),
            Some(fx.remote_url().as_str())
        );
    }

    #[test]
    fn open_tree_adds_a_remote_that_is_not_there_yet() {
        let fx = Fixture::new();
        std::fs::create_dir_all(fx.tree("notes")).expect("mkdir");
        git2::Repository::init(fx.tree("notes")).expect("init");
        let op = fx.op(fx.remote_def("notes", "main"), JobOp::Commit, OpRequest::default());
        let repo = open_tree(&op).expect("open");
        assert_eq!(
            repo.find_remote("origin").expect("origin").url().ok(),
            Some(fx.remote_url().as_str())
        );
    }

    #[test]
    fn require_branch_allows_an_unborn_head() {
        let fx = Fixture::new();
        std::fs::create_dir_all(fx.tree("notes")).expect("mkdir");
        let repo = git2::Repository::init(fx.tree("notes")).expect("init");
        // Nothing is committed, so there is no branch to disagree with.
        require_branch(&repo, "main").expect("an unborn head is not a mismatch");
    }

    #[test]
    fn require_branch_reports_a_branch_mismatch() {
        let fx = Fixture::new();
        std::fs::create_dir_all(fx.tree("notes")).expect("mkdir");
        let repo = git2::Repository::init(fx.tree("notes")).expect("init");
        commit_file(&repo, "f.md", "one\n", "one");
        let err = require_branch(&repo, "main").expect_err("HEAD is on master");
        assert_eq!(err.code(), GitErrorCode::BranchMismatch);
    }

    #[test]
    fn require_branch_reports_a_detached_head() {
        let fx = Fixture::new();
        std::fs::create_dir_all(fx.tree("notes")).expect("mkdir");
        let repo = git2::Repository::init(fx.tree("notes")).expect("init");
        let oid = commit_file(&repo, "f.md", "one\n", "one");
        repo.set_head_detached(oid).expect("detach");
        let err = require_branch(&repo, "master").expect_err("HEAD is detached");
        assert_eq!(err.code(), GitErrorCode::DetachedHead);
    }
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::ops::tests::open_tree`

Expected: FAIL — the file does not compile:

```
error[E0425]: cannot find function `open_tree` in this scope
   --> src/git/ops.rs:NNN:19
    |
NNN |         let err = open_tree(&op).expect_err("nothing has been cloned yet");
    |                   ^^^^^^^^^ not found in this scope

error[E0425]: cannot find function `require_branch` in this scope
   --> src/git/ops.rs:NNN:9
    |
NNN |         require_branch(&repo, "main").expect("an unborn head is not a mismatch");
    |         ^^^^^^^^^^^^^^ not found in this scope

error: could not compile `hitch` (bin "hitch" test) due to 8 previous errors
```

- [ ] **Step 3: Implement the shared preamble**

Append to `src/git/ops.rs`, below `pub fn run` and the eight verb stubs task 6 left under it,
still above the `#[cfg(test)]` line. Every "append" in this task means that same spot; the five
verbs are the exception and are replaced where they already sit.

> **Amended by the post-execution audit (second round).** `open_tree` used to be
> `open_tree_any_state` + `require_clean_state`, in that order, so the reconciliation ran
> *before* the gate — and a verb refused with `dirty_tree` had already rewritten
> `.git/config`'s remote url on the one tree the refusal promises not to touch. Measured:
> seed `origin = https://old.example/a.git`, plant `.git/MERGE_HEAD`, run a Commit whose
> definition names another host; the 409 comes back and the url is the attacker's. The open
> and the reconciliation are split so the gate can sit between them. `reset` still
> reconciles, because it is not refusing and a reset to `upstream` has to resolve against
> the url the definition actually names. Pinned by
> `ops::tests::a_refused_open_leaves_the_remote_url_untouched`.
>
> Separately: `init`'s `Exists` branch and `clone`'s idempotent branch reach an existing
> tree through `Repository::open` rather than this function, so one repo answered
> `200 initialized` / `200 up_to_date` to those two while answering `409 dirty_tree` to
> every other verb — and `init` was not read-only about it, since that branch adds a
> missing remote. Both now call `require_clean_state` directly (directly, so that being
> asked whether a tree exists never becomes a way to rewrite its remote url). Pinned by
> `ops::tests::every_verb_refuses_a_tree_a_human_left_mid_merge`.

```rust
/// The most files any single `DirtySummary` bucket will list.
///
/// A generated-file accident produces tens of thousands of dirty paths; the status
/// endpoint is polled and must not turn into a multi-megabyte response.
pub const MAX_DIRTY_FILES: usize = 200;

/// HEAD as a hex oid, or `None` when HEAD is unborn or detached from any object.
fn head_oid(repo: &git2::Repository) -> Option<String> {
    repo.head().ok().and_then(|h| h.target()).map(|o| o.to_string())
}

/// Open the working tree, refuse a state a human left behind, and only then
/// reconcile its `origin` with the current definition.
///
/// **The order is the contract.** `require_clean_state` declares the tree
/// untouchable, so nothing that runs before it may write to that tree — and
/// reconciling the remote is a `.git/config` write. Gating after it meant a verb
/// refused with `dirty_tree` had already repointed the remote of the repository
/// the refusal promises not to touch, which is the same defect the checkout was
/// held to account for: byte-identical after a refusal, not merely "the working
/// files survived".
///
/// Every verb that *mutates* an existing tree comes through here, so the gate is
/// one line and a verb added later gets it without being told. Three call sites
/// hold the same gate without holding this function, and each says why at its
/// own site: `reset` opts out of it entirely (below), while `init` and `clone`
/// call `require_clean_state` directly on their idempotent branches — those two
/// report on a tree they did not create and must not adopt the definition's
/// remote url as a side effect of being asked whether it exists.
pub fn open_tree(ctx: &OpCtx) -> Result<git2::Repository, GitError> {
    let repo = open_repo(ctx)?;
    require_clean_state(&repo, &ctx.def.id)?;
    reconcile_remote(&repo, &ctx.def)?;
    Ok(repo)
}

/// `open_tree` without the state gate. **Only `reset` may call this.**
///
/// The repository it returns may be half-way through somebody's merge, so the
/// only thing that may be done with it is the operation that repairs that:
/// libgit2's `git_reset` ends every non-soft reset in
/// `git_repository_state_cleanup` (1.9.6, reset.c), which is what makes `reset`
/// the documented way out. Staging, committing, merging or checking out through
/// this handle would do exactly the damage the gate exists to prevent — which is
/// also why there is no second, defensive check further down: nothing else can
/// ever be holding such a handle.
///
/// It still reconciles the remote, unlike a refusal: this one is not refusing.
/// A `reset` to `upstream` reads the tracking branch the definition names, so
/// skipping the reconciliation here would resolve it against a stale url.
pub fn open_tree_any_state(ctx: &OpCtx) -> Result<git2::Repository, GitError> {
    let repo = open_repo(ctx)?;
    reconcile_remote(&repo, &ctx.def)?;
    Ok(repo)
}

/// `Repository::open`, plus the one refusal that has to precede every gate:
/// there is no tree here at all.
fn open_repo(ctx: &OpCtx) -> Result<git2::Repository, GitError> {
    if !ctx.tree.exists() {
        return Err(GitError::no_worktree(&ctx.def.id));
    }
    Ok(git2::Repository::open(&ctx.tree)?)
}

/// Re-point the remote if the definition changed since the last operation.
///
/// A PUT can move a repo to a new host while the tree on disk still points at the
/// old one, and every later fetch/push would silently keep talking to the wrong
/// server. Split out of `open_tree` so the gate can run between the two: this
/// writes `.git/config`, and a refused verb must not.
fn reconcile_remote(repo: &git2::Repository, def: &RepoDef) -> Result<(), GitError> {
    let Some(url) = def.remote.as_deref() else {
        return Ok(());
    };
    match repo.find_remote(&def.remote_name) {
        // git2 0.21 returns `Result<&str, _>` from `Remote::url`, not `Option<&str>`
        // (a non-UTF-8 url is an Err rather than a None), so the comparison has to
        // go through `.ok()`.
        Ok(remote) if remote.url().ok() != Some(url) => {
            repo.remote_set_url(&def.remote_name, url)?;
        }
        Ok(_) => {}
        Err(_) => {
            repo.remote(&def.remote_name, url)?;
        }
    }
    Ok(())
}

/// Refuse a repository that is half-way through an operation a human started.
///
/// §6.6: our own merge is pure (§7.6) and never enters `RepositoryState::Merge` —
/// measured at every step of a real conflicting sync — so a non-Clean state in one
/// of these trees was created by a human running git by hand in it. Nothing here
/// may touch such a tree: `add_all` drops the three conflict stages and stages the
/// `<<<<<<< HEAD` text as resolved content, the commit takes `head_commit.iter()`
/// as its ONLY parent so the branch they were merging is recorded nowhere, the
/// merge that follows calls `cleanup_state()` and deletes their MERGE_HEAD, and
/// with `push` defaulting to true the markers are published. Refusing is the only
/// reading that cannot lose their work.
///
/// Shared with `fill_status` on purpose: one condition, one code, one message,
/// whether the caller asked for a status or a sync.
fn require_clean_state(repo: &git2::Repository, repo_id: &str) -> Result<(), GitError> {
    let state = repo.state();
    if state != git2::RepositoryState::Clean {
        return Err(GitError::repo_state_not_clean(state, repo_id));
    }
    Ok(())
}

/// Mutating ops refuse to guess which branch the caller meant.
pub fn require_branch(repo: &git2::Repository, branch: &str) -> Result<(), GitError> {
    if repo.is_empty()? {
        return Ok(()); // unborn is fine: the first commit creates `branch`
    }
    if repo.head_detached()? {
        return Err(GitError::detached_head());
    }
    let head = repo.head()?;
    let short = head.shorthand()?; // 0.21: Result, not Option
    if short != branch {
        return Err(GitError::branch_mismatch(short, branch));
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::ops::tests::`

Expected: PASS.

```
running 6 tests
test git::ops::tests::open_tree_adds_a_remote_that_is_not_there_yet ... ok
test git::ops::tests::open_tree_refuses_a_missing_worktree ... ok
test git::ops::tests::open_tree_repoints_a_changed_remote_url ... ok
test git::ops::tests::require_branch_allows_an_unborn_head ... ok
test git::ops::tests::require_branch_reports_a_branch_mismatch ... ok
test git::ops::tests::require_branch_reports_a_detached_head ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 5: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): open_tree and require_branch, on a hostile-config fixture

The fixture points libgit2's global config search path at a scratch
.gitconfig that sets init.defaultBranch=master, so every ops test runs
on a machine whose git default disagrees with [git].default_branch.
Remotes are plain absolute paths, which keeps the whole suite offline
and sidesteps Windows file:// drive letters.

git2 0.21 returns Result<&str> from Remote::url, not Option<&str>, so
the remote-repointing comparison goes through .ok()."
```

---

- [ ] **Step 6: Write the failing tests — `init`**

Add to the same test module:

```rust
    #[test]
    fn init_pins_the_configured_branch_against_a_master_default() {
        let fx = Fixture::new();
        let op = fx.op(fx.local_def("notes", "main"), JobOp::Init, OpRequest::default());
        let out = init(&op).expect("init");
        assert_eq!(out.outcome, "initialized");
        assert_eq!(out.branch, "main");

        let repo = open(&fx.tree("notes"));
        assert!(repo.is_empty().expect("is_empty"), "init writes no commit");
        assert_eq!(
            repo.find_reference("HEAD")
                .expect("HEAD")
                .symbolic_target()
                .expect("HEAD is symbolic"),
            Some("refs/heads/main")
        );

        // The control. Without `initial_head`, the very same libgit2 in this very
        // process produces `master` — which is exactly the machine-dependent behaviour
        // the pinning exists to defeat. If this assertion ever fails, the fixture's
        // scoped config has stopped working and the assertion above proves nothing.
        let control = fx.scratch("control");
        git2::Repository::init(&control).expect("control init");
        assert_eq!(
            git2::Repository::open(&control)
                .expect("open control")
                .find_reference("HEAD")
                .expect("HEAD")
                .symbolic_target()
                .expect("HEAD is symbolic"),
            Some("refs/heads/master")
        );
    }

    #[test]
    fn init_is_idempotent_and_records_the_remote() {
        let fx = Fixture::new();
        let op = fx.op(fx.remote_def("notes", "main"), JobOp::Init, OpRequest::default());
        init(&op).expect("first init");
        let repo = open(&fx.tree("notes"));
        commit_file(&repo, "f.md", "one\n", "one");
        let before = head_oid(&repo);
        drop(repo);

        // A retried POST /init must not wipe the tree it already created.
        let out = init(&op).expect("second init");
        assert_eq!(out.outcome, "initialized");
        let repo = open(&fx.tree("notes"));
        assert_eq!(head_oid(&repo), before);
        assert_eq!(
            repo.find_remote("origin").expect("origin").url().ok(),
            Some(fx.remote_url().as_str())
        );
    }
```

- [ ] **Step 7: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::ops::tests::init_`

Expected: FAIL — the file does not compile:

```
error[E0425]: cannot find function `init` in this scope
   --> src/git/ops.rs:NNN:19
    |
NNN |         let out = init(&op).expect("init");
    |                   ^^^^ not found in this scope
```

- [ ] **Step 8: Implement `init`**

Task 6's Step 48 already landed a stub of this name, so this is a **replacement in place**, not
an append — appending a second `init` is `error[E0428]: the name 'init' is defined multiple
times`. In `src/git/ops.rs`, replace the stub

```rust
pub fn init(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("init is not implemented yet"))
}
```

with:

```rust
pub fn init(ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    ctx.phase(Phase::Preparing);
    std::fs::create_dir_all(&ctx.tree)
        .map_err(|e| GitError::io(format!("{}: {e}", ctx.tree.display())))?;

    // `Repository::init` honours the developer's global init.defaultBranch, so an
    // unpinned init produces refs/heads/master on roughly half of all machines and
    // every later refspec (refs/heads/<def.branch>) silently fails to match.
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head(&ctx.def.branch).no_reinit(true);
    let repo = match git2::Repository::init_opts(&ctx.tree, &opts) {
        Ok(repo) => repo,
        // `no_reinit` turns "already a repository" into Exists rather than a
        // destructive re-init; a retried POST /init lands here and is a success.
        Err(e) if e.code() == git2::ErrorCode::Exists => {
            let repo = git2::Repository::open(&ctx.tree)?;
            // The same refusal every mutating verb gives, on the same condition —
            // this branch is the one that reached an *existing* tree. Answering
            // `initialized` for a repository a human is half-way through a merge
            // in, while sync/commit/pull/push all answer 409, makes "is this repo
            // wedged?" depend on which button was pressed. And it is not a
            // read-only answer either: the remote block below writes `.git/config`.
            //
            // Directly rather than through `open_tree`, which would also *repoint*
            // an existing remote; init only ever fills in a missing one, and a
            // retried POST must not quietly become a way to rewrite a url.
            require_clean_state(&repo, &ctx.def.id)?;
            repo
        }
        Err(e) => return Err(e.into()),
    };

    if let Some(url) = ctx.def.remote.as_deref() {
        if repo.find_remote(&ctx.def.remote_name).is_err() {
            repo.remote(&ctx.def.remote_name, url)?;
        }
    }

    let mut out = OpOutcome::new("initialized", &ctx.def.branch);
    out.head_after = head_oid(&repo);
    Ok(out)
}
```

- [ ] **Step 9: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::ops::tests::init_`

Expected: PASS.

```
running 2 tests
test git::ops::tests::init_is_idempotent_and_records_the_remote ... ok
test git::ops::tests::init_pins_the_configured_branch_against_a_master_default ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 10: Trim task 6's dispatch scaffolding to the verbs this task does not own**

`init` is real from the previous step onward, and that makes task 6's scaffolding test
`run_dispatches_to_a_verb_of_the_same_name` red: it loops over all eight ops asserting each one
comes back `internal("<verb> is not implemented yet")`, and `init` against task 6's fixture tree
(`/nonexistent/repos/notes`) now fails inside `create_dir_all` with `io_failed` instead. Trimming
it here rather than at the end of the task is deliberate — every commit in this task has to be
green, and the four remaining verbs land in the commits after this one.

Run it first and watch it fail:

`cargo test --bin hitch git::ops::tests::run_dispatches_to_a_verb`

```
running 1 test
test git::ops::tests::run_dispatches_to_a_verb_of_the_same_name ... FAILED

failures:

---- git::ops::tests::run_dispatches_to_a_verb_of_the_same_name stdout ----
thread 'git::ops::tests::run_dispatches_to_a_verb_of_the_same_name' panicked at src/git/ops.rs:NNN:13:
assertion `left == right` failed
  left: IoFailed
 right: Internal

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; N filtered out
```

Then, in `mod tests` in `src/git/ops.rs`, cut the five verbs this task owns out of that loop.
`Clone`, `Commit`, `Branch` and `Reset` go now as well as `Init`: all four land in the commits
that follow, and the list is not worth revisiting four more times. Replace

```rust
        for op in [
            JobOp::Init,
            JobOp::Clone,
            JobOp::Pull,
            JobOp::Push,
            JobOp::Sync,
            JobOp::Commit,
            JobOp::Branch,
            JobOp::Reset,
        ] {
```

with:

```rust
        for op in [JobOp::Pull, JobOp::Push, JobOp::Sync] {
```

Leave the rest of the test — its `expect_err`, both assertions and the doc comment — exactly as
task 6 wrote it. Task 8 drops `JobOp::Push` from this list when `push` lands and `JobOp::Pull`
when `pull` lands, then deletes the whole test with the last stub, and it is written expecting
to find exactly these three names here.

Run it again and watch it pass:

`cargo test --bin hitch git::ops::tests::run_dispatches_to_a_verb`

```
running 1 test
test git::ops::tests::run_dispatches_to_a_verb_of_the_same_name ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 11: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): init with a pinned initial_head

RepositoryInitOptions::initial_head is the whole point: an unpinned
init honours the machine's global init.defaultBranch and produces
refs/heads/master, after which every refspec built from
refs/heads/<def.branch> matches nothing. no_reinit(true) makes a
retried POST /init an Exists error we treat as success rather than a
destructive re-init.

The test carries a control assertion proving the fixture's scoped
config really does say master, so the pinning assertion cannot pass
vacuously.

Task 6's run_dispatches_to_a_verb_of_the_same_name loop drops to
[Pull, Push, Sync] in the same commit: it asserts every arm still
answers internal('<verb> is not implemented yet'), which stops being
true for a verb the moment that verb becomes real."
```

---

- [ ] **Step 12: Write the failing tests — `clone` and `ensure_branch`**

Add to the same test module:

```rust
    #[test]
    fn clone_checks_out_the_configured_branch_and_sets_upstream() {
        // The remote's default is `master`; the repo definition asks for `main`, which
        // also exists on the remote. `ensure_branch` must switch to it and bind it.
        let fx = Fixture::with_origin_head("master");
        let worker = fx.seed("master", &[("m.md", "from master\n")]);
        let tip = worker
            .head()
            .expect("head")
            .peel_to_commit()
            .expect("commit");
        worker.branch("main", &tip, false).expect("branch main");
        worker.set_head("refs/heads/main").expect("set head");
        let mut co = git2::build::CheckoutBuilder::new();
        co.force();
        worker.checkout_head(Some(&mut co)).expect("checkout");
        commit_file(&worker, "n.md", "from main\n", "add n.md");
        push_branch(&worker, "main");

        let op = fx.op(fx.remote_def("notes", "main"), JobOp::Clone, OpRequest::default());
        let out = clone(&op).expect("clone");
        assert_eq!(out.outcome, "cloned");

        let repo = open(&fx.tree("notes"));
        assert_eq!(
            repo.head().expect("head").shorthand().expect("shorthand"),
            "main"
        );
        assert!(fx.tree("notes").join("n.md").exists());
        assert!(fx.tree("notes").join("m.md").exists());
        let branch = repo
            .find_branch("main", git2::BranchType::Local)
            .expect("local main");
        let upstream = branch.upstream().expect("upstream");
        assert_eq!(upstream.name().expect("name"), Some("origin/main"));
    }

    #[test]
    fn clone_creates_the_configured_branch_when_the_remote_lacks_it() {
        // The remote only has `master`. There is no refs/remotes/origin/main to branch
        // from, so `ensure_branch` forks the current HEAD and leaves it unbound.
        let fx = Fixture::with_origin_head("master");
        fx.seed("master", &[("m.md", "from master\n")]);

        let op = fx.op(fx.remote_def("notes", "main"), JobOp::Clone, OpRequest::default());
        clone(&op).expect("clone");

        let repo = open(&fx.tree("notes"));
        assert_eq!(
            repo.head().expect("head").shorthand().expect("shorthand"),
            "main"
        );
        assert!(fx.tree("notes").join("m.md").exists());
        assert!(
            repo.find_branch("main", git2::BranchType::Local)
                .expect("local main")
                .upstream()
                .is_err(),
            "there is no origin/main to track yet; the first push will create it"
        );
    }

    #[test]
    fn clone_of_an_empty_remote_still_pins_head() {
        // A brand-new remote has no commits, so there is nothing to branch from. The
        // naive `repo.head()?` fallback fails with unborn_branch here and would make
        // cloning a freshly created remote impossible.
        let fx = Fixture::with_origin_head("master");
        let op = fx.op(fx.remote_def("notes", "main"), JobOp::Clone, OpRequest::default());
        let out = clone(&op).expect("clone of an empty remote");
        assert_eq!(out.outcome, "cloned");

        let repo = open(&fx.tree("notes"));
        assert!(repo.is_empty().expect("is_empty"));
        assert_eq!(
            repo.find_reference("HEAD")
                .expect("HEAD")
                .symbolic_target()
                .expect("HEAD is symbolic"),
            Some("refs/heads/main")
        );
    }

    #[test]
    fn clone_into_an_existing_repository_is_idempotent() {
        let fx = Fixture::with_origin_head("main");
        fx.seed("main", &[("f.md", "one\n")]);
        let op = fx.op(fx.remote_def("notes", "main"), JobOp::Clone, OpRequest::default());
        clone(&op).expect("first clone");

        // A retried POST /clone after a dropped response must not be an error.
        let out = clone(&op).expect("second clone");
        assert_eq!(out.outcome, "up_to_date");
        assert!(fx.tree("notes").join("f.md").exists());
    }

    #[test]
    fn clone_into_a_populated_non_repository_is_refused() {
        let fx = Fixture::with_origin_head("main");
        fx.seed("main", &[("f.md", "one\n")]);
        std::fs::create_dir_all(fx.tree("notes")).expect("mkdir");
        std::fs::write(fx.tree("notes").join("someone-elses.txt"), "hi").expect("write");

        let op = fx.op(fx.remote_def("notes", "main"), JobOp::Clone, OpRequest::default());
        let err = clone(&op).expect_err("that directory is not ours");
        assert_eq!(err.code(), GitErrorCode::NotARepository);
        assert!(
            fx.tree("notes").join("someone-elses.txt").exists(),
            "a refused clone touches nothing"
        );
    }

    #[test]
    fn clone_without_a_remote_is_remote_missing() {
        let fx = Fixture::new();
        let op = fx.op(fx.local_def("notes", "main"), JobOp::Clone, OpRequest::default());
        let err = clone(&op).expect_err("nothing to clone from");
        assert_eq!(err.code(), GitErrorCode::RemoteMissing);
    }
```

- [ ] **Step 13: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::ops::tests::clone_`

Expected: FAIL — the file does not compile:

```
error[E0425]: cannot find function `clone` in this scope
   --> src/git/ops.rs:NNN:19
    |
NNN |         let out = clone(&op).expect("clone");
    |                   ^^^^^ not found in this scope
```

- [ ] **Step 14: Implement `ensure_branch`**

Append to `src/git/ops.rs`:

```rust
/// After a clone, the remote's default branch may not be `def.branch`.
pub fn ensure_branch(repo: &git2::Repository, def: &RepoDef) -> Result<(), GitError> {
    let current = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().ok().map(str::to_owned));
    if current.as_deref() == Some(def.branch.as_str()) {
        return Ok(());
    }

    // An empty remote produces a clone with an unborn HEAD: there is no commit to branch
    // from, so point HEAD at the (still unborn) branch and stop. The first commit then
    // lands on `def.branch` instead of whatever name the remote advertised.
    if repo.is_empty()? {
        repo.set_head(&format!("refs/heads/{}", def.branch))?;
        return Ok(());
    }

    let tracking = format!("refs/remotes/{}/{}", def.remote_name, def.branch);
    let target = match repo.find_reference(&tracking) {
        Ok(r) => r
            .target()
            .ok_or_else(|| GitError::internal("remote tracking ref has no target"))?,
        // The remote does not publish this branch yet; fork it from what we just checked
        // out and leave it unbound. The first push creates it and sets the upstream.
        Err(_) => repo.head()?.peel_to_commit()?.id(),
    };
    let commit = repo.find_commit(target)?;
    let mut branch = repo.branch(&def.branch, &commit, true)?;
    if repo.find_reference(&tracking).is_ok() {
        branch.set_upstream(Some(&format!("{}/{}", def.remote_name, def.branch)))?;
    }

    let object = repo.revparse_single(&format!("refs/heads/{}", def.branch))?;
    // Order matters and is API-documented: checkout_tree THEN set_head. The other order
    // leaves the index and working tree describing the branch you just left.
    let mut co = git2::build::CheckoutBuilder::new();
    co.force();
    repo.checkout_tree(&object, Some(&mut co))?;
    repo.set_head(&format!("refs/heads/{}", def.branch))?;
    Ok(())
}
```

- [ ] **Step 15: Implement `clone`**

Another task-6 stub, so again a **replacement in place**. In `src/git/ops.rs`, replace the stub

```rust
pub fn clone(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("clone is not implemented yet"))
}
```

with:

```rust
pub fn clone(ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    ctx.phase(Phase::Preparing);
    let populated = ctx.tree.exists()
        && std::fs::read_dir(&ctx.tree)
            .map_err(|e| GitError::io(format!("{}: {e}", ctx.tree.display())))?
            .next()
            .is_some();
    if populated {
        // Idempotent: an existing repository is reused so a retried POST /clone is not
        // an error. A populated directory that is *not* a repository belongs to someone
        // else and is refused rather than cloned over.
        return match git2::Repository::open(&ctx.tree) {
            Ok(repo) => {
                // Same gate, same reason as `init`'s Exists branch: `up_to_date`
                // is the one answer that tells a caller this repository is fine,
                // and it must not be the one answer a wedged tree still gives.
                // Directly rather than through `open_tree`, because this branch
                // reports on a tree it did not create and does not reconcile it.
                require_clean_state(&repo, &ctx.def.id)?;
                let mut out = OpOutcome::new("up_to_date", &ctx.def.branch);
                out.head_after = head_oid(&repo);
                Ok(out)
            }
            Err(_) => Err(GitError::not_a_repository(&ctx.tree)),
        };
    }

    let url = ctx.def.remote.as_deref().ok_or_else(GitError::remote_missing)?;
    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(crate::git::creds::callbacks(ctx));
    ctx.phase(Phase::Cloning);
    // `Repository::clone` (the free function) takes no callbacks and cannot
    // authenticate; `RepoBuilder` is the only form that can.
    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(fo);
    let repo = builder
        .clone(url, &ctx.tree)
        .map_err(|e| ctx.classify_err(&e))?;

    ctx.phase(Phase::CheckingOut);
    ensure_branch(&repo, &ctx.def)?;

    let mut out = OpOutcome::new("cloned", &ctx.def.branch);
    out.head_after = head_oid(&repo);
    out.learned_fingerprint = ctx.learned_fingerprint();
    Ok(out)
}
```

- [ ] **Step 16: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::ops::tests::clone_`

Expected: PASS.

```
running 6 tests
test git::ops::tests::clone_checks_out_the_configured_branch_and_sets_upstream ... ok
test git::ops::tests::clone_creates_the_configured_branch_when_the_remote_lacks_it ... ok
test git::ops::tests::clone_into_a_populated_non_repository_is_refused ... ok
test git::ops::tests::clone_into_an_existing_repository_is_idempotent ... ok
test git::ops::tests::clone_of_an_empty_remote_still_pins_head ... ok
test git::ops::tests::clone_without_a_remote_is_remote_missing ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 17: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): clone via RepoBuilder, with ensure_branch afterwards

The free Repository::clone function takes no callbacks and cannot
authenticate, so RepoBuilder is the only usable form.

ensure_branch exists because the remote's default branch may not be
def.branch: it adopts refs/remotes/<remote>/<branch> when it exists and
binds the upstream, otherwise forks the checked-out HEAD and leaves it
unbound for the first push to bind. It returns early on an empty clone,
where the design sketch's repo.head()? would fail with unborn_branch and
make cloning a brand-new remote impossible.

Cloning into a directory that already holds a repository is idempotent
(up_to_date); a populated non-repository is refused untouched."
```

---

- [ ] **Step 18: Write the failing tests — `stage_and_commit` and `commit`**

Add to the same test module:

```rust
    /// A tree on `main` with one commit, ready for the commit tests.
    fn seeded_local(fx: &Fixture, id: &str) -> git2::Repository {
        let op = fx.op(fx.local_def(id, "main"), JobOp::Init, OpRequest::default());
        init(&op).expect("init");
        let repo = open(&fx.tree(id));
        commit_file(&repo, "base.md", "base\n", "base");
        repo
    }

    #[test]
    fn commit_stages_everything_and_honours_gitignore() {
        let fx = Fixture::new();
        let repo = seeded_local(&fx, "notes");
        std::fs::write(fx.tree("notes").join(".gitignore"), "secret.env\n").expect("write");
        std::fs::write(fx.tree("notes").join("secret.env"), "TOKEN=hunter2").expect("write");
        std::fs::write(fx.tree("notes").join("note.md"), "hello\n").expect("write");
        std::fs::remove_file(fx.tree("notes").join("base.md")).expect("remove");
        drop(repo);

        let op = fx.op(fx.local_def("notes", "main"), JobOp::Commit, OpRequest::default());
        let out = commit(&op).expect("commit");
        assert_eq!(out.outcome, "committed");
        assert!(out.committed);
        assert_eq!(out.files_committed, 3, "note.md, .gitignore, deleted base.md");

        let repo = open(&fx.tree("notes"));
        let tree = repo
            .find_commit(git2::Oid::from_str(out.commit.as_deref().expect("commit oid")).expect("oid"))
            .expect("find_commit")
            .tree()
            .expect("tree");
        assert!(tree.get_path(Path::new("note.md")).is_ok());
        assert!(
            tree.get_path(Path::new("secret.env")).is_err(),
            "IndexAddOption::DEFAULT honours .gitignore; FORCE would commit this"
        );
        assert!(
            tree.get_path(Path::new("base.md")).is_err(),
            "add_all(['*']) is `git add -A`: it stages deletions of tracked files"
        );
    }

    #[test]
    fn commit_with_nothing_to_do_reports_no_changes() {
        let fx = Fixture::new();
        let repo = seeded_local(&fx, "notes");
        let before = head_oid(&repo);
        drop(repo);

        let op = fx.op(fx.local_def("notes", "main"), JobOp::Commit, OpRequest::default());
        let out = commit(&op).expect("commit");
        assert_eq!(out.outcome, "no_changes");
        assert!(!out.committed);
        assert!(out.commit.is_none());
        assert_eq!(out.head_after, before);
    }

    #[test]
    fn commit_with_allow_empty_writes_a_commit() {
        let fx = Fixture::new();
        let repo = seeded_local(&fx, "notes");
        let before = head_oid(&repo);
        drop(repo);

        let request = OpRequest {
            allow_empty: true,
            ..OpRequest::default()
        };
        let op = fx.op(fx.local_def("notes", "main"), JobOp::Commit, request);
        let out = commit(&op).expect("commit");
        assert_eq!(out.outcome, "committed");
        assert_ne!(out.head_after, before);

        let repo = open(&fx.tree("notes"));
        let head = repo.head().expect("head").peel_to_commit().expect("commit");
        assert_eq!(head.parent_count(), 1);
        assert_eq!(
            head.tree_id(),
            head.parent(0).expect("parent").tree_id(),
            "an empty commit has the same tree as its parent"
        );
    }

    #[test]
    fn commit_honours_a_pathspec() {
        let fx = Fixture::new();
        let repo = seeded_local(&fx, "notes");
        std::fs::write(fx.tree("notes").join("a.md"), "a\n").expect("write");
        std::fs::write(fx.tree("notes").join("b.md"), "b\n").expect("write");
        drop(repo);

        let request = OpRequest {
            paths: Some(vec!["a.md".to_string()]),
            ..OpRequest::default()
        };
        let op = fx.op(fx.local_def("notes", "main"), JobOp::Commit, request);
        let out = commit(&op).expect("commit");
        assert_eq!(out.files_committed, 1);

        let repo = open(&fx.tree("notes"));
        let tree = repo
            .head()
            .expect("head")
            .peel_to_tree()
            .expect("tree");
        assert!(tree.get_path(Path::new("a.md")).is_ok());
        assert!(tree.get_path(Path::new("b.md")).is_err());
    }

    #[test]
    fn commit_uses_the_request_author_then_the_configured_default() {
        let fx = Fixture::new();
        let repo = seeded_local(&fx, "notes");
        std::fs::write(fx.tree("notes").join("a.md"), "a\n").expect("write");
        drop(repo);

        let request = OpRequest {
            author: Some(AuthorSpec {
                name: Some("Ada".to_string()),
                email: Some("ada@example.invalid".to_string()),
            }),
            message: Some("by hand".to_string()),
            ..OpRequest::default()
        };
        let op = fx.op(fx.local_def("notes", "main"), JobOp::Commit, request);
        commit(&op).expect("commit");

        let repo = open(&fx.tree("notes"));
        let head = repo.head().expect("head").peel_to_commit().expect("commit");
        assert_eq!(head.author().name(), Some("Ada"));
        assert_eq!(head.message(), Some("by hand"));
        drop(head);

        std::fs::write(fx.tree("notes").join("b.md"), "b\n").expect("write");
        drop(repo);
        let op = fx.op(fx.local_def("notes", "main"), JobOp::Commit, OpRequest::default());
        commit(&op).expect("commit");
        let repo = open(&fx.tree("notes"));
        let head = repo.head().expect("head").peel_to_commit().expect("commit");
        assert_eq!(head.author().name(), Some("Host App"));
        assert!(
            head.message().expect("message").starts_with("Test App: sync from test-host at "),
            "the generated message names the app and the machine: {:?}",
            head.message()
        );
    }

    #[test]
    fn commit_on_the_wrong_branch_is_refused() {
        let fx = Fixture::new();
        let repo = seeded_local(&fx, "notes");
        let tip = repo.head().expect("head").peel_to_commit().expect("commit");
        repo.branch("side", &tip, false).expect("branch");
        repo.set_head("refs/heads/side").expect("set head");
        std::fs::write(fx.tree("notes").join("a.md"), "a\n").expect("write");
        drop(tip);
        drop(repo);

        let op = fx.op(fx.local_def("notes", "main"), JobOp::Commit, OpRequest::default());
        let err = commit(&op).expect_err("HEAD is on side, the repo is configured for main");
        assert_eq!(err.code(), GitErrorCode::BranchMismatch);
    }
```

- [ ] **Step 19: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::ops::tests::commit_`

Expected: FAIL — the file does not compile:

```
error[E0425]: cannot find function `commit` in this scope
   --> src/git/ops.rs:NNN:19
    |
NNN |         let out = commit(&op).expect("commit");
    |                   ^^^^^^ not found in this scope
```

- [ ] **Step 20: Implement `stage_and_commit`**

Append to `src/git/ops.rs`:

```rust
/// Stage the requested paths and, if the tree actually changed, write one commit.
///
/// Returns `(commit oid, files staged)`. `Ok((None, 0))` means "nothing to commit".
pub fn stage_and_commit(
    repo: &git2::Repository,
    ctx: &OpCtx,
) -> Result<(Option<git2::Oid>, usize), GitError> {
    ctx.phase(Phase::Staging);
    let mut index = repo.index()?;
    let specs: Vec<&str> = ctx
        .request
        .paths
        .as_deref()
        .map(|v| v.iter().map(String::as_str).collect())
        .unwrap_or_else(|| vec!["*"]);
    // add_all(["*"], DEFAULT) is `git add -A`: it stages deletions of tracked files AND
    // brand-new untracked ones, so `update_all` would only miss the new ones and is not
    // additionally needed. DEFAULT honours .gitignore; IndexAddOption::FORCE is never
    // used anywhere in this file, because a generic executor that bypasses .gitignore
    // will eventually commit somebody's .env.
    index.add_all(specs.iter(), git2::IndexAddOption::DEFAULT, None)?;
    index.write()?;

    let head_commit = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let head_tree = head_commit.as_ref().and_then(|c| c.tree().ok());
    let files = repo
        .diff_tree_to_index(head_tree.as_ref(), Some(&index), None)?
        .deltas()
        .count();

    let tree_oid = index.write_tree()?;
    let changed = head_commit.as_ref().is_none_or(|c| c.tree_id() != tree_oid);
    if !changed && !ctx.request.allow_empty {
        return Ok((None, 0));
    }

    ctx.phase(Phase::Committing);
    let tree = repo.find_tree(tree_oid)?;
    // NEVER `repo.signature()`: verified to fail with code=NotFound class=Config
    // "config value 'user.name' was not found" on any machine that has never
    // configured git, which is most machines this template ships to.
    let sig = git2::Signature::now(&ctx.author_name(), &ctx.author_email())?;
    let message = ctx.request.message.clone().unwrap_or_else(|| {
        format!(
            "{}: sync from {} at {}",
            ctx.identity.app_name,
            ctx.identity.hostname,
            crate::git::util::utc_stamp(crate::git::util::now_ms())
        )
    });
    // An empty parent slice is exactly what makes this a root commit.
    let parents: Vec<&git2::Commit> = head_commit.iter().collect();
    let oid = repo.commit(Some("HEAD"), &sig, &sig, &message, &tree, &parents)?;
    Ok((Some(oid), files))
}
```

- [ ] **Step 21: Implement `commit`**

`stage_and_commit` above was new, so it was appended; `commit` is a task-6 stub and is a
**replacement in place**. In `src/git/ops.rs`, replace the stub

```rust
pub fn commit(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("commit is not implemented yet"))
}
```

with:

```rust
pub fn commit(ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    let repo = open_tree(ctx)?;
    require_branch(&repo, &ctx.def.branch)?;
    let head_before = head_oid(&repo);
    let (oid, files) = stage_and_commit(&repo, ctx)?;

    let mut out = OpOutcome::new(
        if oid.is_some() { "committed" } else { "no_changes" },
        &ctx.def.branch,
    );
    out.head_before = head_before;
    out.head_after = head_oid(&repo);
    out.committed = oid.is_some();
    out.commit = oid.map(|o| o.to_string());
    out.files_committed = if oid.is_some() { files } else { 0 };
    Ok(out)
}
```

- [ ] **Step 22: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::ops::tests::commit_`

Expected: PASS.

```
running 6 tests
test git::ops::tests::commit_honours_a_pathspec ... ok
test git::ops::tests::commit_on_the_wrong_branch_is_refused ... ok
test git::ops::tests::commit_stages_everything_and_honours_gitignore ... ok
test git::ops::tests::commit_uses_the_request_author_then_the_configured_default ... ok
test git::ops::tests::commit_with_allow_empty_writes_a_commit ... ok
test git::ops::tests::commit_with_nothing_to_do_reports_no_changes ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 23: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): stage_and_commit and the commit verb

add_all(['*'], IndexAddOption::DEFAULT) is `git add -A` and honours
.gitignore. FORCE is never used in this file: a generic executor that
bypasses .gitignore eventually commits somebody's .env.

Signature::now, never repo.signature(), which fails with
NotFound/Config 'user.name was not found' on any machine that has never
configured git."
```

---

- [ ] **Step 24: Write the failing tests — `status`**

Add to the same test module:

```rust
    fn read_ctx(fx: &Fixture, def: RepoDef) -> ReadCtx {
        ReadCtx {
            tree: fx.tree(&def.id),
            def,
            last_sync: None,
            busy_job: None,
        }
    }

    #[test]
    fn status_walks_absent_uninitialized_unborn_clean_and_dirty() {
        let fx = Fixture::new();
        let def = fx.local_def("notes", "main");

        let s = status(&read_ctx(&fx, def.clone()));
        assert_eq!(s.state, "absent");
        assert!(!s.exists);
        assert!(!s.is_repository);
        assert!(s.error.is_none());

        std::fs::create_dir_all(fx.tree("notes")).expect("mkdir");
        std::fs::write(fx.tree("notes").join("stray.txt"), "x").expect("write");
        let s = status(&read_ctx(&fx, def.clone()));
        assert_eq!(s.state, "uninitialized");
        assert!(s.exists);
        assert!(!s.is_repository);

        let op = fx.op(def.clone(), JobOp::Init, OpRequest::default());
        init(&op).expect("init");
        let s = status(&read_ctx(&fx, def.clone()));
        assert_eq!(s.state, "unborn");
        assert!(s.unborn);
        assert!(s.head.is_none());
        assert_eq!(
            s.branch.as_deref(),
            Some("main"),
            "an unborn HEAD still names the branch the first commit will create"
        );

        let repo = open(&fx.tree("notes"));
        commit_file(&repo, "f.md", "one\n", "one");
        std::fs::remove_file(fx.tree("notes").join("stray.txt")).expect("remove");
        drop(repo);
        let s = status(&read_ctx(&fx, def.clone()));
        assert_eq!(s.state, "clean");
        assert!(s.head.is_some());
        assert_eq!(s.dirty.count, 0);

        std::fs::write(fx.tree("notes").join("f.md"), "two\n").expect("write");
        let s = status(&read_ctx(&fx, def));
        assert_eq!(s.state, "dirty");
        assert_eq!(s.dirty.modified, vec!["f.md".to_string()]);
        assert_eq!(s.dirty.count, 1);
    }

    #[test]
    fn status_buckets_paths_and_truncates_at_the_cap() {
        let fx = Fixture::new();
        let def = fx.local_def("notes", "main");
        let op = fx.op(def.clone(), JobOp::Init, OpRequest::default());
        init(&op).expect("init");
        let repo = open(&fx.tree("notes"));
        commit_file(&repo, "kept.md", "kept\n", "kept");
        commit_file(&repo, "gone.md", "gone\n", "gone");
        commit_file(&repo, "edited.md", "edited\n", "edited");
        drop(repo);

        std::fs::write(fx.tree("notes").join("edited.md"), "changed\n").expect("write");
        std::fs::remove_file(fx.tree("notes").join("gone.md")).expect("remove");
        for i in 0..(MAX_DIRTY_FILES + 5) {
            std::fs::write(fx.tree("notes").join(format!("new-{i:04}.txt")), "x").expect("write");
        }

        let s = status(&read_ctx(&fx, def));
        assert_eq!(s.dirty.modified, vec!["edited.md".to_string()]);
        assert_eq!(s.dirty.deleted, vec!["gone.md".to_string()]);
        assert_eq!(s.dirty.untracked.len(), MAX_DIRTY_FILES);
        assert!(s.dirty.truncated);
        assert_eq!(
            s.dirty.count,
            MAX_DIRTY_FILES + 7,
            "count is every entry seen, not every entry listed"
        );
    }

    #[test]
    fn status_reports_the_upstream_and_ahead_behind() {
        let fx = Fixture::with_origin_head("main");
        fx.seed("main", &[("f.md", "one\n")]);
        let def = fx.remote_def("notes", "main");
        let op = fx.op(def.clone(), JobOp::Clone, OpRequest::default());
        clone(&op).expect("clone");

        let s = status(&read_ctx(&fx, def.clone()));
        assert_eq!(s.state, "clean");
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        assert_eq!((s.ahead, s.behind), (0, 0));
        assert_eq!(
            s.remote.as_ref().map(|r| r.name.as_str()),
            Some("origin")
        );

        let repo = open(&fx.tree("notes"));
        commit_file(&repo, "g.md", "two\n", "two");
        drop(repo);
        let s = status(&read_ctx(&fx, def));
        assert_eq!((s.ahead, s.behind), (1, 0));
    }

    #[test]
    fn status_reports_a_detached_head() {
        let fx = Fixture::new();
        let def = fx.local_def("notes", "main");
        let op = fx.op(def.clone(), JobOp::Init, OpRequest::default());
        init(&op).expect("init");
        let repo = open(&fx.tree("notes"));
        let oid = commit_file(&repo, "f.md", "one\n", "one");
        repo.set_head_detached(oid).expect("detach");
        drop(repo);

        let s = status(&read_ctx(&fx, def));
        assert_eq!(s.state, "detached");
        assert!(s.detached);
        assert!(s.branch.is_none());
        assert_eq!(s.head.as_deref(), Some(oid.to_string().as_str()));
    }

    #[test]
    fn status_reports_a_non_clean_repository_state_as_error() {
        // §6.6: our own merge is pure (§7.6) and never enters RepositoryState::Merge, so a
        // MERGE_HEAD in one of these trees was left by a human running git by hand. The
        // deliberate decision is that startup does NOT run cleanup_state() or reset --hard,
        // because that would silently discard their staged resolution. This is the other
        // half of that decision: `open_tree`'s gate is the refusal, and this is the report.
        // Same call, same code, same message — which is why asserting the substrings here
        // and in merge.rs's verb sweep is what keeps the two from drifting apart.
        //
        // Verified against libgit2 1.9.6: writing .git/MERGE_HEAD is by itself enough to
        // make repo.state() report Merge, on a freshly opened handle and on an open one.
        let fx = Fixture::new();
        let def = fx.local_def("notes", "main");
        let repo = seeded_local(&fx, "notes");
        let oid = head_oid(&repo).expect("the seed committed something");
        drop(repo);
        std::fs::write(
            fx.tree("notes").join(".git").join("MERGE_HEAD"),
            format!("{oid}\n"),
        )
        .expect("plant MERGE_HEAD");

        let s = status(&read_ctx(&fx, def));
        assert_eq!(s.state, "error");
        // Everything read before the state check is still reported, so one call tells the
        // operator both that the repo is stuck and what is in it.
        assert!(s.is_repository);
        assert_eq!(s.head.as_deref(), Some(oid.as_str()));
        assert_eq!(s.branch.as_deref(), Some("main"));

        let e = s.error.expect("a non-Clean state is surfaced, never repaired");
        assert!(
            e.message.contains("Merge"),
            "the error names the state the repo is stuck in: {}",
            e.message
        );
        assert!(
            e.message.contains("/reset"),
            "the error names the way out: {}",
            e.message
        );
    }
```

- [ ] **Step 25: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::ops::tests::status_`

Expected: FAIL — the file does not compile:

```
error[E0422]: cannot find struct, variant or union type `ReadCtx` in this scope
   --> src/git/ops.rs:NNN:9
    |
NNN |         ReadCtx {
    |         ^^^^^^^ not found in this scope

error[E0425]: cannot find function `status` in this scope
   --> src/git/ops.rs:NNN:17
    |
NNN |         let s = status(&read_ctx(&fx, def.clone()));
    |                 ^^^^^^ not found in this scope
```

- [ ] **Step 26: Add the read-path types**

Append to `src/git/ops.rs`:

```rust
/// Everything a read needs. Deliberately not an `OpCtx`: reads take no job, no lease,
/// no credential and no abort flag, and must stay callable while a job is running.
pub struct ReadCtx {
    pub tree: PathBuf,
    pub def: RepoDef,
    pub last_sync: Option<crate::git::state::LastSync>,
    pub busy_job: Option<crate::git::jobs::JobRef>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DirtySummary {
    pub modified: Vec<String>,
    pub added: Vec<String>,
    pub deleted: Vec<String>,
    pub renamed: Vec<String>,
    pub untracked: Vec<String>,
    /// Every entry seen, including the ones the cap left out of the lists above.
    pub count: usize,
    /// True when at least one list hit `MAX_DIRTY_FILES`.
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteInfo {
    pub name: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AutoInfo {
    pub auto_sync_secs: Option<u64>,
    pub sync_on_start: bool,
    pub sync_on_quit: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoStatus {
    pub id: String,
    pub path: String,
    pub exists: bool,
    pub is_repository: bool,
    pub state: &'static str,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub unborn: bool,
    pub detached: bool,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub dirty: DirtySummary,
    pub remote: Option<RemoteInfo>,
    pub auto: AutoInfo,
    pub busy_job: Option<crate::git::jobs::JobRef>,
    pub last_sync: Option<crate::git::state::LastSync>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<GitError>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BranchesResponse {
    pub current: Option<String>,
    pub detached: bool,
    pub local: Vec<String>,
    pub remote: Vec<String>,
    pub upstream: Option<String>,
}
```

- [ ] **Step 27: Implement `status`**

Append to `src/git/ops.rs`:

```rust
/// Never fails: a repo that cannot be read reports `state: "error"` with the reason
/// attached, because a polled status endpoint that 500s tells the caller nothing about
/// which of its repos is broken.
pub fn status(ctx: &ReadCtx) -> RepoStatus {
    let mut out = RepoStatus {
        id: ctx.def.id.clone(),
        path: ctx.tree.display().to_string(),
        exists: ctx.tree.exists(),
        is_repository: false,
        state: "absent",
        branch: None,
        head: None,
        unborn: false,
        detached: false,
        upstream: None,
        ahead: 0,
        behind: 0,
        dirty: DirtySummary::default(),
        // Until the repo is open, the definition is the only source for the remote.
        remote: ctx.def.remote.as_ref().map(|url| RemoteInfo {
            name: ctx.def.remote_name.clone(),
            url: Some(url.clone()),
        }),
        auto: AutoInfo {
            auto_sync_secs: ctx.def.auto_sync_secs,
            sync_on_start: ctx.def.sync_on_start,
            sync_on_quit: ctx.def.sync_on_quit,
        },
        busy_job: ctx.busy_job.clone(),
        last_sync: ctx.last_sync.clone(),
        error: None,
    };
    if !out.exists {
        return out;
    }
    let repo = match git2::Repository::open(&ctx.tree) {
        Ok(repo) => repo,
        // A directory that is not a repository is not an error a caller can retry past;
        // it is the state `uninitialized`, and init or clone is the fix.
        Err(_) => {
            out.state = "uninitialized";
            return out;
        }
    };
    out.is_repository = true;
    if let Err(e) = fill_status(&mut out, &repo, &ctx.def) {
        out.state = "error";
        out.error = Some(e);
    }
    out
}

fn fill_status(
    out: &mut RepoStatus,
    repo: &git2::Repository,
    def: &RepoDef,
) -> Result<(), GitError> {
    if let Ok(remote) = repo.find_remote(&def.remote_name) {
        out.remote = Some(RemoteInfo {
            name: def.remote_name.clone(),
            url: remote.url().ok().map(str::to_owned),
        });
    }

    let mut head_target = None;
    match repo.head() {
        Ok(head) => {
            out.detached = repo.head_detached()?;
            head_target = head.target();
            out.head = head_target.map(|o| o.to_string());
            if !out.detached {
                out.branch = head.shorthand().ok().map(str::to_owned);
            }
        }
        Err(e) if e.code() == git2::ErrorCode::UnbornBranch => {
            out.unborn = true;
            // An unborn HEAD still names the branch the first commit will create, and
            // this is the only place status can learn it. Reporting null makes a
            // freshly initialised repo look broken to a caller.
            out.branch = repo.find_reference("HEAD").ok().and_then(|r| {
                r.symbolic_target()
                    .ok()
                    .flatten()
                    .and_then(|t| t.strip_prefix("refs/heads/"))
                    .map(str::to_owned)
            });
        }
        Err(e) => return Err(e.into()),
    }

    out.dirty = dirty_summary(repo)?;

    let branch = out.branch.clone().unwrap_or_else(|| def.branch.clone());
    if let Some(upstream) = repo
        .find_branch(&branch, git2::BranchType::Local)
        .ok()
        .and_then(|b| b.upstream().ok())
    {
        out.upstream = upstream.name().ok().flatten().map(str::to_owned);
        if let (Some(local), Some(remote)) = (head_target, upstream.get().target()) {
            let (ahead, behind) = repo.graph_ahead_behind(local, remote)?;
            out.ahead = ahead;
            out.behind = behind;
        }
    }

    // §6.6, checked before the cascade because it outranks every entry in it: a repo left
    // mid-merge, mid-rebase or mid-cherry-pick is REPORTED, never repaired. Our own merge
    // is pure (§7.6) and never enters RepositoryState::Merge, so a non-Clean state here was
    // created by a human running git by hand in the tree, and an automatic cleanup_state()
    // or reset --hard would throw away their staged resolution unrecoverably. Returning Err
    // hands `status` the "error" state and attaches the reason; everything above is already
    // filled in, so head, branch, dirty and ahead/behind still reach the caller.
    //
    // The same call `open_tree` makes, deliberately: one condition, one code, one message,
    // whether the caller asked for a status or a sync. That is what makes this message's
    // "discard it with POST …/reset" a description of the system rather than advice.
    require_clean_state(repo, &def.id)?;

    out.state = if out.unborn {
        "unborn"
    } else if out.detached {
        // Detached beats dirty: "you are not on a branch" is the fact that changes what
        // every other button in a UI is allowed to do.
        "detached"
    } else if out.dirty.count > 0 {
        "dirty"
    } else {
        "clean"
    };
    Ok(())
}

fn dirty_summary(repo: &git2::Repository) -> Result<DirtySummary, GitError> {
    let mut so = git2::StatusOptions::new();
    so.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .include_unmodified(false)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);
    let statuses = repo.statuses(Some(&mut so))?;

    let mut out = DirtySummary::default();
    // `count` and `truncated` are locals because `bucket` holds a mutable borrow of
    // `out` for the rest of the iteration.
    let mut count = 0usize;
    let mut truncated = false;
    for entry in statuses.iter() {
        count += 1;
        let s = entry.status();
        // One bucket per entry, most specific bit first: a rename carries the modified
        // bits too, and a path that is INDEX_NEW is `added` even though it also looks
        // new to anyone who has not read the index.
        let bucket = if s.is_index_renamed() || s.is_wt_renamed() {
            &mut out.renamed
        } else if s.is_index_new() {
            &mut out.added
        } else if s.is_wt_new() {
            &mut out.untracked
        } else if s.is_index_deleted() || s.is_wt_deleted() {
            &mut out.deleted
        } else {
            &mut out.modified
        };
        if bucket.len() >= MAX_DIRTY_FILES {
            truncated = true;
            continue;
        }
        // `StatusEntry::path()` is `Result<&str>` and simply fails on a non-UTF-8 index
        // entry, which is legal in git. Decoding the raw bytes keeps the name
        // addressable on unix and turns the genuinely unrepresentable Windows case into
        // one clear io_failed instead of a silently mangled path.
        let path = crate::git::util::bytes_to_path(entry.path_bytes())?;
        bucket.push(path.to_string_lossy().into_owned());
    }
    out.count = count;
    out.truncated = truncated;
    Ok(out)
}
```

- [ ] **Step 28: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::ops::tests::status_`

Expected: PASS.

```
running 5 tests
test git::ops::tests::status_buckets_paths_and_truncates_at_the_cap ... ok
test git::ops::tests::status_reports_a_detached_head ... ok
test git::ops::tests::status_reports_a_non_clean_repository_state_as_error ... ok
test git::ops::tests::status_reports_the_upstream_and_ahead_behind ... ok
test git::ops::tests::status_walks_absent_uninitialized_unborn_clean_and_dirty ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 29: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): read-path types and a total status()

status() never fails: a repo it cannot read reports state=error with
the reason attached, because a polled endpoint that 500s tells the
caller nothing about which of its repos is broken.

A non-Clean repo.state() is the same shape (spec 6.6): an interrupted
merge, rebase or cherry-pick is reported as state=error naming the
state and POST /reset, never silently repaired, because our own merge
never produces one and cleanup_state() would discard a human's staged
resolution.

Dirty paths are bucketed one entry at a time, most specific status bit
first, and each bucket is capped at MAX_DIRTY_FILES with count keeping
the true total. Paths go through util::bytes_to_path: StatusEntry::path
is a Result that simply fails on the non-UTF-8 index entries git
permits."
```

---

- [ ] **Step 30: Write the failing test — `branches`**

Add to the same test module:

```rust
    #[test]
    fn branches_lists_local_and_remote_and_names_the_current_one() {
        let fx = Fixture::with_origin_head("main");
        fx.seed("main", &[("f.md", "one\n")]);
        let def = fx.remote_def("notes", "main");
        let op = fx.op(def.clone(), JobOp::Clone, OpRequest::default());
        clone(&op).expect("clone");

        let repo = open(&fx.tree("notes"));
        let tip = repo.head().expect("head").peel_to_commit().expect("commit");
        repo.branch("side", &tip, false).expect("branch");
        drop(tip);
        drop(repo);

        let out = branches(&fx.tree("notes"), &def).expect("branches");
        assert_eq!(out.current.as_deref(), Some("main"));
        assert!(!out.detached);
        assert_eq!(out.local, vec!["main".to_string(), "side".to_string()]);
        assert_eq!(
            out.remote,
            vec!["origin/main".to_string()],
            "origin/HEAD is a symbolic alias, not a branch anyone can check out"
        );
        assert_eq!(out.upstream.as_deref(), Some("origin/main"));

        let missing = branches(&fx.tree("nope"), &fx.local_def("nope", "main"))
            .expect_err("no tree on disk");
        assert_eq!(missing.code(), GitErrorCode::NoWorktree);
    }
```

- [ ] **Step 31: Run the test and watch it fail**

Run: `cargo test --bin hitch git::ops::tests::branches_`

Expected: FAIL — the file does not compile:

```
error[E0425]: cannot find function `branches` in this scope
   --> src/git/ops.rs:NNN:19
    |
NNN |         let out = branches(&fx.tree("notes"), &def).expect("branches");
    |                   ^^^^^^^^ not found in this scope
```

- [ ] **Step 32: Implement `branches`**

Append to `src/git/ops.rs`:

```rust
pub fn branches(tree: &std::path::Path, def: &RepoDef) -> Result<BranchesResponse, GitError> {
    if !tree.exists() {
        return Err(GitError::no_worktree(&def.id));
    }
    let repo = git2::Repository::open(tree)?;
    let detached = repo.head_detached()?;

    let mut current = None;
    let mut local = Vec::new();
    for entry in repo.branches(Some(git2::BranchType::Local))? {
        let (b, _) = entry?;
        if let Some(name) = b.name()? {
            if b.is_head() && !detached {
                current = Some(name.to_string());
            }
            local.push(name.to_string());
        }
    }

    let mut remote = Vec::new();
    for entry in repo.branches(Some(git2::BranchType::Remote))? {
        let (b, _) = entry?;
        if let Some(name) = b.name()? {
            // `origin/HEAD` is a symbolic alias for whatever the remote's default is;
            // listing it makes a branch picker show the same ref twice.
            if name.ends_with("/HEAD") {
                continue;
            }
            remote.push(name.to_string());
        }
    }

    // libgit2 walks refs in an order that is stable but not obviously so; sorting makes
    // the response diffable.
    local.sort();
    remote.sort();

    let upstream = match repo
        .find_branch(&def.branch, git2::BranchType::Local)
        .ok()
        .and_then(|b| b.upstream().ok())
    {
        Some(u) => u.name().ok().flatten().map(str::to_owned),
        None => None,
    };

    Ok(BranchesResponse {
        current,
        detached,
        local,
        remote,
        upstream,
    })
}
```

- [ ] **Step 33: Run the test and watch it pass**

Run: `cargo test --bin hitch git::ops::tests::branches_`

Expected: PASS.

```
running 1 test
test git::ops::tests::branches_lists_local_and_remote_and_names_the_current_one ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 34: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): branches() for the read API

origin/HEAD is filtered out: it is a symbolic alias for the remote's
default branch, and listing it makes a branch picker show the same ref
twice. Both lists are sorted so the response is diffable."
```

---

- [ ] **Step 35: Write the failing tests — `branch`**

Add to the same test module:

```rust
    #[test]
    fn branch_creates_and_checks_out() {
        let fx = Fixture::new();
        let repo = seeded_local(&fx, "notes");
        let before = head_oid(&repo);
        drop(repo);

        let request = OpRequest {
            branch: Some(BranchRequest {
                name: "side".to_string(),
                create: true,
                from: None,
                checkout: true,
                upstream: None,
            }),
            ..OpRequest::default()
        };
        let op = fx.op(fx.local_def("notes", "main"), JobOp::Branch, request);
        let out = branch(&op).expect("branch");
        assert_eq!(out.outcome, "branch_created");
        assert_eq!(out.branch, "side");
        assert_eq!(out.head_after, before, "a new branch points at the same commit");

        let repo = open(&fx.tree("notes"));
        assert_eq!(
            repo.head().expect("head").shorthand().expect("shorthand"),
            "side"
        );
    }

    #[test]
    fn branch_create_on_an_existing_name_is_already_exists() {
        let fx = Fixture::new();
        let repo = seeded_local(&fx, "notes");
        let tip = repo.head().expect("head").peel_to_commit().expect("commit");
        repo.branch("side", &tip, false).expect("branch");
        drop(tip);
        drop(repo);

        let request = OpRequest {
            branch: Some(BranchRequest {
                name: "side".to_string(),
                create: true,
                from: None,
                checkout: false,
                upstream: None,
            }),
            ..OpRequest::default()
        };
        let op = fx.op(fx.local_def("notes", "main"), JobOp::Branch, request);
        let err = branch(&op).expect_err("force=false must not clobber");
        assert_eq!(err.code(), GitErrorCode::AlreadyExists);
    }

    #[test]
    fn branch_checkout_onto_a_dirty_tree_is_refused() {
        let fx = Fixture::new();
        let repo = seeded_local(&fx, "notes");
        commit_file(&repo, "f.md", "one\n", "one");
        let tip = repo.head().expect("head").peel_to_commit().expect("commit");
        repo.branch("side", &tip, false).expect("branch");
        drop(tip);
        commit_file(&repo, "f.md", "two\n", "two");
        drop(repo);
        std::fs::write(fx.tree("notes").join("f.md"), "my local edit\n").expect("write");

        let request = OpRequest {
            branch: Some(BranchRequest {
                name: "side".to_string(),
                create: false,
                from: None,
                checkout: true,
                upstream: None,
            }),
            ..OpRequest::default()
        };
        let op = fx.op(fx.local_def("notes", "main"), JobOp::Branch, request);
        let err = branch(&op).expect_err("a dirty tree must block, not be clobbered");
        assert_eq!(err.code(), GitErrorCode::DirtyTree);
        assert_eq!(
            std::fs::read_to_string(fx.tree("notes").join("f.md")).expect("read"),
            "my local edit\n"
        );
    }

    #[test]
    fn branch_sets_and_unsets_the_upstream() {
        let fx = Fixture::with_origin_head("main");
        fx.seed("main", &[("f.md", "one\n")]);
        let def = fx.remote_def("notes", "main");
        clone(&fx.op(def.clone(), JobOp::Clone, OpRequest::default())).expect("clone");

        let unset = OpRequest {
            branch: Some(BranchRequest {
                name: "main".to_string(),
                create: false,
                from: None,
                checkout: false,
                upstream: Some(None),
            }),
            ..OpRequest::default()
        };
        let out = branch(&fx.op(def.clone(), JobOp::Branch, unset)).expect("unset upstream");
        assert!(!out.upstream_set);
        let repo = open(&fx.tree("notes"));
        assert!(repo
            .find_branch("main", git2::BranchType::Local)
            .expect("local main")
            .upstream()
            .is_err());
        drop(repo);

        let set = OpRequest {
            branch: Some(BranchRequest {
                name: "main".to_string(),
                create: false,
                from: None,
                checkout: false,
                upstream: Some(Some("origin/main".to_string())),
            }),
            ..OpRequest::default()
        };
        let out = branch(&fx.op(def, JobOp::Branch, set)).expect("set upstream");
        assert!(out.upstream_set);
        let repo = open(&fx.tree("notes"));
        let b = repo
            .find_branch("main", git2::BranchType::Local)
            .expect("local main");
        assert_eq!(
            b.upstream().expect("upstream").name().expect("name"),
            Some("origin/main")
        );
    }
```

- [ ] **Step 36: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::ops::tests::branch_`

Expected: FAIL — the file does not compile:

```
error[E0425]: cannot find function `branch` in this scope
   --> src/git/ops.rs:NNN:19
    |
NNN |         let out = branch(&op).expect("branch");
    |                   ^^^^^^ not found in this scope
```

- [ ] **Step 37: Implement `branch`**

A task-6 stub, so a **replacement in place**. In `src/git/ops.rs`, replace the stub

```rust
pub fn branch(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("branch is not implemented yet"))
}
```

with:

```rust
pub fn branch(ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    let request = ctx.request.branch.as_ref().ok_or_else(|| {
        GitError::invalid_request("branch", "a branch operation needs a branch body")
    })?;
    let repo = open_tree(ctx)?;
    let head_before = head_oid(&repo);

    let mut created = false;
    if request.create {
        let from = match &request.from {
            Some(rev) => repo.revparse_single(rev)?.peel_to_commit()?,
            None => repo.head()?.peel_to_commit()?,
        };
        // force = false: a clash is `already_exists`, never a silent move of somebody
        // else's branch onto a different commit.
        repo.branch(&request.name, &from, false)?;
        created = true;
    }

    if request.checkout {
        ctx.phase(Phase::CheckingOut);
        let object = repo.revparse_single(&format!("refs/heads/{}", request.name))?;
        let mut co = git2::build::CheckoutBuilder::new();
        // safe(), not force(): a dirty tree must block the switch, not be clobbered by it.
        co.safe();
        repo.checkout_tree(&object, Some(&mut co))
            .map_err(|e| GitError::from(e).as_dirty_tree_if_conflict())?;
        // AFTER checkout_tree: the other order leaves the index describing the branch
        // you just left.
        repo.set_head(&format!("refs/heads/{}", request.name))?;
    }

    let mut upstream_set = false;
    if let Some(upstream) = &request.upstream {
        let mut b = repo.find_branch(&request.name, git2::BranchType::Local)?;
        b.set_upstream(upstream.as_deref())?; // None unsets
        upstream_set = upstream.is_some();
    }

    // `BranchBody.checkout` defaults to true, so the non-create path is a checkout in
    // every request the API can produce. A caller that turns both off still gets a
    // truthful head_before/head_after pair.
    let mut out = OpOutcome::new(
        if created { "branch_created" } else { "checked_out" },
        &request.name,
    );
    out.head_before = head_before;
    out.head_after = head_oid(&repo);
    out.upstream_set = upstream_set;
    Ok(out)
}
```

- [ ] **Step 38: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::ops::tests::branch_`

Expected: PASS.

```
running 4 tests
test git::ops::tests::branch_checkout_onto_a_dirty_tree_is_refused ... ok
test git::ops::tests::branch_create_on_an_existing_name_is_already_exists ... ok
test git::ops::tests::branch_creates_and_checks_out ... ok
test git::ops::tests::branch_sets_and_unsets_the_upstream ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 39: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): the branch verb

force=false on branch creation so a name clash is already_exists rather
than a silent move of somebody else's ref. checkout uses safe(), not
force(), so a dirty tree blocks the switch, and the resulting
ErrorCode::Conflict is relabelled dirty_tree because nothing was being
merged. checkout_tree runs before set_head, which is the documented
order."
```

---

- [ ] **Step 40: Write the failing tests — `reset`**

Add to the same test module:

```rust
    fn reset_request(to: &str, clean_untracked: bool, confirm: bool) -> OpRequest {
        OpRequest {
            reset: Some(ResetRequest {
                to: to.to_string(),
                clean_untracked,
                confirm,
            }),
            ..OpRequest::default()
        }
    }

    #[test]
    fn reset_without_confirmation_is_refused() {
        let fx = Fixture::new();
        drop(seeded_local(&fx, "notes"));
        let op = fx.op(
            fx.local_def("notes", "main"),
            JobOp::Reset,
            reset_request("head", false, false),
        );
        let err = reset(&op).expect_err("reset discards work");
        assert_eq!(err.code(), GitErrorCode::ConfirmRequired);
    }

    #[test]
    fn reset_to_head_discards_a_modification_and_keeps_untracked() {
        let fx = Fixture::new();
        let repo = seeded_local(&fx, "notes");
        commit_file(&repo, "f.md", "one\n", "one");
        let before = head_oid(&repo);
        drop(repo);
        std::fs::write(fx.tree("notes").join("f.md"), "edited\n").expect("write");
        std::fs::write(fx.tree("notes").join("scratch.txt"), "mine\n").expect("write");

        let op = fx.op(
            fx.local_def("notes", "main"),
            JobOp::Reset,
            reset_request("head", false, true),
        );
        let out = reset(&op).expect("reset");
        assert_eq!(out.outcome, "reset");
        assert_eq!(out.head_after, before);
        assert_eq!(
            std::fs::read_to_string(fx.tree("notes").join("f.md")).expect("read"),
            "one\n"
        );
        assert!(
            fx.tree("notes").join("scratch.txt").exists(),
            "an untracked file survives unless clean_untracked was asked for"
        );
    }

    #[test]
    fn reset_with_clean_untracked_removes_it_but_spares_ignored_files() {
        let fx = Fixture::new();
        let repo = seeded_local(&fx, "notes");
        std::fs::write(fx.tree("notes").join(".gitignore"), "secret.env\n").expect("write");
        commit_file(&repo, ".gitignore", "secret.env\n", "ignore secrets");
        drop(repo);
        std::fs::write(fx.tree("notes").join("scratch.txt"), "mine\n").expect("write");
        std::fs::write(fx.tree("notes").join("secret.env"), "TOKEN=hunter2").expect("write");

        let op = fx.op(
            fx.local_def("notes", "main"),
            JobOp::Reset,
            reset_request("head", true, true),
        );
        reset(&op).expect("reset");
        assert!(!fx.tree("notes").join("scratch.txt").exists());
        assert!(
            fx.tree("notes").join("secret.env").exists(),
            "remove_ignored is never set: a discard button over HTTP must not delete .env"
        );
    }

    #[test]
    fn reset_to_upstream_rewinds_a_local_commit() {
        let fx = Fixture::with_origin_head("main");
        fx.seed("main", &[("f.md", "one\n")]);
        let def = fx.remote_def("notes", "main");
        clone(&fx.op(def.clone(), JobOp::Clone, OpRequest::default())).expect("clone");

        let repo = open(&fx.tree("notes"));
        let upstream = head_oid(&repo);
        commit_file(&repo, "f.md", "local only\n", "local only");
        assert_ne!(head_oid(&repo), upstream);
        drop(repo);

        let op = fx.op(def, JobOp::Reset, reset_request("upstream", false, true));
        let out = reset(&op).expect("reset");
        assert_eq!(out.head_after, upstream);
        assert_eq!(
            std::fs::read_to_string(fx.tree("notes").join("f.md")).expect("read"),
            "one\n"
        );
    }

    #[test]
    fn reset_on_a_detached_head_is_refused() {
        let fx = Fixture::new();
        let repo = seeded_local(&fx, "notes");
        let oid = commit_file(&repo, "f.md", "one\n", "one");
        repo.set_head_detached(oid).expect("detach");
        drop(repo);

        let op = fx.op(
            fx.local_def("notes", "main"),
            JobOp::Reset,
            reset_request("head", false, true),
        );
        let err = reset(&op).expect_err("we do not know what the caller meant");
        assert_eq!(err.code(), GitErrorCode::DetachedHead);
    }
```

- [ ] **Step 41: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::ops::tests::reset_`

Expected: FAIL — the file does not compile:

```
error[E0425]: cannot find function `reset` in this scope
   --> src/git/ops.rs:NNN:19
    |
NNN |         let err = reset(&op).expect_err("reset discards work");
    |                   ^^^^^ not found in this scope
```

- [ ] **Step 42: Implement `reset`**

The last of the five task-6 stubs this task owns, so again a **replacement in place**. In
`src/git/ops.rs`, replace the stub

```rust
pub fn reset(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("reset is not implemented yet"))
}
```

with:

```rust
pub fn reset(ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    let request = ctx.request.reset.as_ref().ok_or_else(|| {
        GitError::invalid_request("reset", "a reset operation needs a reset body")
    })?;
    if !request.confirm {
        return Err(GitError::confirm_required());
    }
    // The one verb that may open a wedged tree, because it is the one that repairs it:
    // libgit2's `git_reset` ends every non-soft reset in `git_repository_state_cleanup`,
    // which is what the refusal message in `require_clean_state` promises. Routing this
    // through the gated `open_tree` would close the only escape hatch there is.
    let repo = open_tree_any_state(ctx)?;
    require_branch(&repo, &ctx.def.branch)?;
    let head_before = head_oid(&repo);

    ctx.phase(Phase::CheckingOut);
    // `OpRequest` is constructible in-process, not only from JSON, so the api.rs default
    // is not the only thing standing between us and an empty spec.
    let spec = if request.to.is_empty() {
        "head"
    } else {
        request.to.as_str()
    };
    let target = match spec {
        "head" => repo.head()?.peel(git2::ObjectType::Commit)?,
        "upstream" => {
            let local = repo.find_branch(&ctx.def.branch, git2::BranchType::Local)?;
            let upstream = local.upstream()?;
            upstream.get().peel(git2::ObjectType::Commit)?
        }
        rev => repo.revparse_single(rev)?.peel(git2::ObjectType::Commit)?,
    };

    // libgit2 hard-codes opts.checkout_strategy = GIT_CHECKOUT_FORCE for a hard reset
    // (reset.c), silently discarding ANY CheckoutBuilder strategy passed here. Passing
    // one is a no-op, so pass None and do the untracked cleanup separately.
    repo.reset(&target, git2::ResetType::Hard, None)?;

    let mut co = git2::build::CheckoutBuilder::new();
    co.force();
    if request.clean_untracked {
        co.remove_untracked(true);
    }
    // remove_ignored is NEVER set: it deletes .env-style files, and a "discard local
    // changes" button reachable over HTTP must not be able to do that.
    repo.checkout_head(Some(&mut co))?;

    let mut out = OpOutcome::new("reset", &ctx.def.branch);
    out.head_before = head_before;
    out.head_after = head_oid(&repo);
    Ok(out)
}
```

- [ ] **Step 43: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::ops::tests::reset_`

Expected: PASS.

```
running 5 tests
test git::ops::tests::reset_on_a_detached_head_is_refused ... ok
test git::ops::tests::reset_to_head_discards_a_modification_and_keeps_untracked ... ok
test git::ops::tests::reset_to_upstream_rewinds_a_local_commit ... ok
test git::ops::tests::reset_with_clean_untracked_removes_it_but_spares_ignored_files ... ok
test git::ops::tests::reset_without_confirmation_is_refused ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 44: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): the reset verb, with untracked cleanup as a second pass

libgit2 hard-codes GIT_CHECKOUT_FORCE for a hard reset and silently
discards any CheckoutBuilder handed to Repository::reset, so the
untracked cleanup has to be a separate checkout_head. remove_ignored is
never set: a discard-local-changes button reachable over HTTP must not
be able to delete .env."
```

---

- [ ] **Step 45: Write the failing tests — `purge_tree`**

Add to the same test module:

```rust
    #[test]
    fn purge_removes_a_tree_under_the_repos_root() {
        let fx = Fixture::new();
        std::fs::create_dir_all(fx.tree("notes").join("sub")).expect("mkdir");
        std::fs::write(fx.tree("notes").join("sub/f.txt"), "x").expect("write");
        purge_tree(&fx.repos_root_canon(), &fx.tree("notes")).expect("purge");
        assert!(!fx.tree("notes").exists());
    }

    #[test]
    fn purge_of_a_missing_tree_is_a_no_op() {
        let fx = Fixture::new();
        // DELETE is idempotent; a repo that was never cloned has nothing to purge.
        purge_tree(&fx.repos_root_canon(), &fx.tree("never-cloned")).expect("purge");
    }

    #[test]
    fn purge_refuses_to_delete_the_repos_root_itself() {
        let fx = Fixture::new();
        let root = fx.repos_root_canon();
        let err = purge_tree(&root, &fx.repos_root).expect_err("the root is not a repo");
        assert_eq!(err.code(), GitErrorCode::PathRefused);
        assert!(fx.repos_root.exists());

        let outside = fx.scratch("elsewhere");
        std::fs::create_dir_all(&outside).expect("mkdir");
        let escape = fx.repos_root.join("..").join("elsewhere");
        let err = purge_tree(&root, &escape).expect_err("resolves outside the root");
        assert_eq!(err.code(), GitErrorCode::PathRefused);
        assert!(outside.exists());
    }

    #[cfg(unix)]
    #[test]
    fn purge_refuses_a_symlinked_tree() {
        let fx = Fixture::new();
        let precious = fx.scratch("precious");
        std::fs::create_dir_all(&precious).expect("mkdir");
        std::fs::write(precious.join("keep.txt"), "keep").expect("write");
        // `repos/<id>` is a pre-existing symlink pointing out of the root. Canonicalizing
        // alone would follow it and pass the containment check against the *target*,
        // which is exactly how a purge deletes the wrong directory.
        std::os::unix::fs::symlink(&precious, fx.tree("notes")).expect("symlink");

        let err = purge_tree(&fx.repos_root_canon(), &fx.tree("notes"))
            .expect_err("a symlinked tree is refused outright");
        assert_eq!(err.code(), GitErrorCode::PathRefused);
        assert!(precious.join("keep.txt").exists());
    }
```

- [ ] **Step 46: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::ops::tests::purge_`

Expected: FAIL — the file does not compile:

```
error[E0425]: cannot find function `purge_tree` in this scope
   --> src/git/ops.rs:NNN:9
    |
NNN |         purge_tree(&fx.repos_root_canon(), &fx.tree("notes")).expect("purge");
    |         ^^^^^^^^^^ not found in this scope
```

- [ ] **Step 47: Implement `purge_tree`**

Append to `src/git/ops.rs`:

```rust
/// Delete a working tree, with two independent barriers against deleting the wrong one:
/// the repo-id charset (no `/`, no `..`, no absolute path expressible) upstream in
/// `registry::valid_id`, and canonicalized containment here, which catches a
/// pre-existing symlink sitting at `repos/<id>`.
pub fn purge_tree(
    repos_root_canon: &std::path::Path,
    tree: &std::path::Path,
) -> Result<(), GitError> {
    let meta = match std::fs::symlink_metadata(tree) {
        Ok(meta) => meta,
        // Nothing to purge is a success, not an io_failed: DELETE is idempotent.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(GitError::io(format!("{}: {e}", tree.display()))),
    };
    if meta.file_type().is_symlink() {
        // Refused before canonicalize, not after: canonicalize resolves the link and the
        // containment check would then be answering a question about the target.
        return Err(GitError::path_refused(tree, "is a symlink"));
    }
    let real = std::fs::canonicalize(tree)
        .map_err(|e| GitError::io(format!("{}: {e}", tree.display())))?;
    if real.as_path() == repos_root_canon || !real.starts_with(repos_root_canon) {
        return Err(GitError::path_refused(
            &real,
            "resolves outside the repos root",
        ));
    }
    std::fs::remove_dir_all(&real).map_err(|e| GitError::io(format!("{}: {e}", real.display())))
}
```

- [ ] **Step 48: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::ops::tests::purge_`

Expected: PASS.

```
running 4 tests
test git::ops::tests::purge_of_a_missing_tree_is_a_no_op ... ok
test git::ops::tests::purge_refuses_a_symlinked_tree ... ok
test git::ops::tests::purge_refuses_to_delete_the_repos_root_itself ... ok
test git::ops::tests::purge_removes_a_tree_under_the_repos_root ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 49: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): purge_tree with canonicalized containment

Two barriers, both required. The symlink check runs BEFORE canonicalize:
canonicalize resolves the link, after which the containment check is
answering a question about the target rather than about repos/<id>.
A missing tree is a success, so DELETE stays idempotent."
```

---

- [ ] **Step 50: Write the regression test — `run` dispatch**

Add to the same test module:

```rust
    #[test]
    fn run_dispatches_every_verb_this_task_owns() {
        let fx = Fixture::new();
        let def = fx.local_def("notes", "main");

        let out = run(JobOp::Init, &fx.op(def.clone(), JobOp::Init, OpRequest::default()))
            .expect("run init");
        assert_eq!(out.outcome, "initialized");

        let repo = open(&fx.tree("notes"));
        commit_file(&repo, "f.md", "one\n", "one");
        drop(repo);
        std::fs::write(fx.tree("notes").join("g.md"), "two\n").expect("write");

        let out = run(
            JobOp::Commit,
            &fx.op(def.clone(), JobOp::Commit, OpRequest::default()),
        )
        .expect("run commit");
        assert_eq!(out.outcome, "committed");

        let out = run(
            JobOp::Branch,
            &fx.op(
                def.clone(),
                JobOp::Branch,
                OpRequest {
                    branch: Some(BranchRequest {
                        name: "side".to_string(),
                        create: true,
                        from: None,
                        checkout: true,
                        upstream: None,
                    }),
                    ..OpRequest::default()
                },
            ),
        )
        .expect("run branch");
        assert_eq!(out.outcome, "branch_created");

        let out = run(
            JobOp::Reset,
            &fx.op(
                repo_def("notes", "side", None),
                JobOp::Reset,
                reset_request("head", false, true),
            ),
        )
        .expect("run reset");
        assert_eq!(out.outcome, "reset");

        let err = run(JobOp::Sync, &fx.op(def, JobOp::Sync, OpRequest::default()))
            .expect_err("sync arrives in the next task");
        assert_eq!(err.code(), GitErrorCode::Internal);
    }
```

- [ ] **Step 51: Run the test and watch it pass**

Run: `cargo test --bin hitch git::ops::tests::run_dispatches`

Expected: PASS on the very first run. That is correct here rather than a missing red step:
task 6's Step 48 already landed `run` with all eight arms dispatching to a verb of its own
name, and Steps 8, 15, 21, 37 and 42 replaced five of those verbs **in place**. There is no
wiring left to add, so this test is a regression guard — it proves the five replacements kept
the stub signatures and that nobody appended a second definition alongside one.

The filter matches task 6's trimmed scaffolding test as well, and both are green:

```
running 2 tests
test git::ops::tests::run_dispatches_every_verb_this_task_owns ... ok
test git::ops::tests::run_dispatches_to_a_verb_of_the_same_name ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 52: Confirm `run`'s dispatch table**

Nothing to edit — this step is a read, and it is here because it is the one place a silent
`E0428` mistake would otherwise hide. Open `pub fn run` in `src/git/ops.rs` and check it still
reads exactly this:

```rust
pub fn run(op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    match op {
        JobOp::Init => init(ctx),
        JobOp::Clone => clone(ctx),
        // Still task 6's stubs, still returning internal("<verb> is not implemented
        // yet"). Task 8 replaces those three functions in place, exactly as this task
        // replaced its five, so the arms themselves never change again. Do NOT collapse
        // them into a single Err(GitError::internal(..)): that would delete the only
        // thing task 6's scaffolding test asserts about them.
        JobOp::Pull => pull(ctx),
        JobOp::Push => push(ctx),
        JobOp::Sync => sync(ctx),
        JobOp::Commit => commit(ctx),
        JobOp::Branch => branch(ctx),
        JobOp::Reset => reset(ctx),
    }
}
```

If yours differs — an arm collapsed, or a second `pub fn init` further down the file — you
appended a verb instead of replacing its stub, and `cargo build` will already have said
`error[E0428]: the name 'init' is defined multiple times`.

- [ ] **Step 53: Run the whole ops module**

Run: `cargo test --bin hitch git::ops`

Expected: PASS — everything task 6 left in the module plus everything this task has added, with
no `is not implemented yet` failure anywhere.

- [ ] **Step 54: Commit**

```bash
git add src/git/ops.rs
git commit -m "test(git): pin ops::run's dispatch table

Task 6's run already routes every JobOp to a verb of its own name, and
this task replaced five of those verbs in place rather than appending
new ones, so there is no wiring to add here — only the regression test
that says so. Pull, push and sync still reach task 6's stubs until task
8 replaces them the same way."
```

---

- [ ] **Step 55: Write the failing test — the process-global libgit2 timeouts**

Add this test **inside** the existing `#[cfg(test)] mod tests` block in `src/git/mod.rs`
(the one holding `libgit2_is_vendored_with_network_transports`):

```rust
    /// Without these, a black-holed remote pins a `spawn_blocking` thread indefinitely:
    /// our own watchdog lives inside the transfer callbacks, and those never fire before
    /// the transport has produced data. Both libgit2 options default to 0, meaning
    /// "whatever the OS decides", which on Linux is around two hours.
    #[test]
    fn network_timeouts_are_installed_from_the_configured_value() {
        // AMENDED by the post-execution audit (second round): this used to call
        // `init_libgit2_timeouts(45)` and read the pair back with a SAFETY note claiming
        // "nothing else in this test binary writes it". Seven other tests reach
        // `GitService::start`, which installs whatever *their* config says — the same
        // shape of process-global race as the `init.defaultBranch` one in
        // `merge::testkit`, and the same answer: one gate, every writer through it.
        //
        // Under the same gate as the write, and for the reason `TIMEOUTS` documents:
        // seven other tests in this binary reach `GitService::start`, which installs
        // whatever *their* config says. Read outside the gate, this asserts on
        // whichever of them ran last.
        let _guard = super::timeouts_guard();
        super::init_libgit2_timeouts_locked(45).expect("libgit2 accepts both timeouts");
        // SAFETY: reads libgit2 process-global state, holding the lock every writer
        // takes, so the pair cannot move underneath these two assertions.
        unsafe {
            assert_eq!(
                git2::opts::get_server_connect_timeout_in_milliseconds().expect("connect"),
                super::CONNECT_TIMEOUT_MS
            );
            assert_eq!(
                git2::opts::get_server_timeout_in_milliseconds().expect("server"),
                45_000
            );
        }
    }
```

- [ ] **Step 56: Run the test and watch it fail**

Run: `cargo test --bin hitch git::tests::network_timeouts`

Expected: FAIL — the file does not compile:

```
error[E0425]: cannot find function `init_libgit2_timeouts` in module `super`
   --> src/git/mod.rs:NN:16
    |
NN |         super::init_libgit2_timeouts(45).expect("libgit2 accepts both timeouts");
    |                ^^^^^^^^^^^^^^^^^^^^ not found in `super`

error[E0425]: cannot find value `CONNECT_TIMEOUT_MS` in module `super`
   --> src/git/mod.rs:NN:17
    |
NN |                 super::CONNECT_TIMEOUT_MS
    |                        ^^^^^^^^^^^^^^^^^^ not found in `super`
```

- [ ] **Step 57: Implement `init_libgit2_timeouts`**

Add to `src/git/mod.rs`, after the `pub mod` declarations and before the
`#[cfg(test)] mod tests` block:

```rust
/// How long libgit2 may spend establishing a connection before giving up.
///
/// Deliberately not a config key: it is a liveness floor, not a policy, and a caller who
/// wants to wait longer for a slow server wants `[git].network_timeout_secs`.
pub const CONNECT_TIMEOUT_MS: i32 = 10_000;

/// Serialises the two process-global timeout options against each other.
///
/// In a shipped build `GitService::start` is the only writer and runs once, so this
/// costs one uncontended lock for the life of the process. It exists for the test
/// binary, where the claim "one caller, once, on the main thread" is simply false:
/// seven tests reach `start()` on tokio worker threads while an eighth installs a
/// distinctive value and reads it straight back.
static TIMEOUTS: Mutex<()> = Mutex::new(());

/// A poisoned lock here guards no invariant — the protected value is libgit2's, not
/// ours, and a panicking writer leaves it exactly as consistent as it found it.
fn timeouts_guard() -> std::sync::MutexGuard<'static, ()> {
    TIMEOUTS.lock().unwrap_or_else(|p| p.into_inner())
}

/// Install the process-global libgit2 network timeouts. Call once, from
/// `GitService::start`, before any repository is opened.
///
/// Without these, a black-holed remote pins a `spawn_blocking` thread indefinitely: the
/// job watchdog lives inside the transfer callbacks, which never fire before the
/// transport has data. Both libgit2 options default to 0, meaning "the OS default".
pub fn init_libgit2_timeouts(network_timeout_secs: u64) -> Result<(), crate::git::error::GitError> {
    let _guard = timeouts_guard();
    init_libgit2_timeouts_locked(network_timeout_secs)
}

/// The write itself, minus the gate.
///
/// Split out because `std::sync::Mutex` is not reentrant: the test that reads the
/// pair back has to hold `TIMEOUTS` across both the write and the read, so it cannot
/// go through the wrapper above. The caller must already hold `timeouts_guard()`.
fn init_libgit2_timeouts_locked(
    network_timeout_secs: u64,
) -> Result<(), crate::git::error::GitError> {
    let total = i32::try_from(network_timeout_secs.saturating_mul(1000)).unwrap_or(i32::MAX);
    // SAFETY: both setters mutate libgit2 process-global state and are documented as
    // needing external synchronisation. `TIMEOUTS`, held by the caller, is that
    // synchronisation. In a shipped build the stronger property also holds: the single
    // caller runs once, on the main thread, during startup and before any repository
    // has been opened, so no other thread can be inside libgit2 at this point.
    unsafe {
        git2::opts::set_server_connect_timeout_in_milliseconds(CONNECT_TIMEOUT_MS).map_err(|e| {
            crate::git::error::GitError::internal(format!("libgit2 connect timeout: {e}"))
        })?;
        git2::opts::set_server_timeout_in_milliseconds(total).map_err(|e| {
            crate::git::error::GitError::internal(format!("libgit2 server timeout: {e}"))
        })?;
    }
    Ok(())
}
```

- [ ] **Step 58: Run the test and watch it pass**

Run: `cargo test --bin hitch git::tests::`

Expected: PASS.

```
running 2 tests
test git::tests::libgit2_is_vendored_with_network_transports ... ok
test git::tests::network_timeouts_are_installed_from_the_configured_value ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 59: Commit**

```bash
git add src/git/mod.rs
git commit -m "feat(git): process-global libgit2 network timeouts

Both libgit2 options default to 0 (the OS default, hours on Linux).
Without them a black-holed remote pins a spawn_blocking thread forever,
because the job watchdog lives inside the transfer callbacks and those
never fire before the transport has produced data.

GitService::start (task 9) is the single caller."
```

---

- [ ] **Step 60: Run the whole suite, fmt and clippy**

Run:

```bash
cargo fmt
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --bin hitch
```

Expected: all four clean. The two most likely clippy findings and their fixes:

- `clippy::too_many_arguments` on nothing here — but `clippy::needless_range_loop` may fire
  on the truncation test's `for i in 0..(MAX_DIRTY_FILES + 5)`. It does not (there is no
  slice being indexed); if a future edit introduces one, use `.map(|i| …)`.
- `clippy::manual_let_else` on the `let repo = match … { Ok(r) => r, Err(_) => { … return … } }`
  in `status`: the `Err` arm mutates `out` before returning, so it is not a `let … else`.
  If clippy disagrees on your toolchain, it is wrong here — do **not** restructure into a
  `let … else` that drops the `state = "uninitialized"` assignment.

Expected test summary (the git::ops count is the 40 tests this task added plus whatever
task 6 left in the module):

```
test result: ok. NNN passed; 0 failed; 1 ignored; 0 measured; 0 filtered out
```

The one ignored test is the pre-existing `chrome::tests::real_chrome_reports_version`.

- [ ] **Step 61: Commit any formatting fixup**

```bash
git add -A
git commit -m "style(git): cargo fmt after the ops core"
```

If `cargo fmt` changed nothing, skip this step — do not create an empty commit.

---

**Definition of done for Task 7**

- `cargo test --bin hitch` green, `cargo fmt --check` clean,
  `cargo clippy --all-targets -- -D warnings` clean.
- `src/git/ops.rs` names no `tokio`, no `axum`, no `App`, no `Children`, no `HostEvent`.
  Check with `rg -n 'tokio|axum|HostEvent|Children' src/git/ops.rs` — zero hits.
- `rg -n 'IndexAddOption::FORCE|remove_ignored|repo\.signature\(\)' src/git/ops.rs` — zero hits.
  All three are the shapes this task exists to keep out.
- `pub fn run` dispatches all eight arms to a verb of the same name, unchanged since task 6.
  `init`, `clone`, `commit`, `branch` and `reset` are real; `pull`, `push` and `sync` are still
  task 6's stubs returning `internal("<verb> is not implemented yet")` for task 8 to replace
  in place. `rg -n 'is not implemented yet' src/git/ops.rs` — exactly three hits.
- `run_dispatches_to_a_verb_of_the_same_name` loops over `[JobOp::Pull, JobOp::Push,
  JobOp::Sync]` and nothing else. Task 8 removes `JobOp::Push` from that list when `push`
  lands and expects to find the other two still there.
- `rg -n 'repo\.state\(\)' src/git/ops.rs` — one hit, in `fill_status`. Spec §6.6's non-Clean
  repo state is surfaced, not repaired.
