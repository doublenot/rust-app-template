//! Git operations — the blocking half of the service.
//!
//! Every verb here runs inside one `spawn_blocking` closure and keeps its
//! `git2::Repository` alive for the whole job. This file holds the *context* layer:
//! the request the caller made, the identity a commit will carry, the outcome the
//! job records, and the `OpCtx` that libgit2's callbacks write through. The verbs
//! themselves land in later slices.
//!
//! Nothing under `src/git/` may name `App`, `Children`, `chrome_generation`,
//! `server_generation`, `rt.enter()`, or `tokio::process`. The only channel from
//! here back into the event loop is the job outcome.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use crate::config::SettingsSection;
use crate::git::creds::{self, HostKeyChecker, ResolvedCred};
use crate::git::error::{self, AbortReason, GitError, GitErrorCode};
use crate::git::jobs::{AbortFlag, JobOp, JobSlot, Phase, Progress};
use crate::git::merge::{self, MergeOutcome};
use crate::git::registry::{AuthorSpec, CredentialSpec, RepoDef, Warning};
use crate::settings;

/// Who the host says it is when it writes a commit nobody typed.
#[derive(Debug, Clone)]
pub struct Identity {
    pub app_name: String,
    pub identifier: String,
    pub hostname: String,
}

impl Identity {
    /// `gethostname` yields an `OsString`. A non-UTF-8 hostname is converted
    /// lossily rather than refused: the only thing it feeds is the fallback author
    /// email, and a mangled address beats a job that cannot commit at all.
    pub fn new(app_name: &str, identifier: &str) -> Identity {
        let host = gethostname::gethostname()
            .to_string_lossy()
            .trim()
            .to_string();
        Identity {
            app_name: app_name.to_string(),
            identifier: identifier.to_string(),
            hostname: if host.is_empty() {
                "localhost".to_string()
            } else {
                host
            },
        }
    }
}

/// What `sync_settings` needs to validate and write `settings.json`, cloned into
/// every `OpCtx` so the blocking thread never reaches back into `GitService`.
#[derive(Clone)]
pub struct SettingsCtx {
    pub schema: SettingsSection,
    pub settings_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct BranchRequest {
    pub name: String,
    pub create: bool,
    pub from: Option<String>,
    pub checkout: bool,
    /// `None` leaves the upstream alone, `Some(None)` unsets it, `Some(Some(x))`
    /// sets it to `x`. Two states would conflate "the body had no upstream key"
    /// with "the body said `upstream: null`", which are opposite instructions.
    pub upstream: Option<Option<String>>,
}

#[derive(Debug, Clone)]
pub struct ResetRequest {
    pub to: String,
    pub clean_untracked: bool,
    pub confirm: bool,
}

/// Everything one HTTP body, tray click, or timer tick can say about an operation.
/// One struct for all eight verbs: each reads the fields that apply to it.
#[derive(Debug, Clone)]
pub struct OpRequest {
    pub request_id: Option<String>,
    /// Replaces the stored credential wholly for this one call. Never persisted.
    pub credential: Option<CredentialSpec>,
    pub message: Option<String>,
    pub author: Option<AuthorSpec>,
    pub paths: Option<Vec<String>>,
    pub allow_empty: bool,
    /// `sync` only.
    pub push: bool,
    pub force: bool,
    /// `pull` only.
    pub commit_local: bool,
    /// `None` falls back to the repo's `restart_children_on_pull`.
    pub restart_children: Option<bool>,
    pub branch: Option<BranchRequest>,
    pub reset: Option<ResetRequest>,
}

impl Default for OpRequest {
    /// Hand-written for one field. `push` defaults to **true** because `sync` means
    /// "make the remote match"; a derived `false` would turn every sync into a
    /// local-only commit and nobody would notice until they needed the remote.
    fn default() -> OpRequest {
        OpRequest {
            request_id: None,
            credential: None,
            message: None,
            author: None,
            paths: None,
            allow_empty: false,
            push: true,
            force: false,
            commit_local: false,
            restart_children: None,
            branch: None,
            reset: None,
        }
    }
}

impl OpRequest {
    /// A timer-driven sync. Never asks for a child restart: a background pull must
    /// not yank the UI out from under someone mid-edit. A settings change still
    /// restarts, but that is `after_job`'s decision and not this flag.
    pub fn auto() -> OpRequest {
        OpRequest {
            restart_children: Some(false),
            ..OpRequest::default()
        }
    }

    /// Tray "Sync now" and the quit-time sync. The same shape as `auto` today, kept
    /// separate because only this one is ever attached to a user gesture and only
    /// this one is a candidate for a confirmation prompt.
    pub fn manual() -> OpRequest {
        OpRequest::auto()
    }
}

/// What a merge did, in the shape the job result publishes it.
#[derive(Debug, Clone, Serialize)]
pub struct MergeReport {
    /// `up_to_date` | `no_remote_branch` | `adopted_remote` | `fast_forward` |
    /// `merge_commit` | `merge_commit_unrelated`.
    pub kind: &'static str,
    pub merge_commit: Option<String>,
    pub conflicts_resolved: Vec<String>,
    /// The command that reads back the side that lost a conflict. Prefer-local
    /// resolution is only defensible because the other version is still reachable,
    /// so the outcome says out loud how to reach it.
    pub recover_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingsRejected {
    pub error: String,
}

/// The `result` object of a finished job.
///
/// Every field that is not `#[serde(skip)]` is serialized unconditionally, nulls
/// included, so a client reading `result.head_after` never has to tell "absent"
/// from "null" and the shape does not vary between verbs.
#[derive(Debug, Clone, Serialize)]
pub struct OpOutcome {
    /// One of the values in the outcome vocabulary: `up_to_date`, `no_changes`,
    /// `committed`, `fast_forward`, `merged`, `merged_unrelated`, `cloned`,
    /// `initialized`, `pushed`, `reset`, `checked_out`, `branch_created`.
    pub outcome: &'static str,
    pub branch: String,
    pub head_before: Option<String>,
    pub head_after: Option<String>,
    pub committed: bool,
    pub commit: Option<String>,
    pub files_committed: usize,
    pub fetched: bool,
    pub fetched_head: Option<String>,
    pub merge: Option<MergeReport>,
    pub pushed: bool,
    pub force_pushed: bool,
    pub upstream_set: bool,
    pub ahead: usize,
    pub behind: usize,
    pub settings_synced: bool,
    pub settings_rejected: Option<SettingsRejected>,
    /// What `after_job` decided, not what the caller asked for.
    pub restart_requested: bool,
    pub warnings: Vec<Warning>,
    /// Drives the settings-change restart. Host-internal: publishing it would
    /// invite clients to build their own restart logic on top of it.
    #[serde(skip)]
    pub settings_changed: bool,
    /// A host key accepted on first use, on its way to `git-state.json`. Never
    /// published — it is per-host state, not a property of the operation.
    #[serde(skip)]
    pub learned_fingerprint: Option<String>,
}

impl OpOutcome {
    pub fn new(outcome: &'static str, branch: &str) -> OpOutcome {
        OpOutcome {
            outcome,
            branch: branch.to_string(),
            head_before: None,
            head_after: None,
            committed: false,
            commit: None,
            files_committed: 0,
            fetched: false,
            fetched_head: None,
            merge: None,
            pushed: false,
            force_pushed: false,
            upstream_set: false,
            ahead: 0,
            behind: 0,
            settings_synced: false,
            settings_rejected: None,
            restart_requested: false,
            warnings: Vec::new(),
            settings_changed: false,
            learned_fingerprint: None,
        }
    }
}

/// Scratch that the libgit2 callbacks write through a shared `&OpCtx`.
///
/// `RemoteCallbacks` stores each callback as its own `Box<dyn FnMut + 'a>`, so two
/// of them can never both hold `&mut` to one context. Interior mutability is what
/// makes the whole set shareable behind a single `&`. The fields are private to
/// this module; `OpScratch::default()` is the only value any caller needs.
#[derive(Default)]
pub struct OpScratch {
    cred_attempts: Cell<u32>,
    cred_failure: Cell<Option<GitErrorCode>>,
    push_rejections: RefCell<Vec<(String, String)>>,
}

/// Everything one operation needs, moved whole into a `spawn_blocking` closure.
///
/// It deliberately holds no handle back into the host: no `App`, no `Children`, no
/// runtime, no channel. The job slot and the returned `OpOutcome` are the only ways
/// anything here reaches the event loop.
pub struct OpCtx {
    pub tree: PathBuf,
    pub def: RepoDef,
    pub cred: ResolvedCred,
    /// A stored credential existed but was bound to another host, so nothing was
    /// resolved. An auth demand becomes `auth_unbound`, not `auth_missing`.
    pub cred_unbound: bool,
    pub request: OpRequest,
    pub identity: Identity,
    pub host_key: HostKeyChecker,
    pub abort: Arc<AbortFlag>,
    pub slot: Arc<JobSlot>,
    pub deadline: Instant,
    pub settings: Option<SettingsCtx>,
    /// `[git].author_name` / `[git].author_email`, already resolved.
    pub default_author: AuthorSpec,
    pub scratch: OpScratch,
}

impl OpCtx {
    pub fn phase(&self, p: Phase) {
        self.slot.set_phase(p);
    }

    /// True once this operation must stop, for any reason.
    pub fn aborted(&self) -> bool {
        if self.abort.is_aborted() {
            return true;
        }
        // The watchdog trips the same flag from the runtime, but the blocking thread
        // cannot depend on it: a transfer that stalls without ever waking tokio
        // would run past its deadline unnoticed. Recording the reason here is what
        // lets `classify` report `timeout` rather than `network_failed`.
        if Instant::now() >= self.deadline {
            self.abort.abort(AbortReason::Timeout);
            return true;
        }
        false
    }

