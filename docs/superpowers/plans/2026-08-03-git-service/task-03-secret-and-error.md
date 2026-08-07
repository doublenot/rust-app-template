### Task 3: Secret and error types

Three leaf files with no I/O, no threads and no libgit2 calls — the vocabulary every
later task speaks. `Secret` makes credential redaction *structural* (a missing impl,
not a rule to remember). `GitError` gives one `code` string to two surfaces: an HTTP
status at admission and `job.error.code` inside a job, so a client writes one `match`
and uses it in both places. `util.rs` holds the four pure helpers that `registry.rs`,
`state.rs`, `jobs.rs` and `ops.rs` would otherwise each reinvent.

Everything here is verified: the code below was compiled against `git2 0.21.0`,
`axum 0.8`, `serde 1` and `serde_json 1`, and `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings` and `cargo test` are all green on it as
written. Do not re-wrap it — rustfmt will split it straight back.

**Files:**
- Create: `src/git/secret.rs`
- Create: `src/git/error.rs`
- Create: `src/git/util.rs`
- Modify: `src/git/mod.rs:5-6` — three `pub mod` lines and two crate-lint allows,
  inserted between the module doc comment (lines 1-5) and the existing
  `#[cfg(test)] mod tests` block.

**Interfaces:**

- Consumes (all from Task 1; nothing from Task 0 or Task 2):
  - `git2 = { version = "0.21", features = ["https", "ssh", "vendored-libgit2"] }` —
    only the pure types `git2::Error`, `git2::ErrorCode`, `git2::ErrorClass`.
  - `src/git/mod.rs` exists and `mod git;` is declared in `src/main.rs`.
  - Pre-existing dependencies, unchanged: `axum 0.8`, `serde` (derive), `serde_json`,
    `tokio` (`macros` + `rt-multi-thread`, for `#[tokio::test]`).

- Produces (later tasks rely on exactly these signatures):

  ```rust
  // src/git/secret.rs                                       — consumed by tasks 4, 6
  pub struct Secret(String);                                 // Clone, PartialEq, Eq, Deserialize
  impl Secret {
      pub fn new(value: String) -> Self;
      pub fn expose(&self) -> &str;                          // creds::callbacks only
      pub fn expose_for_disk(&self) -> &str;                 // registry::to_wire only
      pub fn expose_for_scrub(&self) -> &str;                // error::scrub only
  }
  impl std::fmt::Debug for Secret;                           // prints exactly `Secret(***)`
  // NO Serialize. NO Display. Both absences are load-bearing.

  // src/git/error.rs                                        — consumed by tasks 4-11
  pub enum GitErrorCode { /* 42 variants, see the table in Step 6 */ }
  impl GitErrorCode {
      pub fn as_str(self) -> &'static str;
      pub fn http_status(self) -> axum::http::StatusCode;
      pub fn retryable(self) -> bool;
  }
  pub enum AbortReason { None, Timeout, Shutdown }
  impl AbortReason { pub fn as_u8(self) -> u8; pub fn from_u8(v: u8) -> AbortReason; }
  pub struct Git2Hint { pub class: String, pub code: String }        // Serialize
  impl Git2Hint { pub fn of(e: &git2::Error) -> Git2Hint; }
  pub struct GitError {
      pub code: GitErrorCode,
      pub message: String,
      pub repo_id: Option<String>,
      pub path: Option<String>,
      pub field: Option<String>,
      pub git2: Option<Git2Hint>,
  }
  impl GitError {
      pub fn new(code: GitErrorCode, message: impl Into<String>) -> GitError;
      pub fn with_repo(self, repo_id: &str) -> GitError;
      pub fn with_path(self, path: &std::path::Path) -> GitError;
      pub fn with_field(self, field: &str) -> GitError;
      pub fn with_git2(self, e: &git2::Error) -> GitError;
      pub fn code(&self) -> GitErrorCode;
      pub fn http_status(&self) -> axum::http::StatusCode;
      pub fn retryable(&self) -> bool;
      pub fn scrubbed(self, secrets: &[&str]) -> GitError;
      pub fn as_dirty_tree_if_conflict(self) -> GitError;
      pub fn internal(message: impl Into<String>) -> GitError;
      pub fn invalid_request(field: &str, message: impl Into<String>) -> GitError;
      pub fn invalid_repo_id(id: &str) -> GitError;
      pub fn insecure_remote(remote: &str, why: &str) -> GitError;
      pub fn repo_not_found(id: &str) -> GitError;
      pub fn repo_busy(id: &str, op: &str) -> GitError;
      pub fn job_not_found(raw: &str) -> GitError;
      pub fn registry_read_only() -> GitError;
      pub fn registry_write_failed(e: impl std::fmt::Display) -> GitError;
      pub fn path_refused(path: &std::path::Path, why: &str) -> GitError;
      pub fn purge_failed(e: impl std::fmt::Display) -> GitError;
      pub fn status_timeout() -> GitError;
      pub fn shutting_down() -> GitError;
      pub fn remote_missing() -> GitError;
      pub fn confirm_required() -> GitError;
      pub fn settings_sync_unavailable() -> GitError;
      pub fn no_worktree(id: &str) -> GitError;
      pub fn not_a_repository(path: &std::path::Path) -> GitError;
      pub fn detached_head() -> GitError;
      pub fn branch_mismatch(actual: &str, expected: &str) -> GitError;
      pub fn unborn_branch(what: &str) -> GitError;
      pub fn dirty_tree(what: &str) -> GitError;
      pub fn checkout_would_overwrite(paths: &[String]) -> GitError;      // DirtyTree
      pub fn repo_state_not_clean(state: git2::RepositoryState,
                                  repo_id: &str) -> GitError;             // DirtyTree
      pub fn merge_path_type_conflict(path: &std::path::Path) -> GitError;
      pub fn merge_unresolvable() -> GitError;
      pub fn push_rejected(refname: &str, status: &str) -> GitError;
      pub fn io(message: impl Into<String>) -> GitError;
  }
  // NOTE: there are deliberately no `auth_missing`, `auth_unbound`, `canceled` or
  // `timeout` constructors. The four *codes* exist and are reachable, but nothing
  // ever builds a GitError from them by name: AuthMissing/AuthUnbound are recorded
  // through task 6's `record_cred_failure`, and Canceled/Timeout arrive through
  // `error::classify`. Adding a named constructor here would be dead code kept
  // alive only by `#![allow(dead_code)]`.
  impl serde::Serialize for GitError;          // job shape: {code,message,retryable,git2?}
  impl std::fmt::Display for GitError;
  impl std::error::Error for GitError;
  impl From<git2::Error> for GitError;
  impl From<std::io::Error> for GitError;
  impl From<serde_json::Error> for GitError;
  impl axum::response::IntoResponse for GitError;   // envelope shape, + Retry-After on 409
  pub fn classify(e: &git2::Error, abort: AbortReason) -> GitErrorCode;
  pub fn scrub(message: &str, secrets: &[&str]) -> String;
  pub fn scrub_serde(message: &str) -> String;             // added by the audit, 2nd round

  // src/git/util.rs                                         — consumed by tasks 4-9
  pub type NowFn = std::sync::Arc<dyn Fn() -> u64 + Send + Sync>;
  pub fn now_ms() -> u64;
  pub fn system_now() -> NowFn;
  pub fn utc_stamp(ms: u64) -> String;
  pub fn b64_nopad(bytes: &[u8]) -> String;
  pub fn bytes_to_path(b: &[u8]) -> Result<std::path::PathBuf, GitError>;   // cfg unix + windows
  ```

  Plus two module-wide lint allows in `src/git/mod.rs` that **every later task depends
  on** (Step 1 explains why each is unavoidable):
  `#![allow(dead_code)]` and `#![allow(clippy::result_large_err)]`.

