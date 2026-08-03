### Task 10: Host wiring

Tasks 1–9 built a git subsystem that talks to nobody. This task connects it to the running
app: the `[server]` child learns how to call back into the host, the tao event loop learns to
consume the three new `HostEvent` variants, the tray grows a "Sync now" entry, `/api/status`
grows an optional `git` block, and `quit()` learns to run the quit syncs.

**The one rule this task exists to preserve — state it in review, and check every line against
it:**

> The git subsystem never touches `App`, `Children`, `chrome_generation`, `server_generation`,
> `rt.enter()`, or `tokio::process`. Its only channel into the tao loop is `HostEvent`.

That is a one-way door. `App` may call `GitService` (`start`, `sync_on_start`,
`sync_all_manual`, `run_quit_syncs`, `repo_count`); `GitService` may only *send*. The reason is
mechanical, not aesthetic: `App::restart()` → `App::kill_children()` → `self.rt.block_on(...)`,
and `Handle::block_on` **panics** when called from inside a runtime worker thread. Every git
job runs on `spawn_blocking`, i.e. inside the runtime. A `GitService` holding an
`Arc<Mutex<Children>>` and calling `restart()` directly would compile, pass a unit test, and
abort the process the first time a real pull moved HEAD. The `HostEvent` hop is what moves the
call back onto the tao thread, which is the only thread allowed to `block_on`. **The hop is
load-bearing.**

No new `rt.enter()` guard appears anywhere in this task. That guard exists in this codebase
solely because `tokio::process::Command::spawn` registers a child with the runtime's signal
driver; git spawns no OS processes, and `Handle::spawn`/`Handle::spawn_blocking` are callable
from any thread without one.

**Files:**

- Modify: `src/supervisor.rs:40-55` (`build_env` + the new `HostAccess`),
  `src/supervisor.rs:140-151` (test `build_env_layers_and_settings_file`).