    /// libgit2's fetch-side transfer callback. Returning `false` cancels.
    pub fn transfer(&self, p: &git2::Progress<'_>) -> bool {
        if self.aborted() {
            return false;
        }
        self.slot.set_progress(Progress {
            received_objects: p.received_objects() as u32,
            total_objects: p.total_objects() as u32,
            indexed_objects: p.indexed_objects() as u32,
            received_bytes: p.received_bytes() as u64,
            // Spec §6.4 specifies `indexed_objects * 100 / total_objects`, not received.
            // Indexing lags receipt, so the two diverge for the whole tail of a fetch and
            // a received-based percent would sit at 100% while indexing is still running.
            percent: percent_of(p.indexed_objects(), p.total_objects()),
        });
        true
    }

    /// libgit2's **push**-side progress callback.
    ///
    /// `transfer_progress` fires only on fetch, so without this a push reports no
    /// progress at all and a large first push looks hung. Two differences from
    /// `transfer`, both forced by libgit2's signature: this returns `()`, so it
    /// **cannot cancel** — `sideband_progress` is the only abort hook that runs
    /// during a push — and there is no separate indexed count, because indexing
    /// happens on the server. `current` is therefore both the received and the
    /// indexed figure, and the percentage is honest either way.
    pub fn push_transfer(&self, current: usize, total: usize, bytes: usize) {
        self.slot.set_progress(Progress {
            received_objects: current as u32,
            total_objects: total as u32,
            indexed_objects: current as u32,
            received_bytes: bytes as u64,
            percent: percent_of(current, total),
        });
    }

    /// Request author, then the repo's, then `[git].author_name`, then the app.
    pub fn author_name(&self) -> String {
        pick(self.request.author.as_ref().and_then(|a| a.name.as_ref()))
            .or_else(|| pick(self.def.author.as_ref().and_then(|a| a.name.as_ref())))
            .or_else(|| pick(self.default_author.name.as_ref()))
            .unwrap_or_else(|| self.identity.app_name.clone())
    }

    /// Same order as `author_name`, resolved independently of it, ending at
    /// `<identifier>@<hostname>` — a syntactically valid address that is obviously
    /// the machine's rather than a person's.
    pub fn author_email(&self) -> String {
        pick(self.request.author.as_ref().and_then(|a| a.email.as_ref()))
            .or_else(|| pick(self.def.author.as_ref().and_then(|a| a.email.as_ref())))
            .or_else(|| pick(self.default_author.email.as_ref()))
            .unwrap_or_else(|| format!("{}@{}", self.identity.identifier, self.identity.hostname))
    }

    /// Count one authentication attempt and return the new total.
    pub fn cred_attempt(&self) -> u32 {
        let n = self.scratch.cred_attempts.get() + 1;
        self.scratch.cred_attempts.set(n);
        n
    }

    /// Remember why authentication failed.
    ///
    /// A plain overwrite is right: every path that records also returns an error to
    /// libgit2 in the same breath, which ends the operation, so at most one code is
    /// recorded per job.
    pub fn record_cred_failure(&self, code: GitErrorCode) {
        self.scratch.cred_failure.set(Some(code));
    }

    /// The message libgit2 carries back out as `git2::Error::message()`, which then
    /// becomes the job's `error.message`. It is the only place the user is told what
    /// to do about a credential, so it names the fix rather than the symptom.
    pub fn auth_missing_message(&self, allowed: git2::CredentialType) -> String {
        if self.cred_unbound {
            let bound = self
                .def
                .credential
                .as_ref()
                .and_then(CredentialSpec::bound_host)
                .unwrap_or("another host");
            let remote = self.def.remote.as_deref().unwrap_or("this remote");
            return format!(
                "the stored credential is bound to {bound} and was not sent to {remote}; \
                 PUT the repo again to rebind it, or send a credential with the request"
            );
        }
        format!(
            "no credential is configured for {}; the remote asked for {}",
            self.def.id,
            describe_allowed(allowed)
        )
    }

    /// Turn a libgit2 error into ours, preferring what our own callbacks recorded.
    pub fn classify_err(&self, e: &git2::Error) -> GitError {
        // From libgit2's side every refusal our credentials callback makes is just
        // "a callback returned an error" — the recorded code is the only thing that
        // can tell `auth_missing` from `auth_unbound` from `auth_failed`.
        let code = self
            .scratch
            .cred_failure
            .get()
            .unwrap_or_else(|| error::classify(e, self.abort.reason()));
        self.scrub_err(
            GitError::new(code, e.message())
                .with_repo(&self.def.id)
                .with_git2(e),
        )
    }

    /// libgit2 quotes remote URLs into its error strings, and those strings land in
    /// job records, `git.log`, and dialogs. Nothing leaves this file unscrubbed.
    pub fn scrub_err(&self, e: GitError) -> GitError {
        e.scrubbed(&self.cred.secrets())
    }

    pub fn learned_fingerprint(&self) -> Option<String> {
        self.host_key.learned()
    }

    /// libgit2 calls this once per ref it pushed. `Some(status)` means the server
    /// refused that ref; a ref that went through reports `None`.
    pub fn record_push_status(&self, refname: &str, status: Option<&str>) {
        if let Some(status) = status {
            self.scratch
                .push_rejections
                .borrow_mut()
                .push((refname.to_string(), status.to_string()));
        }
    }

    /// Turn whatever the server refused into an error, and forget it.
    ///
    /// `Remote::push` returns `Ok(())` even when every ref was rejected — the
    /// rejection arrives only through `push_update_reference` — so a push that does
    /// not call this reports a success it never had. Draining rather than peeking
    /// keeps a retry inside the same job from rediscovering an old rejection.
    pub fn take_push_rejections(&self) -> Result<(), GitError> {
        let rejections = std::mem::take(&mut *self.scratch.push_rejections.borrow_mut());
        match rejections.into_iter().next() {
            None => Ok(()),
            Some((refname, status)) => {
                Err(self
                    .scrub_err(GitError::push_rejected(&refname, &status).with_repo(&self.def.id)))
            }
        }
    }
}

/// `total_objects` is 0 until the remote finishes counting, and a server may send
/// more objects than it announced — neither may produce a panic or a percentage
/// above 100 in a progress bar.
fn percent_of(received: usize, total: usize) -> u8 {
    if total == 0 {
        return 0;
    }
    (received.saturating_mul(100) / total).min(100) as u8
}

/// An empty or whitespace-only author field is the config default (`author_name =
/// ""`), not a choice — treat it as absent rather than committing as "".
fn pick(value: Option<&String>) -> Option<String> {
    value
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Render libgit2's credential-type bitmask for the message a human reads.
fn describe_allowed(allowed: git2::CredentialType) -> String {
    let mut parts = Vec::new();
    if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
        parts.push("a username and password or token");
    }
    if allowed.contains(git2::CredentialType::SSH_KEY)
        || allowed.contains(git2::CredentialType::SSH_MEMORY)
    {
        parts.push("an ssh key");
    }
    if parts.is_empty() {
        // NTLM/Negotiate and keyboard-interactive land here. Naming the gap beats a
        // message that reads as though a credential were merely configured wrongly.
        return "an authentication method this host does not implement".to_string();
    }
    parts.join(" or ")
}

// ---------------------------------------------------------------------------
// Verbs
//
// `run` is the single entry point `GitService` calls from its blocking thread.
// The eight verbs below are stubs in this slice; each is replaced in place, so the
// dispatch table never has to change again.
// ---------------------------------------------------------------------------

pub fn run(op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    match op {
        JobOp::Init => init(ctx),
        JobOp::Clone => clone(ctx),
        JobOp::Pull => pull(ctx),
        JobOp::Push => push(ctx),
        JobOp::Sync => sync(ctx),
        JobOp::Commit => commit(ctx),
        JobOp::Branch => branch(ctx),
        JobOp::Reset => reset(ctx),
    }
}

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
        Err(e) if e.code() == git2::ErrorCode::Exists => git2::Repository::open(&ctx.tree)?,
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
                let mut out = OpOutcome::new("up_to_date", &ctx.def.branch);
                out.head_after = head_oid(&repo);
                Ok(out)
            }
            Err(_) => Err(GitError::not_a_repository(&ctx.tree)),
        };
    }

    let url = ctx
        .def
        .remote
        .as_deref()
        .ok_or_else(GitError::remote_missing)?;
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
        // The merge's checkout refuses rather than overwrites, so this check is no
        // longer what makes it safe — it is what makes it legible, in both
        // directions. A tracked file the merge DOES change, whose working copy is
        // locally modified, is a hard CONFLICT under a safe checkout, so without
        // this it would surface as a refusal halfway through a job instead of a
        // 409 up front that names the two ways out; and one the merge does NOT
        // change is silently SKIPPED rather than refused, so the pull would report
        // success and quietly leave that edit unmerged. Untracked files are still
        // deliberately not counted: nothing in a pull stages them, and refusing on
        // a stray scratch file would make auto-sync useless — the merge checkout
        // refuses only the ones the remote actually collides with.
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