**Two notes on where this diverges from the design document, both deliberate:**

1. **`Git2Hint::of` and `GitError::with_git2` are additions.** The contract requires
   `OpCtx::classify_err` (task 6) to "always attach `Git2Hint`" but names no way to
   build one; these two are it. Task 6 writes
   `GitError::new(code, e.message()).with_git2(e).scrubbed(&secrets)`.
2. **`classify` gains one arm the spec's sketch does not have: `(C::Timeout, _) =>
   Timeout`.** git2 0.21 exposes `ErrorCode::Timeout` for libgit2's own connect/idle
   timeout, whose *class* is `Net` — so without the arm it reports as `network_failed`
   and the §5.6 taxonomy row "`timeout` | our watchdog aborted a transfer, **or
   libgit2's own connect/idle timeout**" is not satisfiable. It sits below the
   `Callback` arm so an aborted transfer is still attributed to our own abort flag.

---

- [ ] **Step 1: Write the failing test — `Secret`**

Create `src/git/secret.rs` containing **only** the test module (the implementation
lands in Step 3):

```rust
//! A string that must never be printed, serialised, or logged.

#[cfg(test)]
mod tests {
    use super::Secret;

    #[test]
    fn debug_never_prints_the_value() {
        let s = Secret::new("ghp_averyrealtoken".to_string());
        assert_eq!(format!("{s:?}"), "Secret(***)");

        // The realistic leak is not `{s:?}` but a derived `Debug` several layers up —
        // `dbg!(&repo_def)`, or a panic payload that happens to carry one.
        let nested = Some(vec![("credential", s)]);
        assert_eq!(
            format!("{nested:?}"),
            r#"Some([("credential", Secret(***))])"#
        );
    }

    #[test]
    fn deserializes_from_a_bare_json_string() {
        let s: Secret =
            serde_json::from_str("\"ghp_abc\"").expect("Secret is #[serde(transparent)]");
        assert_eq!(s.expose(), "ghp_abc");
        assert_eq!(s.expose_for_disk(), "ghp_abc");
        assert_eq!(s.expose_for_scrub(), "ghp_abc");
        assert_eq!(s, Secret::new("ghp_abc".to_string()));
    }

    /// Redaction is structural: `RepoDef` cannot derive a serializer while it holds a
    /// `Secret` that has none. Adding one here would silently re-open that path and no
    /// other test in the tree would go red, so assert on this file's own source text.
    #[test]
    fn secret_has_no_serializer() {
        // Spelled in two halves so this test does not trip over its own source.
        let banned = concat!("Seri", "alize");
        let code = include_str!("secret.rs")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains(banned),
            "src/git/secret.rs must never derive or implement serde's serializer trait for \
             Secret: it is the compile-time barrier that keeps RepoDef un-serialisable \
             (spec §4.6). Expose credential shape through CredentialView instead."
        );
    }
}
```

Then register the module. `src/git/mod.rs` currently reads:

```rust
//! Git service.
//!
//! Root of the host-side git subsystem. Nothing lives here yet beyond the build-spine
//! tripwire below; each later module registers itself by adding its own
//! `pub mod <name>;` line to this file as it lands.

#[cfg(test)]
```

Insert the two lint allows and the first `pub mod` line so it reads:

```rust
//! Git service.
//!
//! Root of the host-side git subsystem. Nothing lives here yet beyond the build-spine
//! tripwire below; each later module registers itself by adding its own
//! `pub mod <name>;` line to this file as it lands.

