# Git service — CANONICAL INTERFACE CONTRACT

**This document is authoritative over the spec for every name, signature, field, and stable string.**
Where it says `[CONTRACT DECISION]` the spec was ambiguous or self-contradictory and this document decides.
Spec = `docs/superpowers/specs/2026-08-03-git-service-design.md`.

Task map: `0` split main.rs→host.rs · `1` build spine · `2` config+paths · `3` secret+error ·
`4` registry+state · `5` jobs · `6` creds · `7` ops core · `8` merge+sync/pull/push ·
`9` GitService+api.rs · `10` host wiring · `11` sync_settings · `12` docs+CI

---

## 1. Module tree and task ownership

| path | owner task | later editors | notes |
|---|---|---|---|
| `src/main.rs` | 0 (split) | 9, 10 | keeps `UserEvent`, `dialog`, `notice`, `missing_chrome_dialog`, `main()`, `run()`, the `event_loop.run` match. Adds `mod git;` (task 1), `mod host;` (task 0). |
| `src/host.rs` | 0 (new) | 10 | `struct App`, `struct Children`, every `impl App` method. |
| `Cargo.toml` | 1 | — | `git2`, `gethostname`, `[target.'cfg(unix)'] openssl-sys`. |
| `app.toml` | 2 | — | commented-out `[git]` block. |
| `src/config.rs` | 2 | — | `GitSection`, `SshHostKeyPolicy`, `validate_branch_name`, `AppConfig::git`, `git_enabled()`, 3 `RuntimePaths` fields. |
| `src/git/mod.rs` | 1 (stub) | 3, 4, 5, 6, 7, 8, 9 (real) | **[CONTRACT DECISION]** task 1 lands **no** `pub mod` lines — the modules do not exist yet and the crate would not compile. Task 1 lands the module doc comment + the `git2::Version` test only. Every later task inserts its own declaration into the alphabetical block as its file lands: task 3 `error`/`secret`/`util` (and the crate-level `allow`s), task 4 `registry`/`state`, task 5 `jobs`, task 6 `creds`/`ops`, task 8 `merge`, task 9 `api`. Task 7 additionally adds `CONNECT_TIMEOUT_MS` + `init_libgit2_timeouts` here; task 9 lands `GitService`. |
| `src/git/secret.rs` | 3 | — | `Secret`. |
| `src/git/error.rs` | 3 | — | `GitError`, `GitErrorCode`, `AbortReason`, `classify`, `scrub`. |
| `src/git/util.rs` | 3 | — | `now_ms`, `utc_stamp`, `b64_nopad`, `bytes_to_path`, `NowFn`. **[CONTRACT DECISION]** the spec puts `bytes_to_path` in `mod.rs` and `utc_stamp` in `ops.rs`/`merge.rs`; both would be back-edges against §2.4's one-way rule (`ops → {creds,merge,error,secret}`), and three tasks would each invent their own. One leaf module below `error.rs` removes the cycle and the duplication. |
| `src/git/registry.rs` | 4 | — | `repos.json`. No git2, no axum, no tokio, no jobs. |
| `src/git/state.rs` | 4 | — | `git-state.json`. serde only. |
| `src/git/jobs.rs` | 5 | — | `JobStore`, `JobSlot`, `AbortFlag`. No git2, no HTTP, no fs. |
| `src/git/creds.rs` | 6 | — | `resolve`, `callbacks`, `HostKeyChecker`. |
| `src/git/ops.rs` | **6** (context types) | 7 (verbs), 8 (sync/pull/push), 11 (sync_settings) | Task 6 creates the file with `OpCtx`/`OpRequest`/`OpOutcome`/`Identity` and `pub fn run` returning `unimplemented` arms; task 7 fills `init/clone/commit/status/branches/branch/reset/purge`; task 8 fills `pull/push/sync`; task 11 fills the settings helpers. |
| `src/git/merge.rs` | 8 | — | `merge_prefer_local` and nothing else. |
| `src/git/api.rs` | 9 | — | axum sub-router, DTOs, error envelope. No git2 type crosses this file. |
| `src/internal_server.rs` | 9 (`git` field, `.nest`, `authorized` visibility, `spawn_state`) | 10 (`HostEvent` arms consumers are in main.rs; `StatusResponse`) | see §7. |
| `src/supervisor.rs` | 10 | — | `HostAccess`, `build_env` signature. |
| `src/tray.rs` | 10 | — | `TrayAction::GitSync`, `menu_model` signature. |
| `README.md` | 12 | — | §9 + §3/§4/§5/§6/§7 updates. |
| `.github/workflows/*` | 12 | — | `perl` + `pkg-config` on the Linux apt line. |

**[CONTRACT DECISION] — sanctioned back-reference.** `creds::callbacks(ctx: &ops::OpCtx)` and
`creds::resolve` are named by `ops.rs`, and `creds.rs` names `ops::OpCtx`. Rust has no module
acyclicity rule inside a crate; §2.2's "may name" column is a review heuristic. The rule that is
enforced: **`ops.rs`, `merge.rs`, `creds.rs`, `jobs.rs`, `registry.rs`, `state.rs`, `error.rs`,
`secret.rs`, `util.rs` never name `axum`, `tokio`, `App`, `Children`, or `HostEvent`.**
`jobs.rs`/`registry.rs`/`state.rs` additionally never name `git2`.

---

## 2. Constants — exact names, types, values, owning file

| const | type | value | file | task |
|---|---|---|---|---|
| `MAX_JOB_RECORDS` | `usize` | `50` | `git/jobs.rs` | 5 |
| `JOB_TTL_SECS` | `u64` | `3_600` | `git/jobs.rs` | 5 |
| `JOB_MIN_AGE_SECS` | `u64` | `60` | `git/jobs.rs` | 5 |
| `PROGRESS_THROTTLE_MS` | `u64` | `100` | `git/jobs.rs` | 5 |
| `AUTO_SYNC_MIN_SECS` | `u64` | `30` | `git/registry.rs` | 4 |
| `AUTO_SYNC_MAX_SECS` | `u64` | `86_400` | `git/registry.rs` | 4 |
| `MAX_REPO_ID_LEN` | `usize` | `64` | `git/registry.rs` | 4 |
| `REGISTRY_VERSION` | `u32` | `1` | `git/registry.rs` | 4 |
| `STATE_VERSION` | `u32` | `1` | `git/state.rs` | 4 |
| `MAX_DIRTY_FILES` | `usize` | `200` | `git/ops.rs` | 7 |
| `MAX_CRED_ATTEMPTS` | `u32` | `2` | `git/creds.rs` | 6 |
| `CONNECT_TIMEOUT_MS` | `i32` | `10_000` | `git/mod.rs` | **7** |
| `STATUS_READ_TIMEOUT_MS` | `u64` | `2_000` | `git/mod.rs` | 9 |
| `QUIT_ABANDON_GRACE_MS` | `u64` | `2_000` | `git/mod.rs` | 9 |
| `GIT_LOG_FILE` | `&str` | `"git.log"` | `git/mod.rs` | 9 |
| `MAX_REQUEST_ID_LEN` | `usize` | `128` | `git/api.rs` | 9 |
| `DEFAULT_JOB_LIST_LIMIT` | `usize` | `50` | `git/api.rs` | 9 |
| `MAX_JOB_LIST_LIMIT` | `usize` | `200` | `git/api.rs` | 9 |
| `MAX_BRANCH_NAME_LEN` | `usize` | `200` | `config.rs` | 2 |
| `RESTART_DEBOUNCE_MS` | `u64` | `2_000` | `host.rs` | 10 |
| `GIT_NOTICE_HISTORY` | `usize` | `16` | `host.rs` | 10 |

`DEFAULT_JOB_LIST_LIMIT` / `MAX_JOB_LIST_LIMIT` / `STATUS_READ_TIMEOUT_MS` / `QUIT_ABANDON_GRACE_MS` /
`AUTO_SYNC_MAX_SECS` / `MAX_REPO_ID_LEN` / `MAX_BRANCH_NAME_LEN` / `GIT_NOTICE_HISTORY` are
`[CONTRACT DECISION]`s: the spec names the value in prose (§5.1, §6.6, §4.2, §4.3, §3, §9.3) but not
the constant. Every one is `pub const` so tests in other files can assert against it.

**[CONTRACT DECISION] — `CONNECT_TIMEOUT_MS` is landed by task 7, not task 9.** Task 7 needs the
libgit2 process-global timeouts in place for its own network-touching tests, so it lands both
`CONNECT_TIMEOUT_MS` and `init_libgit2_timeouts` (§5.10) in `git/mod.rs`. Task 9 only *calls*
`init_libgit2_timeouts(self.cfg.network_timeout_secs)` from `GitService::start`, and must not
re-declare either name.

Reused existing: `supervisor::LOG_MAX_BYTES = 5 * 1024 * 1024`.

---

## 3. Stable strings — the contract surface

### 3.1 Env vars (child process only)
`APP_HOST_URL` = `http://127.0.0.1:<port>` · `APP_HOST_TOKEN` = `<token>` ·
`APP_GIT_ENABLED` = `"1"` **and only pushed when `[git]` is present** (absent, not `"0"`, when off).
Existing, unchanged: `APP_SETTINGS_FILE`, `APP_SETTING_<KEY>`.

### 3.2 HTTP
Header `x-host-token`. Base path `/api/git`. `Location: /api/git/jobs/<job_id>` on 202.
`Retry-After: 1` on every 409.

### 3.3 `JobId` format
`job_<host_instance>_<seq:06x>` — `host_instance` is exactly 8 lowercase hex chars; `seq` is
lowercase hex, zero-padded to 6, **not truncated** if it overflows 6 digits.
Example: `job_7c1e93aa_00002b`.

### 3.4 `code` values (`GitErrorCode::as_str()`) — exact `&'static str`

Admission: `git_disabled` `unauthorized` `invalid_repo_id` `invalid_request` `insecure_remote`
`settings_sync_unavailable` `remote_missing` `confirm_required` `repo_not_found` `repo_busy`
`repo_locked`† `job_not_found` `registry_read_only` `path_refused` `registry_corrupt`
`registry_entries_rejected` `registry_write_failed` `purge_failed` `status_timeout`
`shutting_down` `internal`

Execution: `auth_failed` `auth_missing` `auth_unbound` `host_key_mismatch` `certificate_invalid`
`network_failed` `timeout` `canceled` `not_fast_forward` `push_rejected` `remote_not_found`
`no_worktree` `already_exists` `not_a_repository` `branch_mismatch` `detached_head` `unborn_branch`
`dirty_tree` `merge_path_type_conflict` `merge_unresolvable` `repo_locked`†
`io_failed`

† **`repo_locked` is in both lists.** As an execution code it is libgit2's `ErrorCode::Locked` and
its message names a lock file's absolute path. As an admission code — added by the post-execution
audit (F8) — it is `jobs::repo_locked(repo_id)`, returned **synchronously** by `JobStore::hold_repo`
and therefore by `GitService::put_repo`, `delete_repo` and `start_job` while a `PUT`/`DELETE` for
that id is in flight; that form names the *repo* and carries no `error.job`, because the holder is
the host itself and there is no job to embed. Same code, same 409, same `retryable = true`.

`shutting_down` is a **[CONTRACT DECISION]**: §6.6 requires it, §5.6's table omits it.

**[CONTRACT DECISION] — there is no `settings_invalid` code.** The spec's §5.6 table lists one, but
nothing can ever emit it: by decision 14 and §9.7, invalid pulled settings are deliberately a
*warning on a successful job* (`OpOutcome.settings_rejected` plus `Warning{code:"settings_rejected"}`,
§3.11), because a teammate's typo must not fail an entire git sync forever. A stable string that
nothing can ever emit is a contract violation, so the string and the `GitErrorCode::SettingsInvalid`
variant are both deleted.