- Modify: `src/host.rs:16-38` (`App` fields), `src/host.rs:40` (constants + free helpers go
  immediately above `impl App {`), `src/host.rs:90-96` (`refresh_tray`'s `menu_model` call),
  `src/host.rs:205-208` (the `supervisor::build_env` call inside `start_children`),
  `src/host.rs:328-331` (`quit`), `src/host.rs:446` (new `#[cfg(test)] mod tests` at EOF).
- Modify: `src/tray.rs:6-13` (`TrayAction`), `src/tray.rs:21-47` (`menu_model`),
  `src/tray.rs:122-152` (both tests).
- Modify: `src/internal_server.rs` — the `StatusResponse` struct next to `AppStatus`,
  `async fn api_status` (~line 165), and two new tests.
- Modify: `src/main.rs` — `fn main()` (token hoist + `GitService::new` + `HostState.git`),
  `fn run(` (two new parameters + the `App` literal), the `event_loop.run` match (four new
  arms), and the deletion of `#[expect(dead_code)]` from `notice`.
- Modify: `src/git/mod.rs` — two tests appended to its `#[cfg(test)] mod tests`. This is the
  only file under `src/git/` this task touches, and it adds tests only.

Line numbers for `src/host.rs` are exact (task 0 created it and nothing between tasks 1 and 9
edits it). `src/main.rs` numbers moved by +1 when task 1 inserted `mod git;`, and
`src/internal_server.rs` moved when task 9 added `HostState.git` and the `/api/git` nest —
anchor on the quoted line text, not the number.

**Interfaces:**

- Consumes (all already landed; nothing here defines them):

```rust
// task 2 — src/config.rs
pub struct GitSection { pub tray_sync: bool, pub error_dialogs: bool, pub status_api: bool,
                        pub quit_sync_timeout_secs: u64, /* … */ }
impl AppConfig { pub fn git_enabled(&self) -> bool; }        // self.git.is_some()
pub git: Option<GitSection>                                   // AppConfig's last field

// task 9 — src/internal_server.rs
pub enum HostEvent {
    RestartRequested,
    GitRestartChildren { repo_id: String, reason: &'static str },
    GitFailed { repo_id: String, op: String, code: String, message: String },
    GitConflictsResolved { repo_id: String, merge_commit: String, paths: Vec<String> },
}
pub struct HostState { /* … */ pub git: Option<Arc<crate::git::GitService>> }

// task 9 — src/git/mod.rs
impl GitService {
    pub fn new(cfg: &AppConfig, paths: &RuntimePaths, rt: tokio::runtime::Handle,
               events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
               status: std::sync::Arc<tokio::sync::RwLock<AppStatus>>)
        -> Option<std::sync::Arc<GitService>>;
    #[cfg(test)]
    pub fn with_ops(cfg: &AppConfig, paths: &RuntimePaths, rt: tokio::runtime::Handle,
                    events: tokio::sync::mpsc::UnboundedSender<HostEvent>,
                    status: std::sync::Arc<tokio::sync::RwLock<AppStatus>>,
                    ops: std::sync::Arc<dyn GitOps>) -> std::sync::Arc<GitService>;
    pub fn start(self: &std::sync::Arc<Self>);
    pub fn sync_on_start(self: &std::sync::Arc<Self>);
    pub fn sync_all_manual(self: &std::sync::Arc<Self>);
    pub fn run_quit_syncs(self: &std::sync::Arc<Self>, timeout: std::time::Duration);
    pub fn start_job(self: &std::sync::Arc<Self>, repo_id: &str, op: JobOp, req: OpRequest)
        -> Result<StartOutcome, GitError>;
    pub fn put_repo(self: &std::sync::Arc<Self>, id: &str, body: RepoDef)
        -> Result<PutOutcome, GitError>;
    pub fn status_summary(&self) -> GitStatusSummary;
    pub fn repo_count(&self) -> usize;
    pub fn status_api(&self) -> bool;
    pub fn jobs(&self) -> &std::sync::Arc<JobStore>;
}
pub trait GitOps: Send + Sync { fn run(&self, op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError>; }
pub enum StartOutcome { Started(Arc<JobSlot>), Replay(Arc<JobSlot>), Busy(Arc<JobSlot>) }

// task 6 — src/git/ops.rs
impl OpRequest { pub fn auto() -> OpRequest; pub fn manual() -> OpRequest; }
impl OpOutcome { pub fn new(outcome: &'static str, branch: &str) -> OpOutcome; }

// task 0 — src/main.rs
pub(crate) fn dialog(title: &str, description: &str);      // rfd MessageLevel::Error
pub(crate) fn notice(title: &str, description: &str);      // rfd MessageLevel::Warning
```

- Produces (task 11 consumes the `GitRestartChildren` arm; task 12 documents all of it):

```rust
// src/supervisor.rs
pub struct HostAccess<'a> { pub url: &'a str, pub token: &'a str, pub git_enabled: bool }
pub fn build_env(cfg_env: &BTreeMap<String, String>, settings_env: &[(String, String)],
                 settings_file: &Path, host: &HostAccess<'_>) -> Vec<(String, String)>;

// src/tray.rs
pub enum TrayAction { Open, OpenUrl(String), Settings, GitSync, Restart, Quit }
pub fn menu_model(menu: &MenuSection, app_name: &str, settings_enabled: bool,
                  show_open: bool, show_sync: bool) -> Vec<Entry>;

// src/internal_server.rs
#[derive(serde::Serialize)]
pub(crate) struct StatusResponse {
    #[serde(flatten)] pub app: AppStatus,
    #[serde(skip_serializing_if = "Option::is_none")] pub git: Option<crate::git::GitStatusSummary>,
}

// src/host.rs
pub(crate) const RESTART_DEBOUNCE_MS: u64 = 2_000;
pub(crate) const GIT_NOTICE_HISTORY: usize = 16;
impl App {
    pub(crate) git: Option<std::sync::Arc<crate::git::GitService>>,   // field
    pub(crate) host_token: String,                                    // field
    pub(crate) last_git_restart: Option<std::time::Instant>,          // field
    pub(crate) git_notices: std::collections::VecDeque<String>,       // field
    pub(crate) fn git_restart_ok(&mut self) -> bool;
    pub(crate) fn git_notice_is_new(&mut self, merge_commit: &str) -> bool;
    pub(crate) fn git_log_path(&self) -> std::path::PathBuf;
}

// src/main.rs
fn run(event_loop, cfg, paths, chrome_exe, port, status, rt, _lock_guard, host_log, host_rx,
       host_token: String, git: Option<std::sync::Arc<crate::git::GitService>>) -> !;
```

---

## Cycle A — the tao loop learns to consume git events

- [ ] **Step 1: Write the failing tests for the two loop-safety helpers**

Both pieces of policy the event arms need — the restart debounce and the conflict-notice
de-duplicator — are pure state machines, so they go in free functions that take their state by
`&mut` and their clock by argument. `App` itself cannot be built in a unit test (it owns an
`EventLoopProxy`, a tokio `Handle`, an open log file and a Chrome path), and this is the
codebase's existing answer to that problem: `chrome::find_first_existing(paths, exists)` takes
its filesystem probe as a parameter for exactly the same reason.

`src/host.rs` has no test module yet. Append one at the end of the file, after the closing `}`
of `impl App` (line 446):

```rust

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    #[test]
    fn restart_debounce_drops_a_second_restart_inside_the_window() {
        let t0 = Instant::now();
        let mut last = None;
        assert!(restart_debounce_ok(&mut last, t0), "the first restart is always allowed");
        assert!(
            !restart_debounce_ok(&mut last, t0 + Duration::from_millis(RESTART_DEBOUNCE_MS - 1)),
            "a restart 1ms inside the window must be dropped"
        );
        assert!(
            restart_debounce_ok(&mut last, t0 + Duration::from_millis(RESTART_DEBOUNCE_MS)),
            "the window is closed, not open, at exactly RESTART_DEBOUNCE_MS"
        );
        // An accepted restart re-arms the window from its own timestamp, so a
        // steady drip of events can never restart faster than the debounce.
        assert!(
            !restart_debounce_ok(&mut last, t0 + Duration::from_millis(RESTART_DEBOUNCE_MS + 1)),
            "the accepted restart must become the new mark"
        );
    }

    #[test]
    fn conflict_notice_fires_once_per_merge_commit() {
        let mut seen = VecDeque::new();
        assert!(notice_is_new(&mut seen, "abc123"));
        // A push that fails and is retried re-reports the SAME merge commit.
        // One overwrite, at most one modal.
        assert!(!notice_is_new(&mut seen, "abc123"));
        assert!(notice_is_new(&mut seen, "def456"), "a different merge is a different event");
        assert!(!notice_is_new(&mut seen, "abc123"));
    }

    #[test]
    fn conflict_notice_history_is_bounded_and_evicts_oldest_first() {
        let mut seen = VecDeque::new();
        for i in 0..GIT_NOTICE_HISTORY {
            assert!(notice_is_new(&mut seen, &format!("commit{i}")));
        }
        assert_eq!(seen.len(), GIT_NOTICE_HISTORY);
        assert!(notice_is_new(&mut seen, "overflow"));
        assert_eq!(seen.len(), GIT_NOTICE_HISTORY, "the set must not grow past its bound");
        // commit0 was evicted, so it reports as new again — the price of a
        // bounded set, and 16 dismissals of distinct merges in one session is
        // far outside anything a user does.
        assert!(notice_is_new(&mut seen, "commit0"));
        // …while a recent id is still remembered.
        assert!(!notice_is_new(&mut seen, &format!("commit{}", GIT_NOTICE_HISTORY - 1)));
    }
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app host::tests`

Expected: FAIL — nothing is defined yet:

```
error[E0425]: cannot find function `restart_debounce_ok` in this scope
   --> src/host.rs:455:17
    |
455 |         assert!(restart_debounce_ok(&mut last, t0), "the first restart is always allowed");
    |                 ^^^^^^^^^^^^^^^^^^^ not found in this scope

error[E0425]: cannot find value `RESTART_DEBOUNCE_MS` in this scope
error[E0425]: cannot find function `notice_is_new` in this scope
error[E0425]: cannot find value `GIT_NOTICE_HISTORY` in this scope

error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 9 previous errors
```

(Nine, because `RESTART_DEBOUNCE_MS` is named three times, `notice_is_new` five and
`GIT_NOTICE_HISTORY` four across the three tests; the exact count depends only on how many
call sites rustc reaches before it stops.)

- [ ] **Step 3: Add the constants, the helpers, the two `App` fields and the three `App` methods**

In `src/host.rs`, insert immediately **above** `impl App {` (line 40):

```rust
/// Minimum gap between two git-driven child restarts. A pull that moves HEAD
/// and a settings sync that lands a moment later are one user-visible event,
/// not two — without this the window is torn down twice inside a second.
pub(crate) const RESTART_DEBOUNCE_MS: u64 = 2_000;

/// How many merge-commit ids the conflict-notice de-duplicator remembers.
/// Bounded so a host that runs for weeks cannot grow the set without limit.
pub(crate) const GIT_NOTICE_HISTORY: usize = 16;

/// `true` when `now` is at least `RESTART_DEBOUNCE_MS` past the last accepted
/// git restart, recording `now` as the new mark. Free function taking its state
/// and its clock by argument so it is testable without an event loop, a tokio
/// runtime or a Chrome binary.
fn restart_debounce_ok(last: &mut Option<std::time::Instant>, now: std::time::Instant) -> bool {
    if let Some(prev) = *last {
        if now.duration_since(prev) < Duration::from_millis(RESTART_DEBOUNCE_MS) {
            return false;
        }
    }
    *last = Some(now);
    true
}

/// `true` the first time a merge commit id is seen, remembering the last
/// `GIT_NOTICE_HISTORY` ids, oldest evicted first.
fn notice_is_new(seen: &mut std::collections::VecDeque<String>, merge_commit: &str) -> bool {
    if seen.iter().any(|s| s == merge_commit) {
        return false;
    }
    if seen.len() == GIT_NOTICE_HISTORY {
        seen.pop_front();
    }
    seen.push_back(merge_commit.to_string());
    true
}
```

`Instant::duration_since` saturates to zero rather than panicking when the argument is later,
so a non-monotonic argument in a test cannot blow up the helper.

Add two fields to `struct App` (after `in_tray_mode`, `src/host.rs:37`):

```rust
    /// When the last git-driven restart was accepted. See `RESTART_DEBOUNCE_MS`.
    pub(crate) last_git_restart: Option<std::time::Instant>,
    /// Merge commits already reported by a conflict notice, newest last.
    pub(crate) git_notices: std::collections::VecDeque<String>,
```

And three methods inside `impl App`, next to `log` (`src/host.rs:41`):

```rust
    pub(crate) fn git_restart_ok(&mut self) -> bool {
        restart_debounce_ok(&mut self.last_git_restart, std::time::Instant::now())
    }

    pub(crate) fn git_notice_is_new(&mut self, merge_commit: &str) -> bool {
        notice_is_new(&mut self.git_notices, merge_commit)
    }

    pub(crate) fn git_log_path(&self) -> PathBuf {
        self.paths.logs_dir.join("git.log")
    }
```

- [ ] **Step 4: Build and watch the `App` literal reject the new fields**

Run: `cargo build`

Expected: FAIL — `run()` still builds an `App` with the old field set:

```
error[E0063]: missing fields `last_git_restart` and `git_notices` in initializer of `App`
   --> src/main.rs:190:19
    |
190 |     let mut app = App {
    |                   ^^^ missing `last_git_restart` and `git_notices`

error: could not compile `chrome-host-app` (bin "chrome-host-app") due to 1 previous error
```

- [ ] **Step 5: Fill the literal and add the three `HostEvent` arms**

In `src/main.rs`, add to the `App { … }` literal in `run()`, after `paths,`:

```rust
        last_git_restart: None,
        git_notices: std::collections::VecDeque::new(),
```

Then add three arms to the `event_loop.run` match, immediately after the existing
`HostEvent::RestartRequested` arm. Both are dumb consumers: every policy decision — *whether*
to restart, *whether* the failure is worth reporting — was already made inside `src/git/`.

```rust
            Event::UserEvent(UserEvent::Host(HostEvent::GitRestartChildren {
                repo_id,
                reason,
            })) => {
                // A background sync must never steal focus from a user who
                // deliberately minimised the app, and must not fight a restart
                // that is already in flight.
                if app.in_tray_mode {
                    app.log(&format!(
                        "git[{repo_id}]: restart suppressed ({reason}) — in tray mode"
                    ));
                } else if app.git_restart_ok() {
                    app.log(&format!("git[{repo_id}]: restarting children ({reason})"));
                    app.restart();
                    app.refresh_tray(&mut tray_handle, app.in_tray_mode);
                }
            }
            Event::UserEvent(UserEvent::Host(HostEvent::GitFailed {
                repo_id,
                op,
                code,
                message,
            })) => {
                app.log(&format!("git[{repo_id}]: {op} failed [{code}]: {message}"));
                // GitService sends this only on an ok -> fail transition, and
                // that gate is what makes a modal safe here: dialog() blocks
                // this loop — menu clicks and ChromeExited included — so a repo
                // whose token expired with auto_sync_secs = 300 would otherwise
                // stack one modal every five minutes forever, with the tray
                // unreachable to turn it off.
                if app.cfg.git.as_ref().is_some_and(|g| g.error_dialogs) && !app.in_tray_mode {
                    let log = app.git_log_path();
                    dialog(
                        &app.cfg.app.name,
                        &format!(
                            "Git {op} failed for \"{repo_id}\" ({code}).\n\n{message}\n\nLog: {}",
                            log.display()
                        ),
                    );
                }
            }
            Event::UserEvent(UserEvent::Host(HostEvent::GitConflictsResolved {
                repo_id,
                merge_commit,
                paths,
            })) => {
                app.log(&format!(
                    "git[{repo_id}]: {} file(s) resolved in favour of the local copy in \
                     {merge_commit}: {}",
                    paths.len(),
                    paths.join(", ")
                ));
                // A prefer-local merge that resolved a conflict has, by
                // definition, overwritten an edit made somewhere else. Silent
                // data loss is the one thing worth breaking "git is invisible
                // by default" for. Three guards keep the modal safe: tray-mode
                // suppression, de-duplication on the merge commit (a push that
                // fails and is retried re-reports the SAME merge), and the
                // 10-path cap below so a large merge stays readable.
                if app.cfg.git.as_ref().is_some_and(|g| g.error_dialogs)
                    && !app.in_tray_mode
                    && app.git_notice_is_new(&merge_commit)
                {
                    let log = app.git_log_path();
                    // notice(), not dialog(): the sync SUCCEEDED. Painting it
                    // red would misrepresent what happened and train the user
                    // to dismiss the real failure dialogs too.
                    notice(
                        &app.cfg.app.name,
                        &format!(
                            "\"{repo_id}\" synced, but {n} file(s) had been changed both here \
                             and remotely. The local version was kept:\n\n{list}\n\nThe remote \
                             version is not lost — recover it with:\n  \
                             git show {merge_commit}^2:<path>\n\nLog: {log}",
                            n = paths.len(),
                            list = paths
                                .iter()
                                .take(10)
                                .map(|p| format!("  {p}"))
                                .collect::<Vec<_>>()
                                .join("\n"),
                            log = log.display()
                        ),
                    );
                }
            }
```

`app.git_notice_is_new(...)` takes `&mut app` while `app.cfg.git.as_ref()` took `&app`; that is
fine because `&&` evaluates its operands in sequence and the left borrow ends at the operator.

Finally, delete the `#[expect(dead_code)]` attribute above `pub(crate) fn notice` in
`src/main.rs` (and the two doc-comment lines that explain it — the third arm is now its
caller). Leaving it in place is a hard error: an `#[expect]` whose lint stops firing raises
`unfulfilled_lint_expectation`, which `-D warnings` promotes to an error. That is exactly why
task 0 chose `expect` over `allow`.

- [ ] **Step 6: Run the tests and watch them pass**

Run:

```bash
cargo test --bin chrome-host-app host::tests
cargo fmt --check && echo FMT_CLEAN
cargo clippy --all-targets -- -D warnings
```

Expected: PASS —

```
running 3 tests
test host::tests::conflict_notice_fires_once_per_merge_commit ... ok
test host::tests::conflict_notice_history_is_bounded_and_evicts_oldest_first ... ok
test host::tests::restart_debounce_drops_a_second_restart_inside_the_window ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; N filtered out
FMT_CLEAN
    Finished `dev` profile [unoptimized + debuginfo] target(s)
```

`N` is however many tests tasks 1–9 added; only the `passed`/`failed` numbers matter.

- [ ] **Step 7: Commit**

```bash
git add src/host.rs src/main.rs
git commit -m "feat(git): consume the three git HostEvent variants in the tao loop

GitRestartChildren, GitFailed and GitConflictsResolved get event-loop
arms. Both dialog arms are gated on [git].error_dialogs and suppressed
in tray mode; the restart arm is debounced at RESTART_DEBOUNCE_MS and
the conflict notice is de-duplicated on the merge commit id over a
bounded GIT_NOTICE_HISTORY window.

The debounce and de-duplication are free functions over &mut state so
they are testable without an event loop; App's methods are one-line
wrappers. notice() loses its #[expect(dead_code)] — it now has a caller."
```

---

## Cycle B — the child server's ticket back into the host

- [ ] **Step 8: Write the failing `build_env` tests**

In `src/supervisor.rs`'s `mod tests`, replace `build_env_layers_and_settings_file` with the
version below and add the second test after it:

```rust
    #[test]
    fn build_env_layers_and_settings_file() {
        let mut cfg_env = BTreeMap::new();
        cfg_env.insert("PORT".to_string(), "8080".to_string());
        let settings = vec![("APP_SETTING_THEME".to_string(), "dark".to_string())];
        let host = HostAccess {
            url: "http://127.0.0.1:5051",
            token: "tok",
            git_enabled: false,
        };
        let env = build_env(
            &cfg_env,
            &settings,
            Path::new("/data/settings.json"),
            &host,
        );
        assert!(env.contains(&("PORT".into(), "8080".into())));
        assert!(env.contains(&("APP_SETTING_THEME".into(), "dark".into())));
        assert!(env
            .iter()
            .any(|(k, v)| k == "APP_SETTINGS_FILE" && v.ends_with("settings.json")));
    }

    #[test]
    fn build_env_exports_host_access_and_gates_the_git_flag() {
        let cfg_env = BTreeMap::new();
        let off = HostAccess {
            url: "http://127.0.0.1:5051",
            token: "s3cret",
            git_enabled: false,
        };
        let env = build_env(&cfg_env, &[], Path::new("/d/settings.json"), &off);
        assert!(env.contains(&("APP_HOST_URL".into(), "http://127.0.0.1:5051".into())));
        assert!(env.contains(&("APP_HOST_TOKEN".into(), "s3cret".into())));
        // Absent, not "0": a child that tests for the variable's presence and
        // one that parses its value must agree about what "off" looks like.
        assert!(
            !env.iter().any(|(k, _)| k == "APP_GIT_ENABLED"),
            "APP_GIT_ENABLED must not be exported when [git] is absent"
        );

        let on = HostAccess {
            url: "http://127.0.0.1:5051",
            token: "s3cret",
            git_enabled: true,
        };
        let env = build_env(&cfg_env, &[], Path::new("/d/settings.json"), &on);
        assert!(env.contains(&("APP_GIT_ENABLED".into(), "1".into())));
    }
```

- [ ] **Step 9: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app supervisor::tests::build_env`

Expected: FAIL —

```
error[E0422]: cannot find struct, variant or union type `HostAccess` in this scope
   --> src/supervisor.rs:146:20
    |
146 |         let host = HostAccess {
    |                    ^^^^^^^^^^ not found in this scope

error[E0061]: this function takes 3 arguments but 4 arguments were supplied
   --> src/supervisor.rs:152:19

error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 4 previous errors
```

- [ ] **Step 10: Add `HostAccess` and the three env vars**

In `src/supervisor.rs`, above `pub fn build_env` (line 40):

```rust
/// The `[server]` child's ticket back into the host: the loopback base URL and
/// the bearer token the internal server checks as `x-host-token`.
///
/// This reaches the child process and NOTHING else. `chrome::launch` inherits
/// the host's own environment, which never contains the token — so no page
/// rendered in the browser can read it out of a child's `process.env`, and a
/// compromised page cannot drive `/api/restart` or `/api/git/*`.
pub struct HostAccess<'a> {
    pub url: &'a str,
    pub token: &'a str,
    pub git_enabled: bool,
}
```

and rewrite `build_env` (lines 40-55) as:

```rust
pub fn build_env(
    cfg_env: &BTreeMap<String, String>,
    settings_env: &[(String, String)],
    settings_file: &Path,
    host: &HostAccess<'_>,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = cfg_env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    env.extend(settings_env.iter().cloned());
    env.push((
        "APP_SETTINGS_FILE".to_string(),
        settings_file.to_string_lossy().into_owned(),
    ));
    env.push(("APP_HOST_URL".to_string(), host.url.to_string()));
    env.push(("APP_HOST_TOKEN".to_string(), host.token.to_string()));
    // Pushed only when [git] is present, and absent (not "0") otherwise, so a
    // child can branch on `"APP_GIT_ENABLED" in process.env` alone.
    if host.git_enabled {
        env.push(("APP_GIT_ENABLED".to_string(), "1".to_string()));
    }
    env
}
```

- [ ] **Step 11: Build and watch the call site break**

Run: `cargo build`

Expected: FAIL — the one production caller still passes three arguments:

```
error[E0061]: this function takes 4 arguments but 3 arguments were supplied
   --> src/host.rs:206:17
    |
206 |             let env =
    |                 ^^^
207 |                 supervisor::build_env(&server_cfg.env, &settings_env, &self.paths.settings_file);
    |                                                                                                 ---- argument #4 of type `&HostAccess<'_>` is missing

error: could not compile `chrome-host-app` (bin "chrome-host-app") due to 1 previous error
```

- [ ] **Step 12: Thread the token from `main()` to the call site**

Three edits.

**(a)** `src/host.rs` — add the field to `struct App`, after `port` (line 19):

```rust
    /// The `x-host-token` value the internal server checks. Held here so
    /// `start_children` can hand it to the `[server]` child; it is never given
    /// to Chrome.
    pub(crate) host_token: String,
```

**(b)** `src/host.rs` — replace the `build_env` call in `start_children` (lines 205-208):

```rust
            let host_url = format!("http://127.0.0.1:{}", self.port);
            let env = supervisor::build_env(
                &server_cfg.env,
                &settings_env,
                &self.paths.settings_file,
                &supervisor::HostAccess {
                    url: &host_url,
                    token: &self.host_token,
                    git_enabled: self.cfg.git_enabled(),
                },
            );
```

(`host_url` needs its own binding: `HostAccess` borrows it, so it cannot be a temporary
inside the struct literal.)

**(c)** `src/main.rs` — hoist the token so both consumers get the same value, then pass it to
`run`. In `fn main()`, change `token` in the `HostState` literal to `token.clone()`:

```rust
    let state = HostState {
        app_name: cfg.app.name.clone(),
        token: token.clone(),
```

and add `token` as the eleventh argument at **both** `run(...)` call sites — the
`cfg(target_os = "macos")` branch and the other:

```rust
        run(
            event_loop, cfg, paths, chrome_exe, port, status, rt, guard, host_log, host_rx,
            token,
        );
```

```rust
    run(
        event_loop, cfg, paths, chrome_exe, port, status, rt, guard, host_log, host_rx, token,
    );
```

Add the parameter to `fn run` (after `host_rx`):

```rust
    host_token: String,
```

and the field to the `App` literal, after `port,`:

```rust
        host_token,
```

- [ ] **Step 13: Run the tests, watch them pass, and commit**

Run:

```bash
cargo test --bin chrome-host-app supervisor::tests::build_env
cargo fmt --check && echo FMT_CLEAN
cargo clippy --all-targets -- -D warnings
```

Expected: PASS —

```
running 2 tests
test supervisor::tests::build_env_exports_host_access_and_gates_the_git_flag ... ok
test supervisor::tests::build_env_layers_and_settings_file ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; N filtered out
FMT_CLEAN
    Finished `dev` profile [unoptimized + debuginfo] target(s)
```

```bash
git add src/supervisor.rs src/host.rs src/main.rs
git commit -m "feat(git): export APP_HOST_URL/APP_HOST_TOKEN/APP_GIT_ENABLED to the child

build_env takes a HostAccess so the [server] child can call back into
the internal server. APP_GIT_ENABLED is pushed only when [git] is
present — absent, not \"0\" — so presence alone is a usable test.

These three variables reach the child process and nothing else:
chrome::launch inherits the host's environment, which never holds the
token, so no rendered page can read it out of a child's process.env."
```

---

## Cycle C — construct `GitService` and start it

`GitService::new` returns `None` when `[git]` is absent from `app.toml`, and in that case
creates no directory, writes no `repos.json` and runs no libgit2 code. Every downstream
`Option` in this cycle is that switch, not a maybe.

- [ ] **Step 14: Add the startup arm and watch the compiler ask for the field**

In `src/main.rs`, extend the `Event::NewEvents(StartCause::Init)` arm:

```rust
            Event::NewEvents(StartCause::Init) => {
                // tray must be created after the loop starts (macOS requirement)
                app.refresh_tray(&mut tray_handle, app.in_tray_mode);
                app.start_children();
                // Git goes last, and sync_on_start last of all. Both orderings
                // are deliberate: the child server is already up, so a
                // sync_settings pull restarts a live child instead of racing
                // the first spawn; and a wedged network at launch cannot make
                // the app look dead before the first window appears, because
                // both calls return immediately and do their work on the
                // runtime. start() also creates repos/, applies the libgit2
                // network timeouts, recovers stale index.lock files and spawns
                // one auto-sync task per repo with auto_sync_secs > 0.
                if let Some(git) = &app.git {
                    git.start();
                    git.sync_on_start();
                }
            }
```

Run: `cargo build`

Expected: FAIL —

```
error[E0609]: no field `git` on type `App`
   --> src/main.rs:232:31
    |
232 |                 if let Some(git) = &app.git {
    |                                         ^^^ unknown field
    |
    = note: available fields are: `cfg`, `paths`, `chrome_exe`, `port`, `host_token` ...

error: could not compile `chrome-host-app` (bin "chrome-host-app") due to 1 previous error
```

- [ ] **Step 15: Build the service in `main()` and hand it to both consumers**

**(a)** `src/host.rs` — add the field to `struct App`, after `host_token`:

```rust
    /// The git subsystem, or `None` when `[git]` is absent from app.toml.
    ///
    /// The arrow points one way. `App` calls into `GitService`; `GitService`
    /// holds no reference back — not to `Children`, not to the generation
    /// counters, not to this struct. Its only channel into this loop is
    /// `HostEvent`, because `App::restart()` bottoms out in `rt.block_on()`,
    /// and `block_on` panics when called from a runtime worker thread, which
    /// is where every git job runs.
    pub(crate) git: Option<std::sync::Arc<crate::git::GitService>>,
```

**(b)** `src/main.rs`, in `fn main()` — build the service after the channel and before
`HostState`, and give `HostState` its handle:

```rust
    let status = Arc::new(RwLock::new(AppStatus::Starting));
    let (host_tx, host_rx) = mpsc::unbounded_channel::<HostEvent>();
    // Built before HostState so one token reaches both the HTTP guard and the
    // [server] child's environment, and so HostState can carry the service the
    // /api/git sub-router needs. `None` here is what makes /api/git answer
    // git_disabled and keeps repos/ from ever being created.
    let git_service = git::GitService::new(
        &cfg,
        &paths,
        rt.handle().clone(),
        host_tx.clone(),
        status.clone(),
    );
    let state = HostState {
        app_name: cfg.app.name.clone(),
        token: token.clone(),
        status: status.clone(),
        schema: if cfg.settings_enabled() {
            cfg.settings.clone()
        } else {
            None
        },
        settings_file: paths.settings_file.clone(),
        events: host_tx,
        git: git_service.clone(),
    };
```

**(c)** `src/main.rs` — twelfth argument at both `run(...)` call sites:

```rust
        run(
            event_loop, cfg, paths, chrome_exe, port, status, rt, guard, host_log, host_rx,
            token, git_service,
        );
```

```rust
    run(
        event_loop, cfg, paths, chrome_exe, port, status, rt, guard, host_log, host_rx, token,
        git_service,
    );
```

**(d)** `src/main.rs` — the parameter on `fn run`, after `host_token: String,`:

```rust
    git: Option<std::sync::Arc<crate::git::GitService>>,
```

and the field in the `App` literal, after `host_token,`:

```rust
        git,
```

- [ ] **Step 16: Build, verify, and commit**

Run:

```bash
cargo build
cargo fmt --check && echo FMT_CLEAN
cargo clippy --all-targets -- -D warnings
cargo test 2>&1 | grep -E '^test result'
```

Expected: PASS — a clean build, `FMT_CLEAN`, no clippy output beyond `Finished`, and both
`test result: ok.` lines with `0 failed`.

```bash
git add src/host.rs src/main.rs
git commit -m "feat(git): construct GitService in main() and start it after start_children

main() builds the service before HostState so one token reaches both the
HTTP guard and the child environment, and hands the same Arc to
HostState (for the /api/git sub-router) and to run() (for App).

start() and sync_on_start() fire from StartCause::Init AFTER
start_children(): the child server is already up, so a settings sync
restarts a live child instead of racing the first spawn, and a wedged
network cannot make the app look dead before the first window appears.
Both calls return immediately."
```

---

## Cycle D — the tray "Sync now" entry

- [ ] **Step 17: Write the failing tray tests**

In `src/tray.rs`'s `mod tests`, update both existing tests for the new parameter and add a
third that pins the position of the new entry:

```rust
    #[test]
    fn full_model_order() {
        let m = menu_model(&menu_cfg(), "My App", true, true, true);
        let expected = vec![
            Entry::Action(TrayAction::Open, "Open My App".to_string()),
            Entry::Action(
                TrayAction::OpenUrl("https://example.com".to_string()),
                "Docs".to_string(),
            ),
            Entry::Action(TrayAction::Settings, "Settings…".to_string()),
            Entry::Action(TrayAction::GitSync, "Sync now".to_string()),
            Entry::Action(TrayAction::Restart, "Restart App".to_string()),
            Entry::Separator,
            Entry::Action(TrayAction::Quit, "Quit".to_string()),
        ];
        assert_eq!(m, expected);
    }

    #[test]
    fn minimal_model_hides_open_and_settings() {
        let empty = MenuSection {
            settings: false,
            items: vec![],
        };
        let m = menu_model(&empty, "X", false, false, false);
        let expected = vec![
            Entry::Action(TrayAction::Restart, "Restart App".to_string()),
            Entry::Separator,
            Entry::Action(TrayAction::Quit, "Quit".to_string()),
        ];
        assert_eq!(m, expected);
    }

    #[test]
    fn sync_entry_is_independent_of_settings_and_sits_above_restart() {
        // show_sync is computed from [git].tray_sync AND repo_count > 0, so it
        // must not be entangled with settings_enabled in either direction.
        let empty = MenuSection {
            settings: false,
            items: vec![],
        };
        let m = menu_model(&empty, "X", false, false, true);
        assert_eq!(
            m,
            vec![
                Entry::Action(TrayAction::GitSync, "Sync now".to_string()),
                Entry::Action(TrayAction::Restart, "Restart App".to_string()),
                Entry::Separator,
                Entry::Action(TrayAction::Quit, "Quit".to_string()),
            ]
        );
    }
```

- [ ] **Step 18: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app tray::tests`

Expected: FAIL —

```
error[E0061]: this function takes 4 arguments but 5 arguments were supplied
   --> src/tray.rs:124:17
    |
124 |         let m = menu_model(&menu_cfg(), "My App", true, true, true);
    |                 ^^^^^^^^^^                                    ---- unexpected argument #5 of type `bool`

error[E0599]: no variant or associated item named `GitSync` found for enum `TrayAction`
   --> src/tray.rs:131:39
    |
131 |             Entry::Action(TrayAction::GitSync, "Sync now".to_string()),
    |                                       ^^^^^^^ variant or associated item not found in `TrayAction`

error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 5 previous errors
```

- [ ] **Step 19: Add the variant and the parameter**

In `src/tray.rs`, the enum (lines 6-13) — `GitSync` goes between `Settings` and `Restart` so
the declaration order mirrors the menu order:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum TrayAction {
    Open,
    OpenUrl(String),
    Settings,
    GitSync,
    Restart,
    Quit,
}
```

and `menu_model` (lines 21-47):

```rust
pub fn menu_model(
    menu: &MenuSection,
    app_name: &str,
    settings_enabled: bool,
    show_open: bool,
    show_sync: bool,
) -> Vec<Entry> {
    let mut out = Vec::new();
    if show_open {
        out.push(Entry::Action(TrayAction::Open, format!("Open {app_name}")));
    }
    for item in &menu.items {
        out.push(Entry::Action(
            TrayAction::OpenUrl(item.open_url.clone()),
            item.label.clone(),
        ));
    }
    if settings_enabled {
        out.push(Entry::Action(TrayAction::Settings, "Settings…".to_string()));
    }
    if show_sync {
        out.push(Entry::Action(TrayAction::GitSync, "Sync now".to_string()));
    }
    out.push(Entry::Action(
        TrayAction::Restart,
        "Restart App".to_string(),
    ));
    out.push(Entry::Separator);
    out.push(Entry::Action(TrayAction::Quit, "Quit".to_string()));
    out
}
```

- [ ] **Step 20: Build and watch the two consumers break**

Run: `cargo build`

Expected: FAIL — one arity error and one exhaustiveness error:

```
error[E0061]: this function takes 5 arguments but 4 arguments were supplied
   --> src/host.rs:92:21
    |
 92 |         let model = tray::menu_model(
    |                     ^^^^^^^^^^^^^^^^ argument #5 of type `bool` is missing

error[E0004]: non-exhaustive patterns: `Some(TrayAction::GitSync)` not covered
   --> src/main.rs:200:23
    |
200 |                 match action {
    |                       ^^^^^^ pattern `Some(TrayAction::GitSync)` not covered

error: could not compile `chrome-host-app` (bin "chrome-host-app") due to 2 previous errors
```

The second error is the point of adding the variant before the handler: an unhandled tray
action is a compile error here, not a menu entry that silently does nothing.

- [ ] **Step 21: Compute `show_sync` and handle the action**

**(a)** `src/host.rs`, in `refresh_tray` (line 90):

```rust
    pub(crate) fn refresh_tray(&mut self, tray_handle: &mut Option<tray::Tray>, show_open: bool) {
        let settings_enabled = self.cfg.settings_enabled();
        // Hidden when there is nothing to sync: [git] absent, tray_sync off, or
        // a registry with no repos in it. An entry that cannot do anything is
        // worse than no entry.
        let show_sync = self.cfg.git.as_ref().is_some_and(|g| g.tray_sync)
            && self.git.as_ref().is_some_and(|g| g.repo_count() > 0);
        let model = tray::menu_model(
            &self.cfg.menu,
            &self.cfg.app.name,
            settings_enabled,
            show_open,
            show_sync,
        );
```

**(b)** `src/main.rs`, in the `UserEvent::Menu` match, immediately before the
`Some(tray::TrayAction::OpenUrl(url))` arm:

```rust
                    // Admits one sync per repo and returns instantly; busy
                    // repos are skipped with a log line inside the service.
                    // No refresh_tray: the menu did not change, and reflecting
                    // busy state there would mean rebuilding the tray on every
                    // job transition for a purely cosmetic gain.
                    Some(tray::TrayAction::GitSync) => {
                        if let Some(git) = &app.git {
                            git.sync_all_manual();
                        }
                    }
```

- [ ] **Step 22: Run the tests, watch them pass, and commit**

Run:

```bash
cargo test --bin chrome-host-app tray::tests
cargo fmt --check && echo FMT_CLEAN
cargo clippy --all-targets -- -D warnings
```

Expected: PASS —

```
running 3 tests
test tray::tests::full_model_order ... ok
test tray::tests::minimal_model_hides_open_and_settings ... ok
test tray::tests::sync_entry_is_independent_of_settings_and_sits_above_restart ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; N filtered out
FMT_CLEAN
    Finished `dev` profile [unoptimized + debuginfo] target(s)
```

```bash
git add src/tray.rs src/host.rs src/main.rs
git commit -m "feat(git): add the tray \"Sync now\" entry

menu_model gains show_sync and inserts TrayAction::GitSync directly
above \"Restart App\". refresh_tray shows it only when [git].tray_sync
is on AND the registry has at least one repo, so the entry never
appears with nothing to do.

The handler calls sync_all_manual() and returns; it deliberately does
not refresh the tray, because the menu did not change and mirroring
busy state would mean rebuilding it on every job transition."
```

---

## Cycle E — `quit()` runs the quit syncs, after the children are gone

- [ ] **Step 23: Reorder `quit()`**

There is no unit test for this step: `quit()` ends in `std::process::exit(0)`, which takes the
test harness with it. The checks are the compiler, review against the ordering argument below,
and the smoke run in Step 39.

`src/host.rs:328-331` becomes:

```rust
    pub(crate) fn quit(&mut self) -> ! {
        self.log("quit");
        // Children die FIRST, before any sync. A quit sync can take seconds on
        // a slow network: leaving Chrome and the server alive through it means
        // the user clicked Quit and then watched the window sit there. Worse,
        // a live child could still be rewriting the very settings.json the
        // sync is about to commit, so the pushed copy would be a torn read.
        self.kill_children();
        // Bounded by [git].quit_sync_timeout_secs (0 disables it entirely), so
        // an unreachable remote delays exit by that much and no more.
        let timeout = self
            .cfg
            .git
            .as_ref()
            .map(|g| g.quit_sync_timeout_secs)
            .unwrap_or(0);
        if let Some(git) = &self.git {
            if timeout > 0 {
                git.run_quit_syncs(Duration::from_secs(timeout));
            }
        }
        std::process::exit(0)
    }
```

Note what is deliberately *not* changed: the two other `kill_children()` + `exit(1)` paths
(`refresh_tray`'s tray-build failure and `start_children`'s Chrome-launch failure) stay as they
are. Those are startup aborts — pushing a repo on the way out of a failed launch is not
something the user asked for.

- [ ] **Step 24: Build, verify, and commit**

Run:

```bash
cargo build
cargo fmt --check && echo FMT_CLEAN
cargo clippy --all-targets -- -D warnings
```

Expected: PASS — clean build, `FMT_CLEAN`, no clippy diagnostics.

```bash
git add src/host.rs
git commit -m "feat(git): run the quit syncs in quit(), after kill_children()

Order is log -> kill_children -> run_quit_syncs -> exit. Killing first
means the user's window closes the moment they click Quit instead of
hanging for the length of a network round trip, and no live child can
rewrite settings.json underneath the sync that is committing it.

Skipped entirely when [git] is absent or quit_sync_timeout_secs is 0;
otherwise bounded by that timeout, so an unreachable remote costs at
most that many seconds of exit latency."
```

---

## Cycle F — `/api/status` grows an optional `git` block

- [ ] **Step 25: Write the failing `StatusResponse` tests**

`assets/loading.html` polls `/api/status` in a tight loop while the app starts. Two properties
have to hold: the response must be **byte-identical** to today's when git is off (nothing
downstream may need to change), and the git block must be cheap enough to serve on every poll.
The first is testable here; the second is a property of `status_summary()`, which task 9 builds
from the registry, the job store and the in-memory state file only — **zero libgit2 calls, zero
filesystem access**. A `Repository::open` + `statuses()` per repo per poll would be a genuine
performance bug, which is why live tree state (`dirty`, `ahead`/`behind`) is deliberately absent
from this endpoint; callers who want it use `GET /api/git/repos/{id}/status`.

Add to `src/internal_server.rs`'s `mod tests`:

```rust
    #[test]
    fn status_response_is_byte_identical_to_app_status_when_git_is_off() {
        // loading.html and any external poller parse this shape today. The
        // flatten must add exactly nothing when there is no git block.
        for st in [
            AppStatus::Starting,
            AppStatus::Ready {
                target_url: "http://x/".to_string(),
            },
            AppStatus::Error {
                message: "boom".to_string(),
                log_path: Some("/tmp/server.log".to_string()),
            },
        ] {
            let bare = serde_json::to_string(&st).unwrap();
            let wrapped = serde_json::to_string(&StatusResponse { app: st, git: None }).unwrap();
            assert_eq!(bare, wrapped);
        }
    }

    #[tokio::test]
    async fn status_endpoint_omits_the_git_block_when_git_is_disabled() {
        let (_state, port, _rx) = spawn_state(false).await;
        let body: serde_json::Value = reqwest::get(format!("http://127.0.0.1:{port}/api/status"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(body.get("git").is_none(), "unexpected git block: {body}");
        assert_eq!(
            body.as_object().unwrap().len(),
            1,
            "AppStatus::Starting must serialize to exactly {{\"state\":\"starting\"}}: {body}"
        );
    }
```

The existing `status_endpoint_reports_states` test is left **untouched** — it is the regression
guard that says the change was additive, and editing it would defeat that.

- [ ] **Step 26: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app internal_server::tests::status`

Expected: FAIL —

```
error[E0422]: cannot find struct, variant or union type `StatusResponse` in this scope
   --> src/internal_server.rs:298:56
    |
298 |             let wrapped = serde_json::to_string(&StatusResponse { app: st, git: None }).unwrap();
    |                                                  ^^^^^^^^^^^^^^ not found in this scope

error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 1 previous error
```

- [ ] **Step 27: Add `StatusResponse` and retype the handler**

In `src/internal_server.rs`, immediately after the `AppStatus` enum:

```rust
/// `/api/status` with an optional `git` block. `#[serde(flatten)]` over the
/// internally-tagged `AppStatus` keeps the handler typed instead of degrading
/// to `Json<Value>`, and `skip_serializing_if` makes the response
/// byte-identical to the bare `AppStatus` whenever git is off or
/// `[git].status_api` is false.
///
/// `assets/loading.html` polls this in a tight loop, so whatever fills `git`
/// must stay cheap: `GitService::status_summary()` reads the registry, the job
/// store and the in-memory state file and touches neither libgit2 nor the
/// filesystem. Live tree state is deliberately absent for that reason — use
/// `GET /api/git/repos/{id}/status` for it.
#[derive(serde::Serialize)]
pub(crate) struct StatusResponse {
    #[serde(flatten)]
    pub app: AppStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<crate::git::GitStatusSummary>,
}
```

and replace `async fn api_status` (line ~165):

```rust
async fn api_status(State(s): State<HostState>) -> Json<StatusResponse> {
    let git = s
        .git
        .as_ref()
        .filter(|g| g.status_api())
        .map(|g| g.status_summary());
    Json(StatusResponse {
        app: s.status.read().await.clone(),
        git,
    })
}
```

Note this endpoint stays unauthenticated, exactly as before — the git block it can expose is
repo ids, paths, auto-sync periods and last-sync outcomes, all of which the loading page is
already entitled to. Anything that could carry a credential goes through `/api/git`, which
`authorized` guards.

- [ ] **Step 28: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app internal_server::tests`

Expected: PASS — including the untouched pre-existing test:

```
running 7 tests
test internal_server::tests::status_response_is_byte_identical_to_app_status_when_git_is_off ... ok
test internal_server::tests::settings_page_404_without_schema ... ok
test internal_server::tests::settings_post_404_with_valid_token_but_no_schema ... ok
test internal_server::tests::pages_render_with_app_name ... ok
test internal_server::tests::settings_post_requires_token_and_validates ... ok
test internal_server::tests::restart_endpoint_sends_event ... ok
test internal_server::tests::status_endpoint_reports_states ... ok
test internal_server::tests::status_endpoint_omits_the_git_block_when_git_is_disabled ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

(The count is 8 plus whatever tests task 9 added to this module; order varies run to run.)

- [ ] **Step 29: Commit**

```bash
git add src/internal_server.rs
git commit -m "feat(git): add the optional git block to /api/status

StatusResponse flattens AppStatus and appends git only when [git] is
present and [git].status_api is true, so the response is byte-identical
to today's for every existing caller — asserted for all three AppStatus
variants and over HTTP.

The block is filled from status_summary(), which reads the registry,
the job store and the in-memory state file: no libgit2, no filesystem.
loading.html polls this endpoint in a tight loop and a Repository::open
per repo per poll would be a real performance bug."
```

---

## Cycle G — lock the two host-facing invariants of the service

These two tests live in `src/git/mod.rs` because that is where the behaviour lives. They are
the only edit this task makes under `src/git/`, and they add tests only.

- [ ] **Step 30: Write the ok→fail transition test**

Append to the `#[cfg(test)] mod tests` at the bottom of `src/git/mod.rs`:

```rust
    /// A `GitOps` that replays a fixed script of successes and failures, so the
    /// `after_job` notification path can be driven through an outage and back
    /// with no network, no remote and no real work tree.
    struct ScriptedOps {
        script: Vec<bool>, // true = succeed
        calls: std::sync::atomic::AtomicUsize,
    }

    impl GitOps for ScriptedOps {
        fn run(&self, _op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError> {
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

    async fn next_git_failed(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
    ) -> String {
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                Ok(Some(HostEvent::GitFailed { code, .. })) => return code,
                Ok(Some(other)) => panic!("unexpected event before GitFailed: {other:?}"),
                Ok(None) => panic!("event channel closed"),
                Err(_) => panic!("timed out waiting for GitFailed"),
            }
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
```

`JobOp::Commit` rather than `Sync` is deliberate: the transition rule is op-agnostic (it reads
the repo's previous `last_sync.ok`), and a commit needs no remote, which keeps the test
hermetic — no URL to validate, no network stack to stub.

The names `GitService`, `GitOps`, `StartOutcome`, `JobOp`, `OpCtx`, `OpOutcome`, `OpRequest`,
`GitError`, `GitErrorCode`, `AppConfig`, `RuntimePaths`, `AppStatus` and `HostEvent` all arrive
through this module's existing `use super::*;`. If rustc says `cannot find type X in this
scope` for any of them, add the matching import to the test module
(`use crate::git::jobs::JobOp;`, `use crate::git::ops::{OpCtx, OpOutcome, OpRequest};`,
`use crate::git::error::{GitError, GitErrorCode};`,
`use crate::config::{AppConfig, RuntimePaths};`,
`use crate::internal_server::{AppStatus, HostEvent};`) rather than a glob.

- [ ] **Step 31: Run the test and read the result carefully**

Run: `cargo test --bin chrome-host-app git::tests::git_failed_is_emitted_only_on_an_ok_to_fail_transition -- --nocapture`

This is a characterization test over task 9's `after_job`, so there are two legitimate
outcomes and they need different responses.

**If it PASSES**, task 9 implemented the gate correctly; the test now prevents it from
regressing. Skip Step 32 and go to Step 33.

**If it FAILS**, the expected failure is:

```
thread 'git::tests::git_failed_is_emitted_only_on_an_ok_to_fail_transition' panicked at src/git/mod.rs:NNN:
a third event arrived: the ok -> fail gate is not holding, and a stuck repo will stack a modal per sync
```

which means `after_job` is sending `GitFailed` on every failure rather than only on the edge.
Apply Step 32.

- [ ] **Step 32: (only if Step 31 failed) Gate the emission on the transition**

In `GitService::after_job` in `src/git/mod.rs`, read the previous `LastSync` **before**
`record_last_sync` overwrites it, and gate the send on it:

```rust
        // GitFailed is a transition edge, not a level. `dialog()` is modal and
        // blocks the tao loop, so a repo that fails every five minutes must
        // produce one dialog per outage, not one per attempt.
        let was_healthy = self
            .state
            .last_sync(repo_id)
            .map(|prev| prev.ok)
            .unwrap_or(true);
        self.state.record_last_sync(repo_id, last);
        if let Err(e) = &result {
            if was_healthy {
                let _ = self.events.send(HostEvent::GitFailed {
                    repo_id: repo_id.to_string(),
                    op: op.as_str().to_string(),
                    code: e.code().as_str().to_string(),
                    message: e.message.clone(),
                });
            }
        }
```

The `unwrap_or(true)` is the "or absent" half of the rule: the very first failure after a
fresh start has no previous record and must still be reported.

Re-run the command from Step 31. Expected: PASS.

- [ ] **Step 33: Write and run the auto-sync restart test**

The auto-sync timer is a sleep loop, not a `tokio::time::interval`, and it re-reads
`auto_sync_secs` from the registry every cycle. That buys three things: a `PUT` that changes or
clears the period takes effect after at most one old period, a `DELETE` lets the task exit on
its own, and — because a sleep loop keeps no missed-tick backlog — a laptop that was asleep for
six hours wakes up and does **one** sync, not seventy-two. A tick that lands on a busy repo is
dropped, not queued: a backlog of stale syncs is never useful and would turn one slow network
into an unbounded queue.

The property that actually matters to this task is what those ticks are allowed to do to the
user's window. Append to the same test module:

```rust
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
```

Run: `cargo test --bin chrome-host-app git::tests::an_automatic_sync_can_never_restart_the_children`

Expected: PASS —

```
running 1 test
test git::tests::an_automatic_sync_can_never_restart_the_children ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

If it FAILS with `assertion `left == right` failed: left: None, right: Some(false)`, fix
`OpRequest::auto` / `OpRequest::manual` in `src/git/ops.rs` to set
`restart_children: Some(false)` — a `None` there falls back to
`def.restart_children_on_pull`, which is precisely the config key this invariant forbids a
timer from honouring.

- [ ] **Step 34: Commit**

```bash
git add src/git/mod.rs
git commit -m "test(git): pin the two invariants the tao loop depends on

GitFailed fires on the ok->fail edge only: fail, fail, succeed, fail
produces exactly two events. dialog() is modal and blocks the event
loop including menu clicks, so a repo with an expired token and a
5-minute period would otherwise stack a modal forever with the tray
unreachable.

OpRequest::auto() and ::manual() hard-code restart_children = false, so
no timer and no tray click can tear down the user's window; only an
explicit HTTP call or a real settings change can."
```

---

## Verification

- [ ] **Step 35: Run the whole suite and both lints**

Run:

```bash
cargo fmt --check && echo FMT_CLEAN
cargo clippy --all-targets -- -D warnings
cargo test 2>&1 | grep -E '^test result'
```

Expected: PASS — `FMT_CLEAN`, `Finished` with no diagnostics, and both `test result: ok.`
lines with `0 failed`.

- [ ] **Step 36: Prove `[git]` absent is still a no-op**

The template ships with `[git]` commented out, so a default build must behave exactly as it
did before this feature existed: no `repos/`, no `repos.json`, no `logs/git.log`, no `git`
key in `/api/status`, and `/api/git/*` answering `git_disabled`.

```bash
grep -c '^\[git\]' app.toml
rm -rf ~/.local/share/com.example.chromehost
```

Expected: `0` from the grep — `app.toml`'s `[git]` block is commented out.

- [ ] **Step 37: Smoke-run with git off**

This box runs its desktop on `:0` and terminals do not inherit it, so export the session's
display first or the tray and every dialog will silently fail to appear:

```bash
PID=$(pgrep -n lxqt-session)
export DISPLAY=$(tr '\0' '\n' < /proc/$PID/environ | sed -n 's/^DISPLAY=//p')
export XAUTHORITY=$(tr '\0' '\n' < /proc/$PID/environ | sed -n 's/^XAUTHORITY=//p')
cargo run
```

Check, then quit from the tray:

- the tray menu has **no** "Sync now" entry;
- `ls ~/.local/share/com.example.chromehost` shows `chrome-profile`, `logs`, `app.lock` and
  **no** `repos`, `repos.json` or `git-state.json`;
- `ls ~/.local/share/com.example.chromehost/logs` has `host.log` and `chrome.log`, **no**
  `git.log`;
- the app quits immediately when you click Quit.

- [ ] **Step 38: Smoke-run with git on — set up a remote and two clones**

```bash
rm -rf /tmp/git-smoke && mkdir -p /tmp/git-smoke && cd /tmp/git-smoke
git init --bare origin.git
git clone origin.git seed && cd seed
printf 'hello\n' > shared.txt
git add shared.txt && git -c user.email=a@b -c user.name=a commit -m init
git push origin HEAD:main && cd ..
```

Add to `app.toml` (remember to revert it before committing):

```toml
[git]
tray_sync = true
error_dialogs = true
status_api = true
author_name = "Smoke Test"
author_email = "smoke@example.com"
```

Run `cargo run` with the same `DISPLAY`/`XAUTHORITY` exports, then in another terminal find the
port and token (the token is embedded in the loading page, and the port is whatever the host
bound):

```bash
PORT=$(ss -ltnp 2>/dev/null | grep chrome-host-app | grep -o '127.0.0.1:[0-9]*' | cut -d: -f2 | head -1)
TOKEN=$(curl -s http://127.0.0.1:$PORT/loading | grep -o "TOKEN[^0-9a-f]*[0-9a-f]\{32\}" | grep -o '[0-9a-f]\{32\}')
curl -s -X PUT http://127.0.0.1:$PORT/api/git/repos/notes \
  -H "x-host-token: $TOKEN" -H 'content-type: application/json' \
  -d '{"remote":"/tmp/git-smoke/origin.git","branch":"main","sync_on_quit":true}'
curl -s -X POST http://127.0.0.1:$PORT/api/git/repos/notes/clone -H "x-host-token: $TOKEN"
```

- [ ] **Step 39: Walk the four smoke checks**

1. **Tray sync.** Right-click the tray icon: "Sync now" is present (it appeared because
   `repo_count()` went from 0 to 1 the next time the menu was rebuilt — click Restart App once
   if the menu was built before the `PUT`). Click it; `logs/git.log` gains a `sync … ok` line
   and the app does not stutter.
2. **One conflict dialog, not two.** Create a genuine both-sides edit and sync:
   ```bash
   cd /tmp/git-smoke/seed && git pull --ff-only
   printf 'remote edit\n' > shared.txt
   git -c user.email=a@b -c user.name=a commit -am remote && git push
   printf 'local edit\n' > ~/.local/share/com.example.chromehost/repos/notes/shared.txt
   curl -s -X POST http://127.0.0.1:$PORT/api/git/repos/notes/sync -H "x-host-token: $TOKEN"
   ```
   Exactly **one** warning-level modal appears, naming `shared.txt` and printing
   `git show <oid>^2:<path>`. Dismiss it and fire the same `sync` again: the job succeeds and
   **no second modal** appears, because `git_notice_is_new` remembers the merge commit id.
3. **Failure dialog fires once.** Break the remote and sync twice:
   ```bash
   mv /tmp/git-smoke/origin.git /tmp/git-smoke/gone.git
   curl -s -X POST http://127.0.0.1:$PORT/api/git/repos/notes/sync -H "x-host-token: $TOKEN"
   # dismiss the red dialog, then:
   curl -s -X POST http://127.0.0.1:$PORT/api/git/repos/notes/sync -H "x-host-token: $TOKEN"
   ```
   The **first** produces a red dialog; the **second** produces only a `git.log` line. Restore
   the remote with `mv /tmp/git-smoke/gone.git /tmp/git-smoke/origin.git`.
4. **Clean quit with an in-flight job.** Fire a sync and click Quit from the tray within the
   same second. Chrome and the server disappear immediately; the process exits within
   `quit_sync_timeout_secs`; `git.log`'s last lines show the quit sync resolving (or being
   abandoned) rather than the process hanging.

Finally, `git checkout app.toml` to drop the smoke `[git]` block, and confirm
`git status --short` is clean before the task is considered done.

---

**Definition of done:** `cargo test` green with the eight new tests
(3 in `host::tests`, 1 new + 1 rewritten in `supervisor::tests`, 1 new tray test, 2 in
`internal_server::tests`, 2 in `git::tests`); `cargo fmt --check` and
`cargo clippy --all-targets -- -D warnings` clean; `/api/status` byte-identical with git off;
`app.toml` unmodified in the final tree; and `grep -rn 'Children\|chrome_generation\|server_generation\|rt.enter\|tokio::process' src/git/` returns **nothing** — the one rule still holds.