// The subsystem lands over a dozen commits and is not reachable from `main` until the
// host is wired up in task 10; without this, rustc's dead-code pass flags every item a
// module publishes for the *next* module to consume, and CI runs `clippy -D warnings`.
#![allow(dead_code)]
// `GitError` is 152 bytes: a code plus four optional context strings, all of which the
// HTTP envelope and the job record are contractually required to carry. Boxing it to
// satisfy clippy's 128-byte `Result` budget would put an allocation on every `?` in the
// subsystem to save a move on paths that are already doing filesystem or network I/O.
#![allow(clippy::result_large_err)]

pub mod secret;

#[cfg(test)]
```

Both allows are inner attributes and must sit above the first item; `//!` doc comments
are themselves inner attributes, so following them is correct. Neither is optional —
removing `#![allow(dead_code)]` and running clippy at the end of this task produces
`error: enum GitErrorCode is never used`, `error: function classify is never used`, and
six more like them.

- [ ] **Step 2: Run the test and watch it fail**

This crate is a binary — there is no `--lib` target, and `src/bin/gen_icons.rs` is a
second binary — so unit tests run in the `hitch` bin target:

Run: `cargo test --bin hitch git::secret`

Expected: FAIL — compile error.

```
error[E0432]: unresolved import `super::Secret`
 --> src/git/secret.rs:5:9
  |
5 |     use super::Secret;
  |         ^^^^^^^^^^^^^ no `Secret` in `git::secret`

For more information about this error, try `rustc --explain E0432`.
error: could not compile `hitch` (bin "hitch" test) due to 1 previous error
```

- [ ] **Step 3: Implement `Secret`**

Replace the one-line module doc at the top of `src/git/secret.rs` with the full doc
comment plus the type, leaving the `#[cfg(test)] mod tests` block untouched below it:

```rust
//! A string that must never be printed, serialised, or logged.
//!
//! Every credential the host holds lives in one of these. Three properties are
//! load-bearing, and each of them is a *missing* impl rather than a rule someone
//! has to remember:
//!
//! * **No serde serializer, ever.** `RepoDef` holds `Secret`s, so a future
//!   `#[derive(Serialize)]` on `RepoDef` is a compile error instead of a token
//!   leaking into `GET /api/git/repos`. Giving this type a serializer — even a
//!   redacting one — removes that barrier and is a security regression; the
//!   hand-written `CredentialView` is the only sanctioned path from a credential
//!   to JSON.
//! * **No `Display`.** `format!("{token}")` does not compile.
//! * **A hand-written `Debug` that prints `Secret(***)`.** `RepoDef` derives
//!   `Debug` on top of it, so a stray `dbg!(&def)` or a panic message carrying a
//!   definition cannot leak.
//!
//! The value is reachable only through the three `expose_*` accessors. They are
//! split by purpose so that the audit in spec §8.5 — grep for `.expose()` and
//! expect only credential callbacks — stays literally true as the subsystem grows.

use serde::Deserialize;

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Secret(value)
    }

    /// The value, to hand to libgit2.
    ///
    /// ONLY legal caller: the `cb.credentials` closure in `creds::callbacks`.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The value, to persist into `repos.json` (written `0600`, atomically).
    ///
    /// ONLY legal caller: `registry::to_wire`.
    pub fn expose_for_disk(&self) -> &str {
        &self.0
    }

    /// The value, to find and replace with `***`.
    ///
    /// ONLY legal caller: `error::scrub`, fed by `creds::ResolvedCred::secrets`.
    pub fn expose_for_scrub(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}
```

- [ ] **Step 4: Run the test and watch it pass**

Run: `cargo test --bin hitch git::secret`

Expected: PASS.

```
running 3 tests
test git::secret::tests::debug_never_prints_the_value ... ok
test git::secret::tests::deserializes_from_a_bare_json_string ... ok
test git::secret::tests::secret_has_no_serializer ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

`N` is however many tests the rest of the suite has — 33 if only Task 1 has landed, more
if Task 2 landed first. The number that matters is the left-hand one; `N` is quoted the
same way in every later step.

- [ ] **Step 5: Commit**

```bash
git add src/git/secret.rs src/git/mod.rs
git commit -m "feat(git): Secret, a string with no serializer and no Display

Redaction is structural rather than conventional: RepoDef holds Secrets,
so a future #[derive(Serialize)] on RepoDef is a compile error instead of
a token in a GET response. Debug prints Secret(***) so a stray dbg! or a
panic payload carrying a definition cannot leak either.

The three expose_* accessors are split by purpose so that the spec §8.5
audit -- grep .expose(), expect only credential callbacks -- stays true.

src/git/mod.rs takes two module-wide lint allows: the subsystem is not
reachable from main until it is wired up, and GitError is 152 bytes."
```

- [ ] **Step 6: Write the failing test — the error code table**

Create `src/git/error.rs` with the module doc, the one import the codes need, and the
first batch of tests. The 42-row `TABLE` is the contract surface: `code` strings are
what clients `match` on and the HTTP statuses are what their middleware reacts to.

```rust
//! The error taxonomy: one vocabulary for two surfaces.
//!
//! A problem detected while admitting a request becomes an HTTP status *and* a
//! `code`; the same problem detected inside a running job returns `202` up front
//! and puts the identical `code` string into `job.error.code`. A caller writes one
//! `match` and uses it in both places, which is only true if the mapping lives in
//! exactly one file — this one.

use axum::http::StatusCode;

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
```

Register it in `src/git/mod.rs`, above `pub mod secret;` so the list stays alphabetical:

