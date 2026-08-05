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

pub mod api;
pub mod creds;
pub mod error;
pub mod jobs;
pub mod merge;
pub mod ops;
pub mod registry;
pub mod secret;
pub mod state;
pub mod util;

use crate::config::{AppConfig, GitSection, RuntimePaths};
use crate::git::creds::HostKeyChecker;
use crate::git::error::{AbortReason, GitError, GitErrorCode};
use crate::git::jobs::{
    random_host_instance, Admission, JobId, JobOp, JobRef, JobSlot, JobStore, JOB_MIN_AGE_SECS,
    JOB_TTL_SECS, MAX_JOB_RECORDS,
};
use crate::git::ops::{
    BranchesResponse, Identity, OpCtx, OpOutcome, OpRequest, OpScratch, ReadCtx, RepoStatus,
    SettingsCtx,
};
use crate::git::registry::{
    AuthorSpec, PutOutcome, Registry, RegistryDefaults, RegistryError, RepoDef, RepoView, Warning,
};
use crate::git::state::{LastSync, StateStore};
use crate::git::util::now_ms;
use crate::internal_server::{AppStatus, HostEvent};
use serde::Serialize;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long libgit2 may spend establishing a connection before giving up.
///
/// Deliberately not a config key: it is a liveness floor, not a policy, and a caller who
/// wants to wait longer for a slow server wants `[git].network_timeout_secs`.
pub const CONNECT_TIMEOUT_MS: i32 = 10_000;

/// Install the process-global libgit2 network timeouts. Call once, from
/// `GitService::start`, before any repository is opened.
///
/// Without these, a black-holed remote pins a `spawn_blocking` thread indefinitely: the
/// job watchdog lives inside the transfer callbacks, which never fire before the
/// transport has data. Both libgit2 options default to 0, meaning "the OS default".
pub fn init_libgit2_timeouts(network_timeout_secs: u64) -> Result<(), GitError> {
    let total = i32::try_from(network_timeout_secs.saturating_mul(1000)).unwrap_or(i32::MAX);
    // SAFETY: both setters mutate libgit2 process-global state and are documented as
    // needing external synchronisation. The single caller runs once, on the main thread,
    // during startup and before any repository has been opened, so no other thread can
    // be inside libgit2 at this point.
    unsafe {
        git2::opts::set_server_connect_timeout_in_milliseconds(CONNECT_TIMEOUT_MS)
            .map_err(|e| GitError::internal(format!("libgit2 connect timeout: {e}")))?;
        git2::opts::set_server_timeout_in_milliseconds(total)
            .map_err(|e| GitError::internal(format!("libgit2 server timeout: {e}")))?;
    }
    Ok(())
}

/// Resolve `candidate` and refuse it unless it lands strictly inside `root_canon`.
///
/// Both sides are canonical, which is the only form in which the question is
/// answerable: a symlink at `repos/<id>` pointing outside the root would otherwise pass a
/// textual `starts_with`. The root itself is refused as well — every caller is about to
/// delete or unlink something, and "inside the root" must never include the root.
pub fn contained_in(root_canon: &Path, candidate: &Path) -> Result<PathBuf, GitError> {
    let real = std::fs::canonicalize(candidate)
        .map_err(|e| GitError::path_refused(candidate, &format!("cannot be resolved: {e}")))?;
    if real == root_canon || !real.starts_with(root_canon) {
        return Err(GitError::path_refused(
            &real,
            &format!("resolves outside {}", root_canon.display()),
        ));
    }
    Ok(real)
}

/// Canonical form of a path that may not exist yet.
///
/// `repos/` is created by `GitService::start`, but `repos_root_canon` is computed in
/// `new`, and `canonicalize` fails outright on a missing path. Resolving the deepest
/// ancestor that does exist keeps the root canonical on macOS, where the data directory
/// lives under a symlinked `/var` and a textual root would refuse every purge.
fn canonical_root(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => canonical_root(parent).join(name),
        // A path with no parent is already a root; there is nothing left to resolve.
        _ => path.to_path_buf(),
    }
}

/// A bounded status read (`GET /repos/:id/status`) gives up after this and answers
/// `status_timeout`, so a wedged filesystem cannot hang the caller's poll loop.
pub const STATUS_READ_TIMEOUT_MS: u64 = 2_000;
/// After the quit deadline every job is aborted; this is how long they get to notice
/// before the host logs them as abandoned and exits.
pub const QUIT_ABANDON_GRACE_MS: u64 = 2_000;
pub const GIT_LOG_FILE: &str = "git.log";

