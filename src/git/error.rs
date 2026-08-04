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

#[cfg(test)]
mod tests {
    use super::*;
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
}