```rust
pub mod error;
pub mod secret;
```

- [ ] **Step 7: Run the test and watch it fail**

Run: `cargo test --bin hitch git::error`

Expected: FAIL — compile error.

```
error[E0432]: unresolved import `GitErrorCode`
  --> src/git/error.rs:13:9
   |
13 |     use GitErrorCode as G;
   |         ^^^^^^^^^^^^^^^^^ no external crate `GitErrorCode`

error[E0425]: cannot find type `GitErrorCode` in this scope
  --> src/git/error.rs:20:21
   |
20 |     const TABLE: &[(GitErrorCode, &str, u16, bool)] = &[
   |                     ^^^^^^^^^^^^ not found in this scope

Some errors have detailed explanations: E0425, E0432.
For more information about an error, try `rustc --explain E0425`.
error: could not compile `hitch` (bin "hitch" test) due to 3 previous errors
```

- [ ] **Step 8: Implement `GitErrorCode`**

Insert immediately above the `#[cfg(test)]` line in `src/git/error.rs`:

```rust
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
```

- [ ] **Step 9: Run the test and watch it pass**

Run: `cargo test --bin hitch git::error`

Expected: PASS.

```
running 3 tests
test git::error::tests::auth_failure_is_a_bad_gateway_not_an_unauthorized ... ok
test git::error::tests::code_table_matches_the_contract ... ok
test git::error::tests::wire_strings_are_unique ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 10: Commit**

```bash
git add src/git/error.rs src/git/mod.rs
git commit -m "feat(git): GitErrorCode with its wire strings and HTTP statuses

One vocabulary for two surfaces: an admission failure becomes an HTTP
status and a code, and the identical code appears in job.error.code when
the same problem is found inside a running job, so a client writes one
match and uses it in both places.

auth_failed is 502, never 401: our own x-host-token check passed and it
is the upstream that rejected us, so a client's generic 'refresh my token
and retry' middleware must not fire.

io_failed is not retryable despite the spec's 'maybe' -- a caller that
blindly retries EACCES spins."
```

- [ ] **Step 11: Write the failing test — `AbortReason` and `classify`**

Add to the existing `#[cfg(test)] mod tests` in `src/git/error.rs`: one more import at
the top of the module, and four tests after `auth_failure_is_a_bad_gateway_not_an_unauthorized`.

The import block at the top of `mod tests` becomes:

```rust
    use super::*;
    use git2::{ErrorClass as K, ErrorCode as C};
    use GitErrorCode as G;
```

The tests:

```rust
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
```

`a_callback_abort_is_not_a_user_error` is the one that matters most: a
`transfer_progress` callback returning `false` surfaces as
`code = GenericError, class = Callback`, **not** `ErrorCode::User`, which is the
intuitive guess and is wrong. Since the class is all libgit2 gives back, our own abort
flag is the only thing that can tell a watchdog timeout from a shutdown from a genuine
transport failure — all three arrive as byte-identical `git2::Error`s.

- [ ] **Step 12: Run the test and watch it fail**

Run: `cargo test --bin hitch git::error`

Expected: FAIL — compile error, eleven instances of two kinds:

```
error[E0433]: cannot find type `AbortReason` in this scope
   --> src/git/error.rs:282:13
    |
282 |             AbortReason::None,
    |             ^^^^^^^^^^^ use of undeclared type `AbortReason`

error[E0425]: cannot find function `classify` in this scope
   --> src/git/error.rs:328:17
    |
328 |                 classify(&err(*code, *class), AbortReason::None),
    |                 ^^^^^^^^ not found in this scope
```

- [ ] **Step 13: Implement `AbortReason` and `classify`**

Insert immediately above the `#[cfg(test)]` line in `src/git/error.rs`. No new `use`
lines are needed — `classify` imports git2's enums locally so the aliases `C` and `K`
do not leak into the rest of the file.

```rust
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
```

- [ ] **Step 14: Run the test and watch it pass**

Run: `cargo test --bin hitch git::error`

Expected: PASS.

```
running 7 tests
test git::error::tests::a_callback_abort_is_not_a_user_error ... ok
test git::error::tests::abort_reason_round_trips_through_a_byte ... ok
test git::error::tests::auth_failure_is_a_bad_gateway_not_an_unauthorized ... ok
test git::error::tests::classify_covers_every_row_of_the_taxonomy ... ok
test git::error::tests::code_table_matches_the_contract ... ok
test git::error::tests::the_abort_reason_never_overrides_a_classified_error ... ok
test git::error::tests::wire_strings_are_unique ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 15: Commit**

```bash
git add src/git/error.rs
git commit -m "feat(git): classify a git2::Error into a GitErrorCode

