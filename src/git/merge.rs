//! The prefer-local merge.
//!
//! Two clones of one repository edit the same file and neither user is present
//! to resolve the result. This module's whole job is to make that outcome
//! predictable: the local copy of a genuinely conflicting file wins whole, the
//! remote copy stays reachable as the merge commit's second parent, and the
//! working tree never sees a conflict marker.

use crate::git::error::GitError;
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

    let mut resolved: Vec<String> = Vec::new();
    if idx.has_conflicts() {
        let conflicts: Vec<git2::IndexConflict> = idx
            .conflicts()
            .and_then(|it| it.collect::<Result<Vec<_>, _>>())
            .map_err(|e| ctx.classify_err(&e))?;

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

        for c in conflicts {
            let raw = conflict_path_bytes(&c)?;
            let path = bytes_to_path(&raw)?;
            // Drops all three stages for this path at once, moving the conflict
            // record into the index's resolve-undo section.
            idx.remove_path(&path).map_err(|e| ctx.classify_err(&e))?;
            // A `None` here is "deleted locally, modified remotely": prefer-local
            // means it stays deleted, and the remote version is still reachable
            // through the merge commit's second parent, so that case needs no
            // code at all.
            //
            // IndexEntry derives only Debug — it is NOT Clone — so the `our`
            // entry is MOVED out of the conflict rather than copied. Zeroing the
            // flags is what makes it stage 0: the stage lives in bits 12-13 of
            // `flags`, and Index::add recomputes the path length in the low bits
            // itself.
            if let Some(mut e) = c.our {
                e.flags = 0;
                e.flags_extended = 0;
                idx.add(&e).map_err(|err| ctx.classify_err(&err))?;
            }
            resolved.push(path.to_string_lossy().into_owned());
        }
    }
    // §14 risk 7: the loop above resolves every conflict it saw, so a conflicted
    // index here means `conflicts()` under-reported — a libgit2 behaviour change,
    // not a merge outcome. The debug assertion makes that loud in development;
    // the hard check below is what keeps a release build from writing a tree with
    // conflict stages still in it.
    debug_assert!(
        !idx.has_conflicts(),
        "every conflict conflicts() reported was resolved, yet the index still has some"
    );
    if idx.has_conflicts() {
        return Err(GitError::merge_unresolvable().with_repo(&ctx.def.id));
    }

    // write_tree_to, not write_tree: this index is detached from the repository
    // and has no backing file to write itself into.
    let tree_oid = idx.write_tree_to(repo).map_err(|e| ctx.classify_err(&e))?;
    if tree_oid == ours.tree_id()
        && repo
            .graph_descendant_of(ours.id(), their_oid)
            .map_err(|e| ctx.classify_err(&e))?
    {
        return Ok(None);
    }

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
    let tree = repo.find_tree(tree_oid).map_err(|e| ctx.classify_err(&e))?;
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
        unrelated,
    }))
}

