//! The error taxonomy: one vocabulary for two surfaces.
//!
//! A problem detected while admitting a request becomes an HTTP status *and* a
//! `code`; the same problem detected inside a running job returns `202` up front
//! and puts the identical `code` string into `job.error.code`. A caller writes one
//! `match` and uses it in both places, which is only true if the mapping lives in
//! exactly one file — this one.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitErrorCode {
    // Admission — reachable synchronously, so each one also has an HTTP status.
    GitDisabled,
    Unauthorized,
    InvalidRepoId,
    InvalidRequest,
    InsecureRemote,
    SettingsSyncUnavailable,
    RemoteMissing,
    ConfirmRequired,
    RepoNotFound,
    RepoBusy,
    JobNotFound,
    RegistryReadOnly,
    PathRefused,
    RegistryCorrupt,
    RegistryEntriesRejected,
    RegistryWriteFailed,
    PurgeFailed,
    StatusTimeout,
    ShuttingDown,
    // Execution — normally reached through `job.error`, but `http_status` is total
    // anyway so a rare synchronous path can never produce a code with no status.
    AuthFailed,
    AuthMissing,
    AuthUnbound,
    HostKeyMismatch,
    CertificateInvalid,
    NetworkFailed,
    Timeout,
    Canceled,
    NotFastForward,
    PushRejected,
    RemoteNotFound,
    NoWorktree,
    AlreadyExists,
    NotARepository,
    BranchMismatch,
    DetachedHead,
    UnbornBranch,
    DirtyTree,
    MergePathTypeConflict,
    MergeUnresolvable,
    RepoLocked,
    // NOTE: no `SettingsInvalid`. Invalid settings pulled from a remote are
    // deliberately a *warning on a successful job* (`settings_rejected`), never an
    // error — a teammate's typo must not fail an entire git sync forever (§9.7).
    // A stable wire string nothing can ever emit is a contract violation.
    IoFailed,
    Internal,
}

impl GitErrorCode {
    /// The wire string. This is the contract surface: clients `match` on it, so a
    /// value here may be added but never renamed.
    pub fn as_str(self) -> &'static str {
        match self {
            GitErrorCode::GitDisabled => "git_disabled",
            GitErrorCode::Unauthorized => "unauthorized",
            GitErrorCode::InvalidRepoId => "invalid_repo_id",
            GitErrorCode::InvalidRequest => "invalid_request",
            GitErrorCode::InsecureRemote => "insecure_remote",
            GitErrorCode::SettingsSyncUnavailable => "settings_sync_unavailable",
            GitErrorCode::RemoteMissing => "remote_missing",
            GitErrorCode::ConfirmRequired => "confirm_required",
            GitErrorCode::RepoNotFound => "repo_not_found",
            GitErrorCode::RepoBusy => "repo_busy",
            GitErrorCode::JobNotFound => "job_not_found",
            GitErrorCode::RegistryReadOnly => "registry_read_only",
            GitErrorCode::PathRefused => "path_refused",
            GitErrorCode::RegistryCorrupt => "registry_corrupt",
            GitErrorCode::RegistryEntriesRejected => "registry_entries_rejected",
            GitErrorCode::RegistryWriteFailed => "registry_write_failed",
            GitErrorCode::PurgeFailed => "purge_failed",
            GitErrorCode::StatusTimeout => "status_timeout",
            GitErrorCode::ShuttingDown => "shutting_down",
            GitErrorCode::AuthFailed => "auth_failed",
            GitErrorCode::AuthMissing => "auth_missing",
            GitErrorCode::AuthUnbound => "auth_unbound",
            GitErrorCode::HostKeyMismatch => "host_key_mismatch",
            GitErrorCode::CertificateInvalid => "certificate_invalid",
            GitErrorCode::NetworkFailed => "network_failed",
            GitErrorCode::Timeout => "timeout",
            GitErrorCode::Canceled => "canceled",
            GitErrorCode::NotFastForward => "not_fast_forward",
            GitErrorCode::PushRejected => "push_rejected",
            GitErrorCode::RemoteNotFound => "remote_not_found",
            GitErrorCode::NoWorktree => "no_worktree",
            GitErrorCode::AlreadyExists => "already_exists",
            GitErrorCode::NotARepository => "not_a_repository",
            GitErrorCode::BranchMismatch => "branch_mismatch",
            GitErrorCode::DetachedHead => "detached_head",
            GitErrorCode::UnbornBranch => "unborn_branch",
            GitErrorCode::DirtyTree => "dirty_tree",
            GitErrorCode::MergePathTypeConflict => "merge_path_type_conflict",
            GitErrorCode::MergeUnresolvable => "merge_unresolvable",
            GitErrorCode::RepoLocked => "repo_locked",
            GitErrorCode::IoFailed => "io_failed",
            GitErrorCode::Internal => "internal",
        }
    }

    /// `auth_failed` is **502, never 401**: the request's own `x-host-token` check
    /// passed and it is the *upstream* that rejected us. A 401 would make a client's
    /// generic "refresh my host token and retry" middleware do exactly the wrong thing.
    pub fn http_status(self) -> StatusCode {
        match self {
            GitErrorCode::GitDisabled
            | GitErrorCode::RepoNotFound
            | GitErrorCode::JobNotFound
            | GitErrorCode::RemoteNotFound => StatusCode::NOT_FOUND,
            GitErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
            GitErrorCode::InvalidRepoId
            | GitErrorCode::InvalidRequest
            | GitErrorCode::InsecureRemote
            | GitErrorCode::SettingsSyncUnavailable
            | GitErrorCode::RemoteMissing
            | GitErrorCode::ConfirmRequired => StatusCode::UNPROCESSABLE_ENTITY,
            GitErrorCode::RegistryReadOnly | GitErrorCode::PathRefused => StatusCode::FORBIDDEN,
            GitErrorCode::RepoBusy
            | GitErrorCode::NotFastForward
            | GitErrorCode::PushRejected
            | GitErrorCode::NoWorktree
            | GitErrorCode::AlreadyExists
            | GitErrorCode::NotARepository
            | GitErrorCode::BranchMismatch
            | GitErrorCode::DetachedHead
            | GitErrorCode::UnbornBranch
            | GitErrorCode::DirtyTree
            | GitErrorCode::MergePathTypeConflict
            | GitErrorCode::MergeUnresolvable
            | GitErrorCode::RepoLocked => StatusCode::CONFLICT,
            GitErrorCode::StatusTimeout | GitErrorCode::Timeout => StatusCode::GATEWAY_TIMEOUT,
            GitErrorCode::ShuttingDown | GitErrorCode::Canceled => StatusCode::SERVICE_UNAVAILABLE,
            GitErrorCode::AuthFailed
            | GitErrorCode::AuthMissing
            | GitErrorCode::AuthUnbound
            | GitErrorCode::HostKeyMismatch
            | GitErrorCode::CertificateInvalid
            | GitErrorCode::NetworkFailed => StatusCode::BAD_GATEWAY,
            GitErrorCode::RegistryCorrupt
            | GitErrorCode::RegistryEntriesRejected
            | GitErrorCode::RegistryWriteFailed
            | GitErrorCode::PurgeFailed
            | GitErrorCode::IoFailed
            | GitErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// "The identical request, unchanged, may succeed later."
    ///
    /// `io_failed` is deliberately **not** retryable even though the spec's table says
    /// "maybe": a caller that blindly retries `EACCES` or `ENOSPC` spins forever.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            GitErrorCode::RepoBusy
                | GitErrorCode::StatusTimeout
                | GitErrorCode::NetworkFailed
                | GitErrorCode::Timeout
                | GitErrorCode::NotFastForward
                | GitErrorCode::PushRejected
                | GitErrorCode::RepoLocked
        )
    }
}