A transfer_progress callback returning false surfaces as
code=GenericError class=Callback ('indexer progress callback returned
-1'), NOT as ErrorCode::User -- verified against git2 0.21 / libgit2 1.9.
Since the class is all libgit2 gives back, a watchdog timeout, a shutdown
and a genuine mid-transfer transport failure are byte-identical errors;
AbortReason is what tells them apart, which is also why it cannot be a
bare AtomicBool.

One arm beyond the design sketch: ErrorCode::Timeout (libgit2's own
connect/idle timeout) carries class Net, so without an explicit arm it
would report as network_failed and lose the one signal that tells a
caller to raise [git].network_timeout_secs."
```

- [ ] **Step 16: Write the failing test — `scrub`**

Add to the existing `mod tests` in `src/git/error.rs`, after
`the_abort_reason_never_overrides_a_classified_error`:

```rust
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
```

- [ ] **Step 17: Run the test and watch it fail**

Run: `cargo test --bin hitch git::error::tests::scrub`

Expected: FAIL — compile error, once per call site:

```
error[E0425]: cannot find function `scrub` in this scope
   --> src/git/error.rs:391:20
    |
391 |         assert_eq!(scrub("https://u:p@h/x", &[]), "https://h/x");
    |                    ^^^^^ not found in this scope

error: could not compile `hitch` (bin "hitch" test) due to 8 previous errors
```

- [ ] **Step 18: Implement `scrub`**

Insert immediately above the `#[cfg(test)]` line in `src/git/error.rs`:

```rust
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

/// `scrub`, plus the one thing a *serde* message does that a libgit2 message does not:
/// echo the offending value back verbatim.
///
/// `{"credential": "<token>"}` is the shape that matters, because a hand-edit reaching
/// for that key is by definition holding a secret. serde answers with `invalid type:
/// string "<token>", expected internally tagged enum CredentialSpec`, `Registry::load`
/// pushes that onto `notes`, and `GitService::log_startup` writes it into a `git.log`
/// created 0644 — on every launch, for as long as the entry stays. Widening `scrub`
/// itself would be wrong; a libgit2 message's quoted strings are paths and URLs the
/// operator needs to read.
///
/// Only double-quoted runs are redacted, and only long ones. serde names *fields* and
/// *variants* in backticks and quotes *values* in double quotes, so this reaches the
/// payload and leaves the diagnosis: `invalid type: string "***", expected u64` still
/// carries which entry, which line and column, and what was wanted.
pub fn scrub_serde(message: &str) -> String {
    const LONGEST_INNOCENT: usize = 20;
    let scrubbed = scrub(message, &[]);
    let mut out = String::with_capacity(scrubbed.len());
    let mut rest = scrubbed.as_str();
    while let Some(open) = rest.find('"') {
        out.push_str(&rest[..=open]);
        let body = &rest[open + 1..];
        let mut escaped = false;
        let close = body.char_indices().find(|&(_, c)| {
            if escaped {
                escaped = false;
                return false;
            }
            match c {
                '\\' => {
                    escaped = true;
                    false
                }
                '"' => true,
                _ => false,
            }
        });
        // An unbalanced quote is not a value at all; there is nothing to redact and no
        // reason to mangle the tail looking for one.
        let Some((close, _)) = close else {
            out.push_str(body);
            return out;
        };
        if close > LONGEST_INNOCENT {
            out.push_str("***");
        } else {
            out.push_str(&body[..close]);
        }
        out.push('"');
        rest = &body[close + 1..];
    }
    out.push_str(rest);
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
```

- [ ] **Step 19: Run the test and watch it pass**

Run: `cargo test --bin hitch git::error::tests::scrub`

Expected: PASS.

```
running 3 tests
test git::error::tests::scrub_leaves_short_secrets_alone ... ok
test git::error::tests::scrub_replaces_a_stored_secret_anywhere_in_the_message ... ok
test git::error::tests::scrub_strips_url_userinfo ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 20: Commit**

```bash
git add src/git/error.rs
git commit -m "feat(git): scrub credentials out of error messages

libgit2 quotes the remote URL into most transport errors, and those
strings land in job records, git.log, git-state.json and native dialogs.
Two passes: strip scheme://user:pw@ down to scheme://, then literally
replace every known secret longer than 6 characters with ***.

Bare user@ is stripped as well as user:pw@ -- a PAT is routinely carried
in the username position. scp-form remotes have no scheme, carry no
secret, and are left readable. Six characters is the floor because a
shorter 'secret' is far likelier to be a path fragment, and replacing it
would mangle every message it appears in."
```

- [ ] **Step 21: Write the failing test — `GitError`**

Add to the existing `mod tests` in `src/git/error.rs`, after `scrub_leaves_short_secrets_alone`:

```rust
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
```

`job_error_shape_…` is the load-bearing one: it pins the **job** shape, which is
narrower than the HTTP envelope. `repo_id` is set on that error and deliberately does
**not** appear in the JSON — a job record already names its repo, and `api::ApiError`
(task 9) is what puts `repo_id`/`path`/`field` into the envelope instead.

- [ ] **Step 22: Run the test and watch it fail**

Run: `cargo test --bin hitch git::error`

Expected: FAIL — compile error, nine instances:

```
error[E0433]: cannot find type `GitError` in this scope
   --> src/git/error.rs:511:17
    |
511 |         let e = GitError::new(
    |                 ^^^^^^^^ use of undeclared type `GitError`

error[E0422]: cannot find struct, variant or union type `Git2Hint` in this scope

error: could not compile `hitch` (bin "hitch" test) due to 9 previous errors
```

- [ ] **Step 23: Implement `Git2Hint` and `GitError`**

First extend the import block at the top of `src/git/error.rs` to:

```rust
use axum::http::StatusCode;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use std::path::Path;
```

Then insert this block immediately above the `#[cfg(test)]` line:

```rust
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

    /// Scrubbed at construction, unlike every other constructor here.
    ///
    /// This is the one error whose subject is a string already known to carry a
    /// credential — that is what it is refusing. `scrub` runs at the job, log and
    /// dialog boundaries, which is late enough for a message that merely *might*
    /// contain one and too late for this: a `put` that refuses a remote returns
    /// straight to the caller, so without this the 403 body quotes the token back.
    /// Host and path survive, which is all the operator needs to recognise the repo.
    pub fn insecure_remote(remote: &str, why: &str) -> GitError {
        GitError::new(
            GitErrorCode::InsecureRemote,
            scrub(&format!("remote {remote:?} was refused: {why}"), &[]),
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

    // The two constructors below were added by the post-execution audit (F6, F7). Both
    // reuse `DirtyTree`: `GitErrorCode`, `as_str`, `http_status`, `retryable` and
    // `classify` are byte-identical before and after the audit, and CODE_COUNT is
    // unchanged. Their call sites are tasks 8 and 7 respectively, so neither is dead
    // code at this task's gate.

    /// A checkout refused because the file already at that path is not git's to
    /// replace.
    ///
    /// `dirty_tree` rather than a new code, deliberately: from the caller's side
    /// this is the same situation `pull` already refuses with — uncommitted work
    /// is in the way, deal with it and retry — and 409/non-retryable is already
    /// the right answer. What this adds is the *paths*, because an untracked or
    /// ignored file is one the caller may never have thought of as part of the
    /// repo at all, and libgit2's own message for the refusal is a bare count.
    pub fn checkout_would_overwrite(paths: &[String]) -> GitError {
        // Bounded: this message ends up in a one-line `git.log` record and in a
        // native dialog, and a merge that collides on a whole directory would
        // otherwise produce a paragraph neither of them can show.
        const SHOWN: usize = 10;
        let named = paths
            .iter()
            .take(SHOWN)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let more = match paths.len().saturating_sub(SHOWN) {
            0 => String::new(),
            n => format!(" and {n} more"),
        };
        // No path through `checkout_or_refuse` produces an empty list, but a
        // libgit2 that refused without notifying would otherwise print a dangling
        // colon and read like a bug in this function.
        let which = if paths.is_empty() {
            "local files it does not track".to_string()
        } else {
            format!("{named}{more}")
        };
        GitError::new(
            GitErrorCode::DirtyTree,
            format!(
                "the merge would overwrite {which}; commit, move or delete them \
                 and run it again"
            ),
        )
    }

    /// A repository half-way through an operation **a human started**.
    ///
    /// Deliberately `DirtyTree` and deliberately not a new code: `fill_status` has
    /// shipped this exact predicate — `repo.state() != Clean` — as `dirty_tree`
    /// since task 7, and one condition answering two different strings on the two
    /// surfaces this module exists to unify is what the add-never-rename rule is
    /// protecting against. Not `RepoLocked` either: that one is `retryable = true`,
    /// and an interrupted merge is the opposite of transient — nothing changes
    /// until a human acts, so a retry loop would spin forever.
    ///
    /// The message carries the whole recovery, `confirm` included: the reset body
    /// is mandatory (`confirm_required` is a 422), so an operator told only to
    /// "POST /reset" gets a second error instead of a repair.
    pub fn repo_state_not_clean(state: git2::RepositoryState, repo_id: &str) -> GitError {
        GitError::new(
            GitErrorCode::DirtyTree,
            format!(
                "repository state is {state:?}, not Clean: an interrupted merge, rebase or \
                 cherry-pick that this host did not start is still in progress. Finish it by \
                 hand in the working tree, or discard it with POST \
                 /api/git/repos/{repo_id}/reset {{\"to\":\"head\",\"confirm\":true}}"
            ),
        )
        .with_repo(repo_id)
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
```

- [ ] **Step 24: Run the test and watch it pass**

Run: `cargo test --bin hitch git::error`

Expected: PASS.

```
running 16 tests
test git::error::tests::a_callback_abort_is_not_a_user_error ... ok
test git::error::tests::a_checkout_conflict_can_be_reinterpreted_as_a_dirty_tree ... ok
test git::error::tests::abort_reason_round_trips_through_a_byte ... ok
test git::error::tests::auth_failure_is_a_bad_gateway_not_an_unauthorized ... ok
test git::error::tests::classify_covers_every_row_of_the_taxonomy ... ok
test git::error::tests::code_table_matches_the_contract ... ok
test git::error::tests::display_leads_with_the_code ... ok
test git::error::tests::from_git2_classifies_and_attaches_the_hint ... ok
test git::error::tests::from_io_and_json_errors ... ok
test git::error::tests::job_error_shape_is_code_message_retryable_and_an_optional_hint ... ok
test git::error::tests::scrub_leaves_short_secrets_alone ... ok
test git::error::tests::scrub_replaces_a_stored_secret_anywhere_in_the_message ... ok
test git::error::tests::scrub_strips_url_userinfo ... ok
test git::error::tests::scrubbed_rewrites_the_message_and_keeps_everything_else ... ok
test git::error::tests::the_abort_reason_never_overrides_a_classified_error ... ok
test git::error::tests::wire_strings_are_unique ... ok

test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 25: Commit**

```bash
git add src/git/error.rs
git commit -m "feat(git): GitError, its constructors and its job-error shape

One error type with a hand-written serializer that emits exactly the job
shape -- code, message, retryable, and the optional git2 hint. repo_id,
path and field are carried on the struct but deliberately not serialised
here: a job record already names its repo, and the HTTP envelope is
api::ApiError's job. Two shapes in two types is what lets both worked
examples in the design document be true at once.

as_dirty_tree_if_conflict exists because libgit2 refuses a would-clobber
checkout with ErrorCode::Conflict, which classifies as merge_unresolvable
-- right for a merge, wrong for a checkout, where nothing was merged and
the caller needs to hear 'commit or discard your changes'."
```

- [ ] **Step 26: Write the failing test — the HTTP envelope**

Add to the existing `mod tests` in `src/git/error.rs`, at the end:

```rust
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
```

These pin the envelope's **field order** (`code`, `message`, `repo_id`, `path`,
`field`) and the fact that absent fields vanish instead of serialising as `null` —
neither of which a `serde_json::Map` could give, since that orders alphabetically.
`envelope_omits_absent_fields_…` is the `git_disabled` body verbatim, which is exactly
the case where no `GitService` and therefore no `host_instance` exists.

- [ ] **Step 27: Run the test and watch it fail**

Run: `cargo test --bin hitch git::error`

Expected: FAIL — compile error.

```
error[E0425]: cannot find type `Response` in this scope
   --> src/git/error.rs:986:24
    |
986 |     async fn body_of(r: Response) -> String {
    |                         ^^^^^^^^ not found in this scope

error[E0599]: no method named `into_response` found for struct `git::error::GitError` in the current scope
   --> src/git/error.rs:995:52
    |
995 |         let r = GitError::repo_busy("notes", "sync").into_response();
    |                                                      ^^^^^^^^^^^^^

error: could not compile `hitch` (bin "hitch" test) due to 5 previous errors
```

- [ ] **Step 28: Implement `IntoResponse`**

Extend the import block at the top of `src/git/error.rs` — one new line, keeping the
existing four:

```rust
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use std::path::Path;
```

Then insert immediately above the `#[cfg(test)]` line:

```rust
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
```

- [ ] **Step 29: Run the test and watch it pass**

Run: `cargo test --bin hitch git::error`

Expected: PASS.

```
running 19 tests
...
test git::error::tests::envelope_carries_status_repo_id_and_retry_after ... ok
test git::error::tests::envelope_names_the_offending_field_and_path ... ok
test git::error::tests::envelope_omits_absent_fields_and_sets_no_retry_after ... ok

test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 30: Commit**

```bash
git add src/git/error.rs
git commit -m "feat(git): render a GitError as the API error envelope

Hand-rolled serializer rather than serde_json::json!, because the
envelope's field order is fixed (code, message, repo_id, path, field) and
serde_json's Map is a BTreeMap that would order it alphabetically. Absent
fields vanish rather than serialising as null. Every 409 carries
Retry-After: 1.

This is the envelope for errors raised where no GitService exists and so
no host_instance can be quoted -- the git_disabled fallback and the auth
layer. api::ApiError wraps the same error to add host_instance and, on
repo_busy, the running job."
```

- [ ] **Step 31: Write the failing test — `util.rs`**

Create `src/git/util.rs` containing only the test module:

```rust
//! Small leaf helpers shared by the whole subsystem.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_ms_is_after_this_was_written() {
        // 2026-01-01T00:00:00Z. Catches a clock read that silently returns 0.
        assert!(now_ms() > 1_767_225_600_000);
        assert!(system_now()() > 1_767_225_600_000);
    }

    #[test]
    fn utc_stamp_matches_date_u() {
        assert_eq!(utc_stamp(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc_stamp(999), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc_stamp(1_000), "1970-01-01 00:00:01 UTC");
        assert_eq!(utc_stamp(1_785_766_867_000), "2026-08-03 14:21:07 UTC");
        assert_eq!(utc_stamp(1_785_000_000_205), "2026-07-25 17:20:00 UTC");
        // Leap day, and the last second before the next one.
        assert_eq!(utc_stamp(1_709_164_800_000), "2024-02-29 00:00:00 UTC");
        assert_eq!(utc_stamp(1_709_251_199_000), "2024-02-29 23:59:59 UTC");
        // 2000 is a leap year, 1900 and 2100 are not; a naive %4 rule breaks here.
        assert_eq!(utc_stamp(951_782_400_000), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc_stamp(4_107_542_400_000), "2100-03-01 00:00:00 UTC");
    }

    #[test]
    fn b64_nopad_matches_rfc4648_without_padding() {
        assert_eq!(b64_nopad(b""), "");
        assert_eq!(b64_nopad(b"f"), "Zg");
        assert_eq!(b64_nopad(b"fo"), "Zm8");
        assert_eq!(b64_nopad(b"foo"), "Zm9v");
        assert_eq!(b64_nopad(b"foob"), "Zm9vYg");
        assert_eq!(b64_nopad(b"fooba"), "Zm9vYmE");
        assert_eq!(b64_nopad(b"foobar"), "Zm9vYmFy");
        // Both non-alphanumeric characters of the standard alphabet.
        assert_eq!(b64_nopad(&[0xff, 0xff, 0xff]), "////");
        assert_eq!(b64_nopad(&[0xfb, 0xff, 0xbf]), "+/+/");
        // A sha256 digest is 32 bytes -> 43 unpadded characters.
        assert_eq!(b64_nopad(&[0u8; 32]).len(), 43);
    }

    #[cfg(unix)]
    #[test]
    fn bytes_to_path_keeps_non_utf8_bytes_addressable() {
        use std::os::unix::ffi::OsStrExt;
        let raw = b"caf\xe9/notes.md";
        let p = bytes_to_path(raw).expect("unix paths are bytes");
        assert_eq!(p.as_os_str().as_bytes(), raw);
    }
}
```

Register it in `src/git/mod.rs` so the three lines read:

```rust
pub mod error;
pub mod secret;
pub mod util;
```

The two `utc_stamp` values are `date -u -d @1785766867` and `date -u -d @1785000000`;
regenerate any you doubt with that command rather than by hand.

- [ ] **Step 32: Run the test and watch it fail**

Run: `cargo test --bin hitch git::util`

Expected: FAIL — compile error, one per helper referenced (plus an
`unused import: super::*` warning, since nothing resolves through the glob yet).

```
error[E0425]: cannot find function `now_ms` in this scope
  --> src/git/util.rs:10:17
   |
10 |         assert!(now_ms() > 1_767_225_600_000);
   |                 ^^^^^^ not found in this scope

error[E0425]: cannot find function `utc_stamp` in this scope
error[E0425]: cannot find function `b64_nopad` in this scope
error[E0425]: cannot find function `bytes_to_path` in this scope

error: could not compile `hitch` (bin "hitch" test) due to 19 previous errors
```

- [ ] **Step 33: Implement the helpers**

Replace the one-line module doc at the top of `src/git/util.rs` with the full doc and
the implementations, leaving the test module untouched below:

```rust
//! Small leaf helpers shared by the whole subsystem.
//!
//! Everything here is pure and dependency-free so that `registry.rs`, `state.rs`,
//! `jobs.rs` and `ops.rs` can all use it without any of them naming each other.

use crate::git::error::GitError;

/// A clock, injectable so `jobs.rs` can test retention and throttling with zero
/// `sleep` calls.
pub type NowFn = std::sync::Arc<dyn Fn() -> u64 + Send + Sync>;

/// Milliseconds since the unix epoch.
///
/// Saturating rather than panicking: a machine whose clock is set before 1970 is
/// misconfigured, not a reason to take the app down.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub fn system_now() -> NowFn {
    std::sync::Arc::new(now_ms)
}

/// `1785766867000` -> `"2026-08-03 14:21:07 UTC"`.
///
/// Hand-rolled instead of pulling in `chrono`/`time`: the host needs exactly one
/// format, in one timezone, for log lines and commit messages.
pub fn utc_stamp(ms: u64) -> String {
    let secs = ms / 1000;
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let tod = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 -> (year, month, day).
///
/// Proleptic Gregorian with no leap seconds — the same calendar `date -u` prints.
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = yoe as i64 + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard-alphabet base64 with the `=` padding stripped.
///
/// Twenty lines instead of a dependency, and the only caller is the SSH host-key
/// fingerprint, whose format (`SHA256:<43 chars>`) is defined as unpadded.
pub fn b64_nopad(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        for i in 0..=chunk.len() {
            out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// git2 hands out index and conflict paths as raw bytes.
///
/// Non-UTF-8 paths are legal in git on unix, and `String::from_utf8_lossy` would
/// produce a path that no longer addresses the entry — so convert losslessly there,
/// and refuse on Windows, where such a path cannot be represented at all.
#[cfg(unix)]
pub fn bytes_to_path(b: &[u8]) -> Result<std::path::PathBuf, GitError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b)))
}