/// Decide what to do with the fetched remote tip — and, for the two
/// fast-forward shapes, do it.
pub fn analyse(repo: &git2::Repository, ctx: &OpCtx) -> Result<MergeOutcome, GitError> {
    let tracking = format!("refs/remotes/{}/{}", ctx.def.remote_name, ctx.def.branch);
    let their_ref = match repo.find_reference(&tracking) {
        Ok(r) => r,
        // A remote that has no such branch yet is not an error: push creates it.
        Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(MergeOutcome::NoRemoteBranch),
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
    match merge_prefer_local(repo, their_oid, ctx)? {
        Some(result) => Ok(MergeOutcome::Merged(result)),
        None => Ok(MergeOutcome::UpToDate),
    }
}

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
        // Bound rather than returned inline: the `Reference` temporary borrows
        // `repo`, and a tail expression keeps it alive past the end of the block.
        let oid = repo
            .find_reference("refs/remotes/origin/main")
            .expect("tracking ref")
            .target()
            .expect("tracking ref has a target");
        oid
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

    pub(crate) fn job_with(o: Option<&Origin>, tree: &Path, op: JobOp, request: OpRequest) -> Job {
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

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;
    // Named only by the assertions below; the module itself has no use for it.
    use crate::git::error::GitErrorCode;
    use crate::git::jobs::JobOp;
    use crate::git::ops::{self, OpRequest};
    use std::path::Path;

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
            id: git2::Oid::ZERO_SHA1,
            flags: 0,
            flags_extended: 0,
            path: path.as_bytes().to_vec(),
        }
    }

    /// `OpRequest::default()` has `push = true`. Every merge-matrix test below
    /// is about what lands on disk locally, so none of them push.
    fn merge_only() -> OpRequest {
        OpRequest {
            push: false,
            ..OpRequest::default()
        }
    }

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
        // Same reason as `fetch_main`: the blob borrows `repo`, so its content is
        // copied out before the block ends.
        let blob = repo.find_blob(entry.id()).expect("blob");
        let content = blob.content().to_vec();
        Some(content)
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
            merge_commit: git2::Oid::ZERO_SHA1,
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
                head: git2::Oid::ZERO_SHA1
            },
            MergeOutcome::AdoptedRemote { .. }
        ));
        assert!(matches!(
            MergeOutcome::FastForward {
                head: git2::Oid::ZERO_SHA1
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

    #[test]
    fn non_overlapping_edits_auto_merge() {
        // THE regression guard for FileFavor. `MergeOptions::new()` defaults to
        // `FileFavor::Normal`, which merges non-overlapping hunks *inside* one
        // file. `FileFavor::Ours` resolves at hunk granularity and would take
        // the local side of both hunks, silently dropping the remote's edit —
        // which is not "the local copy of a conflicting file wins whole", it is
        // splicing two people's work together.
        let o = origin_with_main();
        let a = clone_at(&o, "a");
        let b = clone_at(&o, "b");

        // b edits the last line, a edits the first: the two hunks never touch.
        write_file(
            &b,
            "f.md",
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nREMOTE\n",
        );
        commit_all(&b, "b edits the last line");
        push_main(&b);

        write_file(
            &a,
            "f.md",
            "LOCAL\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n",
        );

        let fx = job_with(Some(&o), &a, JobOp::Sync, merge_only());
        let out = ops::sync(&fx.ctx).expect("non-overlapping edits merge cleanly");

        assert_eq!(out.outcome, "merged");
        let report = out.merge.as_ref().expect("a merge report");
        assert_eq!(report.kind, "merge_commit");
        assert!(
            report.conflicts_resolved.is_empty(),
            "nothing collided, so nothing was resolved in favour of either side"
        );
        // Both edits survive. FileFavor::Ours would keep only the local one.
        assert_eq!(
            read_file(&a, "f.md"),
            "LOCAL\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nREMOTE\n"
        );
        assert_no_merge_state(&a);
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

    /// Guards `remove_ignored`, **not** `remove_untracked`.
    ///
    /// `scratch.txt` has to be gitignored or `sync`'s `git add -A` would commit
    /// it before the merge ever runs — and an ignored file is invisible to
    /// `remove_untracked`, which only deletes files git considers untracked.
    /// Falsify by setting `remove_ignored(true)` on the merge checkout;
    /// `remove_untracked(true)` alone leaves this test green, which is why
    /// `an_untracked_file_survives_a_merge_pull` exists next to it.
    #[test]
    fn an_ignored_file_survives_a_merge_sync() {
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
        // The merge checks out with force(); force alone removes nothing that is
        // not tracked, and `remove_ignored` is deliberately never set.
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

    /// Guards `remove_untracked`, which the ignored-file test above cannot reach.
    ///
    /// `pull` never runs `git add -A`, and an untracked file does not make a tree
    /// dirty, so `scratch.txt` is still untracked when `merge_prefer_local` force
    /// checks out its result. Falsify by setting `remove_untracked(true)` there.
    #[test]
    fn an_untracked_file_survives_a_merge_pull() {
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

        // Committed by hand so the tree is clean and `pull` will not refuse it,
        // while still diverging from the remote so the merge really runs.
        write_file(
            &a,
            "f.md",
            "LOCAL\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n",
        );
        commit_all(&a, "a rewrites line one too");
        // Genuinely untracked: not gitignored, and nothing in a pull stages it.
        write_file(&a, "scratch.txt", "not committed\n");

        let fx = job_with(Some(&o), &a, JobOp::Pull, merge_only());
        let out = ops::pull(&fx.ctx).expect("pull");

        assert_eq!(out.outcome, "merged");
        assert_eq!(read_file(&a, "scratch.txt"), "not committed\n");
        let repo = git2::Repository::open(&a).expect("open a");
        assert!(
            repo.head()
                .expect("head")
                .peel_to_commit()
                .expect("head commit")
                .tree()
                .expect("tree")
                .get_path(Path::new("scratch.txt"))
                .is_err(),
            "a pull must not have committed it either"
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
}