/// The single seam between `GitService` and libgit2.
///
/// Injected rather than called directly so the service's own tests — admission, the
/// job lifecycle, the event decisions, every HTTP status — run without a network, a
/// remote, or a working tree. `chrome::find_first_existing(paths, exists)` is the same
/// pattern already in this codebase.
///
/// `status` and `branches` are defaulted so an implementor that only cares about jobs
/// writes neither, and every existing one keeps the real behaviour. They exist on the
/// trait at all because `status_timeout` — the one code that answers 504 — is otherwise
/// only reachable through a genuinely wedged filesystem.
pub trait GitOps: Send + Sync {
    fn run(&self, op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError>;

    fn status(&self, ctx: &ReadCtx) -> RepoStatus {
        ops::status(ctx)
    }

    fn branches(&self, tree: &Path, def: &RepoDef) -> Result<BranchesResponse, GitError> {
        ops::branches(tree, def)
    }
}

/// The production implementation: one call, no state, no decisions.
pub struct RealOps;

impl GitOps for RealOps {
    fn run(&self, op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError> {
        ops::run(op, ctx)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DeleteOutcome {
    pub deleted: bool,
    pub purged: bool,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthorView {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceDefaults {
    pub branch: String,
    pub author: AuthorView,
    pub network_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceFeatures {
    pub tray_sync: bool,
    pub error_dialogs: bool,
    pub status_api: bool,
}

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
pub struct RepoStatusSummary {
    pub id: String,
    pub path: String,
    pub auto_sync_secs: Option<u64>,
    pub busy_job: Option<JobRef>,
    pub last_sync: Option<LastSync>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitStatusSummary {
    pub registry_error: Option<RegistryError>,
    pub repos: Vec<RepoStatusSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoWithStatus {
    pub repo: RepoView,
    pub status: RepoStatus,
}

/// What admission decided, without the lease.
///
/// `RepoLease` never leaves `start_job`: the spec's `Admission::Started(slot, lease)`
/// would hand a second lease to the HTTP layer, and the second drop would release a repo
/// the job still owns.
pub enum StartOutcome {
    Started(Arc<JobSlot>),
    Replay(Arc<JobSlot>),
    Busy(Arc<JobSlot>),
}

impl StartOutcome {
    pub fn slot(&self) -> &Arc<JobSlot> {
        match self {
            StartOutcome::Started(slot) | StartOutcome::Replay(slot) | StartOutcome::Busy(slot) => {
                slot
            }
        }
    }
}

/// `git.log` is one record per line, and libgit2 messages sometimes contain newlines.
/// A wrapped record is unparseable by anything reading the file, including a human.
fn one_line(message: &str) -> String {
    message.replace(['\n', '\r'], " ")
}

/// The one-line summary of a successful job for `git.log`.
fn success_line(out: &OpOutcome) -> String {
    let mut line = format!("{} branch={}", out.outcome, out.branch);
    if let Some(head) = &out.head_after {
        line.push_str(&format!(" head={head}"));
    }
    if out.pushed {
        line.push_str(" pushed");
    }
    if out.restart_requested {
        line.push_str(" restart");
    }
    if let Some(merge) = &out.merge {
        if !merge.conflicts_resolved.is_empty() {
            // The overwritten paths are recorded here as well as in the job result and
            // the merge commit message: prefer-local is lossy and the record has to
            // outlive the job store.
            line.push_str(&format!(
                " conflicts_resolved={}",
                merge.conflicts_resolved.join(",")
            ));
        }
    }
    line
}

/// The watchdog's whole policy, factored out of the timer so it is testable without a
/// two-minute sleep. Returns whether the job was still live.
///
/// The lease is deliberately **not** released: two concurrent libgit2 operations on one
/// working tree would corrupt the index. Visible-but-stuck beats silently-corrupt, which
/// is what `stalled` is for.
fn trip_watchdog(slot: &Arc<JobSlot>) -> bool {
    if slot.is_terminal() {
        return false;
    }
    slot.mark_stalled();
    slot.abort.abort(AbortReason::Timeout);
    true
}

/// Poll every slot to terminal or give up. Returns whether they all finished.
fn wait_until_terminal(slots: &[Arc<JobSlot>], budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        if slots.iter().all(|slot| slot.is_terminal()) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The git subsystem, and the only type in `src/git/` the rest of the host names.
///
/// It deliberately holds **no** `Children`, no `chrome_generation`, no
/// `server_generation`, and it never calls `rt.enter()` or `tokio::process`. Its only
/// channel into the tao event loop is `HostEvent`, which already exists and is already
/// bridged. `status` is the one exception and it is read-only: §9.7's third restart
/// condition ("the app is Ready") is unimplementable without it, and it is read with
/// `try_read` from a blocking thread, never written.
pub struct GitService {
    cfg: GitSection,
    identity: Identity,
    repos_root_canon: PathBuf,
    log_path: PathBuf,
    /// When this host started, captured once at construction.
    ///
    /// `recover_index_locks` compares each `index.lock`'s mtime against it. Reading
    /// `SystemTime::now()` inside that function instead would compare every lock against
    /// the moment of the scan, which is later than every lock in existence — so the
    /// guard would remove all of them, including the one it exists to spare: a lock a
    /// `git` the user is running right now has just written.
    started_at: std::time::SystemTime,
    /// There is deliberately **no** `repos_dir` field. `Registry` is handed the repos
    /// root at `load` and owns it; a second, independently-computed copy here is exactly
    /// the drift task 4 forbids, so every path in this file goes through
    /// `self.registry.repos_dir()`.
    registry: Registry,
    state: StateStore,
    jobs: Arc<JobStore>,
    rt: tokio::runtime::Handle,
    events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
    /// `None` until `start()` opens it, and `None` forever if it cannot be opened: a
    /// log file the host cannot write must not disable git or crash the app.
    log: Mutex<Option<std::fs::File>>,
    settings: Option<SettingsCtx>,
    ops: Arc<dyn GitOps>,
    timers: Mutex<BTreeSet<String>>,
    status: Arc<tokio::sync::RwLock<AppStatus>>,
}

impl GitService {
    /// `None` when `[git]` is absent. Creates no directory and runs no libgit2 code —
    /// with the section absent the feature must be invisible on disk.
    pub fn new(
        cfg: &AppConfig,
        paths: &RuntimePaths,
        rt: tokio::runtime::Handle,
        events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
        status: Arc<tokio::sync::RwLock<AppStatus>>,
    ) -> Option<Arc<GitService>> {
        let git = cfg.git.clone()?;
        Some(Self::build(
            cfg,
            git,
            paths,
            rt,
            events,
            status,
            Arc::new(RealOps),
        ))
    }

    #[cfg(test)]
    pub fn with_ops(
        cfg: &AppConfig,
        paths: &RuntimePaths,
        rt: tokio::runtime::Handle,
        events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
        status: Arc<tokio::sync::RwLock<AppStatus>>,
        ops: Arc<dyn GitOps>,
    ) -> Arc<GitService> {
        let git = cfg
            .git
            .clone()
            .expect("with_ops is only called from tests that supply a [git] section");
        Self::build(cfg, git, paths, rt, events, status, ops)
    }

    fn build(
        cfg: &AppConfig,
        git: GitSection,
        paths: &RuntimePaths,
        rt: tokio::runtime::Handle,
        events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
        status: Arc<tokio::sync::RwLock<AppStatus>>,
        ops: Arc<dyn GitOps>,
    ) -> Arc<GitService> {
        let defaults = RegistryDefaults {
            default_branch: git.default_branch.clone(),
            settings_enabled: cfg.settings_enabled(),
            allow_http: git.allow_http,
            registry_writes: git.registry_writes,
        };
        let settings = if cfg.settings_enabled() {
            cfg.settings.clone().map(|schema| SettingsCtx {
                schema,
                settings_file: paths.settings_file.clone(),
            })
        } else {
            None
        };
        Arc::new(GitService {
            identity: Identity::new(&cfg.app.name, &cfg.app.identifier),
            registry: Registry::load(&paths.registry_file, &paths.repos_dir, defaults),
            state: StateStore::load(&paths.git_state_file),
            jobs: JobStore::new(random_host_instance()),
            repos_root_canon: canonical_root(&paths.repos_dir),
            log_path: paths.logs_dir.join(GIT_LOG_FILE),
            started_at: std::time::SystemTime::now(),
            cfg: git,
            rt,
            events,
            log: Mutex::new(None),
            settings,
            ops,
            timers: Mutex::new(BTreeSet::new()),
            status,
        })
    }

    pub fn host_instance(&self) -> &str {
        self.jobs.host_instance()
    }

    pub fn jobs(&self) -> &Arc<JobStore> {
        &self.jobs
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn repo_count(&self) -> usize {
        self.registry.count()
    }

    pub fn tray_sync(&self) -> bool {
        self.cfg.tray_sync
    }

    pub fn error_dialogs(&self) -> bool {
        self.cfg.error_dialogs
    }

    pub fn status_api(&self) -> bool {
        self.cfg.status_api
    }

    /// One source of truth: `Registry` also turns this off when a `repos.json` it could
    /// neither read nor quarantine is still on disk, and callers must see that, not just
    /// the config key. (It never turned it off for an unwritable *file*, which is what
    /// this said before task 12's audit — `load` has no such path and never had one.)
    pub fn registry_writes(&self) -> bool {
        self.registry.writable()
    }

    pub fn draining(&self) -> bool {
        self.jobs.draining()
    }

    pub fn auto_sync_secs(&self, id: &str) -> Option<u64> {
        self.registry.auto_sync_secs(id)
    }

    /// One source of truth for the repos root: `Registry` was handed it at `load`.
    pub fn tree_path(&self, id: &str) -> PathBuf {
        self.registry.repos_dir().join(id)
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// Everything the service does exactly once, at host startup: open the log, take
    /// ownership of `repos/`, apply libgit2's process-global timeouts, report whatever
    /// the registry and state files had to say, clear stale index locks, and start the
    /// auto-sync timers.
    pub fn start(self: &Arc<Self>) {
        match crate::supervisor::open_log(&self.log_path, crate::supervisor::LOG_MAX_BYTES) {
            Ok(file) => *self.log_guard() = Some(file),
            // `App` owns the host log and `src/git/` is forbidden to touch it, so stderr
            // is the only sink left for "the log itself failed".
            Err(e) => eprintln!("git: cannot open {}: {e}", self.log_path.display()),
        }
        if let Err(e) = std::fs::create_dir_all(self.registry.repos_dir()) {
            self.log_startup(&format!(
                "cannot create {}: {e}",
                self.registry.repos_dir().display()
            ));
        }
        if let Err(e) = init_libgit2_timeouts(self.cfg.network_timeout_secs) {
            self.log_startup(&format!("libgit2 timeouts not applied: {e}"));
        }
        for note in self.registry.notes() {
            self.log_startup(note);
        }
        if let Some(err) = self.registry.error() {
            self.log_startup(&format!("registry {}: {}", err.code, err.message));
        }
        if let Some(err) = self.state.load_error() {
            self.log_startup(&format!("git-state.json unreadable, starting empty: {err}"));
        }
        self.recover_index_locks();
        for id in self.registry.ids() {
            self.spawn_auto_timer(&id);
        }
        self.log_startup(&format!(
            "instance={} repos={} root={}",
            self.jobs.host_instance(),
            self.registry.count(),
            self.registry.repos_dir().display()
        ));
    }

    /// A panic while a log line was being written must not silence the log for the rest
    /// of the process — the guarded value is a file handle, not an invariant.
    fn log_guard(&self) -> std::sync::MutexGuard<'_, Option<std::fs::File>> {
        self.log
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn log(&self, line: &str) {
        if let Some(file) = self.log_guard().as_mut() {
            let _ = writeln!(file, "{line}");
        }
    }

    fn log_startup(&self, message: &str) {
        self.log(&format!(
            "{} startup repo=- job=- ok code=- {}",
            now_ms(),
            one_line(message)
        ));
    }

    /// `std::process::exit(0)` runs no destructors, so an abandoned job can leave one
    /// durable residue: `.git/index.lock`. Clearing it is safe because the host holds
    /// `app.lock` and owns `<data-dir>/repos/`.
    ///
    /// Deliberately **no** `reset --hard` and **no** `cleanup_state()`: our merge never
    /// enters `RepositoryState::Merge`, so a merge state in one of these trees was
    /// created by a human and discarding their staged resolution would be unrecoverable.
    /// `ops::require_clean_state` is the other half of that decision: startup leaves such
    /// a tree alone, and every mutating verb then refuses it rather than committing over
    /// it.
    fn recover_index_locks(&self) {
        // Captured at construction, not here: see `started_at`. The guard only has to
        // distinguish a lock left by a previous run of ours from one a `git` process
        // created after this host came up.
        let started = self.started_at;
        for id in self.registry.ids() {
            let tree = self.tree_path(&id);
            // A symlink planted at `repos/<id>` would point the unlink at a repository
            // the host does not own.
            let Ok(real) = contained_in(&self.repos_root_canon, &tree) else {
                continue;
            };
            let lock = real.join(".git").join("index.lock");
            let Ok(meta) = std::fs::metadata(&lock) else {
                continue;
            };
            match meta.modified() {
                Ok(mtime) if mtime < started => match std::fs::remove_file(&lock) {
                    Ok(()) => {
                        self.log_startup(&format!("repo={id} removed stale {}", lock.display()))
                    }
                    Err(e) => self
                        .log_startup(&format!("repo={id} cannot remove {}: {e}", lock.display())),
                },
                _ => self.log_startup(&format!(
                    "repo={id} {} is newer than this process; left in place",
                    lock.display()
                )),
            }
        }
    }

    pub fn put_repo(self: &Arc<Self>, id: &str, body: RepoDef) -> Result<PutOutcome, GitError> {
        crate::git::registry::validate_id(id)?;
        // Writability before the hold, so a read-only host answers 403 rather than
        // taking a lock it is about to refuse to use.
        self.registry
            .ensure_writable()
            .map_err(|e| e.with_repo(id))?;
        // Held rather than sampled, exactly as in `delete_repo`: a job snapshotted its
        // `RepoDef` at admission, and a job admitted in the gap between a `busy()` read
        // and the write below would run the whole way through against a definition the
        // caller has already been told was replaced.
        let _hold = self.jobs.hold_repo(id)?;
        let outcome = self.registry.put(body)?;
        self.spawn_auto_timer(id);
        Ok(outcome)
    }

    pub fn delete_repo(&self, id: &str, purge: bool) -> Result<DeleteOutcome, GitError> {
        crate::git::registry::validate_id(id)?;
        self.registry
            .ensure_writable()
            .map_err(|e| e.with_repo(id))?;
        // Held for the whole call, not sampled: `start_job` snapshots the definition and
        // resolves credentials *before* it admits, so an auto-sync tick can land in the
        // gap between a `busy()` read and the purge below and leave libgit2 writing into
        // the directory `remove_dir_all` is walking. The hold occupies the same map entry
        // `admit` takes, so the two are decided by one acquisition of the job store's
        // mutex, and `Drop` releases it on every path out of here — the refused purge
        // included. `_hold` and not `_`: the latter drops it on the spot.
        let _hold = self.jobs.hold_repo(id)?;
        let tree = self.tree_path(id);
        // Containment is checked before anything is removed: a refusal must leave both
        // the registry entry and the tree exactly as they were.
        if purge && std::fs::symlink_metadata(&tree).is_ok() {
            contained_in(&self.repos_root_canon, &tree).map_err(|e| e.with_repo(id))?;
        }
        // The definition goes first. Purging first and then failing to save would leave
        // a registered repo with no tree, which no retry can repair; this order leaves
        // at worst an orphaned directory, which a re-`PUT` reuses.
        self.registry.remove(id)?;
        // Both belong to the definition, not to the tree: once the entry is gone the id
        // is unregistered whatever the purge does, and a `last_sync` row or a job record
        // left behind by a refusal below would be inherited by the next `PUT` of the same
        // id. `forget_repo` touches only terminal records, never `busy`, so it cannot
        // clear the hold it is running inside.
        self.state.forget(id);
        self.jobs.forget_repo(id);
        let purged = if purge {
            match ops::purge_tree(&self.repos_root_canon, &tree) {
                Ok(()) => true,
                // A refusal keeps its own 403; anything else is a failed removal (500).
                Err(e) if e.code() == GitErrorCode::PathRefused => return Err(e.with_repo(id)),
                Err(e) => return Err(GitError::purge_failed(e).with_repo(id)),
            }
        } else {
            false
        };
        Ok(DeleteOutcome {
            deleted: true,
            purged,
            path: tree.display().to_string(),
        })
    }

    pub fn repo_view(&self, id: &str) -> Result<RepoView, GitError> {
        let def = self.registry.snapshot(id)?;
        Ok(RepoView::new(&def, self.registry.repos_dir()))
    }

    pub fn list_views(&self) -> Vec<RepoView> {
        self.registry
            .list()
            .iter()
            .map(|def| RepoView::new(def, self.registry.repos_dir()))
            .collect()
    }

    pub fn service_info(&self) -> ServiceInfo {
        ServiceInfo {
            host_instance: self.jobs.host_instance().to_string(),
            repos_root: self.registry.repos_dir().display().to_string(),
            registry_file: self.registry.path().display().to_string(),
            log_file: self.log_path.display().to_string(),
            registry_writable: self.registry.writable(),
            registry_error: self.registry.error(),
            defaults: ServiceDefaults {
                branch: self.cfg.default_branch.clone(),
                author: self.default_author(),
                network_timeout_secs: self.cfg.network_timeout_secs,
            },
            features: ServiceFeatures {
                tray_sync: self.cfg.tray_sync,
                error_dialogs: self.cfg.error_dialogs,
                status_api: self.cfg.status_api,
            },
            job_retention: MAX_JOB_RECORDS,
            job_ttl_secs: JOB_TTL_SECS,
            job_min_age_secs: JOB_MIN_AGE_SECS,
            repo_count: self.registry.count(),
        }
    }

    /// The same fallback chain `OpCtx::author_name`/`author_email` apply, reported so a
    /// caller can see what a commit will be signed with without making one. `""` is the
    /// config default rather than a choice, so it must not win over the fallback.
    fn default_author(&self) -> AuthorView {
        let name = if self.cfg.author_name.trim().is_empty() {
            self.identity.app_name.clone()
        } else {
            self.cfg.author_name.clone()
        };
        let email = if self.cfg.author_email.trim().is_empty() {
            format!("{}@{}", self.identity.identifier, self.identity.hostname)
        } else {
            self.cfg.author_email.clone()
        };
        AuthorView { name, email }
    }

    /// Built purely from the registry, the job store and the in-memory state file —
    /// zero libgit2 calls, zero filesystem access. `assets/loading.html` polls
    /// `/api/status` in a tight loop, and a `Repository::open` + `statuses()` per poll
    /// per repo would be a genuine performance bug.
    pub fn status_summary(&self) -> GitStatusSummary {
        GitStatusSummary {
            registry_error: self.registry.error(),
            repos: self
                .registry
                .list()
                .into_iter()
                .map(|def| RepoStatusSummary {
                    path: self.tree_path(&def.id).display().to_string(),
                    auto_sync_secs: def.auto_sync_secs,
                    busy_job: self.jobs.busy(&def.id).map(|job| job.as_ref_view()),
                    last_sync: self.state.last_sync(&def.id),
                    id: def.id,
                })
                .collect(),
        }
    }

    /// A synchronous read, deliberately **not** taken under the per-repo lease:
    /// observing a repo while it syncs is the main reason to call this. A read taken
    /// mid-checkout is a snapshot; the job's `result` is authoritative.
    pub async fn read_status(&self, repo_id: &str) -> Result<RepoWithStatus, GitError> {
        crate::git::registry::validate_id(repo_id)?;
        let def = self.registry.snapshot(repo_id)?;
        let repo = RepoView::new(&def, self.registry.repos_dir());
        let ctx = ReadCtx {
            tree: self.tree_path(repo_id),
            last_sync: self.state.last_sync(repo_id),
            busy_job: self.jobs.busy(repo_id).map(|job| job.as_ref_view()),
            def,
        };
        let ops = self.ops.clone();
        let status = self.bounded(repo_id, move || ops.status(&ctx)).await?;
        Ok(RepoWithStatus { repo, status })
    }

    pub async fn read_branches(&self, repo_id: &str) -> Result<BranchesResponse, GitError> {
        crate::git::registry::validate_id(repo_id)?;
        let def = self.registry.snapshot(repo_id)?;
        let tree = self.tree_path(repo_id);
        let ops = self.ops.clone();
        self.bounded(repo_id, move || ops.branches(&tree, &def))
            .await?
    }

    /// Run one blocking read with a hard ceiling.
    ///
    /// The blocking task cannot be cancelled — `JoinHandle::abort` does not interrupt a
    /// thread already inside libgit2 — so the timeout bounds the *caller*, not the work.
    /// That is the point: a wedged filesystem must not hang the child's poll loop.
    async fn bounded<T, F>(&self, repo_id: &str, work: F) -> Result<T, GitError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let handle = self.rt.spawn_blocking(work);
        match tokio::time::timeout(Duration::from_millis(STATUS_READ_TIMEOUT_MS), handle).await {
            Ok(Ok(value)) => Ok(value),
            // A panic inside libgit2 kills the read, not the host.
            Ok(Err(e)) => {
                Err(GitError::internal(format!("read task failed: {e}")).with_repo(repo_id))
            }
            Err(_) => Err(GitError::status_timeout().with_repo(repo_id)),
        }
    }

    pub fn start_job(
        self: &Arc<Self>,
        repo_id: &str,
        op: JobOp,
        req: OpRequest,
    ) -> Result<StartOutcome, GitError> {
        crate::git::registry::validate_id(repo_id)?;
        let def = self.registry.snapshot(repo_id)?;

        // Refused before admission, so a request that cannot possibly succeed never
        // creates a job record for the caller to poll. `sync` is deliberately not in
        // this list: §9.4's `remote: null` is a local-only repo whose sync stages and
        // commits (`ops::sync`'s early return), so the verb still has an arm to run.
        // The rule is that admission refuses a verb only when EVERY mode of it is
        // impossible — which is why `reset` is not here either: `to: "upstream"` needs
        // an upstream and fails in the worker, `to: "head"` needs nothing.
        if def.remote.is_none() && matches!(op, JobOp::Clone | JobOp::Pull | JobOp::Push) {
            return Err(GitError::remote_missing().with_repo(repo_id));
        }
        if op == JobOp::Reset && !req.reset.as_ref().is_some_and(|r| r.confirm) {
            // `reset` is the one verb that destroys uncommitted work, so the caller has
            // to say so in the body rather than in the URL.
            return Err(GitError::confirm_required().with_repo(repo_id));
        }

        // Credentials are resolved *before* admission. Resolving after would mean an
        // invalid spec produced a job record that failed with "git worker terminated
        // unexpectedly" — the lease's own drop message — instead of the real reason.
        let resolution = creds::resolve(
            req.credential.as_ref(),
            def.credential.as_ref(),
            def.remote.as_deref(),
        )
        .map_err(|e| e.with_repo(repo_id))?;

        let admission = self.jobs.admit(repo_id, op, req.request_id.as_deref())?;
        let (slot, lease) = match admission {
            Admission::Replay(slot) => return Ok(StartOutcome::Replay(slot)),
            Admission::Busy(slot) => return Ok(StartOutcome::Busy(slot)),
            Admission::Started(slot, lease) => (slot, lease),
        };

        let ctx = OpCtx {
            tree: self.tree_path(repo_id),
            cred: resolution.cred,
            cred_unbound: resolution.unbound,
            identity: self.identity.clone(),
            host_key: HostKeyChecker::new(
                self.cfg.ssh_host_key_policy,
                self.state.fingerprint(repo_id),
            ),
            abort: slot.abort.clone(),
            slot: slot.clone(),
            deadline: Instant::now() + Duration::from_secs(self.cfg.network_timeout_secs),
            settings: self.settings.clone(),
            default_author: AuthorSpec {
                name: Some(self.cfg.author_name.clone()),
                email: Some(self.cfg.author_email.clone()),
            },
            scratch: OpScratch::default(),
            request: req,
            def,
        };

        self.spawn_watchdog(slot.clone());

        let this = self.clone();
        let started = slot.clone();
        // No `rt.enter()` guard here, and none is missing. That guard exists in this
        // codebase solely because `tokio::process::Command::spawn` registers a child
        // with the runtime's signal driver; git spawns no processes and
        // `Handle::spawn_blocking` is callable from any thread. Do not "fix" this.
        self.rt.spawn_blocking(move || {
            // Declared first so it is dropped last: the repo is released the instant the
            // job is terminal, and on the panic path too — `spawn_blocking` contains a
            // libgit2 panic, the lease releases via Drop, and the host keeps running.
            let _lease = lease;
            ctx.slot.begin();
            // The only git2 code in the process. Nothing `git2` escapes this closure:
            // `Repository` is opened inside `ops::run` and stays alive for the whole
            // job, because dropping it while an `Index` is held detaches the index and
            // `write_tree()` then fails.
            let mut outcome = this.ops.run(op, &ctx);
            this.after_job(op, &ctx, &mut outcome);
        });
        Ok(StartOutcome::Started(started))
    }

    /// §9.7: a restart needs all three of — someone asked for it, the operation really
    /// changed something, and the app is in a state where restarting is not a
    /// double-start.
    fn decide_restart(&self, repo_id: &str, ctx: &OpCtx, out: &mut OpOutcome) {
        let asked = ctx
            .request
            .restart_children
            .unwrap_or(ctx.def.restart_children_on_pull);
        // "Actually moved HEAD" is what stops a five-minute auto-sync from restarting
        // Chrome 288 times a day: `up_to_date` leaves the two heads equal.
        let moved = out.head_after.is_some() && out.head_before != out.head_after;
        if !((asked && moved) || out.settings_changed) {
            return;
        }
        if self.app_is_ready() {
            out.restart_requested = true;
            return;
        }
        // A `sync_on_start` repo can finish before `wait_healthy` does. The existing
        // `server_generation` guard makes an early restart *correct*, but the user
        // would watch the server start twice.
        out.warnings.push(Warning {
            code: "restart_deferred",
            message: format!(
                "children were not restarted for \"{repo_id}\": the app is not ready yet"
            ),
        });
    }

    /// `try_read` and never `read`: this runs on a blocking thread with no runtime
    /// context, so it must not await. A momentarily-locked status reads as not-ready,
    /// which defers the restart — the safe direction.
    fn app_is_ready(&self) -> bool {
        match self.status.try_read() {
            Ok(status) => matches!(&*status, AppStatus::Ready { .. }),
            Err(_) => false,
        }
    }

    /// Everything that happens once `ops::run` has returned.
    ///
    /// It publishes the job itself rather than leaving that to the caller, because
    /// `result.restart_requested` and the `restart_deferred` warning are fields *of* the
    /// result: the decision has to be made before `JobSlot::succeed` freezes it.
    fn after_job(&self, op: JobOp, ctx: &OpCtx, outcome: &mut Result<OpOutcome, GitError>) {
        let repo_id = ctx.def.id.clone();
        let job_id = ctx.slot.id.clone();

        // Read before the new record overwrites it: `GitFailed` fires on the ok -> fail
        // transition only.
        let was_ok = self.state.last_sync(&repo_id).map(|last| last.ok);
        if let Ok(out) = outcome.as_mut() {
            self.decide_restart(&repo_id, ctx, out);
        }

        match outcome.as_ref() {
            Ok(out) => match serde_json::to_value(out) {
                Ok(value) => ctx.slot.succeed(&value),
                Err(e) => ctx.slot.fail(&GitError::internal(format!(
                    "job result not serializable: {e}"
                ))),
            },
            Err(e) => ctx.slot.fail(e),
        }

        match outcome.as_ref() {
            Ok(out) => {
                if let Some(fingerprint) = &out.learned_fingerprint {
                    // git-state.json has exactly one writer and it is this thread. The
                    // blocking worker holds no handle to the store, by construction.
                    self.state.record_fingerprint(&repo_id, fingerprint);
                }
                self.state.record_last_sync(
                    &repo_id,
                    LastSync {
                        at_ms: now_ms(),
                        ok: true,
                        op: op.as_str().to_string(),
                        job_id: job_id.to_string(),
                        outcome: Some(out.outcome.to_string()),
                        head: out.head_after.clone(),
                        code: None,
                        message: None,
                    },
                );
                self.log_job(op, &repo_id, &job_id, Ok(&success_line(out)));
                if out.restart_requested {
                    let reason = if out.settings_changed {
                        "settings"
                    } else {
                        "requested"
                    };
                    let _ = self.events.send(HostEvent::GitRestartChildren {
                        repo_id: repo_id.clone(),
                        reason,
                    });
                }
                if let Some(merge) = &out.merge {
                    if !merge.conflicts_resolved.is_empty() {
                        // A prefer-local merge that resolved a conflict has, by
                        // definition, overwritten an edit made somewhere else. Silence
                        // is the one thing not worth defending here.
                        let _ = self.events.send(HostEvent::GitConflictsResolved {
                            repo_id: repo_id.clone(),
                            merge_commit: merge.merge_commit.clone().unwrap_or_default(),
                            paths: merge.conflicts_resolved.clone(),
                        });
                    }
                }
            }
            Err(e) => {
                self.state.record_last_sync(
                    &repo_id,
                    LastSync {
                        at_ms: now_ms(),
                        ok: false,
                        op: op.as_str().to_string(),
                        job_id: job_id.to_string(),
                        outcome: None,
                        head: None,
                        code: Some(e.code().as_str().to_string()),
                        message: Some(e.message.clone()),
                    },
                );
                self.log_job(op, &repo_id, &job_id, Err(e));
                if was_ok != Some(false) {
                    let _ = self.events.send(HostEvent::GitFailed {
                        repo_id: repo_id.clone(),
                        op: op.as_str().to_string(),
                        code: e.code().as_str().to_string(),
                        message: e.message.clone(),
                    });
                }
            }
        }
    }

    fn log_job(&self, op: JobOp, repo_id: &str, job_id: &JobId, result: Result<&str, &GitError>) {
        let (state, code, message) = match result {
            Ok(message) => ("ok", "-", message),
            Err(e) => ("err", e.code().as_str(), e.message.as_str()),
        };
        self.log(&format!(
            "{} {} repo={repo_id} job={job_id} {state} code={code} {}",
            now_ms(),
            op.as_str(),
            one_line(message)
        ));
    }

    /// One timer per job. Its only job is to trip the abort flag so a wedged transfer
    /// ends as `timeout` instead of pinning a blocking thread forever.
    ///
    /// libgit2's process-global connect and idle timeouts (`init_libgit2_timeouts`) are
    /// what actually bound a black-holed *connect* — this watchdog lives in the transfer
    /// callbacks, which never fire before the transport has data.
    fn spawn_watchdog(self: &Arc<Self>, slot: Arc<JobSlot>) {
        let this = self.clone();
        let secs = self.cfg.network_timeout_secs;
        self.rt.spawn(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            if trip_watchdog(&slot) {
                this.log(&format!(
                    "{} {} repo={} job={} err code=timeout no progress for {secs}s; aborting",
                    now_ms(),
                    slot.op.as_str(),
                    slot.repo_id,
                    slot.id
                ));
            }
        });
    }

    pub fn sync_all_manual(self: &Arc<Self>) {
        for id in self.registry.ids() {
            self.trigger(&id, JobOp::Sync, OpRequest::manual(), "manual");
        }
    }

    /// Fired after `App::start_children()`, never before: the child server is already
    /// up, so a `sync_settings` pull restarts a live child rather than racing the first
    /// spawn, and a wedged network at launch cannot make the app look dead.
    pub fn sync_on_start(self: &Arc<Self>) {
        for def in self.registry.list() {
            if def.sync_on_start {
                self.trigger(&def.id, JobOp::Sync, OpRequest::auto(), "sync_on_start");
            }
        }
    }

    /// Admit a job for a background trigger and record what happened. Nothing here can
    /// block: every one of these callers is either the tao thread or a timer task.
    fn trigger(self: &Arc<Self>, id: &str, op: JobOp, req: OpRequest, why: &str) {
        let ts = now_ms();
        match self.start_job(id, op, req) {
            Ok(StartOutcome::Started(slot)) => self.log(&format!(
                "{ts} {} repo={id} job={} ok code=- {why}: started",
                op.as_str(),
                slot.id
            )),
            // A tick on a busy repo is dropped, not queued: a backlog of stale syncs is
            // never useful and would turn one slow network into an unbounded queue.
            Ok(StartOutcome::Replay(slot)) | Ok(StartOutcome::Busy(slot)) => self.log(&format!(
                "{ts} {} repo={id} job={} ok code=- {why}: skipped, already running",
                op.as_str(),
                slot.id
            )),
            Err(e) => self.log(&format!(
                "{ts} {} repo={id} job=- err code={} {why}: {}",
                op.as_str(),
                e.code().as_str(),
                one_line(&e.message)
            )),
        }
    }

    /// One task per repo with an interval, started by `start()` and re-checked on every
    /// `PUT`. The `timers` set is what stops a second `PUT` from double-spawning.
    fn spawn_auto_timer(self: &Arc<Self>, id: &str) {
        if self.auto_sync_secs(id).is_none() {
            return;
        }
        {
            let mut timers = self.timers.lock().unwrap_or_else(|p| p.into_inner());
            if !timers.insert(id.to_string()) {
                return;
            }
        }
        let this = self.clone();
        let id = id.to_string();
        self.rt.spawn(async move {
            // A sleep loop rather than `tokio::time::interval`, so the period is re-read
            // from the registry each cycle: a PUT that changes or clears
            // `auto_sync_secs` takes effect after at most one old period, and a DELETE
            // lets the task exit on its own. `MissedTickBehavior` is moot here — a sleep
            // loop cannot burst after a laptop resume.
            // The condition is re-evaluated every cycle, which is the point: a DELETE or a
            // PUT that clears `auto_sync_secs` ends the task on its own.
            while let Some(secs) = this.auto_sync_secs(&id) {
                tokio::time::sleep(Duration::from_secs(secs)).await;
                if this.draining() {
                    break;
                }
                this.trigger(&id, JobOp::Sync, OpRequest::auto(), "auto");
            }
            let mut timers = this.timers.lock().unwrap_or_else(|p| p.into_inner());
            timers.remove(&id);
        });
    }

    #[cfg(test)]
    fn timer_ids(&self) -> Vec<String> {
        self.timers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// The last thing the host does, after `kill_children()` so the trees are quiescent
    /// and the final commit is a consistent snapshot.
    ///
    /// Blocks the calling thread — `App::quit()` runs on the tao thread, never inside a
    /// runtime worker, so this cannot deadlock the runtime. It polls rather than joining
    /// futures because the work is on the blocking pool and `futures::join_all` is not a
    /// dependency of this crate.
    pub fn run_quit_syncs(self: &Arc<Self>, timeout: Duration) {
        // Admission happens BEFORE draining is set: `JobStore::admit` refuses every
        // request once draining, including ours. The gap between the two is one
        // synchronous loop with no `.await` in it.
        let mut started = Vec::new();
        if !timeout.is_zero() {
            for def in self.registry.list() {
                if !def.sync_on_quit {
                    continue;
                }
                match self.start_job(&def.id, JobOp::Sync, OpRequest::manual()) {
                    Ok(StartOutcome::Started(slot)) => started.push(slot),
                    Ok(other) => self.log(&format!(
                        "{} sync repo={} job={} ok code=- quit: skipped, already running",
                        now_ms(),
                        def.id,
                        other.slot().id
                    )),
                    Err(e) => self.log(&format!(
                        "{} sync repo={} job=- err code={} quit: {}",
                        now_ms(),
                        def.id,
                        e.code().as_str(),
                        one_line(&e.message)
                    )),
                }
            }
        }
        self.jobs.set_draining(true);
        if started.is_empty() {
            return;
        }
        if !wait_until_terminal(&started, timeout) {
            self.jobs.abort_all(AbortReason::Shutdown);
            wait_until_terminal(&started, Duration::from_millis(QUIT_ABANDON_GRACE_MS));
        }
        let abandoned: Vec<String> = started
            .iter()
            .filter(|slot| !slot.is_terminal())
            .map(|slot| slot.id.to_string())
            .collect();
        if !abandoned.is_empty() {
            // `std::process::exit(0)` joins no blocking threads. libgit2 writes objects
            // and refs via write-to-temp + rename, so an abandoned job leaves a valid
            // repository — at worst missing its last operation. The ids are logged so
            // the next run's `last_sync` can be read against them.
            self.log(&format!(
                "{} shutdown repo=- job=- err code=canceled {} job(s) abandoned: {}",
                now_ms(),
                abandoned.len(),
                abandoned.join(", ")
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards the `git2` feature list in `Cargo.toml`, not git2 itself.
    ///
    /// git2 0.21 declares `default = []`. A bare `git2 = "0.21"` therefore compiles,
    /// links, and passes every local-only test while producing a libgit2 with no HTTPS
    /// and no SSH transport: `clone`/`fetch`/`push` against a real remote fail at
    /// runtime with "unsupported URL protocol", possibly months after the dependency
    /// was added. Dropping `vendored-libgit2` is the same trap pointed the other way —
    /// the build silently links whatever libgit2 the build machine happens to have, so
    /// the shipped .deb/.dmg/.msi depends on a system library the user does not have.
    /// libgit2 reports all four capabilities at runtime, so one assertion converts both
    /// silent regressions into a red test.
    /// Ties `config::validate_branch_name` to the library that actually enforces the
    /// rule, in the one direction that matters: **everything we accept, libgit2 must
    /// accept.** The reverse is deliberately false — we reject a leading `-`, `@`, `~`
    /// and friends that libgit2 is happy with, because they are argv and shell hazards.
    ///
    /// This exists because the validator is the single admission gate for
    /// `[git].default_branch`, `repos[].branch` and `POST /api/git/repos/<id>/branch`.
    /// A name that slips through is not caught anywhere else: it reaches libgit2 deep
    /// inside a job, where it surfaces as a generic ref error instead of the
    /// admission-time rejection the API contract promises. An earlier revision checked
    /// `.lock` against the whole name rather than each path component, so `a.lock/b`,
    /// `a/.b` and `x/.` all passed — this test is what would have caught it.
    ///
    /// `is_valid_name` wants a full refname; branches live under `refs/heads/`.
    #[test]
    fn validate_branch_name_never_admits_a_name_libgit2_refuses() {
        // Shapes chosen to probe each rule and its boundary, not to be exhaustive:
        // component dots, `.lock` in every position, separators, and the length limit.
        let corpus = [
            "main",
            "a",
            "feature/x",
            "v1.2.3",
            "a-b_c",
            "release/2026.08",
            "a.b.c",
            "refs/heads/x",
            "x.locked",
            "lock",
            ".",
            "..",
            "...",
            ".x",
            "a.",
            "x.lock",
            "x.lock.",
            "a/.b",
            "x/.",
            "a.lock/b",
            "a/b.lock/c",
            "a..b",
            "a//b",
            "-x",
            "/x",
            "x/",
            "@",
            "a b",
            "a~b",
            "héllo",
            "a\tb",
            "",
        ];
        for name in corpus {
            if crate::config::validate_branch_name(name) {
                assert!(
                    git2::Reference::is_valid_name(&format!("refs/heads/{name}")),
                    "validate_branch_name accepted {name:?}, which libgit2 refuses as a refname"
                );
            }
        }
        // Guard the guard: a corpus that no longer reaches the interesting branch
        // would make the loop above vacuously true.
        assert!(
            corpus.iter().filter(|n| validate(n)).count() >= 8,
            "corpus no longer exercises the accept path"
        );
        assert!(
            corpus.iter().filter(|n| !validate(n)).count() >= 8,
            "corpus no longer exercises the reject path"
        );
        assert!(
            validate(&"a".repeat(200)) && !validate(&"a".repeat(201)),
            "MAX_BRANCH_NAME_LEN boundary moved"
        );
    }

    fn validate(name: &str) -> bool {
        crate::config::validate_branch_name(name)
    }

    #[test]
    fn libgit2_is_vendored_with_network_transports() {
        let v = git2::Version::get();
        assert!(
            v.vendored(),
            "libgit2 must be the vendored build; Cargo.toml needs the `vendored-libgit2` feature: {v:?}"
        );
        assert!(
            v.https(),
            "libgit2 must be built with HTTPS; Cargo.toml needs the `https` feature: {v:?}"
        );
        assert!(
            v.ssh(),
            "libgit2 must be built with SSH; Cargo.toml needs the `ssh` feature: {v:?}"
        );
        assert!(
            v.threads(),
            "libgit2 must be thread-aware; git work runs on tokio's blocking pool: {v:?}"
        );
    }

    /// Without these, a black-holed remote pins a `spawn_blocking` thread indefinitely:
    /// our own watchdog lives inside the transfer callbacks, and those never fire before
    /// the transport has produced data. Both libgit2 options default to 0, meaning
    /// "whatever the OS decides", which on Linux is around two hours.
    #[test]
    fn network_timeouts_are_installed_from_the_configured_value() {
        super::init_libgit2_timeouts(45).expect("libgit2 accepts both timeouts");
        // SAFETY: reads libgit2 process-global state. Nothing else in this test binary
        // writes it, and `init_libgit2_timeouts` has already returned.
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

    fn config(git: &str) -> crate::config::AppConfig {
        crate::config::AppConfig::from_str(&format!(
            "[app]\nname = \"Test App\"\nidentifier = \"com.example.test\"\n{git}"
        ))
        .expect("config parses")
    }

    /// A `GitOps` that records what it was asked to do and hands back a canned answer.
    /// Almost every test in this file is therefore a test of the service, never of
    /// libgit2. The one deliberate exception is `a_local_only_repo_syncs_to_a_real_commit`,
    /// which uses `RealOps` because the arm it covers was dead code for as long as
    /// admission refused the verb — a `FakeOps` version would prove only that the gate
    /// opened, and would leave that arm dead a second time.
    struct FakeOps {
        calls: std::sync::Mutex<Vec<(JobOp, String)>>,
        result: std::sync::Mutex<Result<OpOutcome, GitError>>,
    }

    impl FakeOps {
        fn returning(result: Result<OpOutcome, GitError>) -> Arc<FakeOps> {
            Arc::new(FakeOps {
                calls: std::sync::Mutex::new(Vec::new()),
                result: std::sync::Mutex::new(result),
            })
        }
        fn ok() -> Arc<FakeOps> {
            FakeOps::returning(Ok(OpOutcome::new("up_to_date", "main")))
        }
        fn calls(&self) -> Vec<(JobOp, String)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl GitOps for FakeOps {
        fn run(&self, op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError> {
            self.calls.lock().unwrap().push((op, ctx.def.id.clone()));
            self.result.lock().unwrap().clone()
        }
    }

    /// Everything a service test needs, alive for the test's duration.
    struct Svc {
        _dir: tempfile::TempDir,
        paths: crate::config::RuntimePaths,
        svc: Arc<GitService>,
        events: tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
        status: Arc<tokio::sync::RwLock<AppStatus>>,
    }

    impl Svc {
        fn git_log(&self) -> String {
            std::fs::read_to_string(self.svc.log_path()).unwrap_or_default()
        }
    }

    async fn service(git: &str, ops: Arc<dyn GitOps>) -> Svc {
        service_planted(git, ops, |_| {}).await
    }

    /// The same, with one window opened: `plant` runs after `paths.ensure()` and before
    /// `GitService::with_ops`, which is the only moment a test can decide what the
    /// on-disk registry looks like at startup. `Registry::load` runs once, inside
    /// `with_ops`, and repos.json is deliberately not hot-reloaded — so writing the file
    /// after `service()` returns would be testing nothing at all.
    async fn service_planted(
        git: &str,
        ops: Arc<dyn GitOps>,
        plant: impl FnOnce(&crate::config::RuntimePaths),
    ) -> Svc {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::config::RuntimePaths::under(dir.path(), "com.example.test");
        paths.ensure().unwrap();
        plant(&paths);
        let (tx, events) = tokio::sync::mpsc::unbounded_channel();
        let status = Arc::new(tokio::sync::RwLock::new(AppStatus::Ready {
            target_url: "http://x/".to_string(),
        }));
        let svc = GitService::with_ops(
            &config(git),
            &paths,
            tokio::runtime::Handle::current(),
            tx,
            status.clone(),
            ops,
        );
        svc.start();
        Svc {
            _dir: dir,
            paths,
            svc,
            events,
            status,
        }
    }

    /// Never dialled: every test that uses it replaces `ops::run` with a fake. It exists
    /// because `start_job` refuses `clone`/`pull`/`push` on a repo with no remote before
    /// admission, so a job test needs one configured to get past that guard.
    const REMOTE: &str = "/nonexistent/origin.git";

    fn repo_def(id: &str, remote: Option<&str>) -> RepoDef {
        RepoDef {
            id: id.to_string(),
            remote: remote.map(str::to_string),
            remote_name: "origin".to_string(),
            branch: String::new(),
            credential: None,
            author: None,
            sync_settings: false,
            settings_path: "settings.json".to_string(),
            auto_sync_secs: None,
            sync_on_start: false,
            sync_on_quit: false,
            restart_children_on_pull: false,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn plant_index_lock(tree: &Path, age: Duration) -> PathBuf {
        let lock = tree.join(".git").join("index.lock");
        std::fs::create_dir_all(tree.join(".git")).unwrap();
        std::fs::write(&lock, b"").unwrap();
        let when = std::time::SystemTime::now() - age;
        let f = std::fs::File::options().write(true).open(&lock).unwrap();
        f.set_modified(when).unwrap();
        lock
    }

    /// Blocking jobs run on the blocking pool, so a test that asserts on a result has to
    /// wait for the slot rather than for `start_job` to return. It polls rather than
    /// sleeping a fixed amount so a loaded CI box cannot make the suite flaky, and it
    /// waits for the lease as well as the slot — the repo is only free once both are.
    async fn settle(svc: &Arc<GitService>, repo_id: &str, slot: &Arc<JobSlot>) {
        for _ in 0..400 {
            if slot.is_terminal() && svc.jobs().busy(repo_id).is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("job {} never settled", slot.id);
    }

    fn merged_outcome() -> OpOutcome {
        let mut out = OpOutcome::new("merged", "main");
        out.head_before = Some("9c1f0ae".to_string());
        out.head_after = Some("b4a71dd".to_string());
        out.committed = true;
        out.pushed = true;
        out
    }

    fn next_event(rx: &mut tokio::sync::mpsc::UnboundedReceiver<HostEvent>) -> Option<HostEvent> {
        // The events are sent from the blocking thread before it returns, and `settle`
        // has already waited for that, so a non-blocking read is deterministic here.
        rx.try_recv().ok()
    }

    /// `StartOutcome` holds an `Arc<JobSlot>`, and task 5 deliberately gave `JobSlot` no
    /// `Debug` — a job record holds a `GitError` and is one careless `{:?}` away from a
    /// log line nobody audited. `expect_err` therefore will not compile against it.
    fn refused(result: Result<StartOutcome, GitError>, why: &str) -> GitError {
        match result {
            Ok(_) => panic!("{why}"),
            Err(e) => e,
        }
    }

    #[test]
    fn contained_in_accepts_a_child_and_refuses_an_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repos");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::create_dir_all(dir.path().join("secret")).unwrap();
        let root_canon = std::fs::canonicalize(&root).unwrap();

        let ok = contained_in(&root_canon, &root.join("notes")).expect("a child is contained");
        assert!(ok.ends_with("notes"));

        // The root itself is not "inside" the root: purging it would delete every repo.
        let err = contained_in(&root_canon, &root).expect_err("the root is not contained");
        assert_eq!(err.code(), GitErrorCode::PathRefused);

        let err = contained_in(&root_canon, &dir.path().join("secret"))
            .expect_err("a sibling is not contained");
        assert_eq!(err.code(), GitErrorCode::PathRefused);
        assert!(
            err.path.is_some(),
            "path_refused must name the path it refused"
        );
    }

    #[test]
    fn contained_in_resolves_a_symlink_before_deciding() {
        // The one attack this function exists for: `repos/<id>` is a symlink someone
        // planted, pointing at a tree the host does not own.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repos");
        std::fs::create_dir_all(&root).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let root_canon = std::fs::canonicalize(&root).unwrap();
        let link = root.join("notes");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, &link).unwrap();
        let err = contained_in(&root_canon, &link).expect_err("a symlink out is refused");
        assert_eq!(err.code(), GitErrorCode::PathRefused);
    }

    #[test]
    fn canonical_root_resolves_the_deepest_existing_ancestor() {
        // `repos/` does not exist before `start()` runs, and on macOS the data dir sits
        // under a symlinked /var — a non-canonical root makes every containment check
        // answer path_refused.
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            canonical_root(&dir.path().join("repos")),
            real.join("repos")
        );
        assert_eq!(
            canonical_root(&dir.path().join("a").join("b")),
            real.join("a").join("b")
        );
    }

    #[tokio::test]
    async fn no_git_section_means_no_service_and_no_directories() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::config::RuntimePaths::under(dir.path(), "com.example.test");
        paths.ensure().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let status = Arc::new(tokio::sync::RwLock::new(AppStatus::Starting));
        let svc = GitService::new(
            &config(""),
            &paths,
            tokio::runtime::Handle::current(),
            tx,
            status,
        );
        assert!(svc.is_none(), "[git] absent must yield no service");
        // The whole promise of the off switch: nothing on disk changes.
        assert!(!paths.repos_dir.exists());
        assert!(!paths.registry_file.exists());
        assert!(!paths.logs_dir.join(GIT_LOG_FILE).exists());
    }

    #[tokio::test]
    async fn a_git_section_yields_a_service_that_has_created_nothing_yet() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::config::RuntimePaths::under(dir.path(), "com.example.test");
        paths.ensure().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let status = Arc::new(tokio::sync::RwLock::new(AppStatus::Starting));
        let svc = GitService::new(
            &config("[git]\n"),
            &paths,
            tokio::runtime::Handle::current(),
            tx,
            status,
        )
        .expect("[git] present yields a service");
        assert_eq!(svc.host_instance().len(), 8);
        assert_eq!(svc.repo_count(), 0);
        assert!(svc.registry_writes());
        assert!(!svc.tray_sync() && !svc.error_dialogs() && !svc.status_api());
        assert_eq!(svc.tree_path("notes"), paths.repos_dir.join("notes"));
        assert_eq!(svc.log_path(), paths.logs_dir.join(GIT_LOG_FILE));
        // `new` reads; only `start` writes.
        assert!(!paths.repos_dir.exists());
    }

    #[tokio::test]
    async fn start_creates_the_repos_dir_and_logs_one_startup_line() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        assert!(fx.paths.repos_dir.is_dir(), "start() owns the repos root");
        let log = fx.git_log();
        let line = log.lines().last().expect("a startup line");
        // §3.18: startup records use op=startup and job=-, so one grep finds them all.
        let fields: Vec<&str> = line.split(' ').collect();
        assert!(
            fields[0].parse::<u64>().is_ok(),
            "field 0 is epoch millis: {line}"
        );
        assert_eq!(fields[1], "startup");
        assert_eq!(fields[2], "repo=-");
        assert_eq!(fields[3], "job=-");
        assert_eq!(fields[4], "ok");
        assert_eq!(fields[5], "code=-");
        assert!(line.contains(fx.svc.host_instance()), "{line}");
        assert!(line.contains("repos=0"), "{line}");
    }

    /// §9.7 promises `git.log` has URL userinfo stripped before anything is written, and
    /// `supervisor::open_log` creates that file 0644. The entry whose `insecure_remote`
    /// refusal warns that "a password in the remote URL would be copied into log lines"
    /// was the one doing the copying, on every launch, before a single job ran.
    #[tokio::test]
    async fn a_rejected_entrys_password_never_reaches_git_log() {
        const PASSWORD: &str = "hunter2SUPERSECRET";
        let fx = service_planted("[git]\n", FakeOps::ok(), |paths| {
            std::fs::write(
                &paths.registry_file,
                format!(
                    r#"{{"version":1,"repos":[{{"id":"x","remote":"https://user:{PASSWORD}@github.com/acme/x.git"}}]}}"#
                ),
            )
            .unwrap();
        })
        .await;

        let log = fx.git_log();
        assert!(!log.contains(PASSWORD), "{log}");
        // Silence would be a cheaper fix and a worse one: the operator still has to be
        // able to find the entry that was refused and the host it points at.
        assert!(log.contains("insecure_remote"), "{log}");
        assert!(log.contains("github.com"), "{log}");
    }

    #[tokio::test]
    async fn start_is_idempotent_and_a_second_run_appends() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        let before = fx.git_log().lines().count();
        fx.svc.start();
        assert_eq!(fx.git_log().lines().count(), before * 2);
    }

    #[tokio::test]
    async fn startup_clears_a_stale_index_lock_and_keeps_a_fresh_one() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("stale", repo_def("stale", None)).unwrap();
        fx.svc.put_repo("live", repo_def("live", None)).unwrap();
        let stale = plant_index_lock(&fx.svc.tree_path("stale"), Duration::from_secs(600));
        let live = plant_index_lock(&fx.svc.tree_path("live"), Duration::ZERO);

        fx.svc.start();

        assert!(
            !stale.exists(),
            "a lock older than this process is ours to clear"
        );
        // The documented case the mtime guard exists for: someone is running `git` in
        // the tree right now, and their lock is not ours to remove.
        assert!(
            live.exists(),
            "a lock newer than this process is left alone"
        );
        let log = fx.git_log();
        assert!(log.contains("repo=stale"), "{log}");
        assert!(log.contains("left in place"), "{log}");
    }

    #[tokio::test]
    async fn delete_forgets_the_definition_and_optionally_purges_the_tree() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let tree = fx.svc.tree_path("notes");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("inbox.md"), b"hi").unwrap();

        let out = fx.svc.delete_repo("notes", false).expect("delete");
        assert!(out.deleted && !out.purged);
        assert!(tree.exists(), "without ?purge the tree survives");
        assert_eq!(fx.svc.repo_count(), 0);

        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let out = fx.svc.delete_repo("notes", true).expect("purge");
        assert!(out.purged);
        assert!(!tree.exists());
        assert_eq!(out.path, tree.display().to_string());
    }

    #[tokio::test]
    async fn delete_and_put_take_the_repo_rather_than_sampling_it() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let tree = fx.svc.tree_path("notes");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("inbox.md"), b"hi").unwrap();

        // A stand-in for the window the sampled check left open: `start_job` snapshots
        // the definition and resolves credentials before it admits, so a timer tick
        // already past those steps can be admitted at any instant after a `busy()` read
        // returns. A hold is what a `busy()` sample cannot see — there is no *job*.
        let hold = fx.svc.jobs().hold_repo("notes").expect("idle repo");

        let err = fx.svc.delete_repo("notes", true).expect_err("held");
        assert_eq!(err.code(), GitErrorCode::RepoLocked);
        assert_eq!(err.http_status().as_u16(), 409);
        assert!(
            tree.join("inbox.md").exists(),
            "a purge must never run over a tree somebody else holds"
        );
        assert_eq!(fx.svc.repo_count(), 1);
        let err = fx
            .svc
            .put_repo("notes", repo_def("notes", None))
            .expect_err("held");
        assert_eq!(err.code(), GitErrorCode::RepoLocked);
        let err = refused(
            fx.svc
                .start_job("notes", JobOp::Commit, OpRequest::manual()),
            "a held repo admits no job",
        );
        assert_eq!(err.code(), GitErrorCode::RepoLocked);

        drop(hold);
        let out = fx.svc.delete_repo("notes", true).expect("released");
        assert!(out.deleted && out.purged);
        assert!(!tree.exists());
    }

    /// A guard, not a reproduction: the sampled check answered `repo_busy` here too.
    /// What it pins is that routing DELETE through `hold_repo` keeps the *job* case
    /// answering `repo_busy` and naming the op, rather than collapsing it into
    /// `repo_locked` — which is what keeps `error.job` populated for the one code
    /// `api.rs` documents a client as branching on.
    #[tokio::test]
    async fn delete_of_a_repo_with_a_job_in_flight_is_repo_busy() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let tree = fx.svc.tree_path("notes");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("inbox.md"), b"hi").unwrap();

        let Admission::Started(slot, lease) = fx
            .svc
            .jobs()
            .admit("notes", JobOp::Commit, None)
            .expect("admit")
        else {
            panic!("expected a freshly started job");
        };

        let err = fx.svc.delete_repo("notes", true).expect_err("busy");
        assert_eq!(err.code(), GitErrorCode::RepoBusy);
        assert!(err.message.contains("commit"), "{}", err.message);
        assert!(tree.join("inbox.md").exists());
        assert_eq!(fx.svc.repo_count(), 1);

        slot.succeed(&serde_json::json!({"outcome": "up_to_date"}));
        drop(lease);
        assert!(fx.svc.delete_repo("notes", true).expect("free").purged);
    }

    /// The other half of the finding: a DELETE that gets as far as removing the
    /// definition and *then* refuses the purge used to return before it forgot
    /// anything, leaving a `last_sync` row and terminal job records filed under an id
    /// the registry no longer knows — and the next `PUT` of that id inherited them.
    #[tokio::test]
    async fn a_refused_purge_still_forgets_the_state_and_the_job_records() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let started = fx
            .svc
            .start_job("notes", JobOp::Commit, OpRequest::manual())
            .expect("commit");
        settle(&fx.svc, "notes", started.slot()).await;
        assert!(fx.svc.state.last_sync("notes").is_some(), "a row to leak");

        // `repos/notes` is a symlink to a sibling *inside* the root, so `contained_in`
        // resolves it and passes while `purge_tree` is what refuses — the only DELETE
        // path that returns after the definition is already gone.
        let root = fx.paths.repos_dir.clone();
        std::fs::create_dir_all(root.join("other")).unwrap();
        let link = root.join("notes");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("other"), &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(root.join("other"), &link).unwrap();

        let err = fx
            .svc
            .delete_repo("notes", true)
            .expect_err("refused purge");
        assert_eq!(err.code(), GitErrorCode::PathRefused);
        assert_eq!(fx.svc.repo_count(), 0, "the definition still went");
        assert!(root.join("other").exists(), "nothing was removed");
        assert!(
            fx.svc.state.last_sync("notes").is_none(),
            "an unregistered id must not keep a last_sync row for the next PUT"
        );
        assert!(
            fx.svc
                .jobs()
                .list(&crate::git::jobs::JobFilter {
                    repo_id: Some("notes".to_string()),
                    ..Default::default()
                })
                .is_empty(),
            "nor its job records"
        );
    }

    #[tokio::test]
    async fn delete_of_an_unknown_repo_is_repo_not_found() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        let err = fx.svc.delete_repo("ghost", false).expect_err("unknown id");
        assert_eq!(err.code(), GitErrorCode::RepoNotFound);
    }

    #[tokio::test]
    async fn a_read_only_registry_refuses_both_mutators() {
        let fx = service("[git]\nregistry_writes = false\n", FakeOps::ok()).await;
        let err = fx
            .svc
            .put_repo("notes", repo_def("notes", None))
            .expect_err("read-only");
        assert_eq!(err.code(), GitErrorCode::RegistryReadOnly);
        let err = fx.svc.delete_repo("notes", false).expect_err("read-only");
        assert_eq!(err.code(), GitErrorCode::RegistryReadOnly);
        assert!(!fx.svc.registry_writes());
        assert!(
            fx.svc.service_info().registry_error.is_none(),
            "the config key is a choice, not a fault"
        );
    }

    /// The same 403 for the other reason, and the distinguisher that makes a second wire
    /// code unnecessary: `registry_writable: false` **with** a `registry_error` is a
    /// fault only this message names, **without** one is the config key above.
    #[tokio::test]
    async fn both_mutators_are_refused_when_repos_json_could_not_be_read() {
        // A directory where the file should be: the one read failure every OS and every
        // uid agrees on, where a `chmod 000` is a no-op under root.
        let fx = service_planted("[git]\n", FakeOps::ok(), |paths| {
            std::fs::create_dir(&paths.registry_file).unwrap();
        })
        .await;
        assert!(!fx.svc.registry_writes());
        let info = fx.svc.service_info();
        assert!(!info.registry_writable);
        assert!(info.registry_error.is_some(), "a fault, not a choice");

        let err = fx
            .svc
            .put_repo("notes", repo_def("notes", None))
            .expect_err("unreadable");
        assert_eq!(err.code(), GitErrorCode::RegistryReadOnly);
        assert_eq!(err.repo_id.as_deref(), Some("notes"));
        let err = fx.svc.delete_repo("notes", false).expect_err("unreadable");
        assert_eq!(err.code(), GitErrorCode::RegistryReadOnly);
        assert_eq!(err.repo_id.as_deref(), Some("notes"));

        let log = fx.git_log();
        assert!(log.contains("registry registry_corrupt"), "{log}");
    }

    #[tokio::test]
    async fn an_invalid_repo_id_never_reaches_the_registry() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        for bad in ["../evil", "A", "with space", ""] {
            let err = fx
                .svc
                .put_repo(bad, repo_def(bad, None))
                .expect_err("rejected");
            assert_eq!(err.code(), GitErrorCode::InvalidRepoId, "accepted {bad:?}");
        }
    }

    #[tokio::test]
    async fn service_info_reports_paths_defaults_and_retention() {
        let fx = service("[git]\ndefault_branch = \"trunk\"\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let info = fx.svc.service_info();
        assert_eq!(info.host_instance, fx.svc.host_instance());
        assert_eq!(info.repos_root, fx.paths.repos_dir.display().to_string());
        assert_eq!(
            info.registry_file,
            fx.paths.registry_file.display().to_string()
        );
        assert_eq!(info.log_file, fx.svc.log_path().display().to_string());
        assert!(info.registry_writable);
        assert!(info.registry_error.is_none());
        assert_eq!(info.defaults.branch, "trunk");
        assert_eq!(info.defaults.network_timeout_secs, 120);
        // Reported so a caller can see what a commit will be signed with without
        // having to make one: [app].name and <identifier>@<hostname> are the fallbacks.
        assert_eq!(info.defaults.author.name, "Test App");
        assert!(info.defaults.author.email.starts_with("com.example.test@"));
        assert_eq!(info.job_retention, MAX_JOB_RECORDS);
        assert_eq!(info.job_ttl_secs, JOB_TTL_SECS);
        assert_eq!(info.job_min_age_secs, JOB_MIN_AGE_SECS);
        assert_eq!(info.repo_count, 1);
        assert!(
            !info.features.tray_sync && !info.features.error_dialogs && !info.features.status_api
        );
    }

    #[tokio::test]
    async fn configured_author_overrides_the_fallback() {
        let fx = service(
            "[git]\nauthor_name = \"Notes App\"\nauthor_email = \"notes@acme.dev\"\n",
            FakeOps::ok(),
        )
        .await;
        let info = fx.svc.service_info();
        assert_eq!(info.defaults.author.name, "Notes App");
        assert_eq!(info.defaults.author.email, "notes@acme.dev");
    }

    #[tokio::test]
    async fn status_summary_never_touches_the_filesystem() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        let mut def = repo_def("notes", None);
        def.auto_sync_secs = Some(300);
        fx.svc.put_repo("notes", def).unwrap();
        // Deleting the repos root would break anything that opened a repository; the
        // summary is built from the registry, the job store and the in-memory state
        // file, and `loading.html` polls it in a tight loop.
        std::fs::remove_dir_all(&fx.paths.repos_dir).unwrap();
        let summary = fx.svc.status_summary();
        assert!(summary.registry_error.is_none());
        assert_eq!(summary.repos.len(), 1);
        let repo = &summary.repos[0];
        assert_eq!(repo.id, "notes");
        assert_eq!(repo.path, fx.svc.tree_path("notes").display().to_string());
        assert_eq!(repo.auto_sync_secs, Some(300));
        assert!(repo.busy_job.is_none());
        assert!(repo.last_sync.is_none());
    }

    #[tokio::test]
    async fn read_status_returns_the_definition_and_a_snapshot() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let both = fx.svc.read_status("notes").await.expect("status");
        assert_eq!(both.repo.id, "notes");
        assert_eq!(both.status.id, "notes");
        // Nothing has been cloned or initialised yet.
        assert!(!both.status.exists);
        assert_eq!(both.status.state, "absent");
        assert_eq!(
            both.status.path,
            fx.svc.tree_path("notes").display().to_string()
        );
    }

    #[tokio::test]
    async fn reads_of_an_unknown_or_invalid_repo_fail_before_any_io() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        let err = fx.svc.read_status("ghost").await.expect_err("unknown");
        assert_eq!(err.code(), GitErrorCode::RepoNotFound);
        let err = fx.svc.read_branches("../evil").await.expect_err("invalid");
        assert_eq!(err.code(), GitErrorCode::InvalidRepoId);
    }

    #[tokio::test]
    async fn read_branches_reports_no_worktree_for_an_absent_tree() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let err = fx.svc.read_branches("notes").await.expect_err("no tree");
        assert_eq!(err.code(), GitErrorCode::NoWorktree);
    }