#[cfg(windows)]
pub fn bytes_to_path(b: &[u8]) -> Result<std::path::PathBuf, GitError> {
    std::str::from_utf8(b)
        .map(std::path::PathBuf::from)
        .map_err(|_| GitError::io("non-UTF-8 path in index is not representable on Windows"))
}
```

`GitError` appears in the return type of *both* arms, so the import is used on unix and
on Windows alike — no platform gets a `cfg`-gated unused-import warning.

- [ ] **Step 34: Run the test and watch it pass**

Run: `cargo test --bin hitch git::util`

Expected: PASS.

```
running 4 tests
test git::util::tests::b64_nopad_matches_rfc4648_without_padding ... ok
test git::util::tests::bytes_to_path_keeps_non_utf8_bytes_addressable ... ok
test git::util::tests::now_ms_is_after_this_was_written ... ok
test git::util::tests::utc_stamp_matches_date_u ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

- [ ] **Step 35: Commit**

```bash
git add src/git/util.rs src/git/mod.rs
git commit -m "feat(git): now_ms, utc_stamp, b64_nopad, bytes_to_path

Four pure helpers in one leaf module so registry.rs, state.rs, jobs.rs
and ops.rs can share them without any of them naming each other -- the
design document scatters these across mod.rs, ops.rs and merge.rs, which
would make three modules reach backwards and each grow its own copy.

utc_stamp is Howard Hinnant's civil_from_days rather than a chrono
dependency: one format, one timezone, tested against date -u including
1970, both 2024 and 2000 leap days, and the 2100 century non-leap that a
naive %4 rule gets wrong. b64_nopad exists only for the SSH host-key
fingerprint, whose SHA256:<43 chars> format is defined as unpadded.

bytes_to_path is lossless on unix because non-UTF-8 index paths are legal
there and from_utf8_lossy produces a path that no longer addresses the
entry it came from."
```

- [ ] **Step 36: Run every gate CI runs**

Run, in this order:

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test
```

Expected: all three silent/green.

- `cargo fmt --check` prints nothing. Every block in this task is already
  rustfmt-clean; if it prints a diff you re-wrapped something.
- `cargo clippy --all-targets --locked -- -D warnings` finishes with no output. If it
  reports `error: enum GitErrorCode is never used` (or seven similar), the
  `#![allow(dead_code)]` from Step 1 is missing from `src/git/mod.rs`. If it reports
  `error: the Err-variant returned from this function is very large` at
  `src/git/util.rs`, the `#![allow(clippy::result_large_err)]` is missing.
- `cargo test` runs the 26 tests added here (3 in `git::secret`, 19 in `git::error`,
  4 in `git::util`) on top of everything already in the tree, with the same
  `1 ignored` as before — you have added no ignored test.

- [ ] **Step 37: Confirm the tree is clean**

Run: `git status --short`

Expected: no output. All seven commits (Steps 5, 10, 15, 20, 25, 30, 35) are in and
nothing was left unstaged. If
`cargo fmt --check` did rewrite something in Step 36, amend it into the commit that
introduced it rather than leaving a trailing formatting commit:

```bash
git add -u && git commit --amend --no-edit
```