/// Why a job's abort flag was raised. Lives here, next to `classify`, because a bare
/// `AtomicBool` cannot tell "the watchdog fired" from "the user quit" — and libgit2
/// reports both as the same callback error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortReason {
    None,
    Timeout,
    Shutdown,
}

impl AbortReason {
    pub fn as_u8(self) -> u8 {
        match self {
            AbortReason::None => 0,
            AbortReason::Timeout => 1,
            AbortReason::Shutdown => 2,
        }
    }

    /// Any unknown byte reads back as `None`: the flag is only ever written by
    /// `as_u8`, so an out-of-range value means memory we do not own and "not aborted"
    /// is the answer that cannot invent a failure.
    pub fn from_u8(v: u8) -> AbortReason {
        match v {
            1 => AbortReason::Timeout,
            2 => AbortReason::Shutdown,
            _ => AbortReason::None,
        }
    }
}

/// Map a libgit2 error onto our vocabulary.
///
/// The order of the arms is the whole design: code-first for the codes that are
/// unambiguous, then the callback arm, then class-based fallbacks.
pub fn classify(e: &git2::Error, abort: AbortReason) -> GitErrorCode {
    use git2::{ErrorClass as K, ErrorCode as C};
    use GitErrorCode as G;
    match (e.code(), e.class()) {
        (C::Auth, _) => G::AuthFailed,
        (C::Certificate, K::Ssh) => G::HostKeyMismatch,
        (C::Certificate, _) => G::CertificateInvalid,
        (C::NotFastForward, _) => G::NotFastForward,
        (C::Locked, _) => G::RepoLocked,
        (C::Exists, _) => G::AlreadyExists,
        (C::UnbornBranch, _) => G::UnbornBranch,
        (C::Uncommitted, _) | (C::IndexDirty, _) => G::DirtyTree,
        (C::Unmerged, _) | (C::Conflict, _) | (C::MergeConflict, _) => G::MergeUnresolvable,
        // Verified against git2 0.21 / libgit2 1.9: a `transfer_progress` callback
        // returning `false` surfaces as code = GenericError, class = Callback
        // ("indexer progress callback returned -1") — NOT as `ErrorCode::User`, which
        // is the intuitive guess and is wrong. Since the class is all libgit2 gives
        // us, our own abort flag is the only thing that can tell a watchdog timeout
        // from a shutdown from a genuine transport failure inside the callback.
        (_, K::Callback) => match abort {
            AbortReason::Timeout => G::Timeout,
            AbortReason::Shutdown => G::Canceled,
            AbortReason::None => G::NetworkFailed,
        },
        // libgit2's own connect/idle timeout. Its class is `Net`, so without this arm
        // it would be reported as `network_failed` and lose the one distinction that
        // tells a caller to raise `[git].network_timeout_secs`.
        (C::Timeout, _) => G::Timeout,
        (C::NotFound, K::Net) | (C::NotFound, K::Reference) => G::RemoteNotFound,
        (C::NotFound, K::Repository) => G::NoWorktree,
        (_, K::Net | K::Http | K::Ssl | K::Zlib | K::Indexer) => G::NetworkFailed,
        // libgit2 reuses GIT_ERROR_OS for *socket* errors, so the class alone cannot
        // separate "the server is down" from "EACCES on the working tree". Measured
        // against the vendored libgit2 1.9.6: a failed TCP connect is the only
        // network-origin GIT_ERROR_OS anywhere in the library — `streams/socket.c`
        // (every transport, unix) and `transports/winhttp.c` (Windows) — and both
        // spell it with this prefix; grepping the tree for `"failed to connect`
        // returns exactly those two plus the timeout, which is already class Net.
        //
        // Without this arm the archetypal transient failure — server restarting,
        // laptop offline, VPN down — reports `io_failed`, which is deliberately NOT
        // retryable because that code was reasoned about as local errno. The
        // inversion is what makes it a bug rather than a preference: a DNS miss
        // carries class Net and *is* retryable, so "I typo'd the hostname" would be
        // retryable while "the host is down" would not.
        (_, K::Os) if e.message().starts_with("failed to connect to") => G::NetworkFailed,
        (_, K::Os | K::Filesystem) => G::IoFailed,
        _ => G::Internal,
    }
}