pub fn sync(ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    // A sync of a repo that was never materialised is a clone: the caller asked
    // for "make this tree match the remote", and there is no earlier step they
    // should have had to run first.
    //
    // The test is `exists()` and not "is not a repository", deliberately: `clone`
    // refuses a populated directory that is not one of ours with `not_a_repository`
    // rather than writing over it, and both arms below have to treat "the directory
    // is there but isn't ours" the same way — as somebody else's directory.
    if !ctx.tree.exists() {
        if ctx.def.remote.is_some() {
            return clone(ctx);
        }
        // The same rule with nothing to clone from: README §9.4's `remote: null` is a
        // local-only repo and `sync` is the only verb its child ever calls, so `init`
        // is not an earlier step it should have had to run either. `init` and not a
        // bare `create_dir_all`, because only `init` pins `refs/heads/<def.branch>`
        // against the developer's global `init.defaultBranch`. It falls through rather
        // than returning: an init leaves an unborn HEAD, and the commit below is the
        // half of "sync" a local-only repo actually has. `open_tree`'s state gate is
        // fine with what this leaves — `RepositoryState` is read from marker files a
        // fresh init has none of, never from HEAD or the index.
        init(ctx)?;
    }

    let repo = open_tree(ctx)?;
    require_branch(&repo, &ctx.def.branch)?;

    let mut out = OpOutcome::new("no_changes", &ctx.def.branch);
    out.head_before = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|o| o.to_string());

    // Before staging, so this host's settings ride out on the same commit as
    // everything else in the tree. `sync` is the only verb that does this: a
    // `pull` refuses a dirty tree, and copying in is what would make it dirty.
    let settings_before = settings_copy_in(ctx)?;

    // Committing everything BEFORE the fetch is load-bearing, not convenient.
    // Afterwards every tracked file is at a committed state, which is what keeps
    // the merge's safe checkout from *refusing* a sync over the user's own
    // uncommitted edit, and it collapses "HEAD is unborn" to "the tree really was
    // empty of anything git tracks".
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

    // After the merge and before the push, so settings that arrived with the
    // merge are validated before this host acts on them, and so a rejected file
    // is healed in the tree the next sync will commit.
    settings_apply_back(ctx, settings_before, &mut out)?;

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

pub fn commit(ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    let repo = open_tree(ctx)?;
    require_branch(&repo, &ctx.def.branch)?;
    let head_before = head_oid(&repo);
    let (oid, files) = stage_and_commit(&repo, ctx)?;

    let mut out = OpOutcome::new(
        if oid.is_some() {
            "committed"
        } else {
            "no_changes"
        },
        &ctx.def.branch,
    );
    out.head_before = head_before;
    out.head_after = head_oid(&repo);
    out.committed = oid.is_some();
    out.commit = oid.map(|o| o.to_string());
    out.files_committed = if oid.is_some() { files } else { 0 };
    Ok(out)
}

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
        if created {
            "branch_created"
        } else {
            "checked_out"
        },
        &request.name,
    );
    out.head_before = head_before;
    out.head_after = head_oid(&repo);
    out.upstream_set = upstream_set;
    Ok(out)
}

