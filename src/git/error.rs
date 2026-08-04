//! The error taxonomy: one vocabulary for two surfaces.
//!
//! A problem detected while admitting a request becomes an HTTP status *and* a
//! `code`; the same problem detected inside a running job returns `202` up front
//! and puts the identical `code` string into `job.error.code`. A caller writes one
//! `match` and uses it in both places, which is only true if the mapping lives in
//! exactly one file — this one.

use axum::http::StatusCode;

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
}
