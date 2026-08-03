### Task 8: Prefer-local merge, sync, pull, push

This is the riskiest code in the feature, and every risk in it is a *silent* one: a wrong
branch order turns every fast-forward into a merge commit, a wrong `FileFavor` splices two
people's edits together inside one file, a missing dir/file guard deletes a local file and
reports success, and a missing `push_update_reference` drain reports a push that never
happened. None of those produce an error. All of them are caught only by a test that asserts
on the *bytes on disk* afterwards, which is what the §10.3 matrix at the end of this task is.

Three decisions run through the whole task, and the comments in the code say so:

1. **Never `Repository::merge`.** It writes `MERGE_HEAD`/`MERGE_MSG`, checks out immediately,
   and leaves `RepositoryState::Merge` behind until someone calls `cleanup_state()`.
   `merge_commits` / `merge_trees` are *pure*: they return a detached in-memory `Index` and
   touch nothing on disk. Every failure path before the `commit` line is therefore a no-op on
   disk, and a conflict marker can never reach the working tree even transiently.
2. **`MergeOptions::new()`, never `FileFavor::Ours`.** `Ours` resolves at *hunk* granularity —
   it splices the two sides together inside one file, which is not whole-file prefer-local.
   The `non_overlapping_edits_auto_merge` test is the regression guard.
3. **The four merge-analysis tests run in the order up-to-date → unborn → fast-forward →
   normal.** libgit2 sets `ANALYSIS_FASTFORWARD` *together with* `ANALYSIS_NORMAL` (and
   together with `ANALYSIS_UNBORN`), so testing `is_normal()` first writes a spurious merge
   commit for every fast-forward.

**House rules for every step below:** unit tests live in a `#[cfg(test)] mod tests` at the
bottom of the file they test; `tempfile` for filesystem tests; comments explain WHY (an
invariant, a race, a verified libgit2 behaviour), never what; no `unwrap()` outside
`#[cfg(test)]` unless a panic is the correct behaviour and a comment says so; `cargo fmt` and
`cargo clippy --all-targets -- -D warnings` clean at every commit.

**`src/git/merge.rs` needs no `#![allow(dead_code)]` of its own.** `src/git/mod.rs` carries
`#![allow(dead_code)]` and `#![allow(clippy::result_large_err)]` from task 3, and lint levels
propagate down the module tree.

**This crate has no `--lib` target** (`src/main.rs` is the only entry point and
`src/bin/gen_icons.rs` is a second binary), so unit tests run in the `chrome-host-app` bin
target: `cargo test --bin chrome-host-app <filter>`.

**Where the tests live.** The §10.3 merge matrix is merge *policy*, so it lives in
`src/git/merge.rs`'s `mod tests` even though it is driven end-to-end through `ops::sync` — the
only honest way to assert on `outcome` strings and on the bytes left on disk. The
fetch/push/pull/sync mechanics live in `src/git/ops.rs`'s `mod tests`, next to the functions
they test. The shared local-git harness lives in `merge.rs` as `#[cfg(test)] pub(crate) mod
testkit` so both test modules use one copy.

---

**Files:**
- Create: `src/git/merge.rs`
- Modify: `src/git/mod.rs` — add `pub mod merge;` to the `pub mod` block, keeping it
  alphabetical: `creds`, `error`, `jobs`, `merge`, `ops`, `registry`, `secret`, `state`,
  `util`.
- Modify: `src/git/ops.rs` — extend two `use` lines; replace the `pull`, `push` and `sync`
  stubs; add `fetch`, `push_branch`, and the private `apply_merge` / `merge_report` /
  `record_ahead_behind`; add tests; delete the task-6 scaffolding test
  `run_dispatches_to_a_verb_of_the_same_name` once its list is empty.

**Interfaces:**

- **Consumes** (all already landed; signatures are exact):
  - *task 3* — `crate::git::error::{GitError, GitErrorCode}` with
    `GitError::internal(impl Into<String>)`,
    `GitError::remote_missing()`,
    `GitError::dirty_tree(what: &str)`,
    `GitError::unborn_branch(what: &str)`,
    `GitError::merge_path_type_conflict(path: &std::path::Path)`,
    `GitError::merge_unresolvable()`,
    `.with_repo(&str) -> GitError`, `.code() -> GitErrorCode`, and the **public** fields
    `pub code`, `pub message: String`.
    `GitErrorCode::{NotFastForward, UnbornBranch, DirtyTree, MergePathTypeConflict,
    MergeUnresolvable, RemoteMissing, NoWorktree}`.
  - *task 3* — `crate::git::util::{bytes_to_path, now_ms, utc_stamp}`:
    `bytes_to_path(&[u8]) -> Result<std::path::PathBuf, GitError>`, `now_ms() -> u64`,
    `utc_stamp(u64) -> String`.
  - *task 5* — `crate::git::jobs::{Admission, JobOp, JobStore, Phase, RepoLease}` with
    `JobStore::new(String) -> std::sync::Arc<JobStore>`,
    `JobStore::admit(&Arc<Self>, repo_id: &str, op: JobOp, request_id: Option<&str>)
    -> Result<Admission, GitError>`, `Admission::Started(Arc<JobSlot>, RepoLease)`,
    `Phase::{Fetching, Merging, CheckingOut, Pushing, Finalizing}`.
  - *task 4* — `crate::git::registry::{AuthorSpec, RepoDef}`; `RepoDef` is `Deserialize` with
    `deny_unknown_fields` and fields `id`, `remote: Option<String>`, `remote_name: String`
    (default `"origin"`), `branch: String`.
  - *task 6* — `crate::git::creds::{callbacks, HostKeyChecker, ResolvedCred}` with
    `callbacks<'a>(ctx: &'a OpCtx) -> git2::RemoteCallbacks<'a>` (its `push_update_reference`
    arm calls `ctx.record_push_status`), `HostKeyChecker::new(SshHostKeyPolicy, Option<String>)`.
  - *task 6* — `crate::git::ops::{Identity, OpCtx, OpOutcome, OpRequest, OpScratch,
    MergeReport, SettingsCtx}` with `OpCtx::phase(Phase)`, `OpCtx::author_name() -> String`,
    `OpCtx::author_email() -> String`, `OpCtx::classify_err(&git2::Error) -> GitError`,
    `OpCtx::take_push_rejections() -> Result<(), GitError>`,
    `OpOutcome::new(&'static str, &str) -> OpOutcome`, `OpRequest::default()` (with
    `push = true`), and `OpScratch::default()`. `OpCtx`'s fields are all public except
    `scratch`, which is `pub scratch: OpScratch`.
  - *task 7* — `crate::git::ops::{open_tree, require_branch, stage_and_commit, clone}`:
    `open_tree(&OpCtx) -> Result<git2::Repository, GitError>` (errors `no_worktree` when the
    tree is absent, and re-points the remote from `def.remote`),
    `require_branch(&git2::Repository, &str) -> Result<(), GitError>`,
    `stage_and_commit(&git2::Repository, &OpCtx) -> Result<(Option<git2::Oid>, usize), GitError>`
    (`git add -A` honouring `.gitignore`, then a commit only when the tree changed or
    `allow_empty`), `clone(&OpCtx) -> Result<OpOutcome, GitError>` (outcome `"cloned"`).
  - *task 2* — `crate::config::SshHostKeyPolicy::Tofu`.
  - *task 1* — `git2` 0.21 with `vendored-libgit2`, `https`, `ssh`; `tempfile` (dev-dep).

- **Produces** (later tasks rely on all of these):
  - `src/git/merge.rs`:
    `pub struct MergeResult { pub merge_commit: git2::Oid, pub conflicts_resolved: Vec<String>,
    pub unrelated: bool }`;
    `pub enum MergeOutcome { UpToDate, NoRemoteBranch, AdoptedRemote { head: git2::Oid },
    FastForward { head: git2::Oid }, Merged(MergeResult) }`;
    `pub fn merge_prefer_local(repo: &git2::Repository, their_oid: git2::Oid, ctx: &OpCtx)
    -> Result<Option<MergeResult>, GitError>`;
    `pub fn analyse(repo: &git2::Repository, ctx: &OpCtx) -> Result<MergeOutcome, GitError>`;
    `pub fn conflict_path_bytes(c: &git2::IndexConflict) -> Result<Vec<u8>, GitError>`.
  - `src/git/ops.rs`:
    `pub fn pull(ctx: &OpCtx) -> Result<OpOutcome, GitError>`;
    `pub fn push(ctx: &OpCtx) -> Result<OpOutcome, GitError>`;
    `pub fn sync(ctx: &OpCtx) -> Result<OpOutcome, GitError>`;
    `pub fn fetch(repo: &git2::Repository, ctx: &OpCtx) -> Result<Option<git2::Oid>, GitError>`;
    `pub fn push_branch(repo: &git2::Repository, ctx: &OpCtx, out: &mut OpOutcome)
    -> Result<(), GitError>`.
  - `src/git/merge.rs`, test-only, consumed by task 11's `sync_settings` tests:
    `#[cfg(test)] pub(crate) mod testkit` with `SEEDED: &str`, `Origin { pub root: TempDir,
    pub bare: PathBuf }`, `origin()`, `origin_with_main()`, `init_tree(&Origin, &str) -> PathBuf`,
    `clone_at(&Origin, &str) -> PathBuf`, `write_file(&Path, &str, &str)`,
    `read_file(&Path, &str) -> String`, `commit_all(&Path, &str) -> git2::Oid`,
    `push_main(&Path)`, `fetch_main(&Path) -> git2::Oid`, `head_of(&Path) -> git2::Oid`,
    `origin_main(&Origin) -> git2::Oid`, `Job { pub ctx: OpCtx, .. }`,
    `job(&Origin, &Path, JobOp) -> Job`, `job_local(&Path, JobOp) -> Job`,
    `job_with(Option<&Origin>, &Path, JobOp, OpRequest) -> Job`,
    `assert_no_merge_state(&Path)`.