pub fn reset(ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    let request = ctx.request.reset.as_ref().ok_or_else(|| {
        GitError::invalid_request("reset", "a reset operation needs a reset body")
    })?;
    if !request.confirm {
        return Err(GitError::confirm_required());
    }
    // The one verb that may open a wedged tree, because it is the one that repairs
    // it: `git_reset(HARD)` ends in `git_repository_state_cleanup`, so this is the
    // way out that the refusal every other verb now returns tells the operator to
    // take. Mechanically rewriting this back to `open_tree` seals it.
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

/// The most files any single `DirtySummary` bucket will list.
///
/// A generated-file accident produces tens of thousands of dirty paths; the status
/// endpoint is polled and must not turn into a multi-megabyte response.
pub const MAX_DIRTY_FILES: usize = 200;

/// True when HEAD names a branch that has no commit yet.
///
/// **Not `Repository::is_empty()`.** libgit2 defines that as "HEAD is a symbolic ref
/// whose target equals `init.defaultBranch` **and** the repo contains no reference"
/// (`git_repository_is_empty`, repository.c). Every repo this service creates pins HEAD
/// to `[git].default_branch` instead, so on any machine whose git default differs —
/// which is the machine-dependent behaviour `initial_head` exists to defeat —
/// `is_empty()` answers `false` for a repo that has no commits at all. Using it to mean
/// "unborn" made the first commit into a fresh repo fail with `unborn_branch`, and made
/// cloning an empty remote fail the same way.
fn head_is_unborn(repo: &git2::Repository) -> bool {
    matches!(repo.head(), Err(e) if e.code() == git2::ErrorCode::UnbornBranch)
}

/// HEAD as a hex oid, or `None` when HEAD is unborn or detached from any object.
fn head_oid(repo: &git2::Repository) -> Option<String> {
    repo.head()
        .ok()
        .and_then(|h| h.target())
        .map(|o| o.to_string())
}

/// Open the working tree and reconcile its `origin` with the current definition.
///
/// Refuses a repository left mid-merge, mid-rebase or mid-cherry-pick: see
/// `require_clean_state`. Every verb that touches an existing tree comes through
/// here, so the gate is one line and a verb added later gets it without being
/// told. `reset` is the single exception and says so at its own call site.
pub fn open_tree(ctx: &OpCtx) -> Result<git2::Repository, GitError> {
    let repo = open_tree_any_state(ctx)?;
    require_clean_state(&repo, &ctx.def.id)?;
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
pub fn open_tree_any_state(ctx: &OpCtx) -> Result<git2::Repository, GitError> {
    if !ctx.tree.exists() {
        return Err(GitError::no_worktree(&ctx.def.id));
    }
    let repo = git2::Repository::open(&ctx.tree)?;
    // Re-point the remote if the definition changed since the last operation: a PUT can
    // move a repo to a new host while the tree on disk still points at the old one, and
    // every later fetch/push would silently keep talking to the wrong server.
    if let Some(url) = ctx.def.remote.as_deref() {
        match repo.find_remote(&ctx.def.remote_name) {
            // git2 0.21 returns `Result<&str, _>` from `Remote::url`, not `Option<&str>`
            // (a non-UTF-8 url is an Err rather than a None), so the comparison has to
            // go through `.ok()`.
            Ok(remote) if remote.url().ok() != Some(url) => {
                repo.remote_set_url(&ctx.def.remote_name, url)?;
            }
            Ok(_) => {}
            Err(_) => {
                repo.remote(&ctx.def.remote_name, url)?;
            }
        }
    }
    Ok(repo)
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
    if head_is_unborn(repo) {
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
    if head_is_unborn(repo) {
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
    // mid-merge, mid-rebase or mid-cherry-pick is REPORTED, never repaired. Returning Err
    // hands `status` the "error" state and attaches the reason; everything above is already
    // filled in, so head, branch, dirty and ahead/behind still reach the caller. The same
    // call gates every mutating verb from `open_tree`, which is what makes this message's
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
    //
    // `head_is_unborn`, not `Repository::is_empty()`: libgit2 defines empty as
    // "HEAD points at `init.defaultBranch` and there are no refs", which is
    // false for every repo this service pins to `[git].default_branch` on a
    // machine whose git default differs.
    if head_is_unborn(repo) {
        return Err(GitError::unborn_branch("push").with_repo(&ctx.def.id));
    }

    ctx.phase(Phase::Pushing);
    let branch = ctx.def.branch.as_str();
    let mut remote = repo
        .find_remote(&ctx.def.remote_name)
        .map_err(|e| ctx.classify_err(&e))?;
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

    let mut po = git2::PushOptions::new();
    po.remote_callbacks(creds::callbacks(ctx));
    remote.push(&[spec.as_str()], Some(&mut po)).map_err(|e| {
        let mut err = ctx.classify_err(&e);
        if err.code() == GitErrorCode::NotFastForward {
            // libgit2's own text describes the situation but not the fix,
            // and this error is the one a user hits most often.
            err.message
                .push_str("; pull first, or retry with force=true");
        }
        err
    })?;

    // A server-side refusal — a protected branch, a pre-receive hook — is
    // invisible to libgit2 locally: `push` returns Ok(()) and the refusal
    // arrives only through push_update_reference, which our callbacks record.
    // Without this drain the job reports a success that pushed nothing.
    ctx.take_push_rejections()?;

    out.pushed = true;
    out.force_pushed = ctx.request.force;

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
fn apply_merge(repo: &git2::Repository, ctx: &OpCtx, out: &mut OpOutcome) -> Result<(), GitError> {
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

/// The work-tree copy of `settings.json` for a repo that mirrors settings.
///
/// `settings_path` is registry-validated as relative, `/`-separated and free of
/// `..`, and `Path::join` treats `/` as a separator on Windows too, so no
/// per-platform splitting is needed here.
fn settings_tree_path(ctx: &OpCtx) -> PathBuf {
    ctx.tree.join(&ctx.def.settings_path)
}

/// A change detector, not a checksum.
///
/// Both hashes are produced by the same process inside one job and neither is
/// ever persisted or transmitted, so a fast non-cryptographic 64-bit hash is
/// exactly the right tool: the question is only "did these bytes move while we
/// were fetching and merging".
fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{DefaultHasher, Hasher};
    let mut hasher = DefaultHasher::new();
    hasher.write(bytes);
    hasher.finish()
}

/// The single delegation point to the one writer of any `settings.json`.
///
/// Routing the tree copy through `settings::save` too — rather than copying the
/// host file byte-for-byte — is what normalizes key order and whitespace, so two
/// hosts that agree on the values produce identical bytes and never fight over
/// formatting.
fn write_settings(
    path: &std::path::Path,
    values: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), GitError> {
    settings::save(path, values).map_err(|e| GitError::io(format!("write {}: {e}", path.display())))
}

/// Materialize the host's `settings.json` and copy it into the work tree.
///
/// Returns the hash of the bytes now in the tree, or `None` when this repo does
/// not mirror settings. Prefer-local applies to settings exactly as it does to
/// every other file: this machine's `settings.json` is what gets published, and
/// the merge decides the rest.
pub fn settings_copy_in(ctx: &OpCtx) -> Result<Option<u64>, GitError> {
    if !ctx.def.sync_settings {
        return Ok(None);
    }
    // `sync_settings` with no schema is a registry-validation error, so this is
    // reachable only if `[settings]` was removed from app.toml under a live
    // repos.json. Failing loudly beats a data-sync feature that quietly stops.
    let Some(sc) = ctx.settings.as_ref() else {
        return Err(GitError::settings_sync_unavailable().with_repo(&ctx.def.id));
    };

    // `load` is total: an absent file, junk, an unknown key or a wrong-typed
    // value all collapse to the schema's defaults, which is precisely what
    // "materialize it from the defaults first" means.
    let values = settings::load(&sc.schema, &sc.settings_file);
    if !sc.settings_file.exists() {
        write_settings(&sc.settings_file, &values)?;
    }

    let target = settings_tree_path(ctx);
    write_settings(&target, &values)?;
    let bytes = std::fs::read(&target)
        .map_err(|e| GitError::io(format!("read {}: {e}", target.display())))?;
    Ok(Some(hash_bytes(&bytes)))
}

/// Validate the work tree's `settings.json` after the merge and adopt it.
///
/// Never fails the job over bad *content*: a teammate's typo must not wedge an
/// entire repo's sync forever. A rejected file is reported as a warning on an
/// otherwise successful job and the host's own valid copy is written back into
/// the tree, so the next sync commits it and the next push heals the remote.
pub fn settings_apply_back(
    ctx: &OpCtx,
    before_hash: Option<u64>,
    out: &mut OpOutcome,
) -> Result<(), GitError> {
    // `None` means `settings_copy_in` did nothing: this repo does not mirror
    // settings and there is no copy of ours to compare against.
    let (Some(before), Some(sc)) = (before_hash, ctx.settings.as_ref()) else {
        return Ok(());
    };

    let target = settings_tree_path(ctx);
    let Ok(after) = std::fs::read(&target) else {
        // The copy above was committed before the fetch and the merge prefers
        // local, so the file cannot normally vanish. If it somehow did, heal it
        // rather than fail: the tree copy is the disposable one.
        return heal_tree_copy(ctx, sc);
    };
    if hash_bytes(&after) == before {
        return Ok(());
    }

    let validated = serde_json::from_slice::<serde_json::Value>(&after)
        .map_err(|e| e.to_string())
        .and_then(|incoming| settings::validate_incoming(&sc.schema, &incoming));

    let rejected = match validated {
        Ok(values) => {
            // Read before the write, because this comparison is the only thing
            // standing between an auto-sync and a Chrome restart every five
            // minutes: the file changing is not enough, the values must differ.
            let current = settings::load(&sc.schema, &sc.settings_file);
            // §14 risk 11, accepted rather than fixed: the settings page can
            // POST /api/settings while this write is in flight, and whichever
            // `settings::save` lands second wins the whole file. It is a
            // last-writer-wins race over one small document, not a corruption
            // risk — `save` writes the complete map in one call, so no reader
            // can ever observe a half-written file. Locking it would mean a
            // cross-process lock between the host and its own HTTP handler for
            // a collision that needs a settings edit inside the same second as
            // a sync; the worst case is one lost update, which the next save or
            // sync republishes. Do NOT "fix" this by skipping the write when
            // the file changed underneath: that would silently drop the
            // remote's values instead, which is the strictly worse loss.
            write_settings(&sc.settings_file, &values)?;
            out.settings_synced = true;
            out.settings_changed = values != current;
            return Ok(());
        }
        Err(e) => e,
    };

    let message = format!(
        "{} was rejected and the local settings were left unchanged: {rejected}",
        ctx.def.settings_path
    );
    out.settings_rejected = Some(SettingsRejected { error: rejected });
    out.warnings.push(Warning {
        code: "settings_rejected",
        message,
    });
    heal_tree_copy(ctx, sc)
}

/// Write the host's own valid settings back over the tree copy.
///
/// The next sync stages and commits it, and that push is what heals the remote
/// for everyone else.
fn heal_tree_copy(ctx: &OpCtx, sc: &SettingsCtx) -> Result<(), GitError> {
    let values = settings::load(&sc.schema, &sc.settings_file);
    write_settings(&settings_tree_path(ctx), &values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SshHostKeyPolicy;
    use crate::git::jobs::{Admission, JobStore, RepoLease};
    use crate::git::merge::testkit;
    use std::time::Duration;

    fn def(json: &str) -> RepoDef {
        serde_json::from_str(json).expect("test fixture must parse")
    }

    /// Task 6's context-only fixture. It is structurally `Op` (a context plus the lease
    /// that keeps its job alive), so it reuses that type rather than declaring a second
    /// one — task 7's `Fixture` is the tempdir/origin fixture and owns the name.
    fn fixture(def_json: &str, cred: ResolvedCred, cred_unbound: bool) -> Op {
        let store = JobStore::new("0000dead".to_string());
        let admitted = store
            .admit("notes", JobOp::Sync, None)
            .expect("a fresh store is not draining");
        let Admission::Started(slot, lease) = admitted else {
            panic!("a fresh store must admit the first job");
        };
        let abort = slot.abort.clone();
        Op {
            ctx: OpCtx {
                tree: PathBuf::from("/nonexistent/repos/notes"),
                def: def(def_json),
                cred,
                cred_unbound,
                request: OpRequest::default(),
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

    fn plain() -> Op {
        fixture(r#"{"id":"notes"}"#, ResolvedCred::None, false)
    }

    #[test]
    fn an_operation_request_pushes_by_default() {
        // `sync` means "make the remote match". A derived Default would give
        // `push: false` and turn every sync into a silent local-only commit.
        assert!(OpRequest::default().push);
        assert!(OpRequest::default().restart_children.is_none());
        assert!(!OpRequest::default().force);
        assert!(!OpRequest::default().allow_empty);
        assert!(!OpRequest::default().commit_local);
    }

    #[test]
    fn timer_and_tray_syncs_never_ask_for_a_child_restart() {
        // A background pull must not yank the UI out from under someone mid-edit.
        // A settings change still restarts children, but that is `after_job`'s
        // decision from `settings_changed`, not this flag.
        for req in [OpRequest::auto(), OpRequest::manual()] {
            assert_eq!(req.restart_children, Some(false));
            assert!(req.request_id.is_none());
            assert!(req.push, "an unattended sync still pushes");
        }
    }

    #[test]
    fn an_identity_never_leaves_the_hostname_empty_or_padded() {
        let id = Identity::new("Chrome Host App", "com.example.chromehost");
        assert_eq!(id.app_name, "Chrome Host App");
        assert_eq!(id.identifier, "com.example.chromehost");
        // The hostname is the right-hand side of the fallback author email, so an
        // empty one produces `com.example.chromehost@` and git refuses the commit.
        assert!(!id.hostname.is_empty());
        assert_eq!(id.hostname.trim(), id.hostname);
    }

    #[test]
    fn a_branch_request_distinguishes_absent_upstream_from_null_upstream() {
        // Three states, because "no upstream key in the body" (leave it alone) and
        // `"upstream": null` (unset it) are different instructions.
        let leave = BranchRequest {
            name: "main".to_string(),
            create: false,
            from: None,
            checkout: true,
            upstream: None,
        };
        let unset = BranchRequest {
            upstream: Some(None),
            ..leave.clone()
        };
        let set = BranchRequest {
            upstream: Some(Some("origin/main".to_string())),
            ..leave.clone()
        };
        assert!(leave.upstream.is_none());
        assert_eq!(unset.upstream, Some(None));
        assert_eq!(set.upstream, Some(Some("origin/main".to_string())));
    }

    #[test]
    fn an_outcome_serializes_a_fixed_shape_with_no_secrets_in_it() {
        let value = serde_json::to_value(OpOutcome::new("committed", "main"))
            .expect("OpOutcome must serialize");
        let obj = value.as_object().expect("an object");
        assert_eq!(obj["outcome"], "committed");
        assert_eq!(obj["branch"], "main");
        // Nulls are present, not omitted: a client reading `result.head_after`
        // must not have to distinguish "absent" from "null".
        assert_eq!(obj["head_after"], serde_json::Value::Null);
        assert_eq!(obj["merge"], serde_json::Value::Null);
        assert_eq!(obj["files_committed"], 0);
        assert_eq!(obj["warnings"], serde_json::json!([]));
        assert_eq!(
            obj.len(),
            19,
            "the result object has a fixed shape; adding a field is an API change: {obj:?}"
        );
        // Host-internal bookkeeping, not part of the wire contract. A learned host
        // key fingerprint in particular must not be published to every caller.
        assert!(!obj.contains_key("settings_changed"));
        assert!(!obj.contains_key("learned_fingerprint"));
    }

    #[test]
    fn a_merge_report_carries_the_hint_that_recovers_the_losing_side() {
        let report = MergeReport {
            kind: "merge_commit",
            merge_commit: Some("abc123".to_string()),
            conflicts_resolved: vec!["notes/f.md".to_string()],
            recover_hint: Some("git show abc123^2:notes/f.md".to_string()),
        };
        let value = serde_json::to_value(&report).expect("MergeReport must serialize");
        assert_eq!(value["kind"], "merge_commit");
        assert_eq!(
            value["conflicts_resolved"],
            serde_json::json!(["notes/f.md"])
        );
        assert_eq!(value["recover_hint"], "git show abc123^2:notes/f.md");

        let rejected = SettingsRejected {
            error: "port must be between 1 and 65535".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&rejected).expect("SettingsRejected must serialize")["error"],
            "port must be between 1 and 65535"
        );
    }

    #[test]
    fn a_phase_change_reaches_the_job_slot() {
        let fx = plain();
        fx.ctx.phase(Phase::Cloning);
        assert_eq!(fx.ctx.slot.phase(), Phase::Cloning);
        fx.ctx.phase(Phase::Merging);
        assert_eq!(fx.ctx.slot.phase(), Phase::Merging);
    }

    #[test]
    fn a_context_is_aborted_by_the_flag_or_by_its_own_deadline() {
        let fx = plain();
        assert!(!fx.ctx.aborted());

        let fx = plain();
        fx.ctx.abort.abort(AbortReason::Shutdown);
        assert!(fx.ctx.aborted());
        assert_eq!(fx.ctx.abort.reason(), AbortReason::Shutdown);

        // The watchdog trips the same flag from the runtime, but a transfer that
        // stalls without ever waking tokio must still stop itself — and it must
        // record `Timeout`, or `classify` reports `network_failed` for a deadline.
        let mut fx = plain();
        fx.ctx.deadline = Instant::now() - Duration::from_secs(1);
        assert!(fx.ctx.aborted());
        assert_eq!(fx.ctx.abort.reason(), AbortReason::Timeout);
    }

    #[test]
    fn transfer_percent_never_divides_by_zero_or_exceeds_a_hundred() {
        // `total_objects` is 0 until the remote finishes counting, and a server is
        // free to send more objects than it announced.
        assert_eq!(percent_of(0, 0), 0);
        assert_eq!(percent_of(17, 0), 0);
        assert_eq!(percent_of(0, 200), 0);
        assert_eq!(percent_of(50, 200), 25);
        assert_eq!(percent_of(200, 200), 100);
        assert_eq!(percent_of(500, 200), 100);
        assert_eq!(percent_of(usize::MAX, 3), 100);
    }

    /// A push reports progress through its own libgit2 callback; `transfer_progress`
    /// fires only on fetch. Without `push_transfer` wired up a large first push
    /// looks hung for its whole duration.
    #[test]
    fn a_push_reports_progress_through_its_own_callback() {
        let fx = plain();
        assert!(fx.ctx.slot.view("0000dead").progress.is_none());

        fx.ctx.push_transfer(3, 12, 4_096);
        let p = fx.ctx.slot.view("0000dead").progress.expect("progress");
        assert_eq!(p.total_objects, 12);
        assert_eq!(p.received_objects, 3);
        assert_eq!(p.received_bytes, 4_096);
        assert_eq!(p.percent, 25);
        // The server does the indexing on a push, so there is no separate count to
        // report and `current` stands in for both.
        assert_eq!(p.indexed_objects, 3);

        // Same divide-by-zero guard as the fetch side: libgit2 reports 0/0 before
        // the pack is built.
        fx.ctx.push_transfer(0, 0, 0);
        assert_eq!(
            fx.ctx
                .slot
                .view("0000dead")
                .progress
                .expect("progress")
                .percent,
            0
        );
    }

    #[test]
    fn the_commit_author_falls_back_through_request_repo_config_then_host() {
        let mut fx = plain();
        // Nothing configured anywhere: the host names itself, and the address is
        // obviously a machine's rather than a person's.
        assert_eq!(fx.ctx.author_name(), "Test App");
        assert_eq!(fx.ctx.author_email(), "com.example.test@testhost");

        // `[git].author_name = ""` is the config default, not a choice, so an
        // empty (or whitespace-only) string must not win over the fallback.
        fx.ctx.default_author = AuthorSpec {
            name: Some("   ".to_string()),
            email: Some(String::new()),
        };
        assert_eq!(fx.ctx.author_name(), "Test App");
        assert_eq!(fx.ctx.author_email(), "com.example.test@testhost");

        fx.ctx.default_author = AuthorSpec {
            name: Some("Config Bot".to_string()),
            email: Some("bot@config.example".to_string()),
        };
        assert_eq!(fx.ctx.author_name(), "Config Bot");
        assert_eq!(fx.ctx.author_email(), "bot@config.example");

        fx.ctx.def =
            def(r#"{"id":"notes","author":{"name":"Repo Bot","email":"bot@repo.example"}}"#);
        assert_eq!(fx.ctx.author_name(), "Repo Bot");
        assert_eq!(fx.ctx.author_email(), "bot@repo.example");

        fx.ctx.request.author = Some(AuthorSpec {
            name: Some("Caller".to_string()),
            email: None,
        });
        // Name and email fall back independently: a caller may override one.
        assert_eq!(fx.ctx.author_name(), "Caller");
        assert_eq!(fx.ctx.author_email(), "bot@repo.example");
    }

    #[test]
    fn the_attempt_counter_returns_the_count_it_just_recorded() {
        let fx = plain();
        assert_eq!(fx.ctx.cred_attempt(), 1);
        assert_eq!(fx.ctx.cred_attempt(), 2);
        assert_eq!(fx.ctx.cred_attempt(), 3);
    }

    #[test]
    fn a_recorded_credential_failure_beats_whatever_libgit2_reported() {
        // From libgit2's side every one of our refusals is just "a callback
        // returned an error"; the code we recorded is the only specific thing
        // anybody has.
        let fx = plain();
        let raw = git2::Error::from_str("callback returned an error");
        fx.ctx.record_cred_failure(GitErrorCode::AuthUnbound);
        let err = fx.ctx.classify_err(&raw);
        assert_eq!(err.code(), GitErrorCode::AuthUnbound);
        assert_eq!(err.repo_id.as_deref(), Some("notes"));
        assert_eq!(err.message, "callback returned an error");
        let hint = err
            .git2
            .expect("classify_err always attaches the git2 hint");
        assert_eq!(hint.code, "GenericError");
    }

    #[test]
    fn a_classified_error_is_scrubbed_of_the_credential_it_was_using() {
        let fx = fixture(
            r#"{"id":"notes"}"#,
            ResolvedCred::Token {
                username: "x-access-token".to_string(),
                token: crate::git::secret::Secret::new("ghp-secret-token-value".to_string()),
            },
            false,
        );
        let raw = git2::Error::from_str(
            "failed to connect to https://x-access-token:ghp-secret-token-value@github.com/a/b",
        );
        let err = fx.ctx.classify_err(&raw);
        assert!(
            !err.message.contains("ghp-secret-token-value"),
            "libgit2 quotes URLs into its messages, and this one reaches git.log: {}",
            err.message
        );
        // The same guarantee for an error we built ourselves.
        let ours = fx
            .ctx
            .scrub_err(GitError::internal("token ghp-secret-token-value rejected"));
        assert!(!ours.message.contains("ghp-secret-token-value"));
    }

    #[test]
    fn the_missing_credential_message_names_the_mechanism_the_remote_wanted() {
        let fx = plain();
        let msg = fx
            .ctx
            .auth_missing_message(git2::CredentialType::USER_PASS_PLAINTEXT);
        assert!(msg.contains("notes"), "{msg}");
        assert!(msg.contains("username and password"), "{msg}");

        assert_eq!(
            describe_allowed(git2::CredentialType::SSH_KEY),
            "an ssh key"
        );
        assert_eq!(
            describe_allowed(
                git2::CredentialType::SSH_KEY | git2::CredentialType::USER_PASS_PLAINTEXT
            ),
            "a username and password or token or an ssh key"
        );
        // libgit2 can ask for NTLM/Negotiate. Saying so is better than a message
        // that reads as though a credential were configured wrongly.
        assert_eq!(
            describe_allowed(git2::CredentialType::DEFAULT),
            "an authentication method this host does not implement"
        );
    }

    #[test]
    fn the_unbound_message_names_the_host_the_credential_belongs_to() {
        let fx = fixture(
            r#"{"id":"notes","remote":"https://evil.example/a/b.git",
                "credential":{"kind":"token","token":"stored-token-value",
                              "bound_host":"github.com"}}"#,
            ResolvedCred::None,
            true,
        );
        let msg = fx
            .ctx
            .auth_missing_message(git2::CredentialType::USER_PASS_PLAINTEXT);
        assert!(msg.contains("github.com"), "{msg}");
        assert!(msg.contains("evil.example"), "{msg}");
        assert!(
            msg.contains("PUT"),
            "the message is where the user learns how to rebind: {msg}"
        );
    }

    #[test]
    fn a_learned_host_key_fingerprint_travels_out_through_the_context() {
        let fx = plain();
        assert!(fx.ctx.learned_fingerprint().is_none());
        // `after_job` is the only writer of git-state.json; the blocking thread
        // only carries the value back.
        assert!(matches!(
            fx.ctx
                .host_key
                .check_for_test_sha256(&[0u8; 32], "github.com"),
            Ok(())
        ));
        assert_eq!(
            fx.ctx.learned_fingerprint().as_deref(),
            Some("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
        );
    }

    #[test]
    fn a_rejected_push_is_only_visible_through_the_update_reference_callback() {
        // `Remote::push` returns Ok(()) even when the server refused every ref —
        // the refusal arrives only here. A push that does not drain this reports a
        // success it never had.
        let fx = plain();
        assert!(fx.ctx.take_push_rejections().is_ok());

        fx.ctx
            .record_push_status("refs/heads/main", Some("non-fast-forward"));
        fx.ctx.record_push_status("refs/heads/other", None);
        let e = fx
            .ctx
            .take_push_rejections()
            .expect_err("a recorded rejection must surface");
        assert_eq!(e.code(), GitErrorCode::PushRejected);
        assert!(e.message.contains("refs/heads/main"), "{}", e.message);
        assert!(e.message.contains("non-fast-forward"), "{}", e.message);
        assert_eq!(e.repo_id.as_deref(), Some("notes"));

        // Draining clears the list, so a retry inside the same job does not
        // rediscover a rejection the previous attempt already reported.
        assert!(fx.ctx.take_push_rejections().is_ok());
    }

    use std::path::Path;
    use std::sync::OnceLock;
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
            // SAFETY: `set_search_path` mutates libgit2 process-global state and is
            // documented as needing external synchronisation. `OnceLock::get_or_init` is
            // that synchronisation: every test enters through `Fixture::*` before it
            // touches git2, and `get_or_init` blocks every other thread until this
            // closure returns.
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
            git2::Repository::clone(&self.remote_url(), self.scratch(name)).expect("worker clone")
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

    /// A tree on `main` with one commit, ready for the commit tests.
    fn seeded_local(fx: &Fixture, id: &str) -> git2::Repository {
        let op = fx.op(fx.local_def(id, "main"), JobOp::Init, OpRequest::default());
        init(&op).expect("init");
        let repo = open(&fx.tree(id));
        commit_file(&repo, "base.md", "base\n", "base");
        repo
    }

    fn read_ctx(fx: &Fixture, def: RepoDef) -> ReadCtx {
        ReadCtx {
            tree: fx.tree(&def.id),
            def,
            last_sync: None,
            busy_job: None,
        }
    }

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
    fn open_tree_refuses_a_missing_worktree() {
        let fx = Fixture::new();
        let op = fx.op(
            fx.local_def("notes", "main"),
            JobOp::Commit,
            OpRequest::default(),
        );
        // `git2::Repository` has no `Debug`, so `expect_err` will not compile here.
        let err = match open_tree(&op) {
            Ok(_) => panic!("nothing has been cloned yet"),
            Err(e) => e,
        };
        assert_eq!(err.code(), GitErrorCode::NoWorktree);
    }

    #[test]
    fn open_tree_refuses_a_repository_left_mid_merge() {
        // The seam itself, at the same altitude as the missing-worktree refusal
        // above: every mutating verb reaches an existing tree through `open_tree`,
        // so gating there is what makes the refusal uniform without an exception
        // list — and `reset` is the one caller that has to opt out by name.
        //
        // Writing .git/MERGE_HEAD is by itself enough to make repo.state() report
        // Merge (libgit2 1.9.6), which is the same plant
        // `status_reports_a_non_clean_repository_state_as_error` uses.
        let fx = Fixture::new();
        let repo = seeded_local(&fx, "notes");
        let oid = head_oid(&repo).expect("the seed committed something");
        drop(repo);
        std::fs::write(
            fx.tree("notes").join(".git").join("MERGE_HEAD"),
            format!("{oid}\n"),
        )
        .expect("plant MERGE_HEAD");

        let op = fx.op(
            fx.local_def("notes", "main"),
            JobOp::Commit,
            OpRequest::default(),
        );
        // `git2::Repository` has no `Debug`, so `expect_err` will not compile here.
        let err = match open_tree(&op) {
            Ok(_) => panic!("a wedged tree must not be handed to a mutating verb"),
            Err(e) => e,
        };
        assert_eq!(err.code(), GitErrorCode::DirtyTree);
        assert!(
            err.message.contains("Merge") && err.message.contains("/reset"),
            "the refusal names the state and the way out: {}",
            err.message
        );

        assert!(
            open_tree_any_state(&op).is_ok(),
            "the escape hatch opens the same tree, which is the whole reason it exists"
        );
    }

    #[test]
    fn open_tree_repoints_a_changed_remote_url() {
        let fx = Fixture::new();
        let stale = fx.scratch("stale.git");
        let init_op = fx.op(
            repo_def("notes", "main", Some(stale.to_string_lossy().into_owned())),
            JobOp::Init,
            OpRequest::default(),
        );
        std::fs::create_dir_all(fx.tree("notes")).expect("mkdir");
        git2::Repository::init(fx.tree("notes")).expect("init");
        open_tree(&init_op).expect("first open registers the stale url");

        // The registry was edited between operations; the next op must not keep pushing
        // at the old address.
        let moved = fx.op(
            fx.remote_def("notes", "main"),
            JobOp::Commit,
            OpRequest::default(),
        );
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
        let op = fx.op(
            fx.remote_def("notes", "main"),
            JobOp::Commit,
            OpRequest::default(),
        );
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

    #[test]
    fn init_pins_the_configured_branch_against_a_master_default() {
        let fx = Fixture::new();
        let op = fx.op(
            fx.local_def("notes", "main"),
            JobOp::Init,
            OpRequest::default(),
        );
        let out = init(&op).expect("init");
        assert_eq!(out.outcome, "initialized");
        assert_eq!(out.branch, "main");

        let repo = open(&fx.tree("notes"));
        assert!(head_is_unborn(&repo), "init writes no commit");
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
        let op = fx.op(
            fx.remote_def("notes", "main"),
            JobOp::Init,
            OpRequest::default(),
        );
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
        drop(tip);
        commit_file(&worker, "n.md", "from main\n", "add n.md");
        push_branch(&worker, "main");

        let op = fx.op(
            fx.remote_def("notes", "main"),
            JobOp::Clone,
            OpRequest::default(),
        );
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

        let op = fx.op(
            fx.remote_def("notes", "main"),
            JobOp::Clone,
            OpRequest::default(),
        );
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
        let op = fx.op(
            fx.remote_def("notes", "main"),
            JobOp::Clone,
            OpRequest::default(),
        );
        let out = clone(&op).expect("clone of an empty remote");
        assert_eq!(out.outcome, "cloned");

        let repo = open(&fx.tree("notes"));
        assert!(head_is_unborn(&repo));
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
        let op = fx.op(
            fx.remote_def("notes", "main"),
            JobOp::Clone,
            OpRequest::default(),
        );
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

        let op = fx.op(
            fx.remote_def("notes", "main"),
            JobOp::Clone,
            OpRequest::default(),
        );
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
        let op = fx.op(
            fx.local_def("notes", "main"),
            JobOp::Clone,
            OpRequest::default(),
        );
        let err = clone(&op).expect_err("nothing to clone from");
        assert_eq!(err.code(), GitErrorCode::RemoteMissing);
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

        let op = fx.op(
            fx.local_def("notes", "main"),
            JobOp::Commit,
            OpRequest::default(),
        );
        let out = commit(&op).expect("commit");
        assert_eq!(out.outcome, "committed");
        assert!(out.committed);
        assert_eq!(
            out.files_committed, 3,
            "note.md, .gitignore, deleted base.md"
        );

        let repo = open(&fx.tree("notes"));
        let tree = repo
            .find_commit(
                git2::Oid::from_str(out.commit.as_deref().expect("commit oid")).expect("oid"),
            )
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

        let op = fx.op(
            fx.local_def("notes", "main"),
            JobOp::Commit,
            OpRequest::default(),
        );
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
        let tree = repo.head().expect("head").peel_to_tree().expect("tree");
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
        assert_eq!(head.author().name().ok(), Some("Ada"));
        assert_eq!(head.message().ok(), Some("by hand"));
        drop(head);

        std::fs::write(fx.tree("notes").join("b.md"), "b\n").expect("write");
        drop(repo);
        let op = fx.op(
            fx.local_def("notes", "main"),
            JobOp::Commit,
            OpRequest::default(),
        );
        commit(&op).expect("commit");
        let repo = open(&fx.tree("notes"));
        let head = repo.head().expect("head").peel_to_commit().expect("commit");
        assert_eq!(head.author().name().ok(), Some("Host App"));
        assert!(
            head.message()
                .expect("message")
                .starts_with("Test App: sync from test-host at "),
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

        let op = fx.op(
            fx.local_def("notes", "main"),
            JobOp::Commit,
            OpRequest::default(),
        );
        let err = commit(&op).expect_err("HEAD is on side, the repo is configured for main");
        assert_eq!(err.code(), GitErrorCode::BranchMismatch);
    }

    /// README §9.4: `remote: null` is a local-only repo whose `sync` "inits and commits
    /// and reports `outcome: "committed"`". `sync` is the only verb such a repo's child
    /// process ever calls, so finding no tree cannot mean sending the caller away to an
    /// `init` they should never have had to run — that is the same argument the
    /// tree-absent clone arm has always made, with nothing to clone from.
    ///
    /// The commit this writes is an empty root commit: the tree cannot be pre-populated
    /// without creating the directory, which would take the fallthrough away. That is the
    /// pre-existing shape of `stage_and_commit` against an unborn HEAD (`changed` is true
    /// whenever there is no HEAD commit to compare against) and it is exactly what makes
    /// the `committed` README promises true on the very first sync.
    #[test]
    fn sync_without_a_remote_initializes_the_tree_and_commits() {
        let fx = Fixture::new(); // the fixture's git default is `master` ...
        let def = fx.local_def("notes", "main"); // ... and this repo wants `main`
        assert!(
            !fx.tree("notes").exists(),
            "the absent tree IS the case; a fixture that pre-creates it proves nothing"
        );

        let out = sync(&fx.op(def, JobOp::Sync, OpRequest::default()))
            .expect("a local-only sync inits and commits");

        assert_eq!(out.outcome, "committed");
        assert!(
            out.head_before.is_none() && out.head_after.is_some(),
            "there was no HEAD to move and there is one now"
        );
        // `OpRequest::default()` carries `push: true`, so `!pushed` is what proves the
        // `remote.is_none()` early return ran rather than merely that no remote was
        // configured: without it the job would die in `push_branch`, not skip it.
        assert!(
            !out.fetched && !out.pushed,
            "nothing to fetch from and nothing to publish to"
        );

        let repo = open(&fx.tree("notes"));
        assert_eq!(
            repo.head()
                .expect("HEAD is born by the first sync")
                .shorthand()
                .expect("shorthand"), // 0.21: Result, not Option
            "main",
            "the fallthrough must go through `init`, which pins refs/heads/<def.branch>; \
             a bare Repository::init would answer `master` on this fixture"
        );
    }

    /// The other half of README §9.4, and a lock rather than a fix: this shape — tree
    /// present, `remote: null` — has always worked, and nothing here goes red without the
    /// fallthrough above. What it pins is the `remote.is_none()` early return that sits
    /// between the commit and the fetch; delete that and both legs fail `remote_missing`
    /// from `fetch`, which is the whole of what "local version history with nothing to
    /// publish it to" means.
    #[test]
    fn sync_without_a_remote_commits_an_existing_tree_and_then_reports_no_changes() {
        let fx = Fixture::new();
        let def = fx.local_def("notes", "main");
        init(&fx.op(def.clone(), JobOp::Init, OpRequest::default())).expect("init");
        std::fs::write(fx.tree("notes").join("f.md"), "one\n").expect("write");

        let out = sync(&fx.op(def.clone(), JobOp::Sync, OpRequest::default())).expect("sync");
        assert_eq!(out.outcome, "committed");
        assert!(out.committed);
        assert_eq!(out.files_committed, 1);

        // The idempotent leg: what stops an `auto_sync_secs` timer writing an empty
        // commit every tick for as long as the app is open.
        let again = sync(&fx.op(def, JobOp::Sync, OpRequest::default())).expect("sync");
        assert_eq!(again.outcome, "no_changes");
        assert!(!again.committed);
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
        // Before the commit, not after: `commit_file` stages with `add_all(["*"])`, so a
        // stray file still present here would be committed and then show up as a tracked
        // deletion — leaving the tree dirty for a reason the test is not about.
        std::fs::remove_file(fx.tree("notes").join("stray.txt")).expect("remove");
        commit_file(&repo, "f.md", "one\n", "one");
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
        assert_eq!(s.remote.as_ref().map(|r| r.name.as_str()), Some("origin"));

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
        // half of that decision: the state has to be *visible*, or the repo just looks
        // clean while every mutating verb refuses it for a reason the caller cannot see.
        // `open_tree`'s gate is the refusal; this is the report. Same predicate, same
        // code, same message — `require_clean_state` produces all three.
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

        let e = s
            .error
            .expect("a non-Clean state is surfaced, never repaired");
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

        let missing =
            branches(&fx.tree("nope"), &fx.local_def("nope", "main")).expect_err("no tree on disk");
        assert_eq!(missing.code(), GitErrorCode::NoWorktree);
    }

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
        assert_eq!(
            out.head_after, before,
            "a new branch points at the same commit"
        );

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

    /// The first commit into a freshly initialised repo, on a machine whose git default
    /// disagrees with `[git].default_branch`. This is the real first-use path: create a
    /// repo, write a file, commit.
    ///
    /// It is the exact case `Repository::is_empty()` gets wrong. libgit2 defines "empty"
    /// as "HEAD is symbolic, its target equals `init.defaultBranch`, and the repo holds
    /// no reference", so a repo pinned to `main` on a `master` machine reports **false**
    /// while having no commits at all — and `require_branch` then falls through to
    /// `repo.head()?` and fails the very first commit with `unborn_branch`.
    ///
    /// The plan's `require_branch_allows_an_unborn_head` cannot catch this: it builds its
    /// repo with a plain `Repository::init`, which honours `init.defaultBranch`, so HEAD
    /// and the machine default agree and `is_empty()` happens to answer true.
    #[test]
    fn the_first_commit_into_a_freshly_initialised_repo_succeeds() {
        let fx = Fixture::new(); // the fixture's git default is `master` ...
        let def = fx.local_def("notes", "main"); // ... and this repo wants `main`
        init(&fx.op(def.clone(), JobOp::Init, OpRequest::default())).expect("init");
        std::fs::write(fx.tree("notes").join("first.md"), "hello\n").expect("write");

        let out = commit(&fx.op(def, JobOp::Commit, OpRequest::default()))
            .expect("the first commit must not require a pre-existing HEAD");
        assert_eq!(out.outcome, "committed");
        assert!(out.head_before.is_none(), "there was no HEAD to move");

        let repo = open(&fx.tree("notes"));
        assert_eq!(
            repo.head().expect("head").shorthand().expect("shorthand"),
            "main"
        );
    }

    /// The same libgit2 quirk on the clone path: origin advertises `develop`, the machine
    /// default is `master`, and the definition asks for `main`. `is_empty()` compares
    /// HEAD against the *machine's* default, answers false, and the naive early return is
    /// skipped — taking `ensure_branch` into `repo.head()?` on an unborn HEAD.
    #[test]
    fn clone_of_an_empty_remote_whose_default_matches_neither_side() {
        let fx = Fixture::with_origin_head("develop");
        let op = fx.op(
            fx.remote_def("notes", "main"),
            JobOp::Clone,
            OpRequest::default(),
        );
        let out = clone(&op).expect("clone of an empty remote with a third default");
        assert_eq!(out.outcome, "cloned");

        let repo = open(&fx.tree("notes"));
        assert!(head_is_unborn(&repo));
        assert_eq!(
            repo.find_reference("HEAD")
                .expect("HEAD")
                .symbolic_target()
                .expect("HEAD is symbolic"),
            Some("refs/heads/main")
        );
    }

    #[test]
    fn run_dispatches_every_verb_this_task_owns() {
        let fx = Fixture::new();
        let def = fx.local_def("notes", "main");

        let out = run(
            JobOp::Init,
            &fx.op(def.clone(), JobOp::Init, OpRequest::default()),
        )
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

        // Task 8 made `sync` real, so this arm no longer answers `internal`.
        // The tree is checked out on `side` while this definition says `main`,
        // and only `sync`'s own `require_branch` can produce that code — which
        // is what proves the arm reaches the real verb rather than a stub.
        let err = run(JobOp::Sync, &fx.op(def, JobOp::Sync, OpRequest::default()))
            .expect_err("HEAD is on side and the definition says main");
        assert_eq!(err.code(), GitErrorCode::BranchMismatch);
    }

    /// Two fields is the smallest schema that can prove both "an omitted key
    /// falls back to its default" and "a bad value is rejected".
    fn settings_schema() -> crate::config::SettingsSection {
        crate::config::AppConfig::from_str(
            "[app]\nname = \"X\"\nidentifier = \"com.example.x\"\n\
             [menu]\nsettings = true\n\
             [[settings.fields]]\n\
             key = \"theme\"\nlabel = \"Theme\"\ntype = \"select\"\n\
             default = \"light\"\noptions = [\"light\", \"dark\"]\n\
             [[settings.fields]]\n\
             key = \"notify\"\nlabel = \"Notifications\"\ntype = \"boolean\"\n\
             default = true\n",
        )
        .expect("the fixture config must parse")
        .settings
        .expect("the fixture config declares a settings section")
    }

    /// A `sync_settings` repo whose work tree is not a git repository at all.
    ///
    /// Neither settings helper ever opens one: they move bytes between the
    /// host's `settings.json` and the tree copy, and driving them directly is
    /// what keeps these assertions about the *files* and nothing else. The
    /// host's settings file lives outside the tree, as it does in production
    /// (`<data-dir>/settings.json`).
    struct SettingsFx {
        _dir: tempfile::TempDir,
        tree: PathBuf,
        host_settings: PathBuf,
        job: testkit::Job,
    }

    fn settings_fx() -> SettingsFx {
        let dir = tempfile::tempdir().expect("tempdir");
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(&tree).expect("create the work tree dir");
        let host_settings = dir.path().join("data/settings.json");
        let mut job = testkit::job_local(&tree, JobOp::Sync);
        job.ctx.def.sync_settings = true;
        job.ctx.settings = Some(SettingsCtx {
            schema: settings_schema(),
            settings_file: host_settings.clone(),
        });
        SettingsFx {
            _dir: dir,
            tree,
            host_settings,
            job,
        }
    }

    #[test]
    fn settings_copy_in_materializes_the_host_file_and_copies_it_in() {
        let fx = settings_fx();
        assert!(
            !fx.host_settings.exists(),
            "the fixture must start with no host settings.json"
        );

        let hash = settings_copy_in(&fx.job.ctx).expect("copy in");

        assert!(hash.is_some(), "a sync_settings repo must report a hash");
        assert!(
            fx.host_settings.exists(),
            "a first sync must materialize the host's settings.json from the schema"
        );
        assert_eq!(
            std::fs::read(fx.tree.join("settings.json")).expect("read the tree copy"),
            std::fs::read(&fx.host_settings).expect("read the host copy"),
            "the tree copy must be byte-identical to the host copy"
        );
        let values = crate::settings::load(&settings_schema(), &fx.host_settings);
        assert_eq!(values["theme"], serde_json::json!("light"));
        assert_eq!(values["notify"], serde_json::json!(true));
    }

    #[test]
    fn settings_copy_in_is_a_no_op_without_sync_settings() {
        let mut fx = settings_fx();
        fx.job.ctx.def.sync_settings = false;

        assert_eq!(settings_copy_in(&fx.job.ctx).expect("copy in"), None);
        assert!(
            !fx.tree.join("settings.json").exists(),
            "a repo that did not opt in must never gain a settings.json"
        );
        assert!(!fx.host_settings.exists());
    }

    #[test]
    fn sync_settings_without_a_schema_is_settings_sync_unavailable() {
        // The registry rejects this combination, so it is reachable only when
        // [settings] was deleted from app.toml under a live repos.json. A
        // silently no-op data-sync feature is how people lose data.
        let mut fx = settings_fx();
        fx.job.ctx.settings = None;

        let e = settings_copy_in(&fx.job.ctx).expect_err("a schemaless mirror must not be silent");
        assert_eq!(e.code(), GitErrorCode::SettingsSyncUnavailable);
    }

    #[test]
    fn unchanged_tree_settings_are_not_reapplied() {
        let fx = settings_fx();
        let before = settings_copy_in(&fx.job.ctx).expect("copy in");
        let host_bytes = std::fs::read(&fx.host_settings).expect("read the host copy");

        let mut out = OpOutcome::new("up_to_date", "main");
        settings_apply_back(&fx.job.ctx, before, &mut out).expect("apply back");

        assert!(!out.settings_synced);
        assert!(
            !out.settings_changed,
            "an untouched file must never restart children"
        );
        assert!(out.settings_rejected.is_none());
        assert!(out.warnings.is_empty());
        assert_eq!(
            std::fs::read(&fx.host_settings).expect("read the host copy"),
            host_bytes
        );
    }

    #[test]
    fn valid_pulled_settings_are_saved_and_flag_a_restart() {
        let fx = settings_fx();
        let before = settings_copy_in(&fx.job.ctx).expect("copy in");
        std::fs::write(fx.tree.join("settings.json"), r#"{"theme":"dark"}"#)
            .expect("write the tree copy");

        let mut out = OpOutcome::new("merged", "main");
        settings_apply_back(&fx.job.ctx, before, &mut out).expect("apply back");

        assert!(out.settings_synced);
        assert!(
            out.settings_changed,
            "a changed value is what earns a restart"
        );
        assert!(out.settings_rejected.is_none());
        let values = crate::settings::load(&settings_schema(), &fx.host_settings);
        assert_eq!(values["theme"], serde_json::json!("dark"));
        // The key the remote omitted came back as the schema's default rather
        // than disappearing from the host's file.
        assert_eq!(values["notify"], serde_json::json!(true));
    }

    #[test]
    fn valid_but_identical_settings_do_not_flag_a_restart() {
        // Same values, different formatting. A restart tears down the user's
        // window and discards in-page state, so whitespace must never buy one.
        let fx = settings_fx();
        let before = settings_copy_in(&fx.job.ctx).expect("copy in");
        std::fs::write(
            fx.tree.join("settings.json"),
            r#"{"notify":true,"theme":"light"}"#,
        )
        .expect("write the tree copy");

        let mut out = OpOutcome::new("merged", "main");
        settings_apply_back(&fx.job.ctx, before, &mut out).expect("apply back");

        assert!(out.settings_synced, "the file was still adopted");
        assert!(
            !out.settings_changed,
            "identical values reformatted must not restart children"
        );
    }

    #[test]
    fn rejected_settings_leave_the_local_file_untouched_and_heal_the_tree() {
        let fx = settings_fx();
        let before = settings_copy_in(&fx.job.ctx).expect("copy in");
        let host_bytes = std::fs::read(&fx.host_settings).expect("read the host copy");
        std::fs::write(fx.tree.join("settings.json"), r#"{"theme":"purple"}"#)
            .expect("write the tree copy");

        let mut out = OpOutcome::new("merged", "main");
        settings_apply_back(&fx.job.ctx, before, &mut out)
            .expect("a teammate's typo must not fail the job");

        assert!(!out.settings_synced);
        assert!(!out.settings_changed);
        let rejected = out
            .settings_rejected
            .expect("settings_rejected must carry the reason");
        assert!(
            rejected.error.contains("purple"),
            "the message must name the offending value: {}",
            rejected.error
        );
        assert_eq!(out.warnings.len(), 1);
        assert_eq!(out.warnings[0].code, "settings_rejected");
        assert_eq!(
            std::fs::read(&fx.host_settings).expect("read the host copy"),
            host_bytes,
            "a rejected pull must not touch the local settings.json at all"
        );
        assert_eq!(
            std::fs::read(fx.tree.join("settings.json")).expect("read the tree copy"),
            host_bytes,
            "the tree copy must be healed so the next push fixes the remote"
        );
    }

    #[test]
    fn unparseable_tree_settings_are_rejected_rather_than_fatal() {
        // Broken JSON and a schema violation are the same event from the user's
        // side - someone committed a file this host cannot use - and both must
        // leave the repo syncing.
        let fx = settings_fx();
        let before = settings_copy_in(&fx.job.ctx).expect("copy in");
        std::fs::write(fx.tree.join("settings.json"), "{not json").expect("write the tree copy");

        let mut out = OpOutcome::new("merged", "main");
        settings_apply_back(&fx.job.ctx, before, &mut out).expect("broken JSON must not fail");

        assert!(out.settings_rejected.is_some());
        assert!(!out.settings_synced);
        assert_eq!(out.warnings[0].code, "settings_rejected");
    }

    #[test]
    fn apply_back_without_a_copy_in_does_nothing() {
        let mut fx = settings_fx();
        fx.job.ctx.def.sync_settings = false;

        let mut out = OpOutcome::new("up_to_date", "main");
        settings_apply_back(&fx.job.ctx, None, &mut out).expect("apply back");

        assert!(!out.settings_synced);
        assert!(out.settings_rejected.is_none());
        assert!(!fx.host_settings.exists());
    }

    /// An origin whose `main` already carries this host's exact settings bytes,
    /// plus the `seed` clone that stands in for a teammate.
    ///
    /// Seeding through `settings::save` is what makes the bytes identical, so a
    /// later `settings_copy_in` produces no diff and the only change in the job
    /// under test is the one the teammate pushed.
    fn origin_with_settings() -> (testkit::Origin, PathBuf) {
        let o = testkit::origin_with_main();
        // `origin_with_main` already owns the "seed" tree and pushed `main` from
        // it; cloning a second one over that path is what libgit2 refuses with
        // "exists and is not an empty directory".
        let seed = o.root.path().join("seed");
        crate::settings::save(
            &seed.join("settings.json"),
            &crate::settings::defaults(&settings_schema()),
        )
        .expect("seed settings.json");
        testkit::commit_all(&seed, "seed settings.json");
        testkit::push_main(&seed);
        (o, seed)
    }

    fn mirror_job(
        o: &testkit::Origin,
        tree: &std::path::Path,
        host_settings: &std::path::Path,
    ) -> testkit::Job {
        let mut fx = testkit::job(o, tree, JobOp::Sync);
        fx.ctx.def.sync_settings = true;
        fx.ctx.settings = Some(SettingsCtx {
            schema: settings_schema(),
            settings_file: host_settings.to_path_buf(),
        });
        fx
    }

    #[test]
    fn sync_adopts_settings_that_arrived_with_the_merge() {
        let (o, seed) = origin_with_settings();
        let a = testkit::clone_at(&o, "a");

        let mut wanted = crate::settings::defaults(&settings_schema());
        wanted.insert("theme".to_string(), serde_json::json!("dark"));
        crate::settings::save(&seed.join("settings.json"), &wanted).expect("teammate edit");
        testkit::commit_all(&seed, "switch the theme to dark");
        testkit::push_main(&seed);

        let host_settings = o.root.path().join("data/settings.json");
        let fx = mirror_job(&o, &a, &host_settings);

        let out = sync(&fx.ctx).expect("sync");

        assert!(
            !out.committed,
            "the copy-in wrote byte-identical bytes, so there was nothing to commit"
        );
        assert!(out.settings_synced);
        assert!(
            out.settings_changed,
            "a pulled value change must earn a restart"
        );
        assert!(out.settings_rejected.is_none());
        assert_eq!(
            crate::settings::load(&settings_schema(), &host_settings)["theme"],
            serde_json::json!("dark")
        );
    }

    #[test]
    fn sync_survives_settings_a_teammate_broke() {
        let (o, seed) = origin_with_settings();
        let a = testkit::clone_at(&o, "a");

        std::fs::write(seed.join("settings.json"), "{\"theme\": \"purple\"}\n")
            .expect("teammate typo");
        testkit::commit_all(&seed, "typo the theme");
        testkit::push_main(&seed);

        let host_settings = o.root.path().join("data/settings.json");
        let fx = mirror_job(&o, &a, &host_settings);

        let out = sync(&fx.ctx).expect("a teammate's typo must not fail the whole sync");

        assert!(out.settings_rejected.is_some());
        assert!(!out.settings_synced);
        assert!(!out.settings_changed);
        assert!(out.warnings.iter().any(|w| w.code == "settings_rejected"));
        assert_eq!(
            crate::settings::load(&settings_schema(), &host_settings)["theme"],
            serde_json::json!("light"),
            "the local settings must survive the pull untouched"
        );
        assert_eq!(
            std::fs::read(a.join("settings.json")).expect("read the tree copy"),
            std::fs::read(&host_settings).expect("read the host copy"),
            "the tree copy must be healed so the next push fixes the remote"
        );
    }

    #[test]
    fn first_sync_publishes_the_host_settings_without_a_restart() {
        let o = testkit::origin_with_main();
        let a = testkit::clone_at(&o, "a");
        let host_settings = o.root.path().join("data/settings.json");
        let fx = mirror_job(&o, &a, &host_settings);

        let out = sync(&fx.ctx).expect("sync");

        assert!(
            out.committed,
            "the materialized settings.json must be staged with everything else"
        );
        assert!(!out.settings_synced);
        assert!(
            !out.settings_changed,
            "publishing our own settings must never restart our own children"
        );

        let bare = git2::Repository::open_bare(&o.bare).expect("open the bare origin");
        let head = bare
            .find_reference("refs/heads/main")
            .expect("origin has main")
            .peel_to_commit()
            .expect("main has a commit");
        assert!(
            head.tree()
                .expect("commit tree")
                .get_path(std::path::Path::new("settings.json"))
                .is_ok(),
            "the first sync must publish settings.json to the remote"
        );
    }
}