    /// A `GitOps` whose `status` blocks until the test releases it — a stand-in for the
    /// wedged filesystem (an unresponsive NFS mount, a spinning-up external disk) that
    /// `STATUS_READ_TIMEOUT_MS` exists for. Overriding the read seam is the only way to
    /// hold a bounded read open deterministically; a `sleep` long enough to beat the
    /// ceiling on a loaded CI box would be a flaky test.
    struct StallingOps {
        gate: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl StallingOps {
        fn holding(rx: std::sync::mpsc::Receiver<()>) -> Arc<StallingOps> {
            Arc::new(StallingOps {
                gate: std::sync::Mutex::new(Some(rx)),
            })
        }
    }

    impl GitOps for StallingOps {
        fn run(&self, _op: JobOp, _ctx: &OpCtx) -> Result<OpOutcome, GitError> {
            Ok(OpOutcome::new("up_to_date", "main"))
        }

        fn status(&self, ctx: &ReadCtx) -> RepoStatus {
            if let Some(rx) = self.gate.lock().unwrap().take() {
                // Returns as soon as the test drops the sender. A blocking-pool thread
                // that is never released would hang the runtime's own shutdown.
                let _ = rx.recv();
            }
            ops::status(ctx)
        }
    }

    #[tokio::test]
    async fn read_status_gives_up_at_the_ceiling_and_answers_status_timeout() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let fx = service("[git]\n", StallingOps::holding(rx)).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();