/// Remove credentials from a message before it reaches a job record, `git.log`,
/// `git-state.json`, or a dialog.
///
/// Two passes, because there are two ways a secret gets into a libgit2 message:
/// quoted back as part of a URL, and echoed as part of a transport error.
pub fn scrub(message: &str, secrets: &[&str]) -> String {
    let mut out = strip_userinfo(message);
    for secret in secrets {
        // A short "secret" is far likelier to be a substring of a path, a host, or a
        // hex prefix than an actual credential, and replacing it would mangle every
        // message it appears in. Real tokens and passphrases are much longer than 6.
        if secret.len() > 6 {
            out = out.replace(secret, "***");
        }
    }
    out
}

/// `scheme://user:pw@host/path` -> `scheme://host/path`, everywhere in the string.
///
/// A PAT is routinely carried in the *username* position (`https://ghp_x@host/…`), so
/// a bare `user@` is stripped too. `scp`-style remotes (`git@github.com:acme/x.git`)
/// have no `://` and are left alone — they carry no secret and the user needs to see
/// them. Nothing but the userinfo is touched: host and path are what make the message
/// worth logging at all.
fn strip_userinfo(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(sep) = rest.find("://") {
        let after = sep + 3;
        out.push_str(&rest[..after]);
        let end = rest[after..]
            .find(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace())
            .map_or(rest.len(), |i| after + i);
        let authority = &rest[after..end];
        match authority.rfind('@') {
            Some(at) => out.push_str(&authority[at + 1..]),
            None => out.push_str(authority),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// git2's own class/code, carried through for debugging. Explicitly **not** part of
/// the contract: clients match on `code`, never on this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Git2Hint {
    pub class: String,
    pub code: String,
}

impl Git2Hint {
    pub fn of(e: &git2::Error) -> Git2Hint {
        Git2Hint {
            class: format!("{:?}", e.class()),
            code: format!("{:?}", e.code()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GitError {
    pub code: GitErrorCode,
    pub message: String,
    pub repo_id: Option<String>,
    /// Absolute, display form. Set on `path_refused` only.
    pub path: Option<String>,
    /// Set on `invalid_request` only.
    pub field: Option<String>,
    pub git2: Option<Git2Hint>,
}

impl GitError {
    pub fn new(code: GitErrorCode, message: impl Into<String>) -> GitError {
        GitError {
            code,
            message: message.into(),
            repo_id: None,
            path: None,
            field: None,
            git2: None,
        }
    }

    pub fn with_repo(mut self, repo_id: &str) -> GitError {
        self.repo_id = Some(repo_id.to_string());
        self
    }

    pub fn with_path(mut self, path: &Path) -> GitError {
        self.path = Some(path.display().to_string());
        self
    }

    pub fn with_field(mut self, field: &str) -> GitError {
        self.field = Some(field.to_string());
        self
    }

    pub fn with_git2(mut self, e: &git2::Error) -> GitError {
        self.git2 = Some(Git2Hint::of(e));
        self
    }

    pub fn code(&self) -> GitErrorCode {
        self.code
    }

    pub fn http_status(&self) -> StatusCode {
        self.code.http_status()
    }

    pub fn retryable(&self) -> bool {
        self.code.retryable()
    }

    /// Run `scrub` over the message. Every error crossing into a job record,
    /// `git-state.json`, `git.log`, or a dialog goes through here first.
    pub fn scrubbed(mut self, secrets: &[&str]) -> GitError {
        self.message = scrub(&self.message, secrets);
        self
    }

    /// Reinterpret a merge conflict as a dirty tree.
    ///
    /// libgit2 refuses a checkout that would overwrite local edits with
    /// `ErrorCode::Conflict`, which `classify` maps to `merge_unresolvable` — correct
    /// for a merge, wrong for a checkout, where nothing was merged and the fix the
    /// caller needs to hear is "commit or discard your changes first". Only call sites
    /// that know they were checking out may apply this.
    // Consuming `self` is the point: the error is re-labelled once, on its way out of
    // the checkout call. `wrong_self_convention` wants `as_*` to borrow.
    #[allow(clippy::wrong_self_convention)]
    pub fn as_dirty_tree_if_conflict(mut self) -> GitError {
        if self.code == GitErrorCode::MergeUnresolvable {
            self.code = GitErrorCode::DirtyTree;
        }
        self
    }

    pub fn internal(message: impl Into<String>) -> GitError {
        GitError::new(GitErrorCode::Internal, message)
    }

    pub fn invalid_request(field: &str, message: impl Into<String>) -> GitError {
        GitError::new(GitErrorCode::InvalidRequest, message).with_field(field)
    }

    pub fn invalid_repo_id(id: &str) -> GitError {
        GitError::new(
            GitErrorCode::InvalidRepoId,
            format!(
                "repo id {id:?} is not valid: 1-64 characters of [a-z0-9._-] starting with a \
                 letter or digit, no \"..\", no trailing dot or space, and not a Windows \
                 reserved name"
            ),
        )
    }

    pub fn insecure_remote(remote: &str, why: &str) -> GitError {
        GitError::new(
            GitErrorCode::InsecureRemote,
            format!("remote {remote:?} was refused: {why}"),
        )
    }

    pub fn repo_not_found(id: &str) -> GitError {
        GitError::new(
            GitErrorCode::RepoNotFound,
            format!("repo {id:?} is not in the registry"),
        )
        .with_repo(id)
    }

    pub fn repo_busy(id: &str, op: &str) -> GitError {
        GitError::new(
            GitErrorCode::RepoBusy,
            format!("repo {id:?} is busy running {op}"),
        )
        .with_repo(id)
    }

    pub fn job_not_found(raw: &str) -> GitError {
        GitError::new(
            GitErrorCode::JobNotFound,
            format!("job {raw:?} is unknown, has been evicted, or belongs to an earlier run"),
        )
    }

    pub fn registry_read_only() -> GitError {
        GitError::new(
            GitErrorCode::RegistryReadOnly,
            "repos.json is author-owned: [git].registry_writes is false",
        )
    }

    pub fn registry_write_failed(e: impl std::fmt::Display) -> GitError {
        GitError::new(
            GitErrorCode::RegistryWriteFailed,
            format!("repos.json could not be written: {e}"),
        )
    }

    pub fn path_refused(path: &Path, why: &str) -> GitError {
        GitError::new(
            GitErrorCode::PathRefused,
            format!("refusing to touch {}: {why}", path.display()),
        )
        .with_path(path)
    }

    pub fn purge_failed(e: impl std::fmt::Display) -> GitError {
        GitError::new(
            GitErrorCode::PurgeFailed,
            format!("the working tree could not be removed: {e}"),
        )
    }

    pub fn status_timeout() -> GitError {
        GitError::new(
            GitErrorCode::StatusTimeout,
            "the bounded status read did not finish in time",
        )
    }

    pub fn shutting_down() -> GitError {
        GitError::new(
            GitErrorCode::ShuttingDown,
            "the host is shutting down and is not accepting new git jobs",
        )
    }

    pub fn remote_missing() -> GitError {
        GitError::new(
            GitErrorCode::RemoteMissing,
            "this repo has no remote configured",
        )
    }

    pub fn confirm_required() -> GitError {
        GitError::new(
            GitErrorCode::ConfirmRequired,
            "this operation discards work: retry with \"confirm\": true",
        )
    }

    pub fn settings_sync_unavailable() -> GitError {
        GitError::new(
            GitErrorCode::SettingsSyncUnavailable,
            "sync_settings needs a [settings] section in app.toml",
        )
    }

    pub fn no_worktree(id: &str) -> GitError {
        GitError::new(
            GitErrorCode::NoWorktree,
            format!("repo {id:?} has no working tree yet: run clone or init first"),
        )
        .with_repo(id)
    }

    pub fn not_a_repository(path: &Path) -> GitError {
        GitError::new(
            GitErrorCode::NotARepository,
            format!(
                "{} exists and is not empty but is not a git repository",
                path.display()
            ),
        )
    }

    pub fn detached_head() -> GitError {
        GitError::new(
            GitErrorCode::DetachedHead,
            "HEAD is detached: check out the configured branch first",
        )
    }

    pub fn branch_mismatch(actual: &str, expected: &str) -> GitError {
        GitError::new(
            GitErrorCode::BranchMismatch,
            format!("HEAD is on {actual:?} but this repo is configured for {expected:?}"),
        )
    }

    pub fn unborn_branch(what: &str) -> GitError {
        GitError::new(
            GitErrorCode::UnbornBranch,
            format!("{what} needs at least one commit and this branch has none"),
        )
    }

    pub fn dirty_tree(what: &str) -> GitError {
        GitError::new(
            GitErrorCode::DirtyTree,
            format!("{what} would discard uncommitted changes"),
        )
    }

    pub fn merge_path_type_conflict(path: &Path) -> GitError {
        GitError::new(
            GitErrorCode::MergePathTypeConflict,
            format!(
                "{} is a file on one side of the merge and a directory on the other",
                path.display()
            ),
        )
    }

    pub fn merge_unresolvable() -> GitError {
        GitError::new(
            GitErrorCode::MergeUnresolvable,
            "the merge left conflicts that prefer-local resolution could not settle",
        )
    }

    pub fn push_rejected(refname: &str, status: &str) -> GitError {
        GitError::new(
            GitErrorCode::PushRejected,
            format!("the remote rejected {refname}: {status}"),
        )
    }

    pub fn io(message: impl Into<String>) -> GitError {
        GitError::new(GitErrorCode::IoFailed, message)
    }
}

/// The **job-error** shape: `{"code","message","retryable","git2"?}`.
///
/// Hand-written, and deliberately narrower than the HTTP envelope: `repo_id`, `path`
/// and `field` are not emitted here because a job record already names its repo, and
/// `api::ApiError` puts them into the envelope instead. Keeping the two shapes in two
/// types is what lets both of spec §5's worked examples be true at once.
impl Serialize for GitError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let len = 3 + usize::from(self.git2.is_some());
        let mut st = s.serialize_struct("GitError", len)?;
        st.serialize_field("code", self.code.as_str())?;
        st.serialize_field("message", &self.message)?;
        st.serialize_field("retryable", &self.code.retryable())?;
        if let Some(hint) = &self.git2 {
            st.serialize_field("git2", hint)?;
        }
        st.end()
    }
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for GitError {}

/// The `?` path for call sites with no abort flag and no credential in scope.
///
/// Inside a job, `OpCtx::classify_err` is used instead: it knows the abort reason and
/// the secrets to scrub, both of which this conversion has to assume away.
impl From<git2::Error> for GitError {
    fn from(e: git2::Error) -> GitError {
        GitError::new(classify(&e, AbortReason::None), e.message()).with_git2(&e)
    }
}

impl From<std::io::Error> for GitError {
    fn from(e: std::io::Error) -> GitError {
        GitError::io(e.to_string())
    }
}

impl From<serde_json::Error> for GitError {
    fn from(e: serde_json::Error) -> GitError {
        GitError::internal(format!("json: {e}"))
    }
}

/// The §5.4 envelope for the fields a bare `GitError` carries.
///
/// `api::ApiError` is the richer form — it wraps the same error and adds
/// `host_instance` (always) and `job` (`repo_busy` only). This impl exists for the
/// answers produced where no service, and therefore no host instance, exists: the
/// `git_disabled` fallback and the auth layer's `unauthorized`.
impl IntoResponse for GitError {
    fn into_response(self) -> Response {
        // Hand-rolled so the field order is the envelope's order and the optional
        // fields vanish rather than serialising as null.
        struct Body<'a>(&'a GitError);
        impl Serialize for Body<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                let e = self.0;
                let len = 2
                    + usize::from(e.repo_id.is_some())
                    + usize::from(e.path.is_some())
                    + usize::from(e.field.is_some());
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
                st.end()
            }
        }
        #[derive(Serialize)]
        struct Envelope<'a> {
            error: Body<'a>,
        }

        let status = self.http_status();
        let mut response = (status, axum::Json(Envelope { error: Body(&self) })).into_response();
        if status == StatusCode::CONFLICT {
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from_static("1"),
            );
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{ErrorClass as K, ErrorCode as C};
    use GitErrorCode as G;

    /// Every variant, its wire string, its HTTP status, and its retryability.
    ///
    /// `as_str`, `http_status` and `retryable` are exhaustive matches, so a new
    /// variant cannot be added without the compiler demanding all three; this table is
    /// what pins the *values* they were given.
    const TABLE: &[(GitErrorCode, &str, u16, bool)] = &[
        (G::GitDisabled, "git_disabled", 404, false),
        (G::Unauthorized, "unauthorized", 401, false),
        (G::InvalidRepoId, "invalid_repo_id", 422, false),
        (G::InvalidRequest, "invalid_request", 422, false),
        (G::InsecureRemote, "insecure_remote", 422, false),
        (
            G::SettingsSyncUnavailable,
            "settings_sync_unavailable",
            422,
            false,
        ),
        (G::RemoteMissing, "remote_missing", 422, false),
        (G::ConfirmRequired, "confirm_required", 422, false),
        (G::RepoNotFound, "repo_not_found", 404, false),
        (G::RepoBusy, "repo_busy", 409, true),
        (G::JobNotFound, "job_not_found", 404, false),
        (G::RegistryReadOnly, "registry_read_only", 403, false),
        (G::PathRefused, "path_refused", 403, false),
        (G::RegistryCorrupt, "registry_corrupt", 500, false),
        (
            G::RegistryEntriesRejected,
            "registry_entries_rejected",
            500,
            false,
        ),
        (G::RegistryWriteFailed, "registry_write_failed", 500, false),
        (G::PurgeFailed, "purge_failed", 500, false),
        (G::StatusTimeout, "status_timeout", 504, true),
        (G::ShuttingDown, "shutting_down", 503, false),
        (G::AuthFailed, "auth_failed", 502, false),
        (G::AuthMissing, "auth_missing", 502, false),
        (G::AuthUnbound, "auth_unbound", 502, false),
        (G::HostKeyMismatch, "host_key_mismatch", 502, false),
        (G::CertificateInvalid, "certificate_invalid", 502, false),
        (G::NetworkFailed, "network_failed", 502, true),
        (G::Timeout, "timeout", 504, true),
        (G::Canceled, "canceled", 503, false),
        (G::NotFastForward, "not_fast_forward", 409, true),
        (G::PushRejected, "push_rejected", 409, true),
        (G::RemoteNotFound, "remote_not_found", 404, false),
        (G::NoWorktree, "no_worktree", 409, false),
        (G::AlreadyExists, "already_exists", 409, false),
        (G::NotARepository, "not_a_repository", 409, false),
        (G::BranchMismatch, "branch_mismatch", 409, false),
        (G::DetachedHead, "detached_head", 409, false),
        (G::UnbornBranch, "unborn_branch", 409, false),
        (G::DirtyTree, "dirty_tree", 409, false),
        (
            G::MergePathTypeConflict,
            "merge_path_type_conflict",
            409,
            false,
        ),
        (G::MergeUnresolvable, "merge_unresolvable", 409, false),
        (G::RepoLocked, "repo_locked", 409, true),
        (G::IoFailed, "io_failed", 500, false),
        (G::Internal, "internal", 500, false),
    ];

    /// 42, not 43: `SettingsInvalid` was cut before implementation because invalid
    /// settings pulled from a remote are a warning on a *successful* job, never an
    /// error, so nothing could ever emit the code (see the note on the enum).
    const CODE_COUNT: usize = 42;

    #[test]
    fn code_table_matches_the_contract() {
        assert_eq!(
            TABLE.len(),
            CODE_COUNT,
            "every GitErrorCode variant needs a row"
        );
        for (code, wire, status, retryable) in TABLE {
            assert_eq!(code.as_str(), *wire, "{code:?}");
            assert_eq!(code.http_status().as_u16(), *status, "{code:?}");
            assert_eq!(code.retryable(), *retryable, "{code:?}");
        }
    }

    #[test]
    fn wire_strings_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for (code, wire, _, _) in TABLE {
            assert!(seen.insert(*wire), "duplicate wire string for {code:?}");
        }
        // Distinct wire strings prove the rows are distinct *variants* — `as_str` is a
        // function of the variant — so TABLE.len() == CODE_COUNT plus this assertion
        // together prove the table covers every variant exactly once, without needing
        // a way to enumerate the enum.
        assert_eq!(seen.len(), CODE_COUNT);
    }

    #[test]
    fn auth_failure_is_a_bad_gateway_not_an_unauthorized() {
        assert_eq!(G::AuthFailed.http_status(), StatusCode::BAD_GATEWAY);
        assert_eq!(G::Unauthorized.http_status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn abort_reason_round_trips_through_a_byte() {
        for r in [
            AbortReason::None,
            AbortReason::Timeout,
            AbortReason::Shutdown,
        ] {
            assert_eq!(AbortReason::from_u8(r.as_u8()), r);
        }
        assert_eq!(AbortReason::from_u8(200), AbortReason::None);
    }

    fn err(code: C, class: K) -> git2::Error {
        git2::Error::new(code, class, "synthetic")
    }

    #[test]
    fn classify_covers_every_row_of_the_taxonomy() {
        let cases: &[(C, K, GitErrorCode)] = &[
            (C::Auth, K::Http, G::AuthFailed),
            (C::Auth, K::Ssh, G::AuthFailed),
            (C::Certificate, K::Ssh, G::HostKeyMismatch),
            (C::Certificate, K::Ssl, G::CertificateInvalid),
            (C::Certificate, K::Http, G::CertificateInvalid),
            (C::NotFastForward, K::Reference, G::NotFastForward),
            (C::Locked, K::Index, G::RepoLocked),
            (C::Exists, K::Repository, G::AlreadyExists),
            (C::UnbornBranch, K::Reference, G::UnbornBranch),
            (C::Uncommitted, K::Checkout, G::DirtyTree),
            (C::IndexDirty, K::Index, G::DirtyTree),
            (C::Unmerged, K::Index, G::MergeUnresolvable),
            (C::Conflict, K::Checkout, G::MergeUnresolvable),
            (C::MergeConflict, K::Merge, G::MergeUnresolvable),
            (C::Timeout, K::Net, G::Timeout),
            (C::NotFound, K::Net, G::RemoteNotFound),
            (C::NotFound, K::Reference, G::RemoteNotFound),
            (C::NotFound, K::Repository, G::NoWorktree),
            (C::GenericError, K::Net, G::NetworkFailed),
            (C::GenericError, K::Http, G::NetworkFailed),
            (C::GenericError, K::Ssl, G::NetworkFailed),
            (C::GenericError, K::Zlib, G::NetworkFailed),
            (C::GenericError, K::Indexer, G::NetworkFailed),
            (C::GenericError, K::Os, G::IoFailed),
            (C::GenericError, K::Filesystem, G::IoFailed),
            // Nothing claims these: the tail must be `internal`, not a guess.
            (C::NotFound, K::Object, G::Internal),
            (C::GenericError, K::Config, G::Internal),
        ];
        for (code, class, want) in cases {
            assert_eq!(
                classify(&err(*code, *class), AbortReason::None),
                *want,
                "({code:?}, {class:?})"
            );
        }
    }

    #[test]
    fn a_callback_abort_is_not_a_user_error() {
        let e = err(C::GenericError, K::Callback);
        assert_eq!(classify(&e, AbortReason::Timeout), G::Timeout);
        assert_eq!(classify(&e, AbortReason::Shutdown), G::Canceled);
        assert_eq!(classify(&e, AbortReason::None), G::NetworkFailed);
    }

    #[test]
    fn the_abort_reason_never_overrides_a_classified_error() {
        // A shutdown racing a genuine auth rejection must still report auth_failed:
        // the operation did not fail *because* we aborted it.
        let e = err(C::Auth, K::Http);
        assert_eq!(classify(&e, AbortReason::Shutdown), G::AuthFailed);
    }

    #[test]
    fn a_refused_connect_is_a_network_failure_not_a_disk_failure() {
        let refused = git2::Error::new(
            C::GenericError,
            K::Os,
            "failed to connect to 127.0.0.1: Connection refused",
        );
        assert_eq!(classify(&refused, AbortReason::None), G::NetworkFailed);
        assert!(
            G::NetworkFailed.retryable(),
            "a server that is down may be up on the next attempt"
        );
        // A genuine local errno still lands on io_failed, which is deliberately not
        // retryable — retrying EACCES or ENOSPC spins forever.
        let eacces = git2::Error::new(
            C::GenericError,
            K::Os,
            "failed to stat '/x/y': Permission denied",
        );
        assert_eq!(classify(&eacces, AbortReason::None), G::IoFailed);
        assert!(!G::IoFailed.retryable());
    }

    /// The test above only proves the arm fires for a message *I* typed. This one
    /// proves libgit2 1.9.6 really does produce that class and that prefix, which is
    /// the assumption the arm rests on — if a future libgit2 reworded it or moved the
    /// connect failure to `GIT_ERROR_NET`, the arm would quietly stop matching and
    /// every outage would go back to reporting a non-retryable `io_failed`.
    ///
    /// Hermetic: binds a port, drops the listener, then connects to the now-closed
    /// port over loopback. No external network and no DNS, so it is safe offline and
    /// in a sandbox. `git://` is used deliberately — it goes through `streams/socket.c`
    /// on every platform, avoiding WinHTTP and any configured HTTP proxy.
    #[test]
    fn libgit2_still_reports_a_refused_connect_the_way_the_os_arm_expects() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            l.local_addr().expect("local addr").port()
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        // `Repository` has no `Debug`, so `expect_err` is unavailable here.
        let e = match git2::build::RepoBuilder::new().clone(
            &format!("git://127.0.0.1:{port}/x.git"),
            &tmp.path().join("clone"),
        ) {
            Err(e) => e,
            Ok(_) => panic!("a closed loopback port cannot be cloned from"),
        };
        assert_eq!(
            (e.code(), e.class()),
            (C::GenericError, K::Os),
            "libgit2 changed how it reports a refused connect: {e:?}"
        );
        assert!(
            e.message().starts_with("failed to connect to"),
            "the `K::Os` guard in classify keys on this prefix: {:?}",
            e.message()
        );
        assert_eq!(classify(&e, AbortReason::None), G::NetworkFailed);
    }

    #[test]
    fn scrub_strips_url_userinfo() {
        assert_eq!(scrub("https://u:p@h/x", &[]), "https://h/x");
        assert_eq!(
            scrub(
                "failed to connect to https://ghp_realtoken@github.com/acme/x.git: 403",
                &[]
            ),
            "failed to connect to https://github.com/acme/x.git: 403"
        );
        // scp-form has no scheme and carries no secret: leave it readable.
        assert_eq!(
            scrub("remote 'git@github.com:acme/x.git' not found", &[]),
            "remote 'git@github.com:acme/x.git' not found"
        );
        // No userinfo, nothing to do.
        assert_eq!(scrub("cloning https://h/x", &[]), "cloning https://h/x");
    }

    #[test]
    fn scrub_replaces_a_stored_secret_anywhere_in_the_message() {
        let token = "github_pat_11ABCDEF";
        let msg = format!("unexpected http status 401 (sent {token}) while listing refs");
        assert_eq!(
            scrub(&msg, &[token]),
            "unexpected http status 401 (sent ***) while listing refs"
        );
    }

    #[test]
    fn scrub_leaves_short_secrets_alone() {
        assert_eq!(
            scrub("cloning into abcdef/repo", &["abcdef"]),
            "cloning into abcdef/repo"
        );
        assert_eq!(
            scrub("cloning into abcdefg/repo", &["abcdefg"]),
            "cloning into ***/repo"
        );
    }

    #[test]
    fn scrubbed_rewrites_the_message_and_keeps_everything_else() {
        let e = GitError::new(
            GitErrorCode::AuthFailed,
            "https://u:secretpass@h/x rejected",
        )
        .with_repo("notes")
        .scrubbed(&["secretpass"]);
        assert_eq!(e.message, "https://h/x rejected");
        assert_eq!(e.code, GitErrorCode::AuthFailed);
        assert_eq!(e.repo_id.as_deref(), Some("notes"));
    }

    #[test]
    fn job_error_shape_is_code_message_retryable_and_an_optional_hint() {
        let e = GitError::new(GitErrorCode::NetworkFailed, "boom").with_repo("notes");
        assert_eq!(
            serde_json::to_string(&e).expect("GitError serialises"),
            r#"{"code":"network_failed","message":"boom","retryable":true}"#
        );

        let e = e.with_git2(&err(C::NotFastForward, K::Reference));
        assert_eq!(
            serde_json::to_string(&e).expect("GitError serialises"),
            r#"{"code":"network_failed","message":"boom","retryable":true,"git2":{"class":"Reference","code":"NotFastForward"}}"#
        );
    }

    #[test]
    fn from_git2_classifies_and_attaches_the_hint() {
        let e: GitError = git2::Error::new(C::Locked, K::Index, "index.lock exists").into();
        assert_eq!(e.code, GitErrorCode::RepoLocked);
        assert_eq!(e.message, "index.lock exists");
        assert_eq!(
            e.git2,
            Some(Git2Hint {
                class: "Index".to_string(),
                code: "Locked".to_string()
            })
        );
    }

    #[test]
    fn from_io_and_json_errors() {
        let io: GitError = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope").into();
        assert_eq!(io.code, GitErrorCode::IoFailed);
        assert_eq!(io.message, "nope");

        let json: GitError = serde_json::from_str::<u32>("{").unwrap_err().into();
        assert_eq!(json.code, GitErrorCode::Internal);
        assert!(json.message.starts_with("json: "));
    }

    #[test]
    fn a_checkout_conflict_can_be_reinterpreted_as_a_dirty_tree() {
        let e = GitError::new(GitErrorCode::MergeUnresolvable, "x").as_dirty_tree_if_conflict();
        assert_eq!(e.code, GitErrorCode::DirtyTree);
        // Anything else passes through untouched.
        let e = GitError::new(GitErrorCode::NetworkFailed, "x").as_dirty_tree_if_conflict();
        assert_eq!(e.code, GitErrorCode::NetworkFailed);
    }

    #[test]
    fn display_leads_with_the_code() {
        assert_eq!(
            GitError::repo_not_found("notes").to_string(),
            "repo_not_found: repo \"notes\" is not in the registry"
        );
    }

    async fn body_of(r: Response) -> String {
        let bytes = axum::body::to_bytes(r.into_body(), 64 * 1024)
            .await
            .expect("body");
        String::from_utf8(bytes.to_vec()).expect("utf-8 body")
    }

    #[tokio::test]
    async fn envelope_carries_status_repo_id_and_retry_after() {
        let r = GitError::repo_busy("notes", "sync").into_response();
        assert_eq!(r.status(), StatusCode::CONFLICT);
        assert_eq!(
            r.headers().get("retry-after").and_then(|v| v.to_str().ok()),
            Some("1")
        );
        assert_eq!(
            body_of(r).await,
            r#"{"error":{"code":"repo_busy","message":"repo \"notes\" is busy running sync","repo_id":"notes"}}"#
        );
    }

    #[tokio::test]
    async fn envelope_omits_absent_fields_and_sets_no_retry_after() {
        let r = GitError::new(
            GitErrorCode::GitDisabled,
            "the host was built without a [git] section in app.toml",
        )
        .into_response();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert!(r.headers().get("retry-after").is_none());
        assert_eq!(
            body_of(r).await,
            r#"{"error":{"code":"git_disabled","message":"the host was built without a [git] section in app.toml"}}"#
        );
    }

    #[tokio::test]
    async fn envelope_names_the_offending_field_and_path() {
        let r = GitError::invalid_request("auto_sync_secs", "must be a positive integer")
            .into_response();
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body_of(r).await,
            r#"{"error":{"code":"invalid_request","message":"must be a positive integer","field":"auto_sync_secs"}}"#
        );

        let r = GitError::path_refused(Path::new("/data/repos/../etc"), "outside the repos root")
            .into_response();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            body_of(r).await,
            r#"{"error":{"code":"path_refused","message":"refusing to touch /data/repos/../etc: outside the repos root","path":"/data/repos/../etc"}}"#
        );
    }
}