### 3.5 `outcome` values (`OpOutcome.outcome`)
`up_to_date` `no_changes` `committed` `fast_forward` `merged` `merged_unrelated` `cloned`
`initialized` `pushed` `reset` `checked_out` `branch_created`

### 3.6 `status.state` values
`absent` `uninitialized` `unborn` `clean` `dirty` `detached` `error`

### 3.7 `job.state` values
`running` `succeeded` `failed`

### 3.8 `job.phase` values
`preparing` `cloning` `staging` `committing` `fetching` `merging` `checking_out` `pushing` `finalizing`

### 3.9 `job.op` values
`init` `clone` `pull` `push` `sync` `commit` `branch` `reset`

### 3.10 `result.merge.kind` values
`up_to_date` `no_remote_branch` `adopted_remote` `fast_forward` `merge_commit` `merge_commit_unrelated`

### 3.11 `warnings[].code` values
`auth_unbound` `auto_sync_clamped` `restart_deferred` `settings_rejected`
`settings_symlink_replaced`

`settings_symlink_replaced` was added by the post-execution audit. A remote can track
`settings_path` as a mode-120000 blob, which libgit2 materializes as a real symlink and
`std::fs::write` then follows; `ops::settings_tree_target` unlinks the link and writes a regular
file in its place. The repair is silent to the *filesystem* but never to the *operator* — the job
still succeeds, and this warning is the only way they learn the remote is aiming at their disk.
A linked **directory** component is refused instead, with `path_refused` (403).

### 3.12 `registry_error.rejected[].reason` values
`invalid_repo_id` `invalid_branch_name` `invalid_request` `insecure_remote`
`settings_sync_unavailable` `duplicate_id`

### 3.13 `HostEvent::GitRestartChildren.reason` values
`"settings"` `"requested"`

### 3.14 `credential.kind` values (request, registry, and `CredentialView`)
`none` `token` `ssh_key`

### 3.15 `[git].ssh_host_key_policy` values
`"tofu"` `"accept"`

### 3.16 Config keys (all under `[git]`)
`tray_sync` `error_dialogs` `status_api` `registry_writes` `default_branch` `author_name`
`author_email` `network_timeout_secs` `quit_sync_timeout_secs` `allow_http` `ssh_host_key_policy`

### 3.17 File and directory names (all relative to `<data-dir>`)
`repos/` · `repos/<id>/` · `repos.json` · `repos.json.tmp` · `repos.json.corrupt-<epoch_ms>` ·
`git-state.json` · `git-state.json.tmp` · `logs/git.log` · `logs/git.log.old`

### 3.18 `git.log` line format (exact)
```
<epoch_ms> <op> repo=<id> job=<job_id> ok code=- <scrubbed message>
<epoch_ms> <op> repo=<id> job=<job_id> err code=<code> <scrubbed message>
```
Startup lines use `op=startup` and `job=-`. **Every form carries the same redaction guarantee**,
the startup ones included: they are built from `Registry::notes()` and `Registry::error()`, which
are scrubbed at construction rather than by this caller, because the file is created under the
ordinary umask and not at `0600`.

### 3.19 Fingerprint format
`SHA256:<standard-base64-of-32-bytes, '=' padding stripped>`

### 3.20 Test-only env vars (task 12)
`GIT_TEST_HTTPS_URL` `GIT_TEST_TOKEN` `GIT_TEST_SSH_URL` `GIT_TEST_SSH_KEY`.
Packager escape hatches documented in README: `LIBGIT2_NO_VENDOR=1`, `LIBSSH2_SYS_USE_PKG_CONFIG=1`.

---

## 4. Types