        let err = fx
            .svc
            .read_status("notes")
            .await
            .expect_err("the ceiling has to bound the caller");
        assert_eq!(err.code(), GitErrorCode::StatusTimeout);
        assert_eq!(err.repo_id.as_deref(), Some("notes"));
        // §5.1 row 6 and §5.6: `status_timeout` is the one code that answers 504, and
        // this is the only path in the host that can produce it.
        assert_eq!(err.http_status().as_u16(), 504);

        // This test is the one place in the suite that pays the whole
        // STATUS_READ_TIMEOUT_MS; the constant it is proving is what makes it 2 s.
        drop(tx);
    }

    #[tokio::test]
    async fn a_job_runs_the_injected_ops_records_last_sync_and_logs_one_line() {
        let ops = FakeOps::returning(Ok(merged_outcome()));
        let fx = service("[git]\n", ops.clone()).await;
        fx.svc
            .put_repo("notes", repo_def("notes", Some(REMOTE)))
            .unwrap();

        let started = fx
            .svc
            .start_job("notes", JobOp::Sync, OpRequest::manual())
            .expect("admitted");
        let StartOutcome::Started(slot) = &started else {
            panic!("first job on an idle repo must be Started");
        };
        let slot = slot.clone();
        settle(&fx.svc, "notes", &slot).await;

        assert_eq!(ops.calls(), vec![(JobOp::Sync, "notes".to_string())]);
        assert_eq!(slot.state(), crate::git::jobs::JobState::Succeeded);
        let view = slot.view(fx.svc.host_instance());
        let result = view.result.expect("a succeeded job carries its result");
        assert_eq!(result["outcome"], "merged");
        assert_eq!(result["head_after"], "b4a71dd");

        let last = fx.svc.status_summary().repos[0]
            .last_sync
            .clone()
            .expect("last_sync survives the job");
        assert!(last.ok);
        assert_eq!(last.op, "sync");
        assert_eq!(last.job_id, slot.id.to_string());
        assert_eq!(last.outcome.as_deref(), Some("merged"));
        assert_eq!(last.head.as_deref(), Some("b4a71dd"));
        assert!(last.code.is_none());

        let log = fx.git_log();
        // Startup records carry `job=-`, so a bare `contains("job=")` finds the startup
        // line first. Select the record by the id this job actually got.
        let needle = format!("job={}", slot.id);
        let line = log
            .lines()
            .find(|l| l.contains(&needle))
            .expect("one job record");
        let fields: Vec<&str> = line.split(' ').collect();
        assert_eq!(fields[1], "sync");
        assert_eq!(fields[2], "repo=notes");
        assert_eq!(fields[3], format!("job={}", slot.id));
        assert_eq!(fields[4], "ok");
        assert_eq!(fields[5], "code=-");
        assert!(
            line.contains("merged branch=main head=b4a71dd pushed"),
            "{line}"
        );
    }

