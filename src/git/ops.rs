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
use crate::git::creds::{HostKeyChecker, ResolvedCred};
use crate::git::error::{self, AbortReason, GitError, GitErrorCode};
use crate::git::jobs::{AbortFlag, JobOp, JobSlot, Phase, Progress};
use crate::git::registry::{AuthorSpec, CredentialSpec, RepoDef, Warning};

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

pub fn init(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("init is not implemented yet"))
}

pub fn clone(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("clone is not implemented yet"))
}

pub fn pull(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("pull is not implemented yet"))
}

pub fn push(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("push is not implemented yet"))
}

pub fn sync(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("sync is not implemented yet"))
}

pub fn commit(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("commit is not implemented yet"))
}

pub fn branch(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("branch is not implemented yet"))
}

pub fn reset(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("reset is not implemented yet"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SshHostKeyPolicy;
    use crate::git::jobs::{Admission, JobStore, RepoLease};
    use std::time::Duration;

    fn def(json: &str) -> RepoDef {
        serde_json::from_str(json).expect("test fixture must parse")
    }

    struct Fixture {
        ctx: OpCtx,
        /// Dropping the lease finishes the job the slot belongs to, so the test has
        /// to outlive it.
        _lease: RepoLease,
    }

    fn fixture(def_json: &str, cred: ResolvedCred, cred_unbound: bool) -> Fixture {
        let store = JobStore::new("0000dead".to_string());
        let admitted = store
            .admit("notes", JobOp::Sync, None)
            .expect("a fresh store is not draining");
        let Admission::Started(slot, lease) = admitted else {
            panic!("a fresh store must admit the first job");
        };
        let abort = slot.abort.clone();
        Fixture {
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

    fn plain() -> Fixture {
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

    /// Scaffolding. It proves `run`'s dispatch table is total and that each arm
    /// reaches a verb of the same name; tasks 7 and 8 drop a line from the list
    /// below as each verb lands, and the whole test with the last stub.
    #[test]
    fn run_dispatches_to_a_verb_of_the_same_name() {
        let fx = plain();
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
            let e = run(op, &fx.ctx).expect_err("no verb is implemented in this slice");
            assert_eq!(e.code(), GitErrorCode::Internal);
            assert!(
                e.message.starts_with(op.as_str()),
                "{op:?} reached the wrong verb: {}",
                e.message
            );
        }
    }
}