### 4.1 `src/config.rs` — task 2

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSection {
    #[serde(default)] pub tray_sync: bool,
    #[serde(default)] pub error_dialogs: bool,
    #[serde(default)] pub status_api: bool,
    #[serde(default = "default_registry_writes")] pub registry_writes: bool,
    #[serde(default = "default_branch_name")] pub default_branch: String,
    #[serde(default)] pub author_name: String,
    #[serde(default)] pub author_email: String,
    #[serde(default = "default_network_timeout_secs")] pub network_timeout_secs: u64,
    #[serde(default = "default_quit_sync_timeout_secs")] pub quit_sync_timeout_secs: u64,
    #[serde(default)] pub allow_http: bool,
    #[serde(default)] pub ssh_host_key_policy: SshHostKeyPolicy,
}
fn default_registry_writes() -> bool { true }
fn default_branch_name() -> String { "main".to_string() }
fn default_network_timeout_secs() -> u64 { 120 }
fn default_quit_sync_timeout_secs() -> u64 { 10 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SshHostKeyPolicy { #[default] Tofu, Accept }
```
**[CONTRACT DECISION]** default-fn names follow the existing `default_timeout`/`default_width` house
style, not the spec's `yes`/`d_branch`/`d_net`/`d_quit`.

`AppConfig` gains, as the **last** field (after `settings`):
```rust
    #[serde(default)] pub git: Option<GitSection>,
```

`RuntimePaths` gains three fields, populated in `under()`, **`ensure()` unchanged**:
```rust
    pub repos_dir: PathBuf,        // data_dir.join("repos")
    pub registry_file: PathBuf,    // data_dir.join("repos.json")
    pub git_state_file: PathBuf,   // data_dir.join("git-state.json")
```

### 4.2 `src/git/secret.rs` — task 3

```rust
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self;
    /// ONLY legal caller: the `cb.credentials` closure in `creds::callbacks`.
    pub fn expose(&self) -> &str;
    /// ONLY legal caller: `registry::to_wire` (persisting repos.json).
    pub fn expose_for_disk(&self) -> &str;
    /// ONLY legal caller: `error::scrub` (replacing the value with `***`).
    pub fn expose_for_scrub(&self) -> &str;
}
impl std::fmt::Debug for Secret;   // writes exactly `Secret(***)`
```
**No `Serialize`. No `Display`.** A doc comment on the type records that adding `Serialize` is a
contract violation. **[CONTRACT DECISION]** — three named accessors instead of one: §4.6 forbids
`Serialize` while §4.7 must write the token to `repos.json` and §8.5 must scrub it out of messages.
Splitting the name keeps §8.5's audit (`grep '\.expose()'` ⇒ exactly four hits, all in
`creds::callbacks`) literally true.

### 4.3 `src/git/error.rs` — task 3

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitErrorCode {
    GitDisabled, Unauthorized, InvalidRepoId, InvalidRequest, InsecureRemote,
    SettingsSyncUnavailable, RemoteMissing, ConfirmRequired, RepoNotFound, RepoBusy,
    JobNotFound, RegistryReadOnly, PathRefused, RegistryCorrupt, RegistryEntriesRejected,
    RegistryWriteFailed, PurgeFailed, StatusTimeout, ShuttingDown,
    AuthFailed, AuthMissing, AuthUnbound, HostKeyMismatch, CertificateInvalid,
    NetworkFailed, Timeout, Canceled, NotFastForward, PushRejected, RemoteNotFound,
    NoWorktree, AlreadyExists, NotARepository, BranchMismatch, DetachedHead, UnbornBranch,
    DirtyTree, MergePathTypeConflict, MergeUnresolvable, RepoLocked,
    IoFailed, Internal,
}
```
**[CONTRACT DECISION]** no `SettingsInvalid` variant — see §3.4.

| variant | `as_str()` | `http_status()` | `retryable()` |
|---|---|---|---|
| `GitDisabled` | `git_disabled` | 404 | false |
| `Unauthorized` | `unauthorized` | 401 | false |
| `InvalidRepoId` | `invalid_repo_id` | 422 | false |
| `InvalidRequest` | `invalid_request` | 422 | false |
| `InsecureRemote` | `insecure_remote` | 422 | false |
| `SettingsSyncUnavailable` | `settings_sync_unavailable` | 422 | false |
| `RemoteMissing` | `remote_missing` | 422 | false |
| `ConfirmRequired` | `confirm_required` | 422 | false |
| `RepoNotFound` | `repo_not_found` | 404 | false |
| `RepoBusy` | `repo_busy` | 409 | true |
| `JobNotFound` | `job_not_found` | 404 | false |
| `RegistryReadOnly` | `registry_read_only` | 403 | false |
| `PathRefused` | `path_refused` | 403 | false |
| `RegistryCorrupt` | `registry_corrupt` | 500 | false |
| `RegistryEntriesRejected` | `registry_entries_rejected` | 500 | false |
| `RegistryWriteFailed` | `registry_write_failed` | 500 | false |
| `PurgeFailed` | `purge_failed` | 500 | false |
| `StatusTimeout` | `status_timeout` | 504 | true |
| `ShuttingDown` | `shutting_down` | 503 | false |
| `AuthFailed` | `auth_failed` | **502** | false |
| `AuthMissing` | `auth_missing` | 502 | false |
| `AuthUnbound` | `auth_unbound` | 502 | false |
| `HostKeyMismatch` | `host_key_mismatch` | 502 | false |
| `CertificateInvalid` | `certificate_invalid` | 502 | false |
| `NetworkFailed` | `network_failed` | 502 | **true** |
| `Timeout` | `timeout` | 504 | **true** |
| `Canceled` | `canceled` | 503 | false |
| `NotFastForward` | `not_fast_forward` | 409 | **true** |
| `PushRejected` | `push_rejected` | 409 | **true** |
| `RemoteNotFound` | `remote_not_found` | 404 | false |
| `NoWorktree` | `no_worktree` | 409 | false |
| `AlreadyExists` | `already_exists` | 409 | false |
| `NotARepository` | `not_a_repository` | 409 | false |
| `BranchMismatch` | `branch_mismatch` | 409 | false |
| `DetachedHead` | `detached_head` | 409 | false |
| `UnbornBranch` | `unborn_branch` | 409 | false |
| `DirtyTree` | `dirty_tree` | 409 | false |
| `MergePathTypeConflict` | `merge_path_type_conflict` | 409 | false |
| `MergeUnresolvable` | `merge_unresolvable` | 409 | false |
| `RepoLocked` | `repo_locked` | 409 | **true** | *(two producers — see the † footnote in §3.4)* |
| `IoFailed` | `io_failed` | 500 | false | *(a failed TCP connect is class `Os` but maps to `NetworkFailed` — see the amendment in spec §5.6)* |
| `Internal` | `internal` | 500 | false |

**[CONTRACT DECISION]** `IoFailed.retryable() == false` (§5.6 says "maybe"; a caller that blindly
retries `EACCES` spins). **[CONTRACT DECISION]** `http_status()` is total — every execution code has
a status even though most can only reach HTTP through a rare synchronous path.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Git2Hint { pub class: String, pub code: String }   // e.g. {"class":"Reference","code":"NotFastForward"}

#[derive(Debug, Clone)]
pub struct GitError {
    pub code: GitErrorCode,
    pub message: String,
    pub repo_id: Option<String>,
    pub path: Option<String>,     // absolute, display form; `path_refused` only
    pub field: Option<String>,    // `invalid_request` only
    pub git2: Option<Git2Hint>,
}
```
`impl serde::Serialize for GitError` is **hand-written** and emits exactly the **job-error** shape:
```json
{"code":"…","message":"…","retryable":false,"git2":{"class":"…","code":"…"}}
```
(`git2` omitted when `None`; `repo_id`/`path`/`field` are *not* emitted here — `api::ApiError`
emits them into the HTTP envelope.) **[CONTRACT DECISION]** — the spec's two examples disagree on
which fields appear; splitting job-shape from envelope-shape is what makes both examples true.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortReason { None, Timeout, Shutdown }
impl AbortReason { pub fn as_u8(self) -> u8; pub fn from_u8(v: u8) -> AbortReason; }
```
**[CONTRACT DECISION]** — §5.6's `Code::from_abort_flag()` needs to distinguish `timeout` from
`canceled`; a bare `AtomicBool` cannot. `AbortReason` lives in `error.rs` (no deps); the atomic
wrapper `AbortFlag` lives in `jobs.rs`.

### 4.4 `src/git/util.rs` — task 3

```rust
pub type NowFn = std::sync::Arc<dyn Fn() -> u64 + Send + Sync>;
pub fn now_ms() -> u64;                       // epoch millis, saturating
pub fn system_now() -> NowFn;                 // Arc::new(now_ms)
pub fn utc_stamp(ms: u64) -> String;          // "2026-08-03 14:21:07 UTC"
pub fn b64_nopad(bytes: &[u8]) -> String;     // standard alphabet, '=' stripped, no dependency
#[cfg(unix)]    pub fn bytes_to_path(b: &[u8]) -> Result<std::path::PathBuf, GitError>;
#[cfg(windows)] pub fn bytes_to_path(b: &[u8]) -> Result<std::path::PathBuf, GitError>;
```
(The spec's Windows sketch has a typo'd return type `Result<GitError, GitError>`; the correct type
is `Result<PathBuf, GitError>` on both platforms.)

### 4.5 `src/git/registry.rs` — task 4

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoDef {
    pub id: String,
    #[serde(default)] pub remote: Option<String>,
    #[serde(default = "default_remote_name")] pub remote_name: String,
    #[serde(default)] pub branch: String,          // "" at parse time -> filled from [git].default_branch
    #[serde(default)] pub credential: Option<CredentialSpec>,
    #[serde(default)] pub author: Option<AuthorSpec>,
    #[serde(default)] pub sync_settings: bool,
    #[serde(default = "default_settings_path")] pub settings_path: String,
    #[serde(default)] pub auto_sync_secs: Option<u64>,
    #[serde(default)] pub sync_on_start: bool,
    #[serde(default)] pub sync_on_quit: bool,
    #[serde(default)] pub restart_children_on_pull: bool,
    #[serde(default)] pub created_at_ms: u64,      // 0 -> filled with now
    #[serde(default)] pub updated_at_ms: u64,      // 0 -> filled with now
}
fn default_remote_name() -> String { "origin".to_string() }
fn default_settings_path() -> String { "settings.json".to_string() }
```
**`RepoDef` has no `Serialize`** (it holds `Secret`). **Invariant every consumer may rely on:** a
`RepoDef` obtained from `Registry` always has a non-empty `branch`, a non-zero `created_at_ms`/
`updated_at_ms`, and a `credential` whose `bound_host` is `Some` whenever `remote` is `Some` — the
normalisation happens once, inside `Registry::load` / `Registry::put`. **[CONTRACT DECISION]**:
`branch: String` with an empty-string sentinel rather than `Option<String>`, so `ctx.def.branch` is
a plain `&str` at ~15 call sites in `ops.rs`/`merge.rs`.

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialSpec {
    None,
    Token {
        #[serde(default = "default_token_username")] username: String,
        token: Secret,
        #[serde(default)] bound_host: Option<String>,
    },
    SshKey {
        #[serde(default = "default_ssh_username")] username: String,
        #[serde(default)] private_key_path: Option<std::path::PathBuf>,
        #[serde(default)] private_key: Option<Secret>,
        #[serde(default)] public_key_path: Option<std::path::PathBuf>,
        #[serde(default)] public_key: Option<String>,
        #[serde(default)] passphrase: Option<Secret>,
        #[serde(default)] bound_host: Option<String>,
    },
}
fn default_token_username() -> String { "x-access-token".to_string() }
fn default_ssh_username() -> String { "git".to_string() }

impl CredentialSpec {
    pub fn bound_host(&self) -> Option<&str>;
    pub fn with_bound_host(self, host: Option<String>) -> Self;
    pub fn is_none(&self) -> bool;
}
```
**[CONTRACT DECISION]** `public_key: Option<String>` is added (spec §4.6's JSON lists only
`public_key_path`, but §8.1's `SshMem { public: Option<String> }` needs it and
`deny_unknown_fields` would otherwise reject it). Exactly one of `private_key_path` /
`private_key` must be `Some` — enforced in `validate_def` and in `creds::resolve`
(`invalid_request` otherwise).

*Implementation note for task 4:* if `#[serde(deny_unknown_fields)]` does not compile on the
internally-tagged enum, hand-write the check inside `validate_def` rather than dropping it.

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorSpec {
    #[serde(default)] pub name: Option<String>,
    #[serde(default)] pub email: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RegistryFile {
    pub version: u32,                            // always REGISTRY_VERSION
    pub repos: Vec<RepoDef>,
    pub rejected_raw: Vec<serde_json::Value>,    // written back verbatim on every save
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegistryError {
    pub code: &'static str,                      // "registry_corrupt" | "registry_entries_rejected"
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")] pub quarantined_to: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]   pub rejected: Vec<RejectedEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RejectedEntry {
    pub index: usize,
    pub id: Option<String>,                      // serialized as null when unreadable
    pub reason: &'static str,                    // §3.12
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Warning { pub code: &'static str, pub message: String }

#[derive(Debug, Clone)]
pub struct RegistryDefaults {
    pub default_branch: String,
    pub settings_enabled: bool,
    pub allow_http: bool,
    pub registry_writes: bool,
}

pub struct Registry { /* std::sync::RwLock<RegistryFile>, path, defaults, error */ }

#[derive(Debug)]
pub struct PutOutcome { pub created: bool, pub repo: RepoDef, pub warnings: Vec<Warning> }
```

```rust
#[derive(Debug, Clone, Serialize)]
pub struct RepoView {
    pub id: String,
    pub path: String,              // absolute
    pub exists: bool,
    pub remote: Option<String>,
    pub remote_name: String,
    pub branch: String,
    pub credential: Option<CredentialView>,
    pub has_credentials: bool,
    pub author: Option<AuthorSpec>,
    pub sync_settings: bool,
    pub settings_path: String,
    pub auto_sync_secs: Option<u64>,
    pub sync_on_start: bool,
    pub sync_on_quit: bool,
    pub restart_children_on_pull: bool,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CredentialView {
    None,
    Token { username: String, token_stored: bool, bound_host: Option<String> },
    SshKey {
        username: String,
        private_key_path: Option<String>,
        private_key_stored: bool,
        public_key_path: Option<String>,
        passphrase_stored: bool,
        bound_host: Option<String>,
    },
}
```
`CredentialView` has **no** secret-shaped field, by construction. `has_credentials` is
`!matches!(credential, None | Some(CredentialView::None))`.

### 4.6 `src/git/state.rs` — task 4

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StateFile {
    pub version: u32,                                        // STATE_VERSION
    pub repos: std::collections::BTreeMap<String, RepoState>,
}
impl Default for StateFile;   // version: STATE_VERSION, repos: empty

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepoState {
    #[serde(skip_serializing_if = "Option::is_none")] pub last_sync: Option<LastSync>,
    #[serde(skip_serializing_if = "Option::is_none")] pub ssh_host_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastSync {
    pub at_ms: u64,
    pub ok: bool,
    pub op: String,               // JobOp::as_str(); String keeps state.rs free of jobs.rs
    pub job_id: String,
    pub outcome: Option<String>,
    pub head: Option<String>,
    pub code: Option<String>,
    pub message: Option<String>,  // already scrubbed
}

pub struct StateStore { /* PathBuf + std::sync::Mutex<StateFile> */ }
```

### 4.7 `src/git/jobs.rs` — task 5

```rust
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct JobId(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobOp { Init, Clone, Pull, Push, Sync, Commit, Branch, Reset }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState { Running, Succeeded, Failed }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing, Cloning, Staging, Committing, Fetching, Merging, CheckingOut, Pushing, Finalizing,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub received_objects: u32,
    pub total_objects: u32,
    pub indexed_objects: u32,
    pub received_bytes: u64,
    pub percent: u8,
}

pub struct AbortFlag(std::sync::atomic::AtomicU8);   // AbortReason::as_u8

pub struct JobSlot {
    pub id: JobId,
    pub repo_id: String,
    pub op: JobOp,
    pub request_id: Option<String>,
    pub created_at_ms: u64,
    pub abort: std::sync::Arc<AbortFlag>,
    pub stalled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    live: std::sync::Mutex<JobLive>,                 // private
}
struct JobLive {                                     // private
    state: JobState, phase: Phase, progress: Option<Progress>,
    started_at_ms: u64, finished_at_ms: Option<u64>,
    result: Option<serde_json::Value>, error: Option<GitError>,
    last_progress_write_ms: u64,
}
```
**[CONTRACT DECISION]** `abort: Arc<AbortFlag>` replaces the spec's `Arc<AtomicBool>` — see §4.3.
`started_at_ms` is `u64`, initialised to `created_at_ms` at admission and overwritten by `begin()`,
so the field is never null in a serialized job.

```rust
#[derive(Debug, Clone, Serialize)]
pub struct JobView {                       // full body: `{"job": JobView}`
    pub id: JobId,
    pub host_instance: String,
    pub repo_id: String,
    pub op: JobOp,
    pub state: JobState,
    pub phase: Phase,
    pub stalled: bool,
    pub progress: Option<Progress>,
    pub request_id: Option<String>,
    pub created_at_ms: u64,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub result: Option<serde_json::Value>,
    pub error: Option<GitError>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobRef {                        // embedded in `error.job`, `status.busy_job`
    pub id: JobId,
    pub op: JobOp,
    pub state: JobState,
    pub stalled: bool,
    pub started_at_ms: u64,
    pub request_id: Option<String>,
}

pub struct JobStore { /* Mutex<Inner>, instance: String, seq: AtomicU64, now: NowFn, draining: AtomicBool */ }
struct Inner {
    jobs: BTreeMap<JobId, Arc<JobSlot>>,
    terminal: VecDeque<JobId>,             // oldest first
    busy: BTreeMap<String, BusyBy>,        // repo_id -> whoever holds it  <- THE LOCK
    by_request: BTreeMap<(String, String), JobId>,
}
enum BusyBy { Job(JobId), Maintenance }    // private; Maintenance = a PUT/DELETE in flight

pub enum Admission {
    Started(std::sync::Arc<JobSlot>, RepoLease),
    Replay(std::sync::Arc<JobSlot>),
    Busy(std::sync::Arc<JobSlot>),
}

pub struct RepoLease { /* Arc<JobStore>, repo_id: String, job: Arc<JobSlot> */ }
// No public constructor. Drop: finish_if_running(internal) -> remove busy -> push terminal -> evict.
// The busy removal is guarded on `Some(BusyBy::Job(id)) if *id == self.job.id` — clearing a
// different holder's entry, another job's or a RepoHold's, would break the exclusion.

#[must_use]
pub struct RepoHold { /* Arc<JobStore>, repo_id: String */ }
// No public constructor either; `JobStore::hold_repo` is the only source. Drop: remove the
// `BusyBy::Maintenance` entry, under the same ownership guard.

#[derive(Debug, Clone, Default)]
pub struct JobFilter { pub repo_id: Option<String>, pub state: Option<JobState>, pub limit: usize }
```

### 4.8 `src/git/creds.rs` — task 6

```rust
pub enum ResolvedCred {
    None,
    Token   { username: String, token: Secret },
    SshPath { username: String, public: Option<std::path::PathBuf>, private: std::path::PathBuf,
              pass: Option<Secret> },
    SshMem  { username: String, public: Option<String>, private: Secret, pass: Option<Secret> },
}

#[derive(Debug)]
pub struct CredResolution {
    pub cred: ResolvedCred,
    /// true when a stored credential existed but its `bound_host` did not match the remote.
    /// The credential is NOT transmitted; a later auth demand becomes `auth_unbound`.
    pub unbound: bool,
}

pub struct HostKeyChecker {
    pub policy: SshHostKeyPolicy,
    pub pinned: Option<String>,
    learned: std::cell::RefCell<Option<String>>,
}
```
`ResolvedCred` derives nothing (`Debug` is deliberately absent so it cannot be printed).

### 4.9 `src/git/ops.rs` — struct `OpCtx` and DTOs created by **task 6**

```rust
#[derive(Debug, Clone)]
pub struct Identity {
    pub app_name: String,
    pub identifier: String,
    pub hostname: String,          // gethostname, lossy; "localhost" on failure
}

#[derive(Clone)]
pub struct SettingsCtx {
    pub schema: crate::config::SettingsSection,
    pub settings_file: std::path::PathBuf,
}

#[derive(Debug, Clone)]
pub struct BranchRequest {
    pub name: String,
    pub create: bool,
    pub from: Option<String>,
    pub checkout: bool,
    /// `None` = leave upstream alone; `Some(None)` = unset; `Some(Some(x))` = set to `x`.
    pub upstream: Option<Option<String>>,
}

#[derive(Debug, Clone)]
pub struct ResetRequest { pub to: String, pub clean_untracked: bool, pub confirm: bool }

#[derive(Debug, Clone)]
pub struct OpRequest {
    pub request_id: Option<String>,
    pub credential: Option<CredentialSpec>,
    pub message: Option<String>,
    pub author: Option<AuthorSpec>,
    pub paths: Option<Vec<String>>,
    pub allow_empty: bool,
    pub push: bool,                        // sync only; DEFAULT TRUE
    pub force: bool,
    pub commit_local: bool,                // pull only
    pub restart_children: Option<bool>,    // None -> fall back to def.restart_children_on_pull
    pub branch: Option<BranchRequest>,
    pub reset: Option<ResetRequest>,
}
impl Default for OpRequest;                // hand-written: push = true, everything else false/None
impl OpRequest {
    pub fn auto() -> OpRequest;            // restart_children = Some(false), request_id = None
    pub fn manual() -> OpRequest;          // tray / quit sync: restart_children = Some(false)
}

pub struct OpCtx {
    pub tree: std::path::PathBuf,
    pub def: RepoDef,
    pub cred: ResolvedCred,
    pub cred_unbound: bool,
    pub request: OpRequest,
    pub identity: Identity,
    pub host_key: HostKeyChecker,
    pub abort: std::sync::Arc<AbortFlag>,
    pub slot: std::sync::Arc<JobSlot>,
    pub deadline: std::time::Instant,
    pub settings: Option<SettingsCtx>,
    pub default_author: AuthorSpec,        // resolved [git].author_name / author_email
    // private interior mutability, all `Cell`/`RefCell` — the ctx is moved into one
    // spawn_blocking closure and every callback captures `&'a OpCtx` by shared reference:
    cred_attempts: std::cell::Cell<u32>,
    cred_failure: std::cell::Cell<Option<GitErrorCode>>,
    push_rejections: std::cell::RefCell<Vec<(String, String)>>,
}
```
**[CONTRACT DECISION]** `host_fingerprint` from the spec's sketch is folded into
`host_key: HostKeyChecker` (constructed with the pinned value). **[CONTRACT DECISION]** the
`cb.credentials` closure uses `ctx.cred_attempt()` / `ctx.record_cred_failure()` rather than the
spec's `&'a Cell<u32>` capture — with interior mutability behind `&OpCtx`, no closure needs `&mut`
and the borrow dance disappears.

```rust
#[derive(Debug, Clone, Serialize)]
pub struct MergeReport {
    pub kind: &'static str,                   // §3.10
    pub merge_commit: Option<String>,
    pub conflicts_resolved: Vec<String>,
    pub recover_hint: Option<String>,         // "git show <oid>^2:<path>"
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingsRejected { pub error: String }

#[derive(Debug, Clone, Serialize)]
pub struct OpOutcome {
    pub outcome: &'static str,                // §3.5
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
    pub restart_requested: bool,
    pub warnings: Vec<Warning>,
    #[serde(skip)] pub settings_changed: bool,
    #[serde(skip)] pub learned_fingerprint: Option<String>,
}
impl OpOutcome { pub fn new(outcome: &'static str, branch: &str) -> OpOutcome; }
```
Every non-`skip` field is serialized unconditionally (nulls included) so the `result` object has a
fixed shape. `restart_requested` reflects what `after_job` decided, not what the caller asked.

### 4.10 `src/git/ops.rs` — read (non-job) types, **task 7**

```rust
pub struct ReadCtx {
    pub tree: std::path::PathBuf,
    pub def: RepoDef,
    pub last_sync: Option<LastSync>,
    pub busy_job: Option<JobRef>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DirtySummary {
    pub modified: Vec<String>, pub added: Vec<String>, pub deleted: Vec<String>,
    pub renamed: Vec<String>,  pub untracked: Vec<String>,
    pub count: usize, pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteInfo { pub name: String, pub url: Option<String> }

#[derive(Debug, Clone, Serialize)]
pub struct AutoInfo { pub auto_sync_secs: Option<u64>, pub sync_on_start: bool, pub sync_on_quit: bool }

#[derive(Debug, Clone, Serialize)]
pub struct RepoStatus {
    pub id: String,
    pub path: String,
    pub exists: bool,
    pub is_repository: bool,
    pub state: &'static str,          // §3.6
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
    pub busy_job: Option<JobRef>,
    pub last_sync: Option<LastSync>,
    #[serde(skip_serializing_if = "Option::is_none")] pub error: Option<GitError>,
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

### 4.11 `src/git/merge.rs` — task 8

```rust
pub struct MergeResult {
    pub merge_commit: git2::Oid,
    pub conflicts_resolved: Vec<String>,
    pub unrelated: bool,
}

pub enum MergeOutcome {
    UpToDate,
    NoRemoteBranch,
    AdoptedRemote { head: git2::Oid },
    FastForward { head: git2::Oid },
    Merged(MergeResult),
}
```
Neither type ever crosses `api.rs`; `ops.rs` converts `MergeOutcome` into `OpOutcome.merge:
Option<MergeReport>` plus `OpOutcome.outcome`.

### 4.12 `src/git/mod.rs` — task 9

```rust
pub struct GitService { /* see field list below */ }
```
Fields (all private): `cfg: GitSection`, `identity: Identity`,
`repos_root_canon: PathBuf`, `log_path: PathBuf`, `registry: Registry`, `state: StateStore`,
`jobs: Arc<JobStore>`, `rt: tokio::runtime::Handle`,
`events: tokio::sync::mpsc::UnboundedSender<HostEvent>`,
`log: std::sync::Mutex<Option<std::fs::File>>`,
`settings: Option<SettingsCtx>`, `ops: Arc<dyn GitOps>`,
`timers: std::sync::Mutex<std::collections::BTreeSet<String>>`,
`status: Arc<tokio::sync::RwLock<AppStatus>>`.

**[CONTRACT DECISION] — there is no `repos_dir` field.** `Registry` already owns the repos
directory and exposes it as `registry::Registry::repos_dir() -> &Path` (§5.4); a second,
independently-computed copy on `GitService` is exactly the drift task 4 forbids. `tree_path`,
`RepoView::new`, `delete_repo` and `service_info` all go through `self.registry.repos_dir()`.

**[CONTRACT DECISION] — `log` is `Mutex<Option<File>>`, not `Mutex<File>`.** `GitService::new` is
required to create nothing (§8 invariant 4), so the file is opened in `start()`; and a `git.log`
that cannot be opened must degrade to "no logging", never disable git or crash the host.
`App::chrome_log` already sets that precedent. `GitService::log(&self, line: &str)` is a no-op
while the slot is `None`.

**[CONTRACT DECISION] — `repos_root_canon` is computed by task 9's private `canonical_root`,
not by `std::fs::canonicalize`.** `repos/` does not exist on a first launch and `canonicalize`
fails on a missing path, while on macOS the data dir sits under a symlinked `/var` — a
non-canonical root would make every `contained_in` check answer `path_refused`. `canonical_root`
canonicalizes the deepest ancestor that does exist and re-appends the remainder.

**[CONTRACT DECISION]** `GitService` **does** hold a read-only `Arc<RwLock<AppStatus>>` — §9.1 says
it must not hold `status`, but §9.7's third restart condition ("the app status is `Ready`") is
unimplementable without it. It is read via `try_read()` and never written; the ban that stands is on
`Children`, `chrome_generation`, `server_generation`, `rt.enter()`, and `tokio::process`.

```rust
pub trait GitOps: Send + Sync {
    fn run(&self, op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError>;
}
pub struct RealOps;                 // impl GitOps { ops::run(op, ctx) }

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
pub struct ServiceDefaults { pub branch: String, pub author: AuthorView, pub network_timeout_secs: u64 }
#[derive(Debug, Clone, Serialize)]
pub struct AuthorView { pub name: String, pub email: String }     // non-null, unlike AuthorSpec
#[derive(Debug, Clone, Serialize)]
pub struct ServiceFeatures { pub tray_sync: bool, pub error_dialogs: bool, pub status_api: bool }

#[derive(Debug, Clone, Serialize)]
pub struct GitStatusSummary {
    pub registry_error: Option<RegistryError>,
    pub repos: Vec<RepoStatusSummary>,
}
#[derive(Debug, Clone, Serialize)]
pub struct RepoStatusSummary {
    pub id: String,
    pub path: String,
    pub auto_sync_secs: Option<u64>,
    pub busy_job: Option<JobRef>,
    pub last_sync: Option<LastSync>,     // reuses LastSync verbatim
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoWithStatus { pub repo: RepoView, pub status: RepoStatus }

#[derive(Debug, Clone, Serialize)]
pub struct DeleteOutcome { pub deleted: bool, pub purged: bool, pub path: String }

pub enum StartOutcome {
    Started(std::sync::Arc<JobSlot>),   // 202
    Replay(std::sync::Arc<JobSlot>),    // 200
    Busy(std::sync::Arc<JobSlot>),      // 409
}
```
**[CONTRACT DECISION]** `StartOutcome` exists so the `RepoLease` never escapes `start_job` — the
spec's `Ok(Admission::Started(slot, RepoLease::taken()))` would hand a second lease to the HTTP
layer. `Admission` (with the lease) stays between `jobs.rs` and `mod.rs`; `api.rs` only sees
`StartOutcome`. **[CONTRACT DECISION]** `RepoStatusSummary.last_sync` reuses `LastSync` rather than a
trimmed copy (the §9.6 example omits `head`/`message`; extra fields are additive and one type
cannot drift from another).

### 4.13 `src/git/api.rs` — task 9

Request DTOs — every one is `#[derive(Deserialize)] #[serde(deny_unknown_fields)]`, every one has
`#[serde(default)] pub request_id: Option<String>` unless noted.

```rust
pub struct RepoDefBody {              // no request_id, no id, no timestamps
    #[serde(default)] pub remote: Option<String>,
    #[serde(default = "default_remote_name")] pub remote_name: String,
    #[serde(default)] pub branch: Option<String>,
    #[serde(default)] pub credential: Option<CredentialSpec>,
    #[serde(default)] pub author: Option<AuthorSpec>,
    #[serde(default)] pub sync_settings: bool,
    #[serde(default = "default_settings_path")] pub settings_path: String,
    #[serde(default)] pub auto_sync_secs: Option<u64>,
    #[serde(default)] pub sync_on_start: bool,
    #[serde(default)] pub sync_on_quit: bool,
    #[serde(default)] pub restart_children_on_pull: bool,
}
impl RepoDefBody { pub fn into_def(self, id: String) -> RepoDef; }

pub struct InitBody   { pub request_id: Option<String> }
pub struct CloneBody  { pub request_id: Option<String>, pub credential: Option<CredentialSpec> }
pub struct PullBody   { pub request_id: Option<String>, pub credential: Option<CredentialSpec>,
                        #[serde(default)] pub commit_local: bool,
                        #[serde(default)] pub restart_children: Option<bool> }
pub struct PushBody   { pub request_id: Option<String>, pub credential: Option<CredentialSpec>,
                        #[serde(default)] pub force: bool }
pub struct SyncBody   { pub request_id: Option<String>, pub credential: Option<CredentialSpec>,
                        #[serde(default)] pub message: Option<String>,
                        #[serde(default)] pub author: Option<AuthorSpec>,
                        #[serde(default)] pub allow_empty: bool,
                        #[serde(default = "yes")] pub push: bool,
                        #[serde(default)] pub force: bool,
                        #[serde(default)] pub restart_children: Option<bool> }
pub struct CommitBody { pub request_id: Option<String>,
                        #[serde(default)] pub message: Option<String>,
                        #[serde(default)] pub author: Option<AuthorSpec>,
                        #[serde(default)] pub paths: Option<Vec<String>>,
                        #[serde(default)] pub allow_empty: bool }
pub struct BranchBody { pub request_id: Option<String>,
                        pub name: String,
                        #[serde(default)] pub create: bool,
                        #[serde(default)] pub from: Option<String>,
                        #[serde(default = "yes")] pub checkout: bool,
                        #[serde(default, deserialize_with = "de_double_option")]
                        pub upstream: Option<Option<String>> }
pub struct ResetBody  { pub request_id: Option<String>,
                        #[serde(default)] pub to: Option<String>,          // default "head"
                        #[serde(default)] pub clean_untracked: bool,
                        #[serde(default)] pub confirm: bool }

#[derive(Deserialize)] pub struct JobsQuery {
    #[serde(default)] pub repo_id: Option<String>,
    #[serde(default)] pub state: Option<String>,   // running|succeeded|failed
    #[serde(default)] pub limit: Option<usize>,
}
#[derive(Deserialize)] pub struct DeleteQuery { #[serde(default)] pub purge: bool }
fn yes() -> bool { true }
```

Response DTOs:
```rust
#[derive(Serialize)] pub struct ReposResponse  { pub repos: Vec<RepoView> }
#[derive(Serialize)] pub struct RepoResponse   { pub repo: RepoView, pub warnings: Vec<Warning> }
#[derive(Serialize)] pub struct JobResponse    { pub job: JobView }
#[derive(Serialize)] pub struct JobsResponse   { pub jobs: Vec<JobView> }
// GET /repos/:id and GET /repos/:id/status both return git::RepoWithStatus
// DELETE returns git::DeleteOutcome
// GET /api/git returns git::ServiceInfo

pub struct ApiError {
    pub err: GitError,
    pub host_instance: String,
    pub job: Option<JobRef>,          // Some ONLY for repo_busy
}
impl axum::response::IntoResponse for ApiError;
```
Envelope emitted by `ApiError` (field order fixed):
```json
{"error":{"code":"…","message":"…","repo_id":"…","path":"…","field":"…",
          "host_instance":"…","job":{…}}}
```
`repo_id`, `path`, `field`, `job` are omitted when `None`. `host_instance` is always present.
409 additionally sets `Retry-After: 1`.

**[CONTRACT DECISION]** `GET /repos/:id/status` returns `{"repo":…,"status":…}`, identical to
`GET /repos/:id` — §5.5's worked example beats §5.1's terse `RepoStatus` cell.

### 4.14 `src/internal_server.rs` — tasks 9 and 10

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    RestartRequested,
    GitRestartChildren { repo_id: String, reason: &'static str },
    GitFailed { repo_id: String, op: String, code: String, message: String },
    GitConflictsResolved { repo_id: String, merge_commit: String, paths: Vec<String> },
}
```
All three git variants land in **task 9** (`GitService::after_job` emits them); the `event_loop`
arms that consume them land in task 10. Adding variants does not break the existing `match` in
`run()` — it already ends with `_ => {}`.

```rust
pub struct HostState {
    pub app_name: String,
    pub token: String,
    pub status: Arc<RwLock<AppStatus>>,
    pub schema: Option<SettingsSection>,
    pub settings_file: PathBuf,
    pub events: mpsc::UnboundedSender<HostEvent>,
    pub git: Option<Arc<crate::git::GitService>>,      // NEW, task 9
}

#[derive(serde::Serialize)]
pub(crate) struct StatusResponse {                      // NEW, task 10
    #[serde(flatten)] pub app: AppStatus,
    #[serde(skip_serializing_if = "Option::is_none")] pub git: Option<GitStatusSummary>,
}
```

---

## 5. Function signatures

### 5.1 `config.rs` — task 2
```rust
impl AppConfig {
    pub fn git_enabled(&self) -> bool;                 // self.git.is_some()
}
pub fn validate_branch_name(name: &str) -> bool;
```
`validate_branch_name` rules (single source of truth for `[git].default_branch`,
`repos[].branch`, and `POST /branch`): non-empty; `<= MAX_BRANCH_NAME_LEN` bytes; every char in
`[A-Za-z0-9._/-]`; no `..`; no leading `-` or `/`; no trailing `/`; no `//`; not exactly `@`; no
ASCII control chars; and **per path component** — no component may begin with `.`, end with `.`,
or end with `.lock`.

> **Amended during task 2 execution.** The rule list previously said only "does not end in
> `.lock`", i.e. it tested the *whole name*. Measured against this repo's own libgit2 1.9.6:
> `git2::Reference::is_valid_name` refuses `.`, `.x`, `a.`, `x.lock.`, `a/.b`, `x/.`, `a.lock/b`
> and `a/b.lock/c`, every one of which the old list admitted. Because this function is the only
> admission gate for all three entry points, an admitted name failed *later*, inside a job, as a
> generic libgit2 ref error rather than the admission-time `invalid_request` the API promises.
> The one-directional invariant — everything we accept, libgit2 accepts — is now machine-checked
> by `git::tests::validate_branch_name_never_admits_a_name_libgit2_refuses`. The reverse stays
> deliberately false: we reject a leading `-`, `@` and `~` that libgit2 allows.

`AppConfig::validate` gains a `[git]` arm producing these exact strings:
| rule | string |
|---|---|
| bad `default_branch` | `git.default_branch "{v}" is not a valid branch name` |
| `author_name` has `<`, `>`, `\n` | `git.author_name must not contain '<', '>' or newlines (git signatures cannot represent them)` |
| `author_name` non-empty but blank, or bearing an ASCII control char | `git.author_name must not be blank or contain control characters (git signatures cannot represent them)` |
| bad `author_email` | `git.author_email "{v}" must look like a plain address, e.g. app@example.com` |
| `network_timeout_secs` ∉ `5..=3600` | `git.network_timeout_secs must be between 5 and 3600 (got {v})` |
| `quit_sync_timeout_secs > 120` | `git.quit_sync_timeout_secs must be 120 or less (0 disables sync_on_quit)` |

The `author_email` rule is: non-empty ⇒ exactly the shape `<non-empty>@<non-empty>`, no `<`/`>`,
no whitespace, no ASCII control characters. **Amended during task 2 execution** — the old rule was
a bare `contains('@')`, which admitted `@`, `@e.com`, `a@` and `a\0@e.com`. Measured:
`Signature::now("App", "@")` returns `Ok("App <@>")`, a commit ident attributable to nobody, and an
embedded NUL fails with `data contained a nul byte`. `author_name = " "` was likewise admitted and
fails `Signature::now` with `Signature cannot have an empty name or email` — note `""` is *not*
affected, it remains the documented sentinel meaning "fall back to `[app].name`".

### 5.2 `git/secret.rs` — task 3
See §4.2. No free functions.

### 5.3 `git/error.rs` — task 3
```rust
impl GitErrorCode {
    pub fn as_str(self) -> &'static str;
    pub fn http_status(self) -> axum::http::StatusCode;
    pub fn retryable(self) -> bool;
}
impl GitError {
    pub fn new(code: GitErrorCode, message: impl Into<String>) -> GitError;
    pub fn with_repo(self, repo_id: &str) -> GitError;
    pub fn with_path(self, path: &std::path::Path) -> GitError;
    pub fn with_field(self, field: &str) -> GitError;
    pub fn code(&self) -> GitErrorCode;
    pub fn http_status(&self) -> axum::http::StatusCode;
    pub fn retryable(&self) -> bool;
    pub fn scrubbed(self, secrets: &[&str]) -> GitError;      // scrubs `message` in place
    pub fn as_dirty_tree_if_conflict(self) -> GitError;

    // named constructors used across task boundaries
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
    pub fn checkout_would_overwrite(paths: &[String]) -> GitError;          // DirtyTree; F6
    pub fn repo_state_not_clean(state: git2::RepositoryState,
                                repo_id: &str) -> GitError;                 // DirtyTree; F7
    pub fn merge_path_type_conflict(path: &std::path::Path) -> GitError;
    pub fn merge_unresolvable() -> GitError;
    pub fn push_rejected(refname: &str, status: &str) -> GitError;
    pub fn io(message: impl Into<String>) -> GitError;
}
impl From<git2::Error> for GitError;        // classify(e, AbortReason::None) + Git2Hint
impl From<std::io::Error> for GitError;     // IoFailed
impl From<serde_json::Error> for GitError;  // Internal
impl std::fmt::Display for GitError;
impl std::error::Error for GitError;

pub fn classify(e: &git2::Error, abort: AbortReason) -> GitErrorCode;
pub fn scrub(message: &str, secrets: &[&str]) -> String;
```
**[CONTRACT DECISION] — there are no `auth_missing`, `auth_unbound`, `canceled` or `timeout`
constructors.** They have zero call sites across tasks 4–12 and would survive only on
`#![allow(dead_code)]`. `AuthMissing`/`AuthUnbound` are produced by task 6's
`OpCtx::record_cred_failure` + `auth_missing_message` and surfaced through `OpCtx::classify_err`
(§5.8); `Canceled`/`Timeout` are produced by `error::classify` from the abort reason. The *codes*
`AuthMissing`, `AuthUnbound`, `Canceled` and `Timeout` all remain in §3.4/§4.3 — only the four
never-called named constructors are deleted.

**Two `GitError`s are deliberately constructed at their call sites rather than here**, both added by
the post-execution audit and both reusing an existing code, so neither widens this vocabulary:
`jobs::repo_locked(repo_id)` (`RepoLocked`, §3.4's † footnote) and the write-latch refusal built
inline in `registry::ensure_writable` (`RegistryReadOnly`, whose existing `registry_read_only()`
message — "`[git].registry_writes` is false" — would be an outright lie on that path). Both belong
beside `repo_busy` and `registry_read_only` in `error.rs`; they are elsewhere only because the file
sets of the three fix groups had to stay disjoint. **[CONTRACT DECISION]** the audit added **no**
`GitErrorCode` variant, renamed none, and left `as_str`, `http_status`, `retryable` and `classify`
byte-identical.

`classify`'s `(_, ErrorClass::Callback)` arm: `AbortReason::Timeout => Timeout`,
`AbortReason::Shutdown => Canceled`, `AbortReason::None => NetworkFailed`.
`scrub`: strip `scheme://user:pw@` → `scheme://`, then literal-replace every secret with more than
6 chars by `***`. **[CONTRACT DECISION]** — `scrub` takes `&[&str]`, not `&ResolvedCred` as §8.5
sketches, so `error.rs` stays free of `creds.rs`; `ResolvedCred::secrets()` supplies the slice.

### 5.4 `git/registry.rs` — task 4
```rust
pub fn valid_id(id: &str) -> bool;
pub fn validate_id(id: &str) -> Result<(), GitError>;
pub fn remote_host(remote: &str) -> Option<String>;                 // lowercased, scp-form aware
pub fn validate_remote(remote: &str, allow_http: bool) -> Result<(), GitError>;
pub fn validate_settings_path(path: &str) -> Result<(), GitError>;
pub fn validate_def(def: &RepoDef, d: &RegistryDefaults) -> Result<(), GitError>;

impl RepoView { pub fn new(def: &RepoDef, repos_dir: &std::path::Path) -> RepoView; }
impl CredentialView { pub fn of(spec: &CredentialSpec) -> CredentialView; }

impl Registry {
    /// Never fails, but does not always leave itself writable. Bytes it HAS and cannot use are
    /// quarantined and an empty, writable registry is returned; a file it could not READ, or a
    /// quarantine whose rename failed, is left exactly where it is and latches writes shut.
    pub fn load(path: &std::path::Path, repos_dir: &std::path::Path, defaults: RegistryDefaults) -> Registry;
    pub fn path(&self) -> &std::path::Path;
    /// The one copy of the repos directory. `GitService` holds no second copy — see §4.12.
    pub fn repos_dir(&self) -> &std::path::Path;
    /// `defaults.registry_writes && write_block().is_none()` — the config key is not the only
    /// thing that can turn this off, so `GET /api/git`'s `registry_writable` is not a mirror of it.
    pub fn writable(&self) -> bool;
    /// The gate every mutator goes through; the latch is reported before the config key.
    pub fn ensure_writable(&self) -> Result<(), GitError>;                  // registry_read_only
    /// Scrubbed at construction (`Registry::assembled`), so the guarantee is the type's and not
    /// the caller's: `message` and every `rejected[].id` have had URL userinfo stripped.
    pub fn error(&self) -> Option<RegistryError>;
    /// Non-fatal observations from `load`, for `git.log`. Scrubbed at construction for the same
    /// reason: the caller writes these into a file created under the ordinary umask.
    pub fn notes(&self) -> &[String];
    pub fn count(&self) -> usize;
    pub fn ids(&self) -> Vec<String>;
    pub fn list(&self) -> Vec<RepoDef>;
    pub fn snapshot(&self, id: &str) -> Result<RepoDef, GitError>;    // repo_not_found
    pub fn contains(&self, id: &str) -> bool;
    pub fn auto_sync_secs(&self, id: &str) -> Option<u64>;
    pub fn put(&self, def: RepoDef) -> Result<PutOutcome, GitError>;  // registry_read_only / *_write_failed
    pub fn remove(&self, id: &str) -> Result<RepoDef, GitError>;
}
pub(crate) fn save(path: &std::path::Path, file: &RegistryFile) -> Result<(), GitError>;
pub(crate) fn open_private(path: &std::path::Path) -> std::io::Result<std::fs::File>;
```
`Registry::put` is responsible for: `ensure_writable`, `validate_def`, branch/timestamp
normalisation, `bound_host` population from `remote_host(remote)`, dropping a stored credential
whose `bound_host` no longer matches (emitting `Warning{code:"auth_unbound"}`), clamping
`auto_sync_secs` into `AUTO_SYNC_MIN_SECS..=AUTO_SYNC_MAX_SECS` (emitting
`Warning{code:"auto_sync_clamped"}`; `0` → `None`, not clamped), and the atomic 0600 `save`.

> **Amended by the post-execution audit (F2, F3, F4).** Three corrections to this block, all of
> them things the old wording made it possible to get wrong:
> - "no longer matches" is a plain equality over `Option<String>`, **`None` included**. An absent
>   `bound_host` is not a wildcard: `normalise_def` binds every credential it stores, so an unbound
>   one can only have been written next to a remote with no host at all, and it is keepable only
>   while that stays true. Carrying it onto a remote that *does* have a host would hand it to
>   `normalise_def` three lines later, which binds it to that host — the theft, with the rebinding
>   done for the attacker. `kind: "none"` short-circuits, since its `bound_host` is always `None`.
> - `writable()` is derived, and `put`/`remove` go through `ensure_writable` rather than reading
>   `defaults.registry_writes`, so a caller that reaches `Registry` without going through
>   `GitService` cannot write a registry that latched shut at load.
> - `notes()` and `error()` are scrubbed at the funnel (`Registry::assembled`), not by their
>   callers. `PutOutcome.warnings` is deliberately **not** scrubbed: those messages carry hosts
>   only, never a credential, and widening the guarantee speculatively would hide the host names
>   the `auth_unbound` warning exists to show.

### 5.5 `git/state.rs` — task 4
```rust
impl StateStore {
    pub fn load(path: &std::path::Path) -> StateStore;         // never fails; corrupt -> empty + log
    pub fn path(&self) -> &std::path::Path;
    pub fn fingerprint(&self, repo_id: &str) -> Option<String>;
    pub fn record_fingerprint(&self, repo_id: &str, fingerprint: &str);
    pub fn last_sync(&self, repo_id: &str) -> Option<LastSync>;
    pub fn record_last_sync(&self, repo_id: &str, last: LastSync);
    pub fn forget(&self, repo_id: &str);
    pub fn snapshot(&self) -> StateFile;
}
```
Every mutator persists atomically (0600 temp + rename) and is **best-effort**: a write failure is
swallowed, never propagated.

### 5.6 `git/jobs.rs` — task 5
```rust
impl JobId {
    pub fn new(host_instance: &str, seq: u64) -> JobId;
    pub fn as_str(&self) -> &str;
    pub fn instance(&self) -> Option<&str>;      // parses the middle field
}
impl std::fmt::Display for JobId;

impl JobOp    { pub fn as_str(self) -> &'static str; pub fn is_mutating(self) -> bool; }
impl JobState { pub fn as_str(self) -> &'static str; pub fn parse(s: &str) -> Option<JobState>; }
impl Phase    { pub fn as_str(self) -> &'static str; }

impl AbortFlag {
    pub fn new() -> AbortFlag;
    pub fn abort(&self, reason: AbortReason);
    pub fn is_aborted(&self) -> bool;
    pub fn reason(&self) -> AbortReason;
}

impl JobSlot {
    pub fn begin(&self);
    pub fn set_phase(&self, phase: Phase);
    pub fn phase(&self) -> Phase;
    /// Writes only when PROGRESS_THROTTLE_MS elapsed or `percent` changed. Returns whether it wrote.
    pub fn set_progress(&self, progress: Progress) -> bool;
    pub fn succeed(&self, result: &serde_json::Value);
    pub fn fail(&self, error: &GitError);
    pub fn finish_if_running(&self, error: GitError);
    pub fn state(&self) -> JobState;
    pub fn is_terminal(&self) -> bool;
    pub fn started_at_ms(&self) -> u64;
    pub fn finished_at_ms(&self) -> Option<u64>;
    pub fn mark_stalled(&self);
    pub fn view(&self, host_instance: &str) -> JobView;
    pub fn as_ref_view(&self) -> JobRef;
}

impl JobStore {
    pub fn new(host_instance: String) -> std::sync::Arc<JobStore>;
    pub fn with_clock(host_instance: String, now: NowFn) -> std::sync::Arc<JobStore>;
    pub fn host_instance(&self) -> &str;
    pub fn admit(self: &std::sync::Arc<Self>, repo_id: &str, op: JobOp, request_id: Option<&str>)
        -> Result<Admission, GitError>;                     // Err = shutting_down | repo_locked
    /// Take the repo for the host itself, for the length of a `PUT`/`DELETE`.
    pub fn hold_repo(self: &std::sync::Arc<Self>, repo_id: &str)
        -> Result<RepoHold, GitError>;                      // Err = repo_busy | repo_locked
    pub fn get(&self, id: &JobId) -> Option<std::sync::Arc<JobSlot>>;
    pub fn lookup(&self, raw: &str) -> Result<std::sync::Arc<JobSlot>, GitError>;   // job_not_found
    pub fn list(&self, filter: &JobFilter) -> Vec<std::sync::Arc<JobSlot>>;         // newest first, GCs first
    /// The `BusyBy::Job` arm only. A `RepoHold` answers `None` — a hold is not a job and
    /// `status.busy_job`/`error.job` have nothing to point at — so **nothing may use this to
    /// decide whether a repo is free**. `hold_repo` is how you take it.
    pub fn busy(&self, repo_id: &str) -> Option<std::sync::Arc<JobSlot>>;
    pub fn live_jobs(&self) -> Vec<std::sync::Arc<JobSlot>>;
    pub fn set_draining(&self, draining: bool);
    pub fn draining(&self) -> bool;
    pub fn abort_all(&self, reason: AbortReason);
    pub fn forget_repo(&self, repo_id: &str);
}
pub fn random_host_instance() -> String;      // 8 lowercase hex chars, `rand::rng()`
```
**[CONTRACT DECISION]** `admit` returns `Result` (spec §6.2 shows a bare `Admission`) because §6.6
requires a `shutting_down` outcome and the draining check must sit inside the same lock as the
busy check. `admit` takes `&Arc<Self>` because `RepoLease` holds `Arc<JobStore>`.

Admission order inside one lock: GC expired terminal jobs → **replay check** (running/succeeded →
`Replay`; failed → consume the index entry and fall through) → busy check → insert/mark busy/index.

> **Amended by the post-execution audit (F1, F8).** Two changes to this surface:
> - The busy check now has two arms. `BusyBy::Job` still yields `Admission::Busy(slot)`;
>   `BusyBy::Maintenance` yields `Err(repo_locked)`, an `Err` and not an `Admission` because there
>   is no job to hand back. Replay still runs first, so a matching `request_id` replays past a
>   hold — a replay hands back an existing record and touches no tree, which is what keeps §3.4's
>   "a matching `request_id` can never come back as 409" true now that `put_repo`/`delete_repo`
>   hold the repo for their whole call.
> - Dropping a job's record removes its `by_request` entry **only while the index still names that
>   job** (spec §6.5 rule 5). The same ownership test `RepoLease::drop` applies to `busy`.

### 5.7 `git/creds.rs` — task 6
```rust
pub fn resolve(per_call: Option<&CredentialSpec>,
               stored: Option<&CredentialSpec>,
               remote: Option<&str>) -> Result<CredResolution, GitError>;
pub fn callbacks<'a>(ctx: &'a OpCtx) -> git2::RemoteCallbacks<'a>;
pub fn fingerprint_sha256(raw: &[u8; 32]) -> String;         // "SHA256:…"

impl ResolvedCred {
    pub fn username(&self) -> Option<&str>;
    pub fn is_none(&self) -> bool;
    pub fn secrets(&self) -> Vec<&str>;     // via Secret::expose_for_scrub, for error::scrub
}
impl HostKeyChecker {
    pub fn new(policy: SshHostKeyPolicy, pinned: Option<String>) -> HostKeyChecker;
    pub fn check(&self, cert: &git2::Cert<'_>, host: &str)
        -> Result<git2::CertificateCheckStatus, git2::Error>;
    pub fn learned(&self) -> Option<String>;
}
```
`resolve` precedence: per-call (wholly replaces) → stored (only if `bound_host` matches
`remote_host(remote)`; otherwise `ResolvedCred::None` with `unbound: true`) → `None`.
`Err` is only ever `invalid_request` for an `ssh_key` spec with neither or both key sources.
**Forbidden in this file, forever:** `Cred::default`, `Cred::ssh_key_from_agent`,
`Cred::credential_helper`. A source-grep unit test enforces it.

### 5.8 `git/ops.rs` — tasks 6/7/8/11
```rust
// task 6
impl OpCtx {
    pub fn phase(&self, p: Phase);
    pub fn aborted(&self) -> bool;
    pub fn transfer(&self, p: &git2::Progress<'_>) -> bool;     // false => abort
    pub fn author_name(&self) -> String;
    pub fn author_email(&self) -> String;
    pub fn cred_attempt(&self) -> u32;                          // increments, returns the new count
    pub fn record_cred_failure(&self, code: GitErrorCode);
    pub fn auth_missing_message(&self, allowed: git2::CredentialType) -> String;
    pub fn record_push_status(&self, refname: &str, status: Option<&str>);
    pub fn take_push_rejections(&self) -> Result<(), GitError>;
    pub fn classify_err(&self, e: &git2::Error) -> GitError;
    pub fn scrub_err(&self, e: GitError) -> GitError;
    pub fn learned_fingerprint(&self) -> Option<String>;
}
```
`classify_err` = (1) a recorded cred failure wins (`auth_missing` / `auth_unbound` / `auth_failed`);
(2) otherwise `error::classify(e, ctx.abort.reason())`; always attaches `Git2Hint` and scrubs.
**[CONTRACT DECISION]** — this is how §5.6's "(GenericError, Callback) is ambiguous" and
"`auth_missing` vs `auth_unbound` come from *our* callback" are both resolved without inventing new
git2 error codes.

```rust
// task 7 — verbs
pub fn run(op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError>;
pub fn init(ctx: &OpCtx)   -> Result<OpOutcome, GitError>;
pub fn clone(ctx: &OpCtx)  -> Result<OpOutcome, GitError>;
pub fn commit(ctx: &OpCtx) -> Result<OpOutcome, GitError>;
pub fn branch(ctx: &OpCtx) -> Result<OpOutcome, GitError>;
pub fn reset(ctx: &OpCtx)  -> Result<OpOutcome, GitError>;
// task 7 — shared preamble + reads
/// Gated: refuses a `repo.state() != Clean` with 409 `dirty_tree`. Every verb that touches an
/// existing tree comes through here, so a verb added later is gated without being told.
pub fn open_tree(ctx: &OpCtx) -> Result<git2::Repository, GitError>;
/// The same open with no state gate. **Only `reset` may call this** — it is the verb that
/// repairs a wedged tree, because `git_reset` ends in `git_repository_state_cleanup`.
pub fn open_tree_any_state(ctx: &OpCtx) -> Result<git2::Repository, GitError>;
pub fn require_branch(repo: &git2::Repository, branch: &str) -> Result<(), GitError>;
pub fn ensure_branch(repo: &git2::Repository, def: &RepoDef) -> Result<(), GitError>;
pub fn stage_and_commit(repo: &git2::Repository, ctx: &OpCtx)
    -> Result<(Option<git2::Oid>, usize), GitError>;            // (commit oid, files staged)
pub fn status(ctx: &ReadCtx) -> RepoStatus;                     // never fails
pub fn branches(tree: &std::path::Path, def: &RepoDef) -> Result<BranchesResponse, GitError>;
pub fn purge_tree(repos_root_canon: &std::path::Path, tree: &std::path::Path) -> Result<(), GitError>;
// task 8
pub fn pull(ctx: &OpCtx) -> Result<OpOutcome, GitError>;
pub fn push(ctx: &OpCtx) -> Result<OpOutcome, GitError>;
pub fn sync(ctx: &OpCtx) -> Result<OpOutcome, GitError>;
pub fn fetch(repo: &git2::Repository, ctx: &OpCtx) -> Result<Option<git2::Oid>, GitError>;
pub fn push_branch(repo: &git2::Repository, ctx: &OpCtx, out: &mut OpOutcome) -> Result<(), GitError>;
// task 11
pub fn settings_copy_in(ctx: &OpCtx) -> Result<Option<u64>, GitError>;   // hash of the bytes written
pub fn settings_apply_back(ctx: &OpCtx, before_hash: Option<u64>, out: &mut OpOutcome)
    -> Result<(), GitError>;
```
`run` dispatches: `Init→init`, `Clone→clone`, `Pull→pull`, `Push→push`, `Sync→sync`,
`Commit→commit`, `Branch→branch`, `Reset→reset`. Task 6 lands `run` with every arm returning
`Err(GitError::internal("not implemented"))`; tasks 7/8 replace them.

### 5.9 `git/merge.rs` — task 8
```rust
pub fn merge_prefer_local(repo: &git2::Repository, their_oid: git2::Oid, ctx: &OpCtx)
    -> Result<Option<MergeResult>, GitError>;
pub fn analyse(repo: &git2::Repository, ctx: &OpCtx) -> Result<MergeOutcome, GitError>;
pub fn conflict_path_bytes(c: &git2::IndexConflict) -> Result<Vec<u8>, GitError>;
```
`Ok(None)` from `merge_prefer_local` = "nothing to do" (tree unchanged **and**
`graph_descendant_of(ours, theirs)`).

### 5.10 `git/mod.rs` — tasks 7 and 9
```rust
// task 7 — landed with CONNECT_TIMEOUT_MS (§2); task 9 only calls it, from GitService::start.
pub fn init_libgit2_timeouts(network_timeout_secs: u64) -> Result<(), GitError>;   // §7.1, once
// task 9
pub fn contained_in(root_canon: &std::path::Path, candidate: &std::path::Path)
    -> Result<std::path::PathBuf, GitError>;                                        // path_refused

impl GitService {
    /// Returns None when `cfg.git` is absent. Creates NO directory and runs NO libgit2 code.
    pub fn new(cfg: &AppConfig,
               paths: &RuntimePaths,
               rt: tokio::runtime::Handle,
               events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
               status: std::sync::Arc<tokio::sync::RwLock<AppStatus>>)
        -> Option<std::sync::Arc<GitService>>;
    #[cfg(test)]
    pub fn with_ops(/* same args */, ops: std::sync::Arc<dyn GitOps>) -> std::sync::Arc<GitService>;

    /// mkdir repos_dir, libgit2 timeouts, index.lock recovery, auto-sync timers, startup log line.
    pub fn start(self: &std::sync::Arc<Self>);
    pub fn start_job(self: &std::sync::Arc<Self>, repo_id: &str, op: JobOp, req: OpRequest)
        -> Result<StartOutcome, GitError>;
    pub fn sync_all_manual(self: &std::sync::Arc<Self>);
    pub fn sync_on_start(self: &std::sync::Arc<Self>);
    pub fn run_quit_syncs(self: &std::sync::Arc<Self>, timeout: std::time::Duration);
    pub async fn read_status(&self, repo_id: &str) -> Result<RepoWithStatus, GitError>;
    pub async fn read_branches(&self, repo_id: &str) -> Result<BranchesResponse, GitError>;
    /// `validate_id` -> `registry.ensure_writable` -> `jobs.hold_repo` -> `registry.put`
    /// -> `spawn_auto_timer`. Writability before the hold, so a read-only host answers 403
    /// rather than taking a lock it is about to refuse to use.
    pub fn put_repo(self: &std::sync::Arc<Self>, id: &str, body: RepoDef)
        -> Result<PutOutcome, GitError>;
    /// Same preamble, then: containment check -> `registry.remove` -> `state.forget` ->
    /// `jobs.forget_repo` -> `purge_tree`. The two forgets sit under `remove` and not under
    /// the purge: they belong to the definition, so a refused purge must not leave a
    /// `last_sync` row or a job record for the next `PUT` of that id to inherit.
    pub fn delete_repo(&self, id: &str, purge: bool) -> Result<DeleteOutcome, GitError>;
    pub fn repo_view(&self, id: &str) -> Result<RepoView, GitError>;
    pub fn list_views(&self) -> Vec<RepoView>;
    pub fn service_info(&self) -> ServiceInfo;
    pub fn status_summary(&self) -> GitStatusSummary;
    pub fn host_instance(&self) -> &str;
    pub fn jobs(&self) -> &std::sync::Arc<JobStore>;
    pub fn registry(&self) -> &Registry;
    pub fn repo_count(&self) -> usize;
    pub fn tray_sync(&self) -> bool;
    pub fn error_dialogs(&self) -> bool;
    pub fn status_api(&self) -> bool;
    pub fn registry_writes(&self) -> bool;
    pub fn draining(&self) -> bool;
    pub fn auto_sync_secs(&self, id: &str) -> Option<u64>;
    pub fn tree_path(&self, id: &str) -> std::path::PathBuf;
    pub fn log_path(&self) -> &std::path::Path;
    pub fn log(&self, line: &str);
}
```
`read_status`/`read_branches` are `async`: `spawn_blocking` bounded by `STATUS_READ_TIMEOUT_MS`,
`status_timeout` on expiry, **no per-repo lock taken**.
Private: `after_job`, `spawn_watchdog`, `spawn_auto_timer`, `recover_index_locks`, `log_job`.

### 5.11 `git/api.rs` — task 9
```rust
pub fn router(state: HostState) -> axum::Router<HostState>;   // the 17 routes + the auth layer
pub fn disabled_router() -> axum::Router<HostState>;          // one fallback answering git_disabled
```
Both are nested at `/api/git` by `internal_server::router`. Handlers are private `async fn`s;
the only names crossing the task boundary are `router`, `disabled_router`, the DTOs in §4.13, and
`ApiError`.

**[CONTRACT DECISION] — `router` takes a `HostState` by value.** `axum::middleware::from_fn_with_state(state, f)`
needs a state *value*, and the no-value `from_fn` form does not support the `State` extractor, so a
no-argument constructor cannot carry the single auth layer §5 requires. `internal_server::router`
already owns a `HostState` at the nest site and passes a clone (§6.4); the return type is still
`Router<HostState>` and the handlers are still resolved by the outer `.with_state(state)`.
Verified against axum 0.8.9.

Route table (path shapes are axum 0.8 syntax):
```
GET    /                       -> ServiceInfo
GET    /repos                  -> ReposResponse
PUT    /repos/{id}             -> 201|200 RepoResponse
GET    /repos/{id}             -> RepoWithStatus
DELETE /repos/{id}?purge=      -> DeleteOutcome
GET    /repos/{id}/status      -> RepoWithStatus
GET    /repos/{id}/branches    -> BranchesResponse
POST   /repos/{id}/init        -> 202|200 JobResponse
POST   /repos/{id}/clone       -> 202|200 JobResponse
POST   /repos/{id}/pull        -> 202|200 JobResponse
POST   /repos/{id}/push        -> 202|200 JobResponse
POST   /repos/{id}/sync        -> 202|200 JobResponse
POST   /repos/{id}/commit      -> 202|200 JobResponse
POST   /repos/{id}/branch      -> 202|200 JobResponse
POST   /repos/{id}/reset       -> 202|200 JobResponse
GET    /jobs?repo_id=&state=&limit=  -> JobsResponse
GET    /jobs/{job_id}          -> JobResponse
```
**[CONTRACT DECISION] — the disabled side is a nested fallback, not a wildcard route.**
`disabled_router()` is `axum::Router::new().fallback(git_disabled)` and is mounted with
`.nest("/api/git", disabled_router())`. It answers **404 `git_disabled`** for `/api/git` itself,
for every sub-path, and for every method. The wildcard alternative
(`.route("/api/git/{*rest}", any(git_disabled))`) was measured against axum 0.8 and rejected: it
leaves `GET /api/git` — the very first row of the route table above — answering a bare 404 with an
empty body, which is indistinguishable from a typo'd path.

**Accepted edge:** in every shape tried, `GET /api/git/` (with a trailing slash) returns a bare 404
with an empty body rather than `git_disabled`. This is axum's normalisation of the nested empty
path and is deliberately not worked around; no client constructs that URL.

---

## 6. Signature changes to existing functions

### 6.1 `supervisor::build_env` — task 10
```rust
// BEFORE
pub fn build_env(cfg_env: &BTreeMap<String, String>,
                 settings_env: &[(String, String)],
                 settings_file: &Path) -> Vec<(String, String)>
// AFTER
pub struct HostAccess<'a> { pub url: &'a str, pub token: &'a str, pub git_enabled: bool }

pub fn build_env(cfg_env: &BTreeMap<String, String>,
                 settings_env: &[(String, String)],
                 settings_file: &Path,
                 host: &HostAccess<'_>) -> Vec<(String, String)>
```
Appends `APP_HOST_URL`, `APP_HOST_TOKEN`, and — only when `host.git_enabled` — `APP_GIT_ENABLED="1"`.
**Call sites to update:**
1. `src/host.rs` — `App::start_children` (the line that is `main.rs:256` before task 0).
2. `src/supervisor.rs` — test `build_env_layers_and_settings_file`.

### 6.2 `tray::menu_model` — task 10
```rust
// BEFORE
pub fn menu_model(menu: &MenuSection, app_name: &str,
                  settings_enabled: bool, show_open: bool) -> Vec<Entry>
// AFTER
pub fn menu_model(menu: &MenuSection, app_name: &str,
                  settings_enabled: bool, show_open: bool, show_sync: bool) -> Vec<Entry>
```
`TrayAction` gains `GitSync`. When `show_sync`, `Entry::Action(TrayAction::GitSync, "Sync now")` is
pushed **immediately before** `"Restart App"`. Full order:
`Open <app>?` → config items → `Settings…`? → `Sync now`? → `Restart App` → separator → `Quit`.
**Call sites to update:**
1. `src/host.rs` — `App::refresh_tray` (its own signature is unchanged; it computes
   `show_sync = self.cfg.git.as_ref().is_some_and(|g| g.tray_sync) && self.git.as_ref().is_some_and(|g| g.repo_count() > 0)`).
2. `src/tray.rs` — tests `full_model_order` (add `true`) and `minimal_model_hides_open_and_settings`
   (add `false`).

### 6.3 `internal_server::authorized` — task 9
```rust
// BEFORE
fn authorized(headers: &HeaderMap, state: &HostState) -> bool
// AFTER
pub(crate) fn authorized(headers: &HeaderMap, state: &HostState) -> bool
```
New caller: `git::api`'s `from_fn_with_state` auth layer.

### 6.4 `internal_server::router` — task 9
Gains, before `.with_state(state)`:
```rust
let router = match &state.git {
    Some(_) => router.nest("/api/git", crate::git::api::router(state.clone())),
    None => router.nest("/api/git", crate::git::api::disabled_router()),
};
```
The match is on `&state.git` so `state` is still owned for the `state.clone()` in the arm and for
the trailing `.with_state(state)`. Which of the two routers is nested is the on/off switch for the
whole feature.

### 6.5 `internal_server::api_status` — task 10
```rust
// BEFORE
async fn api_status(State(s): State<HostState>) -> Json<AppStatus>
// AFTER
async fn api_status(State(s): State<HostState>) -> Json<StatusResponse>
```
Byte-identical output when `status_api` is false. Existing test `status_endpoint_reports_states`
must keep passing unchanged.

### 6.6 `internal_server::tests::spawn_state` — task 9
Gains `git: None` in the `HostState` literal. Task 9 additionally adds
`async fn spawn_git_state(enabled: bool, ops: Arc<dyn GitOps>) -> (HostState, u16, TempDir)`.

### 6.7 `main.rs::run` — task 10
```rust
// BEFORE  (10 params)
fn run(event_loop, cfg, paths, chrome_exe, port, status, rt, _lock_guard, host_log, host_rx) -> !
// AFTER   (12 params)
fn run(event_loop, cfg, paths, chrome_exe, port, status, rt, _lock_guard, host_log, host_rx,
       host_token: String, git: Option<std::sync::Arc<crate::git::GitService>>) -> !
```
Both `run(...)` call sites in `main()` (the `cfg(target_os = "macos")` branch and the other) must be
updated. `main()` must build the token **before** `HostState` and clone it into both.

### 6.8 `main.rs::dialog` and friends — task 0
`dialog`, `missing_chrome_dialog`, `UserEvent`, `App`, `Children`, and every `App` field become
`pub(crate)` so `src/host.rs` can use them. `notice` is created **by task 0**, `pub(crate)` from
birth and with no caller yet:
```rust
#[expect(dead_code)]                                    // removed by task 10, its first caller
pub(crate) fn notice(title: &str, description: &str);   // rfd MessageLevel::Warning, Ok button
```
`#[expect(dead_code)]` rather than `#[allow]` deliberately: once task 10 adds the first
`notice(...)` call the attribute itself becomes an unfulfilled-expectation warning — an error under
`-D warnings` — so **task 10 must delete the attribute in the same step that adds the caller**.
That is the only edit task 10 makes to `notice`.

### 6.9 `App` — new members and methods, task 10
```rust
pub(crate) git: Option<std::sync::Arc<crate::git::GitService>>,
pub(crate) host_token: String,
pub(crate) last_git_restart: Option<std::time::Instant>,
pub(crate) git_notices: std::collections::VecDeque<String>,   // bounded at GIT_NOTICE_HISTORY

impl App {
    pub(crate) fn git_restart_ok(&mut self) -> bool;               // RESTART_DEBOUNCE_MS gate
    pub(crate) fn git_notice_is_new(&mut self, merge_commit: &str) -> bool;
    pub(crate) fn git_log_path(&self) -> std::path::PathBuf;       // paths.logs_dir.join("git.log")
}
```
`App::quit()` order becomes: `log("quit")` → `kill_children()` →
`git.run_quit_syncs(Duration::from_secs(cfg.git.quit_sync_timeout_secs))` (skipped when `0` or
`None`) → `exit(0)`.

---

## 7. Task dependency order

| task | consumes (types/functions from) | produces that later tasks need |
|---|---|---|
| **0** split | — | `crate::host`, `pub(crate)` `App`/`Children`/`UserEvent`/`dialog`, `notice` (`#[expect(dead_code)]` until task 10) |
| **1** spine | 0 | `git2`/`gethostname`/`openssl-sys` deps, `mod git;`, `src/git/mod.rs` — the file only, with **no `pub mod` lines** (the modules do not exist yet; each later task adds its own — see §1) |
| **2** config | 1 | `GitSection`, `SshHostKeyPolicy`, `validate_branch_name`, `MAX_BRANCH_NAME_LEN`, `AppConfig::git`, `git_enabled()`, `RuntimePaths::{repos_dir, registry_file, git_state_file}` |
| **3** secret+error | 1 | `Secret`, `GitError`, `GitErrorCode`, `Git2Hint`, `AbortReason`, `classify`, `scrub`, `util::{now_ms, utc_stamp, b64_nopad, bytes_to_path, NowFn}` |
| **4** registry+state | 2 (`validate_branch_name`, `RuntimePaths`), 3 (`Secret`, `GitError`, `util`) | `RepoDef`, `CredentialSpec`, `AuthorSpec`, `RepoView`, `CredentialView`, `Registry`, `RegistryFile`, `RegistryError`, `RejectedEntry`, `Warning`, `RegistryDefaults`, `PutOutcome`, `StateStore`, `StateFile`, `RepoState`, `LastSync`, `AUTO_SYNC_*`, `MAX_REPO_ID_LEN` |
| **5** jobs | 3 (`GitError`, `AbortReason`, `NowFn`, `now_ms`) | `JobId`, `JobOp`, `JobState`, `Phase`, `Progress`, `AbortFlag`, `JobSlot`, `JobView`, `JobRef`, `JobStore`, `Admission`, `RepoLease`, `JobFilter`, `MAX_JOB_RECORDS`, `JOB_TTL_SECS`, `JOB_MIN_AGE_SECS`, `PROGRESS_THROTTLE_MS` |
| **6** creds | 2 (`SshHostKeyPolicy`, `SettingsSection`), 3, 4 (`CredentialSpec`, `RepoDef`, `AuthorSpec`, `Warning`), 5 (`JobSlot`, `AbortFlag`, `Phase`, `JobOp`) | `ResolvedCred`, `CredResolution`, `HostKeyChecker`, `resolve`, `callbacks`, `MAX_CRED_ATTEMPTS`, **and all of `ops.rs`'s context layer**: `OpCtx`, `OpRequest`, `BranchRequest`, `ResetRequest`, `OpOutcome`, `MergeReport`, `SettingsRejected`, `Identity`, `SettingsCtx`, `ops::run` skeleton |
| **7** ops core | 4, 5, 6 | `init/clone/commit/branch/reset`, `open_tree`, `require_branch`, `ensure_branch`, `stage_and_commit`, `status`, `branches`, `purge_tree`, `ReadCtx`, `RepoStatus`, `DirtySummary`, `RemoteInfo`, `AutoInfo`, `BranchesResponse`, `MAX_DIRTY_FILES`, **and in `git/mod.rs`** `CONNECT_TIMEOUT_MS` + `init_libgit2_timeouts` |
| **8** merge+net | 6, 7 | `merge_prefer_local`, `analyse`, `MergeResult`, `MergeOutcome`, `pull`, `push`, `sync`, `fetch`, `push_branch` |
| **9** service+api | 2, 3, 4, 5, 6, 7, 8 + `internal_server::{HostState, HostEvent, AppStatus, authorized}` + `supervisor::{open_log, LOG_MAX_BYTES}` | `GitService`, `GitOps`, `RealOps`, `StartOutcome`, `ServiceInfo`, `ServiceDefaults`, `AuthorView`, `ServiceFeatures`, `GitStatusSummary`, `RepoStatusSummary`, `RepoWithStatus`, `DeleteOutcome`, `contained_in`, `api::{router, disabled_router, ApiError, …DTOs}`, the three `HostEvent` variants, `HostState.git` |
| **10** host wiring | 0, 2, 9 | `HostAccess`, `build_env`/`menu_model` signatures, `TrayAction::GitSync`, `StatusResponse`, the first `notice(...)` caller (and the removal of task 0's `#[expect(dead_code)]` on it), `App::{git, host_token, git_restart_ok, git_notice_is_new}`, `RESTART_DEBOUNCE_MS`, `GIT_NOTICE_HISTORY`, `run()` signature |
| **11** sync_settings | 6, 7, 8, 9 (+ `settings::{defaults, load, validate_incoming, save}`), 10 (`HostEvent::GitRestartChildren` arm) | `settings_copy_in`, `settings_apply_back`, `OpOutcome.settings_*` |
| **12** docs+CI | everything | README §9, CI apt line, the four `#[ignore]`d tests |

Critical path: `0 → 1 → 2/3 → 4/5 → 6 → 7 → 8 → 9 → 10 → 11 → 12`. Tasks **2 and 3** may run in
parallel; **4 and 5** may run in parallel.

---

## 8. Cross-cutting invariants every writer must preserve

1. No `git2` value escapes the `spawn_blocking` closure. `Repository` is opened in `ops::run` and
   dropped there, and stays alive for the whole job (an `Index` detaches otherwise).
2. Nothing under `src/git/` names `App`, `Children`, `chrome_generation`, `server_generation`,
   `rt.enter()`, or `tokio::process`. The only channel into the tao loop is `HostEvent`.
3. No new `rt.enter()` guard anywhere — git spawns no OS processes.
4. `[git]` absent ⇒ `GitService::new` returns `None`, `repos_dir` is never created, `repos.json` is
   never written, and no libgit2 call runs.
5. `repos.json` is written **only** by `PUT`/`DELETE`. Never by a sync, a timer, or a job.
6. `settings.json` is written **only** by `settings::save` and validated **only** by
   `settings::validate_incoming`.
7. Unit tests live in `#[cfg(test)] mod tests` at the bottom of the file they test; `tempfile` for
   filesystem tests; no `unwrap()` in non-test code except where a panic is correct and commented.
8. `cargo fmt` + `cargo clippy -- -D warnings` clean at the end of every task.