    #[tokio::test]
    async fn a_failed_job_records_the_code_and_the_message() {
        let ops = FakeOps::returning(Err(GitError::new(
            GitErrorCode::NetworkFailed,
            "could not connect to github.com",
        )));
        let fx = service("[git]\n", ops).await;
        fx.svc
            .put_repo("notes", repo_def("notes", Some(REMOTE)))
            .unwrap();
        let started = fx
            .svc
            .start_job("notes", JobOp::Pull, OpRequest::manual())
            .expect("admitted");
        let slot = started.slot().clone();
        settle(&fx.svc, "notes", &slot).await;

        assert_eq!(slot.state(), crate::git::jobs::JobState::Failed);
        // The failure lives in the job, never in an HTTP status.
        let view = slot.view(fx.svc.host_instance());
        assert_eq!(
            view.error.expect("job error").code(),
            GitErrorCode::NetworkFailed
        );
        let last = fx.svc.status_summary().repos[0].last_sync.clone().unwrap();
        assert!(!last.ok);
        assert_eq!(last.code.as_deref(), Some("network_failed"));
        let log = fx.git_log();
        assert!(log.contains("err code=network_failed"), "{log}");
    }

    #[tokio::test]
    async fn a_repeated_request_id_replays_and_the_lease_is_released() {
        // The dropped-response case: the caller never saw the 202 and asks again with
        // the same id. Replay is checked before the busy check, so it can never 409.
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc
            .put_repo("notes", repo_def("notes", Some(REMOTE)))
            .unwrap();
        let mut req = OpRequest::manual();
        req.request_id = Some("boot-1".to_string());
        let first = fx.svc.start_job("notes", JobOp::Sync, req.clone()).unwrap();
        let slot = first.slot().clone();

        // Replay comes before the busy check, so a matching request_id can never 409.
        let again = fx.svc.start_job("notes", JobOp::Sync, req).unwrap();
        let StartOutcome::Replay(replayed) = &again else {
            panic!("a repeated request_id must replay, got a different admission");
        };
        assert_eq!(replayed.id, slot.id);

        settle(&fx.svc, "notes", &slot).await;
        assert!(
            fx.svc.jobs().busy("notes").is_none(),
            "the lease is released"
        );
    }