- **Three deliberate refinements of the interface contract**, called out so the consistency
  audit sees them rather than flagging them:
  1. **`MergeResult` and `MergeOutcome` both `#[derive(Debug)]`.** The contract lists neither
     derives nor their absence. Neither type can hold a secret (an `Oid`, a `Vec<String>` of
     repo-relative paths, and a `bool`), and every `analyse` test reads
     `panic!("expected …, got {other:?}")`.
  2. **Three private helpers in `ops.rs` that the contract does not name**, because they are
     internal to this task's three verbs: `fn apply_merge(&git2::Repository, &OpCtx,
     &mut OpOutcome) -> Result<(), GitError>`, `fn merge_report(kind: &'static str) -> MergeReport`,
     and `fn record_ahead_behind(&git2::Repository, &OpCtx, &mut OpOutcome)`.
  3. **`testkit` is `pub(crate)`, not private to `merge.rs`**, so `ops.rs`'s tests use the same
     local-git harness instead of growing a second one. It is behind `#[cfg(test)]` and
     therefore absent from every non-test build.

---

- [ ] **Step 1: Write the failing test for the conflict path accessor**

Create `src/git/merge.rs` with exactly this content:

```rust
//! The prefer-local merge.
//!
//! Two clones of one repository edit the same file and neither user is present
//! to resolve the result. This module's whole job is to make that outcome
//! predictable: the local copy of a genuinely conflicting file wins whole, the
//! remote copy stays reachable as the merge commit's second parent, and the
//! working tree never sees a conflict marker.

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> git2::IndexEntry {
        git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode: 0o100644,
            uid: 0,
            gid: 0,
            file_size: 0,
            id: git2::Oid::zero(),
            flags: 0,
            flags_extended: 0,
            path: path.as_bytes().to_vec(),
        }
    }

    #[test]
    fn conflict_path_bytes_reads_whichever_stage_carries_it() {
        // Which stage holds the path depends on the conflict's shape: an
        // add/add has no ancestor and a delete/modify has no `our`, so a
        // resolver that only ever reads `our` drops half the conflicts.
        let ours_only = git2::IndexConflict {
            ancestor: None,
            our: Some(entry("a.md")),
            their: None,
        };
        assert_eq!(
            conflict_path_bytes(&ours_only).expect("our side carries the path"),
            b"a.md".to_vec()
        );

        let theirs_only = git2::IndexConflict {
            ancestor: None,
            our: None,
            their: Some(entry("b.md")),
        };
        assert_eq!(
            conflict_path_bytes(&theirs_only).expect("their side carries the path"),
            b"b.md".to_vec()
        );

        let ancestor_only = git2::IndexConflict {
            ancestor: Some(entry("c.md")),
            our: None,
            their: None,
        };
        assert_eq!(
            conflict_path_bytes(&ancestor_only).expect("the ancestor carries the path"),
            b"c.md".to_vec()
        );

        let empty = git2::IndexConflict {
            ancestor: None,
            our: None,
            their: None,
        };
        assert_eq!(
            conflict_path_bytes(&empty)
                .expect_err("a conflict with no side at all is a libgit2 bug, not a merge")
                .code(),
            GitErrorCode::Internal
        );
    }

    #[test]
    fn a_merge_outcome_names_what_it_did() {
        // The five shapes `analyse` can return, so that adding a sixth forces a
        // decision about what `ops` should report for it.
        let result = MergeResult {
            merge_commit: git2::Oid::zero(),
            conflicts_resolved: vec!["f.md".to_string()],
            unrelated: true,
        };
        assert!(matches!(MergeOutcome::UpToDate, MergeOutcome::UpToDate));
        assert!(matches!(
            MergeOutcome::NoRemoteBranch,
            MergeOutcome::NoRemoteBranch
        ));
        assert!(matches!(
            MergeOutcome::AdoptedRemote {
                head: git2::Oid::zero()
            },
            MergeOutcome::AdoptedRemote { .. }
        ));
        assert!(matches!(
            MergeOutcome::FastForward {
                head: git2::Oid::zero()
            },
            MergeOutcome::FastForward { .. }
        ));
        match MergeOutcome::Merged(result) {
            MergeOutcome::Merged(r) => {
                assert!(r.unrelated);
                assert_eq!(r.conflicts_resolved, vec!["f.md".to_string()]);
            }
            other => panic!("expected Merged, got {other:?}"),
        }
    }
}
```

Then register the module. In `src/git/mod.rs`, add `pub mod merge;` to the `pub mod` block so
the list reads:

```rust
pub mod creds;
pub mod error;
pub mod jobs;
pub mod merge;
pub mod ops;
pub mod registry;
pub mod secret;
pub mod state;
pub mod util;
```

- [ ] **Step 2: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app git::merge`

Expected: FAIL — compile error:

```
error[E0425]: cannot find function `conflict_path_bytes` in this scope
error[E0422]: cannot find struct, variant or union type `MergeResult` in this scope
error[E0433]: failed to resolve: use of undeclared type `MergeOutcome`
error[E0433]: failed to resolve: use of undeclared type `GitErrorCode`
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 12 previous errors
```

- [ ] **Step 3: Implement the merge vocabulary and `conflict_path_bytes`**

Insert this block into `src/git/merge.rs` immediately below the module doc comment and above
the `#[cfg(test)]` line.

```rust
use crate::git::error::{GitError, GitErrorCode};
use crate::git::jobs::Phase;
use crate::git::ops::OpCtx;
use crate::git::util::{bytes_to_path, now_ms, utc_stamp};

/// What a completed prefer-local merge did.
#[derive(Debug)]
pub struct MergeResult {
    pub merge_commit: git2::Oid,
    /// Repo-relative paths whose local copy was kept whole. Every one of them is
    /// still recoverable through the merge commit's second parent.
    pub conflicts_resolved: Vec<String>,
    /// The two histories had no common ancestor, so the merge ran two-way
    /// against an empty tree.
    pub unrelated: bool,
}

/// What `analyse` decided — and, for the two fast-forward shapes, already did.
#[derive(Debug)]
pub enum MergeOutcome {
    UpToDate,
    /// The remote has no such branch yet. Not an error: push creates it.
    NoRemoteBranch,
    AdoptedRemote {
        head: git2::Oid,
    },
    FastForward {
        head: git2::Oid,
    },
    Merged(MergeResult),
}

/// The path a conflict is about, as bytes.
///
/// Which stage carries it depends on the conflict's shape: an add/add has no
/// ancestor, a delete/modify has no `our`. Bytes rather than `String` because a
/// non-UTF-8 path is legal in git and `String::from_utf8_lossy` produces one
/// that no longer addresses the index entry.
pub fn conflict_path_bytes(c: &git2::IndexConflict) -> Result<Vec<u8>, GitError> {
    c.our
        .as_ref()
        .or(c.their.as_ref())
        .or(c.ancestor.as_ref())
        .map(|e| e.path.clone())
        .ok_or_else(|| GitError::internal("index conflict with no entry on any stage"))
}
```

`Phase`, `bytes_to_path`, `now_ms` and `utc_stamp` are imported now because every later step in
this task adds code that uses them; importing them here keeps the `use` block in one place.
They are unused until Step 23, so **the build will warn on them until then** —
`cargo clippy --all-targets -- -D warnings` is deliberately not part of the gate until the
final verification step for exactly this reason. If you prefer a clean intermediate build, add
only `GitError`/`GitErrorCode` now and add the other four in Step 23.

- [ ] **Step 4: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::merge`

Expected: PASS — `test result: ok. 2 passed; 0 failed`.

- [ ] **Step 5: Commit**

```bash
git add src/git/merge.rs src/git/mod.rs
git commit -m "feat(git): add the merge vocabulary and the conflict path accessor

conflict_path_bytes reads whichever of the three stages exists: an add/add
carries no ancestor and a delete/modify carries no 'our', so reading only one
stage silently drops half the conflicts. Bytes, not String, because a non-UTF-8
path is legal in git and a lossy conversion no longer addresses the entry."
```

---

- [ ] **Step 6: Write the local-git test harness and the failing tests for `fetch`**

Two edits. First, add the harness to `src/git/merge.rs`, between the production code and the
existing `#[cfg(test)] mod tests` line:

```rust
/// A bare "origin" plus clones of it, driven entirely through libgit2's local
/// transport: no network, no daemon, no credential, no running server.
///
/// It lives here rather than in `ops.rs` because both test modules need it and a
/// second copy would drift. It is `pub(crate)` and `#[cfg(test)]`, so it exists
/// in no shipped build.
#[cfg(test)]
pub(crate) mod testkit {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use crate::config::SshHostKeyPolicy;
    use crate::git::creds::{HostKeyChecker, ResolvedCred};
    use crate::git::jobs::{Admission, JobOp, JobStore, RepoLease};
    use crate::git::ops::{Identity, OpCtx, OpRequest, OpScratch};
    use crate::git::registry::{AuthorSpec, RepoDef};

    /// The file every seeded fixture starts from. Nine lines, so a test can edit
    /// the top and the bottom without the two hunks ever touching.
    pub(crate) const SEEDED: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n";

    pub(crate) struct Origin {
        /// Every tree in one test lives under here, so one `TempDir` drop cleans
        /// the whole fixture up.
        pub root: TempDir,
        pub bare: PathBuf,
    }

    /// Remotes are the bare repo's plain absolute path, never a `file://` URL:
    /// that sidesteps Windows drive-letter parsing in libgit2 entirely.
    fn url_of(o: &Origin) -> &str {
        o.bare.to_str().expect("tempdir paths are utf-8")
    }

    /// An empty bare remote.
    ///
    /// `initial_head` is pinned because `Repository::init` honours the
    /// developer's global `init.defaultBranch`: on a machine that defaults to
    /// `master`, every `refs/heads/main` refspec in this file would match
    /// nothing and the whole suite would pass for the wrong reason.
    pub(crate) fn origin() -> Origin {
        let root = tempfile::tempdir().expect("tempdir");
        let bare = root.path().join("origin.git");
        let mut opts = git2::RepositoryInitOptions::new();
        opts.bare(true).initial_head("main");
        git2::Repository::init_opts(&bare, &opts).expect("init bare origin");
        Origin { root, bare }
    }

    /// `origin()` with one commit on `main` containing `f.md`.
    pub(crate) fn origin_with_main() -> Origin {
        let o = origin();
        let seed = init_tree(&o, "seed");
        write_file(&seed, "f.md", SEEDED);
        commit_all(&seed, "seed f.md");
        push_main(&seed);
        o
    }

    /// A working tree that was never cloned: `git init` with `main` pinned, plus
    /// the origin remote. This is "the host created this repo itself".
    pub(crate) fn init_tree(o: &Origin, name: &str) -> PathBuf {
        let tree = o.root.path().join(name);
        std::fs::create_dir_all(&tree).expect("create tree dir");
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = git2::Repository::init_opts(&tree, &opts).expect("init tree");
        repo.remote("origin", url_of(o)).expect("add origin");
        tree
    }

    pub(crate) fn clone_at(o: &Origin, name: &str) -> PathBuf {
        let tree = o.root.path().join(name);
        git2::build::RepoBuilder::new()
            .clone(url_of(o), &tree)
            .expect("clone origin");
        tree
    }

    pub(crate) fn write_file(tree: &Path, rel: &str, body: &str) {
        let target = tree.join(rel);
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir).expect("create parent dir");
        }
        std::fs::write(target, body).expect("write file");
    }

    pub(crate) fn read_file(tree: &Path, rel: &str) -> String {
        std::fs::read_to_string(tree.join(rel)).expect("read file")
    }

    /// `git add -A && git commit`, with an explicit signature: these tests must
    /// not depend on the developer's global `user.name`.
    pub(crate) fn commit_all(tree: &Path, message: &str) -> git2::Oid {
        let repo = git2::Repository::open(tree).expect("open tree");
        let mut index = repo.index().expect("index");
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .expect("add_all");
        index.write().expect("write index");
        let tree_oid = index.write_tree().expect("write_tree");
        let tree_obj = repo.find_tree(tree_oid).expect("find_tree");
        let sig = git2::Signature::now("Test", "test@example.com").expect("signature");
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree_obj, &parents)
            .expect("commit")
    }

    pub(crate) fn push_main(tree: &Path) {
        let repo = git2::Repository::open(tree).expect("open tree");
        let mut remote = repo.find_remote("origin").expect("find origin");
        remote
            .push(&["refs/heads/main:refs/heads/main"], None)
            .expect("push main");
    }

    /// A raw fetch, so the merge tests do not depend on `ops::fetch`.
    pub(crate) fn fetch_main(tree: &Path) -> git2::Oid {
        let repo = git2::Repository::open(tree).expect("open tree");
        let mut remote = repo.find_remote("origin").expect("find origin");
        remote
            .fetch(&["+refs/heads/main:refs/remotes/origin/main"], None, None)
            .expect("fetch main");
        repo.find_reference("refs/remotes/origin/main")
            .expect("tracking ref")
            .target()
            .expect("tracking ref has a target")
    }

    pub(crate) fn head_of(tree: &Path) -> git2::Oid {
        let repo = git2::Repository::open(tree).expect("open tree");
        let head = repo.head().expect("head");
        head.target().expect("head has a target")
    }

    pub(crate) fn origin_main(o: &Origin) -> git2::Oid {
        let repo = git2::Repository::open_bare(&o.bare).expect("open bare origin");
        let main = repo
            .find_reference("refs/heads/main")
            .expect("origin has refs/heads/main");
        main.target().expect("refs/heads/main has a target")
    }

    pub(crate) struct Job {
        pub ctx: OpCtx,
        /// Dropping the lease finishes the job the slot belongs to, so it has to
        /// outlive the context that holds the slot.
        _lease: RepoLease,
    }

    pub(crate) fn job(o: &Origin, tree: &Path, op: JobOp) -> Job {
        job_with(Some(o), tree, op, OpRequest::default())
    }

    /// A repo with no remote at all — a legitimate configuration: local version
    /// history for the child process, with nothing to publish it to.
    pub(crate) fn job_local(tree: &Path, op: JobOp) -> Job {
        job_with(None, tree, op, OpRequest::default())
    }

    pub(crate) fn job_with(
        o: Option<&Origin>,
        tree: &Path,
        op: JobOp,
        request: OpRequest,
    ) -> Job {
        let store = JobStore::new("0000dead".to_string());
        let admitted = store
            .admit("notes", op, None)
            .expect("a fresh store is not draining");
        let Admission::Started(slot, lease) = admitted else {
            panic!("a fresh store must admit the first job");
        };
        let abort = slot.abort.clone();
        let remote = match o {
            Some(o) => serde_json::to_string(url_of(o)).expect("a string always serializes"),
            None => "null".to_string(),
        };
        let def: RepoDef = serde_json::from_str(&format!(
            r#"{{"id":"notes","remote":{remote},"branch":"main"}}"#
        ))
        .expect("the test repo definition must parse");
        Job {
            ctx: OpCtx {
                tree: tree.to_path_buf(),
                def,
                cred: ResolvedCred::None,
                cred_unbound: false,
                request,
                identity: Identity {
                    app_name: "Test App".to_string(),
                    identifier: "com.example.test".to_string(),
                    hostname: "testhost".to_string(),
                },
                host_key: HostKeyChecker::new(SshHostKeyPolicy::Tofu, None),
                abort,
                slot,
                deadline: Instant::now() + Duration::from_secs(60),
                settings: None,
                default_author: AuthorSpec::default(),
                scratch: OpScratch::default(),
            },
            _lease: lease,
        }
    }

    /// The four properties that must hold after every merge, successful or not.
    ///
    /// `merge_commits` is pure — it returns a detached index and touches nothing
    /// on disk — so none of these can be violated unless someone reaches for
    /// `Repository::merge` or lets libgit2 synthesise marker content.
    pub(crate) fn assert_no_merge_state(tree: &Path) {
        assert!(
            !tree.join(".git/MERGE_HEAD").exists(),
            "MERGE_HEAD was left behind"
        );
        assert!(
            !tree.join(".git/MERGE_MSG").exists(),
            "MERGE_MSG was left behind"
        );
        let repo = git2::Repository::open(tree).expect("open tree");
        assert_eq!(
            repo.state(),
            git2::RepositoryState::Clean,
            "the repository was left mid-operation"
        );
        assert!(
            !repo.head_detached().expect("head_detached"),
            "HEAD was left detached"
        );
        for file in worktree_files(tree) {
            let body = std::fs::read(&file).expect("read a worktree file");
            assert!(
                !body.windows(7).any(|w| w == b"<<<<<<<"),
                "conflict markers reached {}",
                file.display()
            );
        }
    }

    fn worktree_files(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(next) = stack.pop() {
            for entry in std::fs::read_dir(&next).expect("read_dir") {
                let path = entry.expect("dir entry").path();
                if path.file_name().is_some_and(|name| name == ".git") {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files
    }
}
```

Second, add the two `fetch` tests to `mod tests` in `src/git/ops.rs`. Add this import line
alongside the ones task 6 put there:

```rust
    use crate::git::merge::testkit;
```

and these two tests:

```rust
    #[test]
    fn fetch_uses_an_explicit_single_branch_refspec() {
        let o = testkit::origin_with_main();
        let a = testkit::clone_at(&o, "a");
        let b = testkit::clone_at(&o, "b");

        // A second branch that appears on the remote *after* the clone. The
        // remote's configured refspec is `+refs/heads/*:refs/remotes/origin/*`
        // and would drag it down; a sync that only ever touches one branch must
        // not pay for every branch the remote grows.
        {
            let repo = git2::Repository::open(&b).expect("open b");
            let head = repo
                .head()
                .expect("head")
                .peel_to_commit()
                .expect("head commit");
            repo.branch("sidecar", &head, false)
                .expect("create sidecar");
            let mut remote = repo.find_remote("origin").expect("find origin");
            remote
                .push(&["refs/heads/sidecar:refs/heads/sidecar"], None)
                .expect("push sidecar");
        }
        testkit::write_file(&b, "f.md", "moved on\n");
        testkit::commit_all(&b, "b moves ahead");
        testkit::push_main(&b);

        let fx = testkit::job(&o, &a, JobOp::Sync);
        let repo = git2::Repository::open(&a).expect("open a");
        let fetched = fetch(&repo, &fx.ctx).expect("fetch main");

        assert_eq!(fetched, Some(testkit::origin_main(&o)));
        assert!(repo.find_reference("refs/remotes/origin/main").is_ok());
        assert!(
            repo.find_reference("refs/remotes/origin/sidecar").is_err(),
            "the fetch pulled down a branch it was never asked for"
        );
    }

    #[test]
    fn fetch_of_a_branch_the_remote_lacks_is_not_an_error() {
        // An empty remote is the normal state right after `init` + `PUT`. A
        // refspec whose source does not exist on the remote matches nothing and
        // fetches nothing; push is what will create the branch.
        let o = testkit::origin();
        let a = testkit::init_tree(&o, "a");
        testkit::write_file(&a, "f.md", "local only\n");
        testkit::commit_all(&a, "a first commit");

        let fx = testkit::job(&o, &a, JobOp::Sync);
        let repo = git2::Repository::open(&a).expect("open a");
        assert_eq!(
            fetch(&repo, &fx.ctx).expect("an absent remote branch is not a failure"),
            None
        );
    }
```

- [ ] **Step 7: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::ops::tests::fetch`

Expected: FAIL — compile error:

```
error[E0425]: cannot find function `fetch` in this scope
   --> src/git/ops.rs:NNN:23
    |
    |         let fetched = fetch(&repo, &fx.ctx).expect("fetch main");
    |                       ^^^^^ not found in this scope
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 2 previous errors
```

- [ ] **Step 8: Implement `ops::fetch`**

In `src/git/ops.rs`, change the creds import so the module itself is in scope:

```rust
use crate::git::creds::{self, HostKeyChecker, ResolvedCred};
```

Then insert this function immediately above the `#[cfg(test)]` line.

```rust
/// Fetch `def.branch` from `def.remote_name` and report the remote's tip.
///
/// `Ok(None)` means the remote has no such branch yet — not an error: `push`
/// creates it.
pub fn fetch(repo: &git2::Repository, ctx: &OpCtx) -> Result<Option<git2::Oid>, GitError> {
    if ctx.def.remote.is_none() {
        return Err(GitError::remote_missing().with_repo(&ctx.def.id));
    }
    ctx.phase(Phase::Fetching);
    let mut remote = repo
        .find_remote(&ctx.def.remote_name)
        .map_err(|e| ctx.classify_err(&e))?;

    // An explicit single-branch refspec rather than the remote's configured
    // `+refs/heads/*:refs/remotes/<r>/*`: a sync that only ever touches one
    // branch must not download every branch the remote has grown.
    let branch = ctx.def.branch.as_str();
    let remote_name = ctx.def.remote_name.as_str();
    let refspec = format!("+refs/heads/{branch}:refs/remotes/{remote_name}/{branch}");

    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(creds::callbacks(ctx));
    // Tags are out of scope. The default (`AutotagOption::Auto`) downloads every
    // tag that points into the fetched history, and the host never pushes one
    // back, so they would only ever accumulate in one direction.
    fo.download_tags(git2::AutotagOption::None);
    remote
        .fetch(&[refspec.as_str()], Some(&mut fo), Some("host sync"))
        .map_err(|e| ctx.classify_err(&e))?;

    // A refspec whose source does not exist on the remote matches nothing and
    // is not an error, so the tracking ref — not the fetch's return value — is
    // what says whether the remote has this branch at all.
    let tracking = format!("refs/remotes/{remote_name}/{branch}");
    match repo.find_reference(&tracking) {
        Ok(r) => Ok(r.target()),
        Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
        Err(e) => Err(ctx.classify_err(&e)),
    }
}
```

- [ ] **Step 9: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::ops::tests::fetch`

Expected: PASS — `test result: ok. 2 passed; 0 failed`.

- [ ] **Step 10: Commit**

```bash
git add src/git/merge.rs src/git/ops.rs
git commit -m "feat(git): fetch one branch with an explicit refspec

The refspec is explicit so a sync does not download every branch the remote has
grown, and AutotagOption::None keeps tags out: the host never pushes one back,
so autotagged tags would accumulate in one direction forever. A refspec whose
source is absent on the remote matches nothing rather than failing, so the
tracking ref is what reports whether the branch exists yet."
```

---

- [ ] **Step 11: Write the failing tests for pushing a branch**

Add to `mod tests` in `src/git/ops.rs`:

```rust
    #[test]
    fn first_push_sets_the_upstream() {
        // Clone sets `branch.<b>.merge` automatically; a manual push does not.
        // Without this, a repo the host created with init + push has no
        // upstream and every later graph_ahead_behind fails with NotFound
        // forever.
        let o = testkit::origin();
        let a = testkit::init_tree(&o, "a");
        testkit::write_file(&a, "f.md", "local only\n");
        let head = testkit::commit_all(&a, "a first commit");

        let fx = testkit::job(&o, &a, JobOp::Push);
        let out = push(&fx.ctx).expect("the first push creates the branch");

        assert_eq!(out.outcome, "pushed");
        assert!(out.pushed);
        assert!(out.upstream_set);
        assert!(!out.force_pushed);
        assert_eq!(testkit::origin_main(&o), head);

        let repo = git2::Repository::open(&a).expect("open a");
        let branch = repo
            .find_branch("main", git2::BranchType::Local)
            .expect("main");
        let upstream = branch.upstream().expect("the upstream is set");
        assert_eq!(
            upstream.name().expect("upstream name").unwrap_or_default(),
            "origin/main"
        );

        // A second push finds the upstream already there and leaves it alone.
        testkit::write_file(&a, "f.md", "local again\n");
        testkit::commit_all(&a, "a second commit");
        let fx = testkit::job(&o, &a, JobOp::Push);
        let out = push(&fx.ctx).expect("a fast-forward push");
        assert!(out.pushed);
        assert!(!out.upstream_set);
    }

    #[test]
    fn push_on_an_unborn_branch_is_refused() {
        // libgit2 fails this deep inside the refspec matcher with "src refspec
        // '...' does not match any existing object", which tells the user
        // nothing. Name the real problem.
        let o = testkit::origin();
        let a = testkit::init_tree(&o, "a");

        let fx = testkit::job(&o, &a, JobOp::Push);
        let e = push(&fx.ctx).expect_err("there is nothing to push");
        assert_eq!(e.code(), GitErrorCode::UnbornBranch);
    }
```

- [ ] **Step 12: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::ops::tests::push`

Expected: FAIL — two assertion failures, because `push` is still task 6's stub:

```
---- git::ops::tests::first_push_sets_the_upstream stdout ----
thread 'git::ops::tests::first_push_sets_the_upstream' panicked at src/git/ops.rs:NNN:
the first push creates the branch: GitError { code: Internal,
message: "push is not implemented yet", repo_id: None, path: None, field: None, git2: None }

---- git::ops::tests::push_on_an_unborn_branch_is_refused stdout ----
assertion `left == right` failed
  left: Internal
 right: UnbornBranch

failures:
    git::ops::tests::first_push_sets_the_upstream
    git::ops::tests::push_on_an_unborn_branch_is_refused
```

- [ ] **Step 13: Implement `push_branch` and replace the `push` stub**

In `src/git/ops.rs`, replace the stub

```rust
pub fn push(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("push is not implemented yet"))
}
```

with:

```rust
pub fn push(ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    let repo = open_tree(ctx)?;
    require_branch(&repo, &ctx.def.branch)?;

    let mut out = OpOutcome::new("pushed", &ctx.def.branch);
    // A push moves the remote, never HEAD, so before and after are the same.
    let head = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|o| o.to_string());
    out.head_before = head.clone();
    out.head_after = head;

    push_branch(&repo, ctx, &mut out)?;
    record_ahead_behind(&repo, ctx, &mut out);
    ctx.phase(Phase::Finalizing);
    Ok(out)
}

/// Publish `def.branch` to the remote and make sure it has an upstream.
pub fn push_branch(
    repo: &git2::Repository,
    ctx: &OpCtx,
    out: &mut OpOutcome,
) -> Result<(), GitError> {
    if ctx.def.remote.is_none() {
        return Err(GitError::remote_missing().with_repo(&ctx.def.id));
    }
    // Pushing a branch with no commits fails deep inside libgit2's refspec
    // matcher with "src refspec '...' does not match any existing object".
    if repo.is_empty().map_err(|e| ctx.classify_err(&e))? {
        return Err(GitError::unborn_branch("push").with_repo(&ctx.def.id));
    }

    ctx.phase(Phase::Pushing);
    let branch = ctx.def.branch.as_str();
    let mut remote = repo
        .find_remote(&ctx.def.remote_name)
        .map_err(|e| ctx.classify_err(&e))?;
    let spec = format!("refs/heads/{branch}:refs/heads/{branch}");

    let mut po = git2::PushOptions::new();
    po.remote_callbacks(creds::callbacks(ctx));
    remote
        .push(&[spec.as_str()], Some(&mut po))
        .map_err(|e| ctx.classify_err(&e))?;

    // A server-side refusal — a protected branch, a pre-receive hook — is
    // invisible to libgit2 locally: `push` returns Ok(()) and the refusal
    // arrives only through push_update_reference, which our callbacks record.
    // Without this drain the job reports a success that pushed nothing.
    ctx.take_push_rejections()?;

    out.pushed = true;

    // Clone sets the upstream; a manual push does not. Without it a repo the
    // host created with init + push has no `branch.<b>.merge`, and every later
    // graph_ahead_behind through Branch::upstream() fails with NotFound forever.
    let mut b = repo
        .find_branch(branch, git2::BranchType::Local)
        .map_err(|e| ctx.classify_err(&e))?;
    if b.upstream().is_err() {
        b.set_upstream(Some(&format!("{}/{branch}", ctx.def.remote_name)))
            .map_err(|e| ctx.classify_err(&e))?;
        out.upstream_set = true;
    }
    Ok(())
}

/// Fill in how far the local branch is from its upstream.
///
/// Best-effort by design: a repo whose upstream has never been set is the
/// normal state before the first push, and reporting (0, 0) for it beats
/// failing an operation that otherwise succeeded.
fn record_ahead_behind(repo: &git2::Repository, ctx: &OpCtx, out: &mut OpOutcome) {
    let local = repo.head().ok().and_then(|h| h.target());
    let upstream = repo
        .find_branch(&ctx.def.branch, git2::BranchType::Local)
        .ok()
        .and_then(|b| b.upstream().ok())
        .and_then(|u| u.get().target());
    let (ahead, behind) = local
        .zip(upstream)
        .and_then(|(l, u)| repo.graph_ahead_behind(l, u).ok())
        .unwrap_or_default();
    out.ahead = ahead;
    out.behind = behind;
}
```

Then fix the task-6 scaffolding test. In `mod tests`, the loop in
`run_dispatches_to_a_verb_of_the_same_name` now lists only the verbs still stubbed — task 7
removed the other five, leaving `JobOp::Pull`, `JobOp::Push` and `JobOp::Sync`. Delete the
`JobOp::Push,` line from that list so it reads:

```rust
        for op in [JobOp::Pull, JobOp::Sync] {
```

- [ ] **Step 14: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::ops`

Expected: PASS — the two new push tests pass and
`run_dispatches_to_a_verb_of_the_same_name` still passes for the two remaining stubs.

- [ ] **Step 15: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): push a branch and bind its upstream on the first push

Remote::push returns Ok even when the server refused every ref, so the
push_update_reference drain is not optional. set_upstream after the first push
is what keeps graph_ahead_behind working: clone sets branch.<b>.merge, a manual
push does not, and without it every later ahead/behind read fails forever."
```

---

- [ ] **Step 16: Write the failing tests for the two push rejection paths**

Add to `mod tests` in `src/git/ops.rs`:

```rust
    #[test]
    fn push_non_fast_forward_is_caught_client_side() {
        let o = testkit::origin_with_main();
        let a = testkit::clone_at(&o, "a");
        let b = testkit::clone_at(&o, "b");

        testkit::write_file(&b, "f.md", "from b\n");
        testkit::commit_all(&b, "b pushes first");
        testkit::push_main(&b);

        testkit::write_file(&a, "f.md", "from a\n");
        testkit::commit_all(&a, "a diverges");

        let fx = testkit::job(&o, &a, JobOp::Push);
        let e = push(&fx.ctx).expect_err("someone else pushed first");

        // libgit2 refuses this before any server round trip, so
        // push_update_reference is never invoked for it: `push` returning Err
        // is the only signal this case has.
        assert_eq!(e.code(), GitErrorCode::NotFastForward);
        assert!(e.message.contains("force=true"), "{}", e.message);
        assert_eq!(testkit::origin_main(&o), testkit::head_of(&b));
    }

    #[test]
    fn a_forced_push_overwrites_the_remote() {
        let o = testkit::origin_with_main();
        let a = testkit::clone_at(&o, "a");
        let b = testkit::clone_at(&o, "b");

        testkit::write_file(&b, "f.md", "from b\n");
        testkit::commit_all(&b, "b pushes first");
        testkit::push_main(&b);

        testkit::write_file(&a, "f.md", "from a\n");
        testkit::commit_all(&a, "a diverges");

        // The leading `+` comes solely from this flag.
        let fx = testkit::job_with(
            Some(&o),
            &a,
            JobOp::Push,
            OpRequest {
                force: true,
                ..OpRequest::default()
            },
        );
        let out = push(&fx.ctx).expect("a forced push replaces the remote tip");

        assert!(out.pushed);
        assert!(out.force_pushed);
        assert_eq!(testkit::origin_main(&o), testkit::head_of(&a));
    }
```

- [ ] **Step 17: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::ops::tests::push git::ops::tests::a_forced`

Expected: FAIL — two assertion failures:

```
---- git::ops::tests::push_non_fast_forward_is_caught_client_side stdout ----
thread '...' panicked at src/git/ops.rs:NNN:
cannot push non-fastforwardable reference

---- git::ops::tests::a_forced_push_overwrites_the_remote stdout ----
thread '...' panicked at src/git/ops.rs:NNN:
a forced push replaces the remote tip: GitError { code: NotFastForward, ... }
```

(The first fails on `assert!(e.message.contains("force=true"))` — the code is already right,
the message does not yet tell the user what to do. The second fails outright because the
refspec has no `+`.)

- [ ] **Step 18: Add the force refspec and the non-fast-forward guidance**

In `src/git/ops.rs`, inside `push_branch`, replace

```rust
    let spec = format!("refs/heads/{branch}:refs/heads/{branch}");
```

with:

```rust
    // The leading `+` is a force push and comes SOLELY from an explicit
    // `force: true` in the request — never from config, and never as a fallback
    // after a rejection. git2 0.21's PushOptions has no expected-old-oid
    // parameter, so there is no force-with-lease to fall back to either; a
    // fetch-compare-then-force approximation is a TOCTOU window, not a lease.
    let spec = if ctx.request.force {
        format!("+refs/heads/{branch}:refs/heads/{branch}")
    } else {
        format!("refs/heads/{branch}:refs/heads/{branch}")
    };
```

Replace the `remote.push(...)` call with:

```rust
    remote
        .push(&[spec.as_str()], Some(&mut po))
        .map_err(|e| {
            let mut err = ctx.classify_err(&e);
            if err.code() == GitErrorCode::NotFastForward {
                // libgit2's own text describes the situation but not the fix,
                // and this error is the one a user hits most often.
                err.message
                    .push_str("; pull first, or retry with force=true");
            }
            err
        })?;
```

And record the flag on the outcome — replace

```rust
    out.pushed = true;
```

with:

```rust
    out.pushed = true;
    out.force_pushed = ctx.request.force;
```

- [ ] **Step 19: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::ops`

Expected: PASS — `test result: ok.` with the four push tests green.

- [ ] **Step 20: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): force-push only on an explicit request, and say how to recover

The leading + comes from request.force and nothing else. Non-fast-forward is
caught client-side before any round trip, so libgit2's message is the only one
the user sees; it now names the two ways out."
```

---

- [ ] **Step 21: Write the failing tests for merge analysis**

Add a test module body to `src/git/merge.rs`. Replace the existing
`#[cfg(test)] mod tests { use super::*;` header with the full import list, then add the four
tests below the two that are already there.

```rust
#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;
    use crate::git::jobs::JobOp;
    use crate::git::ops::{self, OpRequest};
    use std::path::Path;
```

(`use super::*` also brings in `GitError`, `Phase`, `OpCtx` and the three `util` functions from
the module's own import block; `JobOp`, `ops`, `OpRequest` and `Path` are not among them.
`ops`, `OpRequest` and `Path` are unused until Step 31 — a `cargo test` run before then will
warn about them, which the final verification step cleans up.)

```rust
    #[test]
    fn an_absent_remote_branch_is_not_an_error() {
        // The normal state right after `init` + `PUT`: push is what will create
        // the branch, so analysis must not treat this as a failure.
        let o = origin();
        let a = init_tree(&o, "a");
        write_file(&a, "f.md", "local only\n");
        commit_all(&a, "a first commit");

        let fx = job(&o, &a, JobOp::Sync);
        let repo = git2::Repository::open(&a).expect("open a");
        match analyse(&repo, &fx.ctx).expect("an absent remote branch is not a failure") {
            MergeOutcome::NoRemoteBranch => {}
            other => panic!("expected NoRemoteBranch, got {other:?}"),
        }
    }

    #[test]
    fn an_up_to_date_branch_needs_no_merge() {
        let o = origin_with_main();
        let a = clone_at(&o, "a");
        let before = head_of(&a);
        fetch_main(&a);

        let fx = job(&o, &a, JobOp::Sync);
        let repo = git2::Repository::open(&a).expect("open a");
        match analyse(&repo, &fx.ctx).expect("up to date") {
            MergeOutcome::UpToDate => {}
            other => panic!("expected UpToDate, got {other:?}"),
        }
        assert_eq!(head_of(&a), before, "an up-to-date analysis moved HEAD");
    }

    #[test]
    fn an_unborn_head_adopts_the_remote_history() {
        // Reachable only when the tree was genuinely empty: sync commits
        // everything before it fetches, so an unborn HEAD here means there was
        // nothing to commit.
        let o = origin_with_main();
        let a = init_tree(&o, "a");
        let their = fetch_main(&a);

        let fx = job(&o, &a, JobOp::Sync);
        let repo = git2::Repository::open(&a).expect("open a");
        match analyse(&repo, &fx.ctx).expect("an unborn head adopts the remote") {
            MergeOutcome::AdoptedRemote { head } => assert_eq!(head, their),
            other => panic!("expected AdoptedRemote, got {other:?}"),
        }
        assert_eq!(head_of(&a), their);
        assert_eq!(read_file(&a, "f.md"), SEEDED);
        assert_no_merge_state(&a);
    }

    #[test]
    fn a_fast_forward_never_reaches_the_normal_branch() {
        let o = origin_with_main();
        let a = clone_at(&o, "a");
        let b = clone_at(&o, "b");
        write_file(&b, "f.md", "moved on\n");
        commit_all(&b, "b moves ahead");
        push_main(&b);
        let their = fetch_main(&a);

        let repo = git2::Repository::open(&a).expect("open a");

        // The reason the branch order inside `analyse` is load-bearing: libgit2
        // reports a fast-forwardable situation as FASTFORWARD *and* NORMAL, so
        // a chain that tests is_normal() first writes a merge commit — with a
        // diff of nothing — for every fast-forward there will ever be.
        let tracking = repo
            .find_reference("refs/remotes/origin/main")
            .expect("tracking ref");
        let annotated = repo
            .reference_to_annotated_commit(&tracking)
            .expect("annotated commit");
        let (analysis, _preference) = repo.merge_analysis(&[&annotated]).expect("analysis");
        assert!(analysis.is_fast_forward());
        assert!(
            analysis.is_normal(),
            "libgit2 sets NORMAL alongside FASTFORWARD; the branch order in analyse depends on it"
        );

        let fx = job(&o, &a, JobOp::Sync);
        match analyse(&repo, &fx.ctx).expect("fast-forward") {
            MergeOutcome::FastForward { head } => assert_eq!(head, their),
            other => panic!("expected FastForward, got {other:?}"),
        }
        assert_eq!(head_of(&a), their);
        assert_eq!(read_file(&a, "f.md"), "moved on\n");
        assert_eq!(
            repo.find_commit(their).expect("commit").parent_count(),
            1,
            "a fast-forward must not create a second parent"
        );
        assert_no_merge_state(&a);
    }
```

- [ ] **Step 22: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::merge`

Expected: FAIL — compile error:

```
error[E0425]: cannot find function `analyse` in this scope
   --> src/git/merge.rs:NNN:15
    |
    |         match analyse(&repo, &fx.ctx).expect("an absent remote branch is not a failure") {
    |               ^^^^^^^ not found in this scope
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 4 previous errors
```

- [ ] **Step 23: Implement `analyse`**

Insert this function into `src/git/merge.rs` immediately below `conflict_path_bytes` and above
the `#[cfg(test)] pub(crate) mod testkit` line.

```rust
/// Decide what to do with the fetched remote tip — and, for the two
/// fast-forward shapes, do it.
pub fn analyse(repo: &git2::Repository, ctx: &OpCtx) -> Result<MergeOutcome, GitError> {
    let tracking = format!("refs/remotes/{}/{}", ctx.def.remote_name, ctx.def.branch);
    let their_ref = match repo.find_reference(&tracking) {
        Ok(r) => r,
        // A remote that has no such branch yet is not an error: push creates it.
        Err(e) if e.code() == git2::ErrorCode::NotFound => {
            return Ok(MergeOutcome::NoRemoteBranch)
        }
        Err(e) => return Err(ctx.classify_err(&e)),
    };
    let their = repo
        .reference_to_annotated_commit(&their_ref)
        .map_err(|e| ctx.classify_err(&e))?;
    let their_oid = their.id();
    let (analysis, _preference) = repo
        .merge_analysis(&[&their])
        .map_err(|e| ctx.classify_err(&e))?;

    let refname = format!("refs/heads/{}", ctx.def.branch);

    // The order of these tests is the whole point of this function. libgit2 sets
    // ANALYSIS_FASTFORWARD together with ANALYSIS_NORMAL (and, for an unborn
    // HEAD, together with ANALYSIS_UNBORN), so a chain that tests is_normal()
    // first turns every fast-forward into a spurious merge commit.
    if analysis.is_up_to_date() {
        return Ok(MergeOutcome::UpToDate);
    }

    if analysis.is_unborn() {
        // Reachable only when the tree was genuinely empty: sync commits
        // everything before it fetches, so an unborn HEAD here means there was
        // nothing to commit and the forced checkout below can lose nothing.
        ctx.phase(Phase::CheckingOut);
        repo.reference(&refname, their_oid, true, "sync: adopt remote history")
            .map_err(|e| ctx.classify_err(&e))?;
        repo.set_head(&refname).map_err(|e| ctx.classify_err(&e))?;
        let mut co = git2::build::CheckoutBuilder::new();
        co.force();
        repo.checkout_head(Some(&mut co))
            .map_err(|e| ctx.classify_err(&e))?;
        return Ok(MergeOutcome::AdoptedRemote { head: their_oid });
    }

    if analysis.is_fast_forward() {
        // Repository::merge does NOT fast-forward and does NOT move HEAD; it
        // leaves RepositoryState::Merge behind for someone else to clean up.
        // Do the ref surgery ourselves.
        ctx.phase(Phase::CheckingOut);
        let mut r = repo
            .find_reference(&refname)
            .map_err(|e| ctx.classify_err(&e))?;
        r.set_target(their_oid, "sync: fast-forward")
            .map_err(|e| ctx.classify_err(&e))?;
        repo.set_head(&refname).map_err(|e| ctx.classify_err(&e))?;
        let mut co = git2::build::CheckoutBuilder::new();
        co.force();
        repo.checkout_head(Some(&mut co))
            .map_err(|e| ctx.classify_err(&e))?;
        return Ok(MergeOutcome::FastForward { head: their_oid });
    }

    // Everything left is ANALYSIS_NORMAL: both sides moved.
    Err(GitError::internal(
        "a diverged branch needs the prefer-local merge",
    ))
}
```

The final `Err` is a stub that Step 38 replaces with the call into `merge_prefer_local`; it
exists so this step compiles and so every test above proves the *other three* branches in
isolation.

- [ ] **Step 24: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::merge`

Expected: PASS — `test result: ok. 6 passed; 0 failed`.

- [ ] **Step 25: Commit**

```bash
git add src/git/merge.rs
git commit -m "feat(git): analyse the fetched tip in an order that keeps fast-forwards fast

libgit2 sets ANALYSIS_FASTFORWARD together with ANALYSIS_NORMAL, so testing
is_normal() first would write a merge commit with an empty diff for every
fast-forward. Repository::merge neither fast-forwards nor moves HEAD and leaves
RepositoryState::Merge behind, so the ref surgery is done by hand."
```

---

- [ ] **Step 26: Write the failing tests for `pull`**

Add to `mod tests` in `src/git/ops.rs`:

```rust
    #[test]
    fn pull_fast_forwards_a_clean_tree() {
        let o = testkit::origin_with_main();
        let a = testkit::clone_at(&o, "a");
        let b = testkit::clone_at(&o, "b");
        testkit::write_file(&b, "f.md", "moved on\n");
        testkit::commit_all(&b, "b moves ahead");
        testkit::push_main(&b);

        let fx = testkit::job(&o, &a, JobOp::Pull);
        let out = pull(&fx.ctx).expect("a clean tree fast-forwards");

        assert_eq!(out.outcome, "fast_forward");
        assert!(out.fetched);
        assert!(!out.committed);
        assert!(!out.pushed);
        assert_eq!(
            out.merge.as_ref().expect("a merge report").kind,
            "fast_forward"
        );
        assert_eq!(testkit::read_file(&a, "f.md"), "moved on\n");
        assert_eq!(out.ahead, 0);
        assert_eq!(out.behind, 0);
    }

    #[test]
    fn pull_with_a_dirty_tree_is_refused() {
        // The merge force-checks-out its result, which is only provably
        // non-destructive when every tracked file is already committed. A pull
        // that silently committed on the caller's behalf would be worse.
        let o = testkit::origin_with_main();
        let a = testkit::clone_at(&o, "a");
        testkit::write_file(&a, "f.md", "uncommitted\n");

        let fx = testkit::job(&o, &a, JobOp::Pull);
        let e = pull(&fx.ctx).expect_err("a dirty tree blocks a pull");

        assert_eq!(e.code(), GitErrorCode::DirtyTree);
        assert!(e.message.contains("commit_local"), "{}", e.message);
        assert_eq!(testkit::read_file(&a, "f.md"), "uncommitted\n");
    }

    #[test]
    fn an_untracked_file_alone_does_not_block_a_pull() {
        // Nothing in a pull touches untracked files, and refusing on a stray
        // scratch file would make auto-sync useless in practice.
        let o = testkit::origin_with_main();
        let a = testkit::clone_at(&o, "a");
        testkit::write_file(&a, "scratch.txt", "not committed\n");

        let fx = testkit::job(&o, &a, JobOp::Pull);
        let out = pull(&fx.ctx).expect("an untracked file is not a dirty tree");

        assert_eq!(out.outcome, "up_to_date");
        assert_eq!(testkit::read_file(&a, "scratch.txt"), "not committed\n");
    }

    #[test]
    fn pull_with_commit_local_commits_the_tree_first() {
        let o = testkit::origin_with_main();
        let a = testkit::clone_at(&o, "a");
        testkit::write_file(&a, "f.md", "committed by the pull\n");

        let fx = testkit::job_with(
            Some(&o),
            &a,
            JobOp::Pull,
            OpRequest {
                commit_local: true,
                ..OpRequest::default()
            },
        );
        let out = pull(&fx.ctx).expect("commit_local makes the tree committable");

        assert!(out.committed);
        assert_eq!(out.outcome, "committed");
        assert!(!out.pushed, "pull never pushes");
        assert_eq!(
            out.merge.as_ref().expect("a merge report").kind,
            "up_to_date"
        );
    }

    #[test]
    fn pull_without_a_remote_is_refused() {
        let o = testkit::origin();
        let a = testkit::init_tree(&o, "a");
        testkit::write_file(&a, "f.md", "local only\n");
        testkit::commit_all(&a, "a first commit");

        let fx = testkit::job_local(&a, JobOp::Pull);
        let e = pull(&fx.ctx).expect_err("there is nothing to pull from");
        assert_eq!(e.code(), GitErrorCode::RemoteMissing);
    }
```

- [ ] **Step 27: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::ops::tests::pull git::ops::tests::an_untracked`

Expected: FAIL — five failures, all against task 6's stub:

```
---- git::ops::tests::pull_fast_forwards_a_clean_tree stdout ----
thread '...' panicked at src/git/ops.rs:NNN:
a clean tree fast-forwards: GitError { code: Internal,
message: "pull is not implemented yet", repo_id: None, path: None, field: None, git2: None }
...
test result: FAILED. 0 passed; 5 failed
```

- [ ] **Step 28: Implement `merge_report`, `apply_merge` and `pull`**

In `src/git/ops.rs`, add the merge import alongside the others:

```rust
use crate::git::merge::{self, MergeOutcome};
```

Replace the stub

```rust
pub fn pull(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("pull is not implemented yet"))
}
```

with:

```rust
pub fn pull(ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    let repo = open_tree(ctx)?;
    require_branch(&repo, &ctx.def.branch)?;
    if ctx.def.remote.is_none() {
        return Err(GitError::remote_missing().with_repo(&ctx.def.id));
    }

    let mut out = OpOutcome::new("up_to_date", &ctx.def.branch);
    out.head_before = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|o| o.to_string());

    if ctx.request.commit_local {
        let (commit, files) = stage_and_commit(&repo, ctx)?;
        out.committed = commit.is_some();
        out.commit = commit.map(|o| o.to_string());
        out.files_committed = files;
        if commit.is_some() {
            out.outcome = "committed";
        }
    } else {
        // The merge force-checks-out its result, which is provably
        // non-destructive only when every tracked file is already at a
        // committed state. Untracked files are deliberately not counted:
        // nothing in a pull touches them, and refusing on a stray scratch file
        // would make auto-sync useless.
        let mut so = git2::StatusOptions::new();
        so.include_untracked(false)
            .include_ignored(false)
            .include_unmodified(false);
        let dirty = !repo
            .statuses(Some(&mut so))
            .map_err(|e| ctx.classify_err(&e))?
            .is_empty();
        if dirty {
            let mut e = GitError::dirty_tree("pull").with_repo(&ctx.def.id);
            // The code alone does not tell the caller which of the two ways out
            // exists, and this is the error an integrator hits first.
            e.message
                .push_str("; commit them first, or retry with commit_local=true");
            return Err(e);
        }
    }

    let fetched = fetch(&repo, ctx)?;
    out.fetched = true;
    out.fetched_head = fetched.map(|o| o.to_string());

    apply_merge(&repo, ctx, &mut out)?;

    out.head_after = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|o| o.to_string());
    record_ahead_behind(&repo, ctx, &mut out);
    ctx.phase(Phase::Finalizing);
    Ok(out)
}

/// A merge report for the four outcomes that write no merge commit.
fn merge_report(kind: &'static str) -> MergeReport {
    MergeReport {
        kind,
        merge_commit: None,
        conflicts_resolved: Vec::new(),
        recover_hint: None,
    }
}

/// Run the merge and fold its result into the outcome.
///
/// The merge outranks the commit that preceded it when both happened: a merge
/// is what a sync is *for*, and a caller reading `outcome == "merged"` still
/// learns about the commit from `committed` / `commit`.
fn apply_merge(
    repo: &git2::Repository,
    ctx: &OpCtx,
    out: &mut OpOutcome,
) -> Result<(), GitError> {
    let report = match merge::analyse(repo, ctx)? {
        MergeOutcome::UpToDate => merge_report("up_to_date"),
        MergeOutcome::NoRemoteBranch => merge_report("no_remote_branch"),
        MergeOutcome::AdoptedRemote { .. } => {
            out.outcome = "fast_forward";
            merge_report("adopted_remote")
        }
        MergeOutcome::FastForward { .. } => {
            out.outcome = "fast_forward";
            merge_report("fast_forward")
        }
        MergeOutcome::Merged(result) => {
            out.outcome = if result.unrelated {
                "merged_unrelated"
            } else {
                "merged"
            };
            // One path, because a hint has to be runnable as written. The merge
            // commit's own message enumerates every path it overwrote.
            let recover_hint = result
                .conflicts_resolved
                .first()
                .map(|p| format!("git show {}^2:{p}", result.merge_commit));
            MergeReport {
                kind: if result.unrelated {
                    "merge_commit_unrelated"
                } else {
                    "merge_commit"
                },
                merge_commit: Some(result.merge_commit.to_string()),
                conflicts_resolved: result.conflicts_resolved,
                recover_hint,
            }
        }
    };
    out.merge = Some(report);
    Ok(())
}
```

Then remove `JobOp::Pull,` from the `run_dispatches_to_a_verb_of_the_same_name` loop so it
reads:

```rust
        for op in [JobOp::Sync] {
```

- [ ] **Step 29: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::ops`

Expected: PASS — the five pull tests are green.

- [ ] **Step 30: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): implement pull as fetch plus the analysed merge

A dirty tree is refused rather than committed on the caller's behalf, because
the merge force-checks-out its result and that is safe only when every tracked
file is already committed. Untracked files do not count as dirty: nothing in a
pull touches them."
```

---

- [ ] **Step 31: Write the failing tests for `sync`**

Two edits. First, add to `mod tests` in `src/git/ops.rs`:

```rust
    #[test]
    fn sync_on_an_absent_tree_clones() {
        // The caller asked for "make this tree match the remote"; there is no
        // earlier step they should have had to run first.
        let o = testkit::origin_with_main();
        let tree = o.root.path().join("fresh");

        let fx = testkit::job(&o, &tree, JobOp::Sync);
        let out = sync(&fx.ctx).expect("a sync of an unmaterialised repo clones it");

        assert_eq!(out.outcome, "cloned");
        assert_eq!(testkit::read_file(&tree, "f.md"), testkit::SEEDED);
    }

    #[test]
    fn sync_without_a_remote_only_commits() {
        // Local version history for the child process, with nothing to publish
        // it to, is a legitimate configuration.
        let o = testkit::origin();
        let a = testkit::init_tree(&o, "a");
        testkit::write_file(&a, "notes.md", "local only\n");

        let fx = testkit::job_local(&a, JobOp::Sync);
        let out = sync(&fx.ctx).expect("a repo with no remote still commits");

        assert_eq!(out.outcome, "committed");
        assert!(out.committed);
        assert!(!out.fetched);
        assert!(!out.pushed);
        assert!(out.merge.is_none());
    }

    #[test]
    fn sync_commits_and_publishes_in_one_pass() {
        let o = testkit::origin_with_main();
        let a = testkit::clone_at(&o, "a");
        testkit::write_file(&a, "f.md", "edited locally\n");

        let fx = testkit::job(&o, &a, JobOp::Sync);
        let out = sync(&fx.ctx).expect("sync");

        assert_eq!(out.outcome, "committed");
        assert!(out.committed);
        assert!(out.fetched);
        assert!(out.pushed);
        assert_eq!(
            out.merge.as_ref().expect("a merge report").kind,
            "up_to_date"
        );
        assert_eq!(testkit::origin_main(&o), testkit::head_of(&a));
        assert_eq!(out.ahead, 0);
        assert_eq!(out.behind, 0);
    }

    #[test]
    fn sync_reports_pushed_when_there_was_nothing_new_to_commit() {
        let o = testkit::origin_with_main();
        let a = testkit::clone_at(&o, "a");
        testkit::write_file(&a, "f.md", "edited locally\n");
        testkit::commit_all(&a, "a commits by hand");

        let fx = testkit::job(&o, &a, JobOp::Sync);
        let out = sync(&fx.ctx).expect("sync");

        assert!(!out.committed);
        assert!(out.pushed);
        assert_eq!(out.outcome, "pushed");
        assert_eq!(testkit::origin_main(&o), testkit::head_of(&a));
    }

    #[test]
    fn sync_creates_the_branch_on_an_empty_remote() {
        let o = testkit::origin();
        let a = testkit::init_tree(&o, "a");
        testkit::write_file(&a, "f.md", "local only\n");

        let fx = testkit::job(&o, &a, JobOp::Sync);
        let out = sync(&fx.ctx).expect("sync");

        assert_eq!(
            out.merge.as_ref().expect("a merge report").kind,
            "no_remote_branch"
        );
        assert_eq!(out.outcome, "committed");
        assert!(out.pushed);
        assert!(out.upstream_set);
        assert_eq!(testkit::origin_main(&o), testkit::head_of(&a));
    }
```

Second, add the first two rows of the §10.3 merge matrix to `mod tests` in
`src/git/merge.rs`. Both are non-diverging cases, so they exercise `analyse`'s already-finished
branches end to end. Every merge-matrix test below sets `push: false` — the merge policy is
what is under test, and a push would only add a second way for the test to fail.

```rust
    /// `OpRequest::default()` has `push = true`. Every merge-matrix test below
    /// is about what lands on disk locally, so none of them push.
    fn merge_only() -> OpRequest {
        OpRequest {
            push: false,
            ..OpRequest::default()
        }
    }

    #[test]
    fn up_to_date_is_a_noop() {
        let o = origin_with_main();
        let a = clone_at(&o, "a");
        let before = head_of(&a);

        let fx = job_with(Some(&o), &a, JobOp::Sync, merge_only());
        let out = ops::sync(&fx.ctx).expect("sync");

        assert_eq!(out.outcome, "no_changes");
        assert!(!out.committed);
        assert_eq!(
            out.merge.as_ref().expect("a merge report").kind,
            "up_to_date"
        );
        assert_eq!(head_of(&a), before, "an up-to-date sync wrote a commit");
        assert_no_merge_state(&a);
    }

    #[test]
    fn fast_forward_does_not_create_a_merge_commit() {
        let o = origin_with_main();
        let a = clone_at(&o, "a");
        let b = clone_at(&o, "b");
        write_file(&b, "f.md", "moved on\n");
        let theirs = commit_all(&b, "b moves ahead");
        push_main(&b);

        let fx = job_with(Some(&o), &a, JobOp::Sync, merge_only());
        let out = ops::sync(&fx.ctx).expect("sync");

        assert_eq!(out.outcome, "fast_forward");
        assert_eq!(
            out.merge.as_ref().expect("a merge report").kind,
            "fast_forward"
        );
        assert_eq!(head_of(&a), theirs);
        let repo = git2::Repository::open(&a).expect("open a");
        assert_eq!(
            repo.find_commit(theirs).expect("commit").parent_count(),
            1,
            "a fast-forward must not create a second parent"
        );
        assert_eq!(read_file(&a, "f.md"), "moved on\n");
        assert_no_merge_state(&a);
    }
```

- [ ] **Step 32: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::ops::tests::sync git::merge::tests::up_to_date git::merge::tests::fast_forward`

Expected: FAIL — seven failures, all against task 6's stub:

```
---- git::ops::tests::sync_on_an_absent_tree_clones stdout ----
thread '...' panicked at src/git/ops.rs:NNN:
a sync of an unmaterialised repo clones it: GitError { code: Internal,
message: "sync is not implemented yet", repo_id: None, path: None, field: None, git2: None }
...
test result: FAILED. 0 passed; 7 failed
```

- [ ] **Step 33: Implement `sync`**

In `src/git/ops.rs`, replace the stub

```rust
pub fn sync(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("sync is not implemented yet"))
}
```

with:

```rust
pub fn sync(ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    // A sync of a repo that was never materialised is a clone: the caller asked
    // for "make this tree match the remote", and there is no earlier step they
    // should have had to run first.
    if !ctx.tree.exists() && ctx.def.remote.is_some() {
        return clone(ctx);
    }

    let repo = open_tree(ctx)?;
    require_branch(&repo, &ctx.def.branch)?;

    let mut out = OpOutcome::new("no_changes", &ctx.def.branch);
    out.head_before = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|o| o.to_string());

    // Committing everything BEFORE the fetch is load-bearing, not convenient.
    // Afterwards every tracked file is at a committed state, which is what makes
    // the merge's forced checkout provably unable to destroy work, and it
    // collapses "HEAD is unborn" to "the tree really was empty".
    let (commit, files) = stage_and_commit(&repo, ctx)?;
    out.committed = commit.is_some();
    out.commit = commit.map(|o| o.to_string());
    out.files_committed = files;
    if commit.is_some() {
        out.outcome = "committed";
    }

    if ctx.def.remote.is_none() {
        // Local version history for the child process, with nothing to publish
        // it to, is a legitimate configuration.
        out.head_after = repo
            .head()
            .ok()
            .and_then(|h| h.target())
            .map(|o| o.to_string());
        ctx.phase(Phase::Finalizing);
        return Ok(out);
    }

    let fetched = fetch(&repo, ctx)?;
    out.fetched = true;
    out.fetched_head = fetched.map(|o| o.to_string());

    apply_merge(&repo, ctx, &mut out)?;

    record_ahead_behind(&repo, ctx, &mut out);
    // Measured before the push, because afterwards it is 0 by construction and
    // every no-op sync would claim it had published something.
    let had_local_commits = out.ahead > 0;

    if ctx.request.push {
        push_branch(&repo, ctx, &mut out)?;
        if out.outcome == "no_changes" && had_local_commits {
            out.outcome = "pushed";
        }
    }

    out.head_after = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|o| o.to_string());
    record_ahead_behind(&repo, ctx, &mut out);
    ctx.phase(Phase::Finalizing);
    Ok(out)
}
```

Then delete the whole `run_dispatches_to_a_verb_of_the_same_name` test from `mod tests` — its
list is now empty, and every arm of `run` reaches a real verb. Task 6's comment on that test
said so.

- [ ] **Step 34: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::`

Expected: PASS — every `git::ops` and `git::merge` test is green; the seven from Step 31 now
pass and no test named `run_dispatches` remains.

- [ ] **Step 35: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): implement sync as commit, fetch, merge, push

Committing before the fetch is what makes the merge's forced checkout provably
unable to destroy work and collapses 'HEAD is unborn' to 'the tree was empty'.
ahead is read before the push because afterwards it is zero by construction and
every no-op sync would otherwise claim it published something."
```

---

- [ ] **Step 36: Write the failing test for a clean auto-merge**

Add to `mod tests` in `src/git/merge.rs`:

```rust
    #[test]
    fn non_overlapping_edits_auto_merge() {
        // THE regression guard for FileFavor. `MergeOptions::new()` defaults to
        // FileFavor::Normal, which merges both hunks. FileFavor::Ours resolves
        // at *hunk* granularity — it would splice the two sides together inside
        // one file and throw away b's edit, which is not whole-file
        // prefer-local, and this test is what notices.
        let o = origin_with_main();
        let a = clone_at(&o, "a");
        let b = clone_at(&o, "b");

        write_file(
            &b,
            "f.md",
            "ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n",
        );
        commit_all(&b, "b edits the top");
        push_main(&b);

        write_file(
            &a,
            "f.md",
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nNINE\n",
        );

        let fx = job_with(Some(&o), &a, JobOp::Sync, merge_only());
        let out = ops::sync(&fx.ctx).expect("non-overlapping edits merge cleanly");

        assert_eq!(out.outcome, "merged");
        assert!(out.committed, "the sync committed a's edit before merging");
        let report = out.merge.as_ref().expect("a merge report");
        assert_eq!(report.kind, "merge_commit");
        assert!(
            report.conflicts_resolved.is_empty(),
            "non-overlapping edits are not a conflict: {:?}",
            report.conflicts_resolved
        );
        assert_eq!(report.recover_hint, None);
        assert_eq!(
            read_file(&a, "f.md"),
            "ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nNINE\n",
            "both edits must survive"
        );

        let repo = git2::Repository::open(&a).expect("open a");
        let head = repo
            .head()
            .expect("head")
            .peel_to_commit()
            .expect("head commit");
        assert_eq!(head.parent_count(), 2);
        assert_no_merge_state(&a);
    }
```

- [ ] **Step 37: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app git::merge::tests::non_overlapping`

Expected: FAIL — `analyse`'s normal arm is still the Step 23 stub:

```
---- git::merge::tests::non_overlapping_edits_auto_merge stdout ----
thread '...' panicked at src/git/merge.rs:NNN:
non-overlapping edits merge cleanly: GitError { code: Internal,
message: "a diverged branch needs the prefer-local merge", repo_id: None,
path: None, field: None, git2: None }
```

- [ ] **Step 38: Implement `merge_prefer_local` and wire it into `analyse`**

Insert this function into `src/git/merge.rs` immediately above `analyse`.

```rust
/// Merge `their_oid` into HEAD, keeping the local copy of any file the two
/// sides genuinely collide on.
///
/// `Ok(None)` means there was nothing to do: the merge changed no tree *and*
/// their history is already contained in ours.
pub fn merge_prefer_local(
    repo: &git2::Repository,
    their_oid: git2::Oid,
    ctx: &OpCtx,
) -> Result<Option<MergeResult>, GitError> {
    ctx.phase(Phase::Merging);
    let ours = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|e| ctx.classify_err(&e))?;
    let theirs = repo
        .find_commit(their_oid)
        .map_err(|e| ctx.classify_err(&e))?;

    // The default is FileFavor::Normal and it has to stay that way. `Ours`
    // resolves at *hunk* granularity: it splices the two sides together inside
    // one file, which is not "the local copy of a conflicting file wins whole".
    // With Normal, non-overlapping edits to one file still auto-merge and the
    // prefer-local rule applies only where the sides really collide. (`Ours`
    // also leaves every tree-level conflict unresolved anyway.)
    let opts = git2::MergeOptions::new();

    // Never Repository::merge: it writes MERGE_HEAD/MERGE_MSG, checks out
    // immediately, and leaves RepositoryState::Merge behind. merge_commits is
    // pure — a detached in-memory index, nothing on disk — so every failure
    // path before the `commit` line below is a no-op and the next sync just
    // redoes the whole thing.
    let mut idx = repo
        .merge_commits(&ours, &theirs, Some(&opts))
        .map_err(|e| ctx.classify_err(&e))?;

    if idx.has_conflicts() {
        return Err(GitError::merge_unresolvable().with_repo(&ctx.def.id));
    }

    // write_tree_to, not write_tree: this index is detached from the repository
    // and has no backing file to write itself into.
    let tree_oid = idx
        .write_tree_to(repo)
        .map_err(|e| ctx.classify_err(&e))?;
    if tree_oid == ours.tree_id()
        && repo
            .graph_descendant_of(ours.id(), their_oid)
            .map_err(|e| ctx.classify_err(&e))?
    {
        return Ok(None);
    }

    let resolved: Vec<String> = Vec::new();
    let sig = git2::Signature::now(&ctx.author_name(), &ctx.author_email())
        .map_err(|e| ctx.classify_err(&e))?;
    let list = if resolved.is_empty() {
        "  (none)".to_string()
    } else {
        resolved
            .iter()
            .map(|p| format!("  {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let msg = format!(
        "Merge {remote}/{branch} into {branch}\n\n\
         Automatic two-way sync by {app} on {host} at {ts}.\n\
         Conflicting files were resolved in favour of the local copy:\n{list}\n\n\
         The remote version of each file above is preserved as the second parent:\n\
         git show <this-commit>^2:<path>",
        remote = ctx.def.remote_name,
        branch = ctx.def.branch,
        app = ctx.identity.app_name,
        host = ctx.identity.hostname,
        ts = utc_stamp(now_ms()),
    );

    // Parent 0 is ours and parent 1 is theirs, which is what makes
    // `git show <merge>^2:<path>` the recovery command the message promises.
    // Some("HEAD") advances the branch ref and leaves HEAD attached.
    let tree = repo
        .find_tree(tree_oid)
        .map_err(|e| ctx.classify_err(&e))?;
    let merge_commit = repo
        .commit(Some("HEAD"), &sig, &sig, &msg, &tree, &[&ours, &theirs])
        .map_err(|e| ctx.classify_err(&e))?;

    ctx.phase(Phase::CheckingOut);
    // Forcing is safe here and only here: the merged tree was computed from
    // `ours`, which the commit-all step already wrote, so there is no
    // uncommitted tracked work for it to destroy. `force` alone does not remove
    // untracked files — `remove_untracked` is deliberately not set.
    let mut co = git2::build::CheckoutBuilder::new();
    co.force();
    repo.checkout_head(Some(&mut co))
        .map_err(|e| ctx.classify_err(&e))?;
    // Defensive: merge_commits sets no MERGE_HEAD, so there should be nothing to
    // clean. If an earlier operation left state behind, this is the one place
    // that reliably runs after a successful merge.
    let _ = repo.cleanup_state();

    Ok(Some(MergeResult {
        merge_commit,
        conflicts_resolved: resolved,
        unrelated: false,
    }))
}
```

Then, in `analyse`, replace the stub

```rust
    // Everything left is ANALYSIS_NORMAL: both sides moved.
    Err(GitError::internal(
        "a diverged branch needs the prefer-local merge",
    ))
```

with:

```rust
    // Everything left is ANALYSIS_NORMAL: both sides moved.
    match merge_prefer_local(repo, their_oid, ctx)? {
        Some(result) => Ok(MergeOutcome::Merged(result)),
        None => Ok(MergeOutcome::UpToDate),
    }
```

- [ ] **Step 39: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::merge`

Expected: PASS — `non_overlapping_edits_auto_merge` is green along with the seven earlier
`git::merge` tests.

- [ ] **Step 40: Commit**

```bash
git add src/git/merge.rs
git commit -m "feat(git): merge in memory with merge_commits and the default file favor

Repository::merge writes MERGE_HEAD, checks out immediately and leaves
RepositoryState::Merge behind; merge_commits is pure, so every failure path
before the commit line is a no-op on disk. FileFavor stays Normal: Ours resolves
at hunk granularity and would splice the two sides together inside one file."
```

---

- [ ] **Step 41: Write the failing tests for whole-file prefer-local resolution**

Five rows of the §10.3 matrix, all the same behaviour seen from different angles: what happens
to a file the two sides really collided on. Add to `mod tests` in `src/git/merge.rs`:

```rust
    /// The blob at `path` in the merge commit's second parent — the copy that
    /// prefer-local resolution set aside.
    fn remote_side(tree: &Path, path: &str) -> Option<Vec<u8>> {
        let repo = git2::Repository::open(tree).expect("open tree");
        let head = repo
            .head()
            .expect("head")
            .peel_to_commit()
            .expect("head commit");
        let theirs = head.parent(1).expect("a merge commit has a second parent");
        let entry = theirs
            .tree()
            .expect("remote tree")
            .get_path(Path::new(path))
            .ok()?;
        Some(
            repo.find_blob(entry.id())
                .expect("blob")
                .content()
                .to_vec(),
        )
    }

    #[test]
    fn prefers_local_whole_file_on_conflict() {
        let o = origin_with_main();
        let a = clone_at(&o, "a");
        let b = clone_at(&o, "b");

        write_file(
            &b,
            "f.md",
            "REMOTE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n",
        );
        commit_all(&b, "b rewrites line one");
        push_main(&b);

        let local = "LOCAL\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n";
        write_file(&a, "f.md", local);

        let fx = job_with(Some(&o), &a, JobOp::Sync, merge_only());
        let out = ops::sync(&fx.ctx).expect("prefer-local resolves this");

        assert_eq!(out.outcome, "merged");
        let report = out.merge.as_ref().expect("a merge report");
        assert_eq!(report.kind, "merge_commit");
        assert_eq!(report.conflicts_resolved, vec!["f.md".to_string()]);
        // Byte-for-byte: a hunk-level resolution would produce something that
        // merely *contains* the local line.
        assert_eq!(read_file(&a, "f.md"), local);
        assert!(!read_file(&a, "f.md").contains("<<<<<<<"));

        let repo = git2::Repository::open(&a).expect("open a");
        let head = repo
            .head()
            .expect("head")
            .peel_to_commit()
            .expect("head commit");
        assert_eq!(head.parent_count(), 2);
        assert_no_merge_state(&a);
    }

    #[test]
    fn losing_remote_version_stays_recoverable() {
        // Prefer-local is only defensible because the other version is still
        // reachable, so the outcome says out loud how to reach it.
        let o = origin_with_main();
        let a = clone_at(&o, "a");
        let b = clone_at(&o, "b");

        let remote = "REMOTE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n";
        write_file(&b, "f.md", remote);
        commit_all(&b, "b rewrites line one");
        push_main(&b);

        write_file(
            &a,
            "f.md",
            "LOCAL\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n",
        );

        let fx = job_with(Some(&o), &a, JobOp::Sync, merge_only());
        let out = ops::sync(&fx.ctx).expect("sync");

        let report = out.merge.as_ref().expect("a merge report");
        let sha = report
            .merge_commit
            .as_ref()
            .expect("a merge commit was written");
        assert_eq!(
            report.recover_hint.as_deref(),
            Some(format!("git show {sha}^2:f.md").as_str())
        );
        assert_eq!(
            remote_side(&a, "f.md").expect("f.md on the remote side"),
            remote.as_bytes()
        );

        // The commit message names every overwritten path and the command.
        let repo = git2::Repository::open(&a).expect("open a");
        let head = repo
            .head()
            .expect("head")
            .peel_to_commit()
            .expect("head commit");
        let message = head.message().unwrap_or_default();
        assert!(message.contains("  f.md"), "{message}");
        assert!(
            message.contains("git show <this-commit>^2:<path>"),
            "{message}"
        );
        assert_no_merge_state(&a);
    }

    #[test]
    fn add_add_with_different_content() {
        let o = origin_with_main();
        let a = clone_at(&o, "a");
        let b = clone_at(&o, "b");

        write_file(&b, "new.txt", "from b\n");
        commit_all(&b, "b adds new.txt");
        push_main(&b);

        write_file(&a, "new.txt", "from a\n");

        let fx = job_with(Some(&o), &a, JobOp::Sync, merge_only());
        let out = ops::sync(&fx.ctx).expect("sync");

        assert_eq!(out.outcome, "merged");
        assert_eq!(
            out.merge
                .as_ref()
                .expect("a merge report")
                .conflicts_resolved,
            vec!["new.txt".to_string()]
        );
        assert_eq!(read_file(&a, "new.txt"), "from a\n");
        assert_eq!(
            remote_side(&a, "new.txt").expect("new.txt on the remote side"),
            b"from b\n"
        );
        assert_no_merge_state(&a);
    }

    #[test]
    fn local_delete_beats_remote_modify() {
        let o = origin_with_main();
        let a = clone_at(&o, "a");
        let b = clone_at(&o, "b");

        let remote = "REMOTE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n";
        write_file(&b, "f.md", remote);
        commit_all(&b, "b modifies f.md");
        push_main(&b);

        std::fs::remove_file(a.join("f.md")).expect("delete f.md locally");

        let fx = job_with(Some(&o), &a, JobOp::Sync, merge_only());
        let out = ops::sync(&fx.ctx).expect("sync");

        assert_eq!(out.outcome, "merged");
        assert_eq!(
            out.merge
                .as_ref()
                .expect("a merge report")
                .conflicts_resolved,
            vec!["f.md".to_string()]
        );
        assert!(
            !a.join("f.md").exists(),
            "the local deletion must win the whole file"
        );
        assert_eq!(
            remote_side(&a, "f.md").expect("f.md on the remote side"),
            remote.as_bytes()
        );
        assert_no_merge_state(&a);
    }

    #[test]
    fn remote_delete_loses_to_local_modify() {
        let o = origin_with_main();
        let a = clone_at(&o, "a");
        let b = clone_at(&o, "b");

        std::fs::remove_file(b.join("f.md")).expect("delete f.md remotely");
        commit_all(&b, "b deletes f.md");
        push_main(&b);

        let local = "LOCAL\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n";
        write_file(&a, "f.md", local);

        let fx = job_with(Some(&o), &a, JobOp::Sync, merge_only());
        let out = ops::sync(&fx.ctx).expect("sync");

        assert_eq!(out.outcome, "merged");
        assert_eq!(
            out.merge
                .as_ref()
                .expect("a merge report")
                .conflicts_resolved,
            vec!["f.md".to_string()]
        );
        assert_eq!(read_file(&a, "f.md"), local);
        assert_eq!(
            remote_side(&a, "f.md"),
            None,
            "the remote side of the merge really did delete it"
        );
        assert_no_merge_state(&a);
    }
```

- [ ] **Step 42: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::merge`

Expected: FAIL — five failures. `merge_prefer_local` currently refuses any conflicted index:

```
---- git::merge::tests::prefers_local_whole_file_on_conflict stdout ----
thread '...' panicked at src/git/merge.rs:NNN:
prefer-local resolves this: GitError { code: MergeUnresolvable,
message: "...", repo_id: Some("notes"), path: None, field: None, git2: None }

failures:
    git::merge::tests::add_add_with_different_content
    git::merge::tests::local_delete_beats_remote_modify
    git::merge::tests::losing_remote_version_stays_recoverable
    git::merge::tests::prefers_local_whole_file_on_conflict
    git::merge::tests::remote_delete_loses_to_local_modify
```

- [ ] **Step 43: Implement whole-file prefer-local resolution**

In `src/git/merge.rs`, inside `merge_prefer_local`, replace

```rust
    if idx.has_conflicts() {
        return Err(GitError::merge_unresolvable().with_repo(&ctx.def.id));
    }
```

with:

```rust
    let mut resolved: Vec<String> = Vec::new();
    if idx.has_conflicts() {
        let conflicts: Vec<git2::IndexConflict> = idx
            .conflicts()
            .and_then(|it| it.collect::<Result<Vec<_>, _>>())
            .map_err(|e| ctx.classify_err(&e))?;

        for c in conflicts {
            let raw = conflict_path_bytes(&c)?;
            let path = bytes_to_path(&raw)?;
            // Drops all three stages for this path at once, moving the conflict
            // record into the index's resolve-undo section.
            idx.remove_path(&path)
                .map_err(|e| ctx.classify_err(&e))?;
            match c.our {
                // IndexEntry derives only Debug — it is NOT Clone — so the `our`
                // entry is MOVED out of the conflict rather than copied. Zeroing
                // the flags is what makes it stage 0: the stage lives in bits
                // 12-13 of `flags`, and Index::add recomputes the path length in
                // the low bits itself.
                Some(mut e) => {
                    e.flags = 0;
                    e.flags_extended = 0;
                    idx.add(&e).map_err(|err| ctx.classify_err(&err))?;
                }
                // Deleted locally, modified remotely: prefer-local means it
                // stays deleted. The remote version is still reachable via `^2`.
                None => {}
            }
            resolved.push(path.to_string_lossy().into_owned());
        }
    }
    if idx.has_conflicts() {
        return Err(GitError::merge_unresolvable().with_repo(&ctx.def.id));
    }
```

Then delete the now-duplicate declaration further down:

```rust
    let resolved: Vec<String> = Vec::new();
```

and change the returned struct's `conflicts_resolved` to move the real vector — it already
reads `conflicts_resolved: resolved,`, so no edit is needed there.

- [ ] **Step 44: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::merge`

Expected: PASS — `test result: ok. 13 passed; 0 failed`.

- [ ] **Step 45: Commit**

```bash
git add src/git/merge.rs
git commit -m "feat(git): resolve conflicts to the local copy, whole file at a time

remove_path drops all three stages; the 'our' entry is moved out of the conflict
because IndexEntry is not Clone, and its flags are zeroed so it re-enters at
stage 0. A path deleted locally and modified remotely stays deleted: the remote
copy is still reachable through the merge commit's second parent."
```

---

- [ ] **Step 46: Write the failing test for the dir/file collision**

Add to `mod tests` in `src/git/merge.rs`:

```rust
    #[test]
    fn dir_file_collision_is_refused() {
        // A local file `x` against a remote directory `x/inner.txt` produces an
        // index with `x` at stage 2 and `x/inner.txt` at stage 0. Whole-file
        // resolution writes a tree from that *successfully*, in which the remote
        // directory wins and the local file is silently gone — with a clean
        // statuses() afterwards, so nothing downstream ever notices.
        let o = origin_with_main();
        let a = clone_at(&o, "a");
        let b = clone_at(&o, "b");

        write_file(&b, "x/inner.txt", "remote inner\n");
        commit_all(&b, "b adds the x directory");
        push_main(&b);

        write_file(&a, "x", "local file\n");
        // Committed by hand so the sync's commit-all step is a no-op and HEAD is
        // stable across the failure being asserted on.
        let before = commit_all(&a, "a adds the x file");

        let fx = job_with(Some(&o), &a, JobOp::Sync, merge_only());
        let e = ops::sync(&fx.ctx)
            .expect_err("a file against a directory has no whole-file resolution");

        assert_eq!(e.code(), GitErrorCode::MergePathTypeConflict);
        assert_eq!(
            read_file(&a, "x"),
            "local file\n",
            "the refusal must be a no-op on disk"
        );
        assert_eq!(head_of(&a), before, "the refusal must not move HEAD");
        assert_no_merge_state(&a);
    }
```

- [ ] **Step 47: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app git::merge::tests::dir_file`

Expected: FAIL — the merge *succeeds*, which is exactly the hole:

```
---- git::merge::tests::dir_file_collision_is_refused stdout ----
thread '...' panicked at src/git/merge.rs:NNN:
a file against a directory has no whole-file resolution: OpOutcome {
outcome: "merged", branch: "main", ... }
```

(If you instead reach the `read_file` assertion, it panics with
`read file: Os { code: 21, kind: IsADirectory, message: "Is a directory" }` — the local file
was replaced by the remote's directory. Either way, the merge must not have succeeded.)

- [ ] **Step 48: Implement the dir/file type guard**

In `src/git/merge.rs`, inside `merge_prefer_local`, insert this block immediately after the
`let conflicts: Vec<git2::IndexConflict> = …;` binding and before the `for c in conflicts`
loop:

```rust
        let our_tree = ours.tree().map_err(|e| ctx.classify_err(&e))?;
        let their_tree = theirs.tree().map_err(|e| ctx.classify_err(&e))?;

        // One side has a blob at this path and the other has a tree. Whole-file
        // resolution is not defined for that: it writes a tree in which one side
        // simply disappears, and reports success. Refuse before touching
        // anything, and let a human decide.
        for c in &conflicts {
            let raw = conflict_path_bytes(c)?;
            let p = bytes_to_path(&raw)?;
            let ours_kind = our_tree.get_path(&p).ok().and_then(|e| e.kind());
            let theirs_kind = their_tree.get_path(&p).ok().and_then(|e| e.kind());
            if let (Some(a), Some(b)) = (ours_kind, theirs_kind) {
                if a != b {
                    return Err(GitError::merge_path_type_conflict(&p).with_repo(&ctx.def.id));
                }
            }
        }
```

A modify/delete conflict reaches the same loop with `None` on one side, which is why the guard
fires only when *both* kinds are known and they differ.

- [ ] **Step 49: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::merge`

Expected: PASS — `test result: ok. 14 passed; 0 failed`.

- [ ] **Step 50: Commit**

```bash
git add src/git/merge.rs
git commit -m "feat(git): refuse a merge where one side has a file and the other a directory

Whole-file resolution writes a tree for this case successfully, with the local
file silently gone and a clean statuses() afterwards. The guard checks both
sides' entry kinds and refuses before anything reaches disk."
```

---

- [ ] **Step 51: Write the failing test for unrelated histories**

Add to `mod tests` in `src/git/merge.rs`:

```rust
    #[test]
    fn unrelated_histories_use_empty_base() {
        // A local `init` + commit against a remote that already has its own root
        // commit: there is no merge base at all. This is what a host does when
        // it materialises a repo before the remote is reachable.
        let o = origin_with_main();
        let a = init_tree(&o, "a");
        write_file(&a, "local.txt", "only local\n");

        let fx = job_with(Some(&o), &a, JobOp::Sync, merge_only());
        let out = ops::sync(&fx.ctx).expect("an empty ancestor makes this well defined");

        assert_eq!(out.outcome, "merged_unrelated");
        assert_eq!(
            out.merge.as_ref().expect("a merge report").kind,
            "merge_commit_unrelated"
        );
        assert_eq!(read_file(&a, "local.txt"), "only local\n");
        assert_eq!(read_file(&a, "f.md"), SEEDED, "both trees must be present");

        let repo = git2::Repository::open(&a).expect("open a");
        let head = repo
            .head()
            .expect("head")
            .peel_to_commit()
            .expect("head commit");
        assert_eq!(head.parent_count(), 2);
        assert_no_merge_state(&a);
    }
```

- [ ] **Step 52: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app git::merge::tests::unrelated`

Expected: FAIL — `merge_commits` cannot find a base:

```
---- git::merge::tests::unrelated_histories_use_empty_base stdout ----
thread '...' panicked at src/git/merge.rs:NNN:
an empty ancestor makes this well defined: GitError { code: Internal,
message: "no merge base found", repo_id: Some("notes"), path: None, field: None,
git2: Some(Git2Hint { class: "Merge", code: "NotFound" }) }
```

- [ ] **Step 53: Implement the two-way merge against an empty ancestor**

In `src/git/merge.rs`, inside `merge_prefer_local`, replace

```rust
    let mut idx = repo
        .merge_commits(&ours, &theirs, Some(&opts))
        .map_err(|e| ctx.classify_err(&e))?;
```

with:

```rust
    let unrelated = matches!(
        repo.merge_base(ours.id(), their_oid),
        Err(ref e) if e.code() == git2::ErrorCode::NotFound
    );
    let mut idx = if unrelated {
        // No common ancestor exists, so a two-way merge against an empty one is
        // the well-defined reading: every path present on both sides becomes an
        // add/add conflict, which prefer-local resolves exactly as it resolves
        // any other.
        let empty_oid = repo
            .treebuilder(None)
            .and_then(|b| b.write())
            .map_err(|e| ctx.classify_err(&e))?;
        let empty = repo
            .find_tree(empty_oid)
            .map_err(|e| ctx.classify_err(&e))?;
        let our_tree = ours.tree().map_err(|e| ctx.classify_err(&e))?;
        let their_tree = theirs.tree().map_err(|e| ctx.classify_err(&e))?;
        repo.merge_trees(&empty, &our_tree, &their_tree, Some(&opts))
            .map_err(|e| ctx.classify_err(&e))?
    } else {
        // Never Repository::merge: it writes MERGE_HEAD/MERGE_MSG, checks out
        // immediately, and leaves RepositoryState::Merge behind. merge_commits
        // is pure — a detached in-memory index, nothing on disk — so every
        // failure path before the `commit` line below is a no-op and the next
        // sync just redoes the whole thing.
        repo.merge_commits(&ours, &theirs, Some(&opts))
            .map_err(|e| ctx.classify_err(&e))?
    };
```

(The "never Repository::merge" comment moves down into the `else` arm; delete the copy that was
above the old binding.)

Then, at the end of the function, replace

```rust
        unrelated: false,
    }))
```

with:

```rust
        unrelated,
    }))
```

- [ ] **Step 54: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::merge`

Expected: PASS — `test result: ok. 15 passed; 0 failed`.

- [ ] **Step 55: Commit**

```bash
git add src/git/merge.rs
git commit -m "feat(git): merge unrelated histories against an empty ancestor

A local init plus commit against a remote with its own root commit has no merge
base. A two-way merge against an empty tree is the well-defined reading: every
path on both sides becomes an add/add conflict, which prefer-local already
resolves."
```

---

- [ ] **Step 56: Write the two regression guards**

These two lock in decisions that are invisible in a diff: one line that was *not* written
(`remove_untracked`) and one API that was *not* called (`Repository::merge`). Add to
`mod tests` in `src/git/merge.rs`:

```rust
    #[test]
    fn untracked_file_survives_a_merge_sync() {
        let o = origin_with_main();
        let a = clone_at(&o, "a");
        // Ignored, so `git add -A` does not quietly turn it into a tracked file
        // and hide what this test is about.
        write_file(&a, ".gitignore", "scratch.txt\n");
        commit_all(&a, "a ignores scratch.txt");
        push_main(&a);

        let b = clone_at(&o, "b");
        write_file(
            &b,
            "f.md",
            "REMOTE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n",
        );
        commit_all(&b, "b rewrites line one");
        push_main(&b);

        write_file(
            &a,
            "f.md",
            "LOCAL\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n",
        );
        write_file(&a, "scratch.txt", "not committed\n");

        let fx = job_with(Some(&o), &a, JobOp::Sync, merge_only());
        let out = ops::sync(&fx.ctx).expect("sync");

        assert_eq!(out.outcome, "merged");
        // The merge checks out with force(); force alone does not remove
        // untracked files, and `remove_untracked` is deliberately never set.
        assert_eq!(read_file(&a, "scratch.txt"), "not committed\n");

        let repo = git2::Repository::open(&a).expect("open a");
        let head = repo
            .head()
            .expect("head")
            .peel_to_commit()
            .expect("head commit");
        assert!(
            head.tree()
                .expect("tree")
                .get_path(Path::new("scratch.txt"))
                .is_err(),
            "the ignored scratch file must not have been committed"
        );
        assert_no_merge_state(&a);
    }

    #[test]
    fn no_merge_state_is_ever_left() {
        // Spelled out once, against the case most likely to violate it: a
        // conflicted merge that succeeded. Repository::merge would leave
        // MERGE_HEAD, MERGE_MSG and RepositoryState::Merge here; merge_commits
        // cannot, because it never touches the repository at all.
        let o = origin_with_main();
        let a = clone_at(&o, "a");
        let b = clone_at(&o, "b");

        write_file(
            &b,
            "f.md",
            "REMOTE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n",
        );
        commit_all(&b, "b rewrites line one");
        push_main(&b);

        write_file(
            &a,
            "f.md",
            "LOCAL\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n",
        );

        let fx = job_with(Some(&o), &a, JobOp::Sync, merge_only());
        let out = ops::sync(&fx.ctx).expect("sync");
        assert_eq!(out.outcome, "merged");

        assert!(!a.join(".git/MERGE_HEAD").exists());
        assert!(!a.join(".git/MERGE_MSG").exists());
        let repo = git2::Repository::open(&a).expect("open a");
        assert_eq!(repo.state(), git2::RepositoryState::Clean);
        assert!(!repo.head_detached().expect("head_detached"));
        assert!(!read_file(&a, "f.md").contains("<<<<<<<"));
        // And the same four properties, checked the way every other merge test
        // in this file checks them.
        assert_no_merge_state(&a);
    }
```

- [ ] **Step 57: Run the guards, then prove they bite**

Run: `cargo test --bin chrome-host-app git::merge`

Expected: PASS — `test result: ok. 17 passed; 0 failed`. **These two are expected to pass on
the first run**: no production code changes for them, because the behaviour they lock in is
already correct. That makes them worth falsifying once, so you know they are not vacuous:

1. In `merge_prefer_local`, temporarily change `co.force();` to
   `co.force().remove_untracked(true);` and re-run.
   Expected: FAIL — `untracked_file_survives_a_merge_sync` panics with
   `read file: Os { code: 2, kind: NotFound, message: "No such file or directory" }`.
   Revert.
2. In `merge_prefer_local`, temporarily replace the `let _ = repo.cleanup_state();` line with
   `repo.merge(&[], None, None).ok();` and re-run.
   Expected: FAIL — `no_merge_state_is_ever_left` panics on
   `assert!(!a.join(".git/MERGE_HEAD").exists())`.
   Revert.

Confirm `cargo test --bin chrome-host-app git::merge` is green again before committing.

- [ ] **Step 58: Commit**

```bash
git add src/git/merge.rs
git commit -m "test(git): lock in the two merge decisions that are invisible in a diff

remove_untracked is never set on the merge's forced checkout, and
Repository::merge is never called. Both are absences, so only a test that
asserts on the tree afterwards can defend them."
```

---

- [ ] **Step 59: Verify the whole task and commit any cleanup**

Run, in order:

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test --bin chrome-host-app
```

Expected: `cargo fmt` produces no diff (or only whitespace, which you commit); clippy is
silent; the full suite is green. Two things to look for specifically:

- **Unused imports.** `src/git/merge.rs`'s module-level `use` block imports `Phase`,
  `bytes_to_path`, `now_ms` and `utc_stamp`, and its `mod tests` imports `ops`, `OpRequest` and
  `Path`; every one of them is used by the end of the task. If clippy reports one as unused, a
  step above was skipped.
- **The dispatch scaffolding.** `git grep run_dispatches_to_a_verb_of_the_same_name` must
  return nothing: Step 33 deleted it, and `ops::run` now reaches a real implementation for all
  eight `JobOp` values.

Then:

```bash
git add -A
git commit -m "chore(git): format and lint the merge and sync slice"
```

(If `git status` is clean after the three commands, skip the commit — the task is already in a
committed, green state.)