    #[tokio::test]
    async fn remote_verbs_need_a_remote_and_reset_needs_confirmation() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();

        for op in [JobOp::Clone, JobOp::Pull, JobOp::Push] {
            let err = refused(
                fx.svc.start_job("notes", op, OpRequest::manual()),
                "no remote",
            );
            assert_eq!(err.code(), GitErrorCode::RemoteMissing, "{op:?}");
        }
        assert!(
            matches!(
                fx.svc.start_job("notes", JobOp::Sync, OpRequest::manual()),
                Ok(StartOutcome::Started(_))
            ),
            "§9.4: a local-only sync is admitted, not refused"
        );
        let err = refused(
            fx.svc.start_job("notes", JobOp::Reset, OpRequest::manual()),
            "unconfirmed reset",
        );
        assert_eq!(err.code(), GitErrorCode::ConfirmRequired);
    }

    /// §9.4: `remote: null` is "local version history for the child process".
    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_only_repo_syncs_to_a_real_commit() {
        let fx = service("[git]\n", Arc::new(RealOps)).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let tree = fx.svc.tree_path("notes");

        // Through the `init` verb and not a hand-rolled `Repository::init`: `ops::init`
        // pins refs/heads/<def.branch> against the developer's global
        // init.defaultBranch, so the branch assertion below cannot pass by accident. A
        // bare `create_dir_all` would be worse still — it leaves a plain directory that
        // is not a repository, `ops::sync` sees `tree.exists()` and skips its own init,
        // and the job dies in `open_tree`.
        let started = fx
            .svc
            .start_job("notes", JobOp::Init, OpRequest::manual())
            .unwrap();
        settle(&fx.svc, "notes", started.slot()).await;
        std::fs::write(tree.join("inbox.md"), b"# hello\n").unwrap();

        let StartOutcome::Started(slot) = fx
            .svc
            .start_job("notes", JobOp::Sync, OpRequest::manual())
            .expect("§9.4 admits a sync on a repo with no remote")
        else {
            panic!("expected a freshly started job");
        };
        settle(&fx.svc, "notes", &slot).await;

        let job = slot.view(fx.svc.host_instance());
        assert_eq!(job.state, JobState::Succeeded, "job error: {:?}", job.error);
        let result = job.result.expect("a succeeded job publishes its result");
        assert_eq!(result["outcome"], "committed"); // README.md:549, verbatim
        assert_eq!(result["committed"], true);
        assert_eq!(result["files_committed"], 1);
        assert_eq!(result["fetched"], false, "a local-only sync must not fetch");
        // `OpRequest::manual()` carries `push: true` (OpRequest::default), so this is
        // the assertion that proves `ops::sync`'s early return rather than merely a
        // missing remote: without it the job would fail in `push_branch`.
        assert_eq!(result["pushed"], false);
        assert!(result["merge"].is_null());

        // The history, not the report.
        let repo = git2::Repository::open(&tree).expect("sync left a repository");
        let head = repo.head().expect("HEAD is born after the first sync");
        assert_eq!(head.shorthand().expect("shorthand"), "main");
        let commit = head.peel_to_commit().unwrap();
        assert_eq!(
            commit.parent_count(),
            0,
            "the first sync writes a root commit"
        );
        assert!(commit
            .tree()
            .unwrap()
            .get_path(Path::new("inbox.md"))
            .is_ok());
        let first = commit.id();

        // The idempotent leg: what stops a 30-second timer writing an empty commit a
        // thousand times a day.
        let StartOutcome::Started(again) = fx
            .svc
            .start_job("notes", JobOp::Sync, OpRequest::manual())
            .unwrap()
        else {
            panic!("expected a freshly started job");
        };
        settle(&fx.svc, "notes", &again).await;
        let result = again.view(fx.svc.host_instance()).result.expect("result");
        assert_eq!(result["outcome"], "no_changes");
        assert_eq!(result["committed"], false);
        assert_eq!(
            repo.head().unwrap().target().unwrap(),
            first,
            "HEAD did not move"
        );

        // GitStatusSummary and git-state.json cope with a repo that has no remote.
        let last = fx.svc.status_summary().repos[0]
            .last_sync
            .clone()
            .expect("after_job recorded the sync");
        assert!(last.ok);
        assert_eq!(last.outcome.as_deref(), Some("no_changes"));
    }

    #[tokio::test]
    async fn a_job_for_an_unknown_or_invalid_repo_is_refused_before_admission() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        let err = refused(
            fx.svc
                .start_job("ghost", JobOp::Commit, OpRequest::manual()),
            "unknown",
        );
        assert_eq!(err.code(), GitErrorCode::RepoNotFound);
        let err = refused(
            fx.svc
                .start_job("../evil", JobOp::Commit, OpRequest::manual()),
            "invalid",
        );
        assert_eq!(err.code(), GitErrorCode::InvalidRepoId);
    }

    #[tokio::test]
    async fn git_failed_fires_on_the_transition_into_failure_and_not_again() {
        let ops = FakeOps::returning(Err(GitError::new(
            GitErrorCode::AuthFailed,
            "remote rejected the token",
        )));
        let mut fx = service("[git]\n", ops).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();

        for expected in [true, false] {
            let started = fx
                .svc
                .start_job("notes", JobOp::Commit, OpRequest::manual())
                .unwrap();
            let slot = started.slot().clone();
            settle(&fx.svc, "notes", &slot).await;
            let event = next_event(&mut fx.events);
            assert_eq!(
                event.is_some(),
                expected,
                // dialog() is modal and blocks the tao loop, so a repo whose token
                // expired with auto_sync_secs = 300 would otherwise stack a modal every
                // five minutes with the tray unreachable.
                "GitFailed must fire once per outage, not once per attempt"
            );
            if let Some(HostEvent::GitFailed {
                repo_id, op, code, ..
            }) = event
            {
                assert_eq!(repo_id, "notes");
                assert_eq!(op, "commit");
                assert_eq!(code, "auth_failed");
            }
        }
    }

    #[tokio::test]
    async fn conflicts_resolved_reports_the_merge_commit_and_the_paths() {
        let mut out = merged_outcome();
        out.merge = Some(crate::git::ops::MergeReport {
            kind: "merge_commit",
            merge_commit: Some("b4a71dd".to_string()),
            conflicts_resolved: vec!["notes/todo.md".to_string()],
            recover_hint: Some("git show b4a71dd^2:notes/todo.md".to_string()),
        });
        let mut fx = service("[git]\n", FakeOps::returning(Ok(out))).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let started = fx
            .svc
            .start_job("notes", JobOp::Commit, OpRequest::manual())
            .unwrap();
        settle(&fx.svc, "notes", started.slot()).await;

        let Some(HostEvent::GitConflictsResolved {
            repo_id,
            merge_commit,
            paths,
        }) = next_event(&mut fx.events)
        else {
            panic!("a lossy success must be reported");
        };
        assert_eq!(repo_id, "notes");
        assert_eq!(merge_commit, "b4a71dd");
        assert_eq!(paths, vec!["notes/todo.md".to_string()]);
        assert!(fx.git_log().contains("conflicts_resolved=notes/todo.md"));
    }

    #[tokio::test]
    async fn a_restart_is_requested_only_when_head_moved_and_the_app_is_ready() {
        let mut fx = service("[git]\n", FakeOps::returning(Ok(merged_outcome()))).await;
        let mut def = repo_def("notes", None);
        def.restart_children_on_pull = true;
        fx.svc.put_repo("notes", def).unwrap();

        // `OpRequest::default()` leaves `restart_children` at `None`, which falls back
        // to the repo's `restart_children_on_pull`. `auto()` and `manual()` both pin it
        // to `Some(false)` — see the next test.
        let started = fx
            .svc
            .start_job("notes", JobOp::Commit, OpRequest::default())
            .unwrap();
        settle(&fx.svc, "notes", started.slot()).await;
        assert!(matches!(
            next_event(&mut fx.events),
            Some(HostEvent::GitRestartChildren {
                reason: "requested",
                ..
            })
        ));
        let view = started.slot().view(fx.svc.host_instance());
        assert_eq!(view.result.unwrap()["restart_requested"], true);
    }

    #[tokio::test]
    async fn a_restart_needs_both_a_moved_head_and_a_caller_that_wants_one() {
        let mut def = repo_def("notes", None);
        def.restart_children_on_pull = true;

        // `up_to_date` leaves head_before == head_after. This is what stops a
        // five-minute auto-sync from restarting Chrome 288 times a day.
        let mut fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", def.clone()).unwrap();
        let started = fx
            .svc
            .start_job("notes", JobOp::Commit, OpRequest::default())
            .unwrap();
        settle(&fx.svc, "notes", started.slot()).await;
        assert!(
            next_event(&mut fx.events).is_none(),
            "an unmoved head cannot restart"
        );

        // A background trigger pins `restart_children` to `Some(false)`, and no config
        // key can turn it on: only an explicit HTTP call, or a real settings change,
        // may restart the user's window.
        let mut fx = service("[git]\n", FakeOps::returning(Ok(merged_outcome()))).await;
        def.remote = Some(REMOTE.to_string());
        fx.svc.put_repo("notes", def).unwrap();
        let started = fx
            .svc
            .start_job("notes", JobOp::Sync, OpRequest::auto())
            .unwrap();
        settle(&fx.svc, "notes", started.slot()).await;
        assert!(
            next_event(&mut fx.events).is_none(),
            "auto-sync never restarts"
        );
    }

    #[tokio::test]
    async fn a_restart_before_the_app_is_ready_is_deferred_into_a_warning() {
        let mut fx = service("[git]\n", FakeOps::returning(Ok(merged_outcome()))).await;
        *fx.status.write().await = AppStatus::Starting;
        let mut def = repo_def("notes", None);
        def.restart_children_on_pull = true;
        fx.svc.put_repo("notes", def).unwrap();
        let started = fx
            .svc
            .start_job("notes", JobOp::Commit, OpRequest::default())
            .unwrap();
        settle(&fx.svc, "notes", started.slot()).await;

        assert!(
            next_event(&mut fx.events).is_none(),
            "no restart mid-startup"
        );
        let view = started.slot().view(fx.svc.host_instance());
        let result = view.result.unwrap();
        assert_eq!(result["restart_requested"], false);
        assert_eq!(result["warnings"][0]["code"], "restart_deferred");
    }

    #[tokio::test]
    async fn the_watchdog_marks_a_live_job_stalled_and_trips_its_abort_flag() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        fx.svc.put_repo("notes", repo_def("notes", None)).unwrap();
        let started = fx
            .svc
            .start_job("notes", JobOp::Init, OpRequest::manual())
            .unwrap();
        let slot = started.slot().clone();
        settle(&fx.svc, "notes", &slot).await;

        // A terminal job is never touched: the deadline outliving the work is the
        // normal case, and a stalled flag on a finished job would be a lie.
        assert!(!trip_watchdog(&slot));
        assert!(!slot.view(fx.svc.host_instance()).stalled);

        // Against a live job the flag goes up and the reason is Timeout, which is what
        // makes `classify` report `timeout` rather than `network_failed`.
        let live = fx
            .svc
            .jobs()
            .admit("other", JobOp::Sync, None)
            .expect("admitted");
        let Admission::Started(live_slot, _lease) = live else {
            panic!("an idle repo must admit");
        };
        assert!(trip_watchdog(&live_slot));
        assert!(live_slot.view(fx.svc.host_instance()).stalled);
        assert!(live_slot.abort.is_aborted());
        assert_eq!(live_slot.abort.reason(), AbortReason::Timeout);
    }

    #[tokio::test]
    async fn sync_all_manual_admits_one_job_per_repo_and_skips_busy_ones() {
        let ops = FakeOps::ok();
        let fx = service("[git]\n", ops.clone()).await;
        fx.svc.put_repo("a", repo_def("a", Some(REMOTE))).unwrap();
        fx.svc.put_repo("b", repo_def("b", Some(REMOTE))).unwrap();

        fx.svc.sync_all_manual();
        for id in ["a", "b"] {
            for _ in 0..400 {
                if fx.svc.jobs().busy(id).is_none() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        let mut ran: Vec<String> = ops.calls().into_iter().map(|(_, id)| id).collect();
        ran.sort();
        assert_eq!(ran, vec!["a".to_string(), "b".to_string()]);
        assert!(fx.git_log().contains("manual: started"), "{}", fx.git_log());
    }

    #[tokio::test]
    async fn sync_on_start_only_touches_repos_that_asked_for_it() {
        let ops = FakeOps::ok();
        let fx = service("[git]\n", ops.clone()).await;
        let mut eager = repo_def("eager", Some(REMOTE));
        eager.sync_on_start = true;
        fx.svc.put_repo("eager", eager).unwrap();
        fx.svc.put_repo("quiet", repo_def("quiet", None)).unwrap();

        fx.svc.sync_on_start();
        for _ in 0..400 {
            if fx.svc.jobs().busy("eager").is_none() && !ops.calls().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(ops.calls(), vec![(JobOp::Sync, "eager".to_string())]);
    }

    #[tokio::test]
    async fn an_auto_sync_repo_gets_exactly_one_timer() {
        let fx = service("[git]\n", FakeOps::ok()).await;
        let mut def = repo_def("notes", None);
        def.auto_sync_secs = Some(300);
        fx.svc.put_repo("notes", def.clone()).unwrap();
        assert_eq!(fx.svc.timer_ids(), vec!["notes".to_string()]);
        // A second PUT must not start a second ticker for the same repo.
        fx.svc.put_repo("notes", def).unwrap();
        assert_eq!(fx.svc.timer_ids(), vec!["notes".to_string()]);
        // A repo with no interval never gets one.
        fx.svc.put_repo("quiet", repo_def("quiet", None)).unwrap();
        assert_eq!(fx.svc.timer_ids(), vec!["notes".to_string()]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn quit_syncs_run_then_drain_the_store() {
        let ops = FakeOps::ok();
        let fx = service("[git]\n", ops.clone()).await;
        let mut leaving = repo_def("leaving", Some(REMOTE));
        leaving.sync_on_quit = true;
        fx.svc.put_repo("leaving", leaving).unwrap();
        fx.svc
            .put_repo("staying", repo_def("staying", Some(REMOTE)))
            .unwrap();

        fx.svc.run_quit_syncs(Duration::from_secs(5));
        println!(
            "PROBE log:
{}",
            fx.git_log()
        );
        println!("PROBE calls: {:?}", ops.calls());

        assert_eq!(ops.calls(), vec![(JobOp::Sync, "leaving".to_string())]);
        // Every later admission answers `shutting_down` (503): the window is closing
        // and a new job could only be abandoned.
        assert!(fx.svc.draining());
        let err = refused(
            fx.svc
                .start_job("staying", JobOp::Sync, OpRequest::manual()),
            "draining",
        );
        assert_eq!(err.code(), GitErrorCode::ShuttingDown);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_zero_quit_timeout_skips_the_syncs_but_still_drains() {
        let ops = FakeOps::ok();
        let fx = service("[git]\n", ops.clone()).await;
        let mut leaving = repo_def("leaving", Some(REMOTE));
        leaving.sync_on_quit = true;
        fx.svc.put_repo("leaving", leaving).unwrap();

        fx.svc.run_quit_syncs(Duration::ZERO);

        assert!(ops.calls().is_empty(), "0 disables the whole step");
        assert!(fx.svc.draining());
    }

    /// A `GitOps` that replays a fixed script of successes and failures, so the
    /// `after_job` notification path can be driven through an outage and back
    /// with no network, no remote and no real work tree.
    struct ScriptedOps {
        script: Vec<bool>, // true = succeed
        calls: std::sync::atomic::AtomicUsize,
    }

    impl GitOps for ScriptedOps {
        fn run(&self, _op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.script[n] {
                Ok(OpOutcome::new("no_changes", &ctx.def.branch))
            } else {
                Err(GitError::new(GitErrorCode::AuthFailed, "token expired"))
            }
        }
    }

    /// Start one job and wait until the repo's busy slot clears, which happens
    /// when the `RepoLease` drops — i.e. after the job and its notification
    /// have run. Keeps the four jobs strictly sequential so the event order is
    /// deterministic.
    async fn run_one_job(svc: &std::sync::Arc<GitService>, id: &str) {
        match svc.start_job(id, JobOp::Commit, OpRequest::manual()) {
            Ok(StartOutcome::Started(_)) => {}
            Ok(_) => panic!("expected a freshly started job, got a replay or a busy repo"),
            Err(e) => panic!("start_job refused the job: {e}"),
        }
        for _ in 0..400 {
            if svc.jobs().busy(id).is_none() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("job did not finish within 2s");
    }

    async fn next_git_failed(rx: &mut tokio::sync::mpsc::UnboundedReceiver<HostEvent>) -> String {
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
            Ok(Some(HostEvent::GitFailed { code, .. })) => code,
            Ok(Some(other)) => panic!("unexpected event before GitFailed: {other:?}"),
            Ok(None) => panic!("event channel closed"),
            Err(_) => panic!("timed out waiting for GitFailed"),
        }
    }

    #[tokio::test]
    async fn git_failed_is_emitted_only_on_an_ok_to_fail_transition() {
        // dialog() is modal and blocks the tao loop, menu clicks included. A
        // repo whose token expired with auto_sync_secs = 300 must therefore
        // produce ONE dialog per outage, not one every five minutes forever
        // with the tray unreachable to turn it off. That is what this asserts:
        // fail, fail, succeed, fail => exactly two events.
        let dir = tempfile::tempdir().unwrap();
        let cfg = AppConfig::from_str(
            "[app]\nname = \"X\"\nidentifier = \"com.example.x\"\n\
             [git]\nerror_dialogs = true\n",
        )
        .unwrap();
        let paths = RuntimePaths::under(dir.path(), "com.example.x");
        paths.ensure().unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let status = std::sync::Arc::new(tokio::sync::RwLock::new(AppStatus::Ready {
            target_url: "http://x/".to_string(),
        }));
        let ops = std::sync::Arc::new(ScriptedOps {
            script: vec![false, false, true, false],
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let svc = GitService::with_ops(
            &cfg,
            &paths,
            tokio::runtime::Handle::current(),
            tx,
            status,
            ops,
        );
        svc.start();
        svc.put_repo(
            "notes",
            serde_json::from_value(serde_json::json!({ "id": "notes" })).unwrap(),
        )
        .unwrap();

        for _ in 0..4 {
            run_one_job(&svc, "notes").await;
        }

        assert_eq!(next_git_failed(&mut rx).await, "auth_failed");
        assert_eq!(next_git_failed(&mut rx).await, "auth_failed");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
                .await
                .is_err(),
            "a third event arrived: the ok -> fail gate is not holding, and a stuck repo \
             will stack a modal per sync"
        );
    }

    #[test]
    fn an_automatic_sync_can_never_restart_the_children() {
        // restart_children is hard-coded on OpRequest::auto(), and there is no
        // config key anywhere that can flip it. Only an explicit HTTP call
        // (PullBody/SyncBody.restart_children), a repo's
        // restart_children_on_pull, or a real sync_settings change may tear
        // down the user's window — never a timer. Without this, a 5-minute
        // auto-sync on a repo with restart_children_on_pull would restart
        // Chrome 288 times a day.
        assert_eq!(OpRequest::auto().restart_children, Some(false));
        // Tray "Sync now" and the quit sync take the same line: the user asked
        // to sync, not to have their window replaced.
        assert_eq!(OpRequest::manual().restart_children, Some(false));
    }

    /// A `GitOps` that reports a settings outcome without touching a work tree,
    /// so `after_job`'s restart decision can be driven directly with no remote,
    /// no libgit2 and no filesystem.
    struct SettingsOps {
        changed: bool,
    }

    impl GitOps for SettingsOps {
        fn run(&self, _op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError> {
            let mut out = OpOutcome::new("merged", &ctx.def.branch);
            // HEAD moved in both cases, so the only variable between the two
            // tests is whether the settings values actually changed.
            out.head_before = Some("1111111111111111111111111111111111111111".to_string());
            out.head_after = Some("2222222222222222222222222222222222222222".to_string());
            out.settings_synced = true;
            out.settings_changed = self.changed;
            Ok(out)
        }
    }

    /// Run one auto-triggered sync on a `sync_settings` repo and return every
    /// host event it produced.
    ///
    /// `OpRequest::auto()` is the point of the test: it hard-codes
    /// `restart_children: Some(false)`, which is what every timer and every
    /// `sync_on_start` uses.
    async fn settings_sync_events(changed: bool) -> Vec<HostEvent> {
        let dir = tempfile::tempdir().unwrap();
        let cfg = AppConfig::from_str(
            "[app]\nname = \"X\"\nidentifier = \"com.example.x\"\n\
             [menu]\nsettings = true\n\
             [[settings.fields]]\n\
             key = \"theme\"\nlabel = \"Theme\"\ntype = \"select\"\n\
             default = \"light\"\noptions = [\"light\", \"dark\"]\n\
             [git]\n",
        )
        .unwrap();
        let paths = RuntimePaths::under(dir.path(), "com.example.x");
        paths.ensure().unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let status = std::sync::Arc::new(tokio::sync::RwLock::new(AppStatus::Ready {
            target_url: "http://x/".to_string(),
        }));
        let svc = GitService::with_ops(
            &cfg,
            &paths,
            tokio::runtime::Handle::current(),
            tx,
            status,
            std::sync::Arc::new(SettingsOps { changed }),
        );
        svc.start();
        // No `remote`: §9.4's local-only repo is admitted for `sync`, and `SettingsOps`
        // never opens anything anyway. The earlier fake URL existed only to get past an
        // admission guard that should never have refused `sync` in the first place —
        // which makes this fixture a free mutation check on that guard.
        svc.put_repo(
            "notes",
            serde_json::from_value(serde_json::json!({
                "id": "notes",
                "sync_settings": true,
            }))
            .unwrap(),
        )
        .unwrap();

        match svc.start_job("notes", JobOp::Sync, OpRequest::auto()) {
            Ok(StartOutcome::Started(_)) => {}
            Ok(_) => panic!("expected a freshly started job, got a replay or a busy repo"),
            Err(e) => panic!("start_job refused the job: {e}"),
        }
        // The busy slot clears when the RepoLease drops, which is after the job
        // and its notification have both run.
        let mut finished = false;
        for _ in 0..400 {
            if svc.jobs().busy("notes").is_none() {
                finished = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(finished, "job did not finish within 2s");

        let mut events = Vec::new();
        while let Ok(Some(e)) =
            tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
        {
            events.push(e);
        }
        events
    }

    #[tokio::test]
    async fn a_settings_change_restarts_children_even_when_the_request_said_no() {
        let events = settings_sync_events(true).await;
        assert_eq!(
            events.len(),
            1,
            "expected exactly one restart request, got {events:?}"
        );
        match &events[0] {
            HostEvent::GitRestartChildren { repo_id, reason } => {
                assert_eq!(repo_id, "notes");
                assert_eq!(
                    *reason, "settings",
                    "a restart caused by the settings mirror must say so"
                );
            }
            other => panic!("expected GitRestartChildren, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_sync_that_changed_no_settings_restarts_nothing() {
        // The auto-sync case: HEAD moved, the request said not to restart, and
        // the settings were identical. A five-minute timer must not relaunch
        // the user's window 288 times a day.
        let events = settings_sync_events(false).await;
        assert!(events.is_empty(), "expected no host events, got {events:?}");
    }

    // ── network integration tests ─────────────────────────────────────────
    // The only tests in this crate that touch a real remote, and therefore the
    // only #[ignore]d ones here. They live in mod.rs rather than in ops.rs or
    // creds.rs because each exercises the whole GitService -> jobs -> ops ->
    // creds path; none is a unit test of a single file. CI runs
    // `cargo test --locked` with no secrets and no network.

    use super::{GitService, StartOutcome};
    use crate::config::{AppConfig, RuntimePaths};
    use crate::git::jobs::{JobOp, JobState, JobView};
    use crate::git::ops::OpRequest;
    use crate::git::registry::{CredentialSpec, RepoDef};
    use crate::git::secret::Secret;
    use crate::git::state::StateStore;
    use crate::internal_server::{AppStatus, HostEvent};
    use std::path::Path;
    use std::sync::Arc;

    const TEST_TOML: &str = r#"
[app]
name = "GitNetTest"
identifier = "com.example.gitnettest"

[git]
default_branch = "main"
network_timeout_secs = 60
"#;

    /// Owns the tokio runtime and the event receiver for as long as the test
    /// runs. Dropping the runtime would cancel the `spawn_blocking` the job is
    /// executing on, and `run_job` below would then spin until its deadline on
    /// a job that no longer has a worker.
    struct Harness {
        #[allow(dead_code)]
        rt: tokio::runtime::Runtime,
        #[allow(dead_code)]
        events: tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
        paths: RuntimePaths,
        svc: Arc<GitService>,
    }

    fn harness(base: &Path) -> Harness {
        let cfg = AppConfig::from_str(TEST_TOML).expect("test config parses");
        let paths = RuntimePaths::under(base, &cfg.app.identifier);
        paths.ensure().expect("create data dirs");
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let (tx, events) = tokio::sync::mpsc::unbounded_channel();
        // Ready, so `after_job` is not in its "defer the restart" branch.
        let status = Arc::new(tokio::sync::RwLock::new(AppStatus::Ready {
            target_url: "http://127.0.0.1:1/".to_string(),
        }));
        let svc = GitService::new(&cfg, &paths, rt.handle().clone(), tx, status)
            .expect("[git] is present in TEST_TOML, so the service exists");
        svc.start();
        Harness {
            rt,
            events,
            paths,
            svc,
        }
    }

    fn def(id: &str, remote: &str, credential: Option<CredentialSpec>) -> RepoDef {
        RepoDef {
            id: id.to_string(),
            remote: Some(remote.to_string()),
            remote_name: "origin".to_string(),
            // "" and 0 are the sentinels Registry::put normalises: the branch
            // is filled from [git].default_branch, the timestamps from now.
            branch: String::new(),
            credential,
            author: None,
            sync_settings: false,
            settings_path: "settings.json".to_string(),
            auto_sync_secs: None,
            sync_on_start: false,
            sync_on_quit: false,
            restart_children_on_pull: false,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    /// Admit a job and block until it is terminal. Sleeping is legal here and
    /// nowhere else in this crate: these tests wait on a real network, so the
    /// zero-sleep rule the `jobs.rs` tests hold to does not apply.
    fn run_job(h: &Harness, id: &str, op: JobOp) -> JobView {
        let slot = match h
            .svc
            .start_job(id, op, OpRequest::manual())
            .expect("job admitted")
        {
            StartOutcome::Started(s) | StartOutcome::Replay(s) | StartOutcome::Busy(s) => s,
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        while !slot.is_terminal() {
            assert!(
                std::time::Instant::now() < deadline,
                "job {} never reached a terminal state",
                slot.id
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        slot.view(h.svc.host_instance())
    }

    fn code_of(view: &JobView) -> &'static str {
        view.error.as_ref().map(|e| e.code.as_str()).unwrap_or("-")
    }

    fn message_of(view: &JobView) -> Option<String> {
        view.error.as_ref().map(|e| e.message.clone())
    }

    /// The canary for the vendored-TLS story. A `vendored-libgit2` +
    /// vendored-OpenSSL build has to find the machine's CA bundle by itself
    /// (libgit2's initialiser probes for it). If this fails on a distro, every
    /// `https://` remote fails on that distro — so run it manually on every
    /// platform the app ships to.
    #[test]
    #[ignore = "requires network; run with cargo test -- --ignored"]
    fn vendored_tls_trusts_public_ca() {
        let tmp = tempfile::tempdir().unwrap();
        let h = harness(tmp.path());
        h.svc
            .put_repo(
                "libc",
                def("libc", "https://github.com/rust-lang/libc.git", None),
            )
            .expect("define repo");

        let view = run_job(&h, "libc", JobOp::Clone);
        assert_eq!(
            view.state,
            JobState::Succeeded,
            "clone failed [{}]: {:?}",
            code_of(&view),
            message_of(&view)
        );
        assert!(
            h.svc.tree_path("libc").join(".git").is_dir(),
            "clone reported success but left no .git directory"
        );
    }

    /// The whole HTTPS + stored-credential path against a real server,
    /// including a *server-side* rejection. That last part cannot be covered
    /// offline: libgit2's local transport does not run server hooks, so the
    /// bare-repo fixtures in `ops.rs` can produce a client-side non-fast-
    /// forward but never a `push_update_reference` status line from a server.
    ///
    /// `GIT_TEST_HTTPS_URL` must be a scratch repository you own that already
    /// has a `main` branch with at least one commit; this test pushes to it.
    #[test]
    #[ignore = "requires GIT_TEST_HTTPS_URL and GIT_TEST_TOKEN"]
    fn real_https_clone_and_push_and_server_rejection() {
        let (url, token) = match (
            std::env::var("GIT_TEST_HTTPS_URL"),
            std::env::var("GIT_TEST_TOKEN"),
        ) {
            (Ok(u), Ok(t)) => (u, t),
            _ => panic!("set GIT_TEST_HTTPS_URL and GIT_TEST_TOKEN"),
        };
        let cred = CredentialSpec::Token {
            username: "x-access-token".to_string(),
            token: Secret::new(token.clone()),
            bound_host: None,
        };
        let tmp = tempfile::tempdir().unwrap();
        let h = harness(tmp.path());

        // Two independent clones of the same remote, so one can get ahead of
        // the other without a second process.
        for id in ["alpha", "beta"] {
            h.svc
                .put_repo(id, def(id, &url, Some(cred.clone())))
                .expect("define repo");
            let view = run_job(&h, id, JobOp::Clone);
            assert_eq!(
                view.state,
                JobState::Succeeded,
                "{id} clone failed [{}]: {:?}",
                code_of(&view),
                message_of(&view)
            );
        }

        // alpha advances the remote.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        std::fs::write(
            h.svc.tree_path("alpha").join("host-test.txt"),
            format!("{stamp}\n"),
        )
        .unwrap();
        let view = run_job(&h, "alpha", JobOp::Sync);
        assert_eq!(
            view.state,
            JobState::Succeeded,
            "alpha sync failed [{}]: {:?}",
            code_of(&view),
            message_of(&view)
        );
        let result = view.result.as_ref().expect("a succeeded job has a result");
        assert_eq!(result["pushed"], serde_json::Value::Bool(true));

        // Leak canary: a stored PAT must not appear anywhere on the wire type.
        let body = serde_json::to_string(&view).unwrap();
        assert!(!body.contains(&token), "the job view leaked the token");

        // beta is now one commit behind, so its push must be refused.
        std::fs::write(h.svc.tree_path("beta").join("host-test.txt"), "divergent\n").unwrap();
        let view = run_job(&h, "beta", JobOp::Commit);
        assert_eq!(
            view.state,
            JobState::Succeeded,
            "beta commit failed [{}]: {:?}",
            code_of(&view),
            message_of(&view)
        );
        let view = run_job(&h, "beta", JobOp::Push);
        assert_eq!(
            view.state,
            JobState::Failed,
            "a diverged push was accepted — nothing rejected it"
        );
        // Either end may catch it first: libgit2 can refuse a non-fast-forward
        // refspec locally, or the server can answer with a rejection status.
        assert!(
            matches!(code_of(&view), "push_rejected" | "not_fast_forward"),
            "unexpected rejection code {}: {:?}",
            code_of(&view),
            message_of(&view)
        );
    }

    /// TOFU host-key pinning, end to end: the first SSH clone learns the
    /// remote's key into `git-state.json`, and a second host whose pin does
    /// not match refuses to talk to that remote at all.
    ///
    /// `GIT_TEST_SSH_KEY` is the path to an *unencrypted* private key with
    /// access to `GIT_TEST_SSH_URL`.
    #[test]
    #[ignore = "requires GIT_TEST_SSH_URL and GIT_TEST_SSH_KEY"]
    fn real_ssh_clone_pins_the_host_key() {
        let (url, key) = match (
            std::env::var("GIT_TEST_SSH_URL"),
            std::env::var("GIT_TEST_SSH_KEY"),
        ) {
            (Ok(u), Ok(k)) => (u, k),
            _ => panic!("set GIT_TEST_SSH_URL and GIT_TEST_SSH_KEY"),
        };
        let cred = CredentialSpec::SshKey {
            username: "git".to_string(),
            private_key_path: Some(std::path::PathBuf::from(&key)),
            private_key: None,
            public_key_path: None,
            public_key: None,
            passphrase: None,
            bound_host: None,
        };
        let tmp = tempfile::tempdir().unwrap();

        // First host: clones, and learns the fingerprint on the way.
        let state_file = {
            let h = harness(tmp.path());
            h.svc
                .put_repo("sshrepo", def("sshrepo", &url, Some(cred)))
                .expect("define repo");
            let view = run_job(&h, "sshrepo", JobOp::Clone);
            assert_eq!(
                view.state,
                JobState::Succeeded,
                "ssh clone failed [{}]: {:?}",
                code_of(&view),
                message_of(&view)
            );
            // Clear the tree so the second host has to reach the network again.
            std::fs::remove_dir_all(h.svc.tree_path("sshrepo")).unwrap();
            h.paths.git_state_file.clone()
        };

        let learned = StateStore::load(&state_file)
            .fingerprint("sshrepo")
            .expect("TOFU recorded a fingerprint");
        assert!(
            learned.starts_with("SHA256:"),
            "unexpected fingerprint format: {learned}"
        );

        // Poison the pin. A fresh host must now refuse the same remote, which
        // is the whole point of pinning: repos.json survives, only the pin
        // changed.
        StateStore::load(&state_file)
            .record_fingerprint("sshrepo", &format!("SHA256:{}", "A".repeat(43)));

        let h = harness(tmp.path());
        let view = run_job(&h, "sshrepo", JobOp::Clone);
        assert_eq!(
            view.state,
            JobState::Failed,
            "a mismatched host-key pin was accepted"
        );
        assert_eq!(
            code_of(&view),
            "host_key_mismatch",
            "{:?}",
            message_of(&view)
        );
    }

    /// libgit2 re-invokes the credentials callback up to fifteen times before
    /// giving up ("too many redirects or authentication replays"), retrying the
    /// same rejected identity each time. `creds::callbacks` self-limits at
    /// MAX_CRED_ATTEMPTS, and this is the only honest test of that cap: it
    /// needs a server that actually rejects. No env vars — the URL is a
    /// repository that does not exist, which GitHub answers with a credential
    /// challenge rather than a 404.
    #[test]
    #[ignore = "requires network; asserts a wrong token fails fast instead of looping 15×"]
    fn wrong_token_fails_once() {
        let bogus = "this-is-not-a-valid-token";
        let tmp = tempfile::tempdir().unwrap();
        let h = harness(tmp.path());
        let cred = CredentialSpec::Token {
            username: "x-access-token".to_string(),
            token: Secret::new(bogus.to_string()),
            bound_host: None,
        };
        h.svc
            .put_repo(
                "nope",
                def(
                    "nope",
                    "https://github.com/rust-lang/does-not-exist-9f3a2b41.git",
                    Some(cred),
                ),
            )
            .expect("define repo");

        let started = std::time::Instant::now();
        let view = run_job(&h, "nope", JobOp::Clone);
        let elapsed = started.elapsed();

        assert_eq!(
            view.state,
            JobState::Failed,
            "a bogus token was accepted for a nonexistent repository"
        );
        assert_eq!(code_of(&view), "auth_failed", "{:?}", message_of(&view));
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "took {elapsed:?} — the credential retry cap is not holding"
        );
        let body = serde_json::to_string(&view).unwrap();
        assert!(!body.contains(bogus), "the error leaked the token");
    }
}
