use crate::config::{AppConfig, OnClose, RuntimePaths};
use crate::internal_server::AppStatus;
use crate::{chrome, dialog, settings, supervisor, tray, UserEvent};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

pub(crate) struct Children {
    pub(crate) chrome: Option<tokio::process::Child>,
    pub(crate) server: Option<supervisor::ServerHandle>,
}

pub(crate) struct App {
    pub(crate) cfg: AppConfig,
    pub(crate) paths: RuntimePaths,
    pub(crate) chrome_exe: PathBuf,
    pub(crate) port: u16,
    pub(crate) status: Arc<RwLock<AppStatus>>,
    pub(crate) children: Arc<Mutex<Children>>,
    /// Bumped whenever the Chrome child is killed on purpose, so its watcher
    /// (and any exit event already in flight) is recognized as intentional.
    /// Kept separate from `server_generation` because tray "Open" replaces
    /// only the Chrome child: bumping a shared counter there would also
    /// retire the still-valid server watcher and make the in-flight
    /// `wait_healthy` task discard its result, stranding the status at
    /// `Starting`.
    pub(crate) chrome_generation: Arc<AtomicU64>,
    /// Bumped whenever the server child is killed on purpose. Guards the
    /// server watcher and the `wait_healthy` status write.
    pub(crate) server_generation: Arc<AtomicU64>,
    pub(crate) host_log: std::fs::File,
    pub(crate) rt: tokio::runtime::Handle,
    pub(crate) proxy: tao::event_loop::EventLoopProxy<UserEvent>,
    pub(crate) in_tray_mode: bool,
    /// The git subsystem, or `None` when the config has no `[git]` section.
    ///
    /// `App` may call `GitService`; `GitService` may only send `HostEvent`s back.
    /// That asymmetry is load-bearing — see the module-level note on `HostEvent`.
    pub(crate) git: Option<Arc<crate::git::GitService>>,
    /// The loopback token, so `build_env` can hand it to the `[server]` child.
    pub(crate) host_token: String,
    /// When the last git-driven restart was accepted. See `RESTART_DEBOUNCE_MS`.
    pub(crate) last_git_restart: Option<std::time::Instant>,
    /// Merge commits already reported by a conflict notice, newest last.
    pub(crate) git_notices: std::collections::VecDeque<String>,
}

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

/// How long `quit()` may spend on quit syncs, or `None` when it must not run any.
///
/// Lifted out of `quit()` so the §14 risk 12 ordering — quit syncs run on a bounded
/// clock, and `0` means "do not sync at all" rather than "sync forever" — has a machine
/// check that does not need an event loop.
pub(crate) fn quit_sync_timeout(cfg: &AppConfig) -> Option<Duration> {
    match cfg.git.as_ref()?.quit_sync_timeout_secs {
        0 => None,
        secs => Some(Duration::from_secs(secs)),
    }
}

impl App {
    pub(crate) fn log(&mut self, msg: &str) {
        let _ = writeln!(self.host_log, "{msg}");
    }

    pub(crate) fn git_restart_ok(&mut self) -> bool {
        restart_debounce_ok(&mut self.last_git_restart, std::time::Instant::now())
    }

    pub(crate) fn git_notice_is_new(&mut self, merge_commit: &str) -> bool {
        notice_is_new(&mut self.git_notices, merge_commit)
    }

    pub(crate) fn git_log_path(&self) -> PathBuf {
        // `GIT_LOG_FILE` exists so this name has exactly one definition; hard-coding the
        // literal here re-opens precisely the drift the constant prevents.
        self.paths.logs_dir.join(crate::git::GIT_LOG_FILE)
    }

    /// Log file capturing Chrome's own stdout/stderr (GPU probes, GCM
    /// chatter, ML init lines), rotated like the other logs. `None` when it
    /// can't be opened — Chrome's output is then discarded rather than
    /// inherited, so the host's terminal stays clean either way.
    fn chrome_log(&mut self) -> Option<std::fs::File> {
        let path = self.paths.logs_dir.join("chrome.log");
        match supervisor::open_log(&path, supervisor::LOG_MAX_BYTES) {
            Ok(f) => Some(f),
            Err(e) => {
                self.log(&format!("chrome: cannot open chrome.log: {e}"));
                None
            }
        }
    }

    fn target_url(&self) -> String {
        match &self.cfg.server {
            Some(s) if self.cfg.app.url.is_empty() => s.health_check_url.clone(),
            _ if !self.cfg.app.url.is_empty() => self.cfg.app.url.clone(),
            _ => format!("http://127.0.0.1:{}/placeholder", self.port),
        }
    }

    fn initial_chrome_url(&self) -> String {
        if self.cfg.server.is_some() {
            format!("http://127.0.0.1:{}/loading", self.port)
        } else {
            self.target_url()
        }
    }

    pub(crate) fn chrome_generation(&self) -> u64 {
        self.chrome_generation.load(Ordering::SeqCst)
    }

    pub(crate) fn server_generation(&self) -> u64 {
        self.server_generation.load(Ordering::SeqCst)
    }

    /// Build (first call) or rebuild (later calls) the tray menu so it always
    /// matches current state. Centralizing this keeps every state transition
    /// — restart, tray actions, chrome-exit handling — in sync with
    /// `in_tray_mode`, instead of each call site carrying its own copy of the
    /// menu-model + rebuild boilerplate (which is how the "Open" entry
    /// previously went stale after `HostEvent::RestartRequested`).
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
        match tray_handle {
            Some(t) => {
                if let Err(e) = tray::rebuild_menu(t, &model) {
                    self.log(&format!("tray: failed to rebuild menu: {e}"));
                }
            }
            None => match tray::build(&model, &self.cfg.app.name) {
                Ok(t) => *tray_handle = Some(t),
                Err(e) => {
                    self.log(&format!("tray: failed to build menu: {e}"));
                    if self.cfg.window.on_close == OnClose::Tray {
                        // Under on_close = "tray" the tray is the ONLY way
                        // back once the window is closed. Continuing headless
                        // with no tray icon would leave the process
                        // unreachable yet still holding app.lock (so no
                        // relaunch is possible either) until killed by hand.
                        // Fail loudly instead of limping on invisibly.
                        dialog(
                            &self.cfg.app.name,
                            "Failed to create the system tray icon. This app is \
                             configured to stay running in the tray when its window \
                             is closed, which requires a tray icon to work. Exiting.",
                        );
                        self.kill_children();
                        std::process::exit(1);
                    }
                }
            },
        }
    }

    /// Spawn the poll loop that watches a Chrome child for exit, tagged with
    /// `generation`. A watcher whose generation has been superseded by a
    /// restart retires as soon as it notices (right after it re-acquires the
    /// lock) instead of adopting whatever child is currently stored and
    /// misreporting that child's eventual exit under its own stale tag.
    fn spawn_chrome_watcher(&self, generation: u64) {
        let children = self.children.clone();
        let proxy = self.proxy.clone();
        let gen_counter = self.chrome_generation.clone();
        self.rt.spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let mut guard = children.lock().await;
                if gen_counter.load(Ordering::SeqCst) != generation {
                    break; // superseded by a restart; not our child anymore
                }
                let Some(chrome) = guard.chrome.as_mut() else {
                    break;
                };
                match chrome.try_wait() {
                    Ok(Some(_)) => {
                        guard.chrome = None;
                        drop(guard);
                        let _ = proxy.send_event(UserEvent::ChromeExited { generation });
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        });
    }

    /// Mirror of `spawn_chrome_watcher` for the app server child.
    fn spawn_server_watcher(&self, generation: u64) {
        let children = self.children.clone();
        let proxy = self.proxy.clone();
        let gen_counter = self.server_generation.clone();
        self.rt.spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let mut guard = children.lock().await;
                if gen_counter.load(Ordering::SeqCst) != generation {
                    break;
                }
                let Some(server) = guard.server.as_mut() else {
                    break;
                };
                match server.child.try_wait() {
                    Ok(Some(_)) => {
                        guard.server = None;
                        drop(guard);
                        let _ = proxy.send_event(UserEvent::ServerExited { generation });
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        });
    }

    /// Spawn server (if configured) + Chrome; watch both for exit.
    pub(crate) fn start_children(&mut self) {
        let chrome_gen = self.chrome_generation();
        let server_gen = self.server_generation();

        if let Some(server_cfg) = self.cfg.server.clone() {
            self.rt.block_on(async {
                *self.status.write().await = AppStatus::Starting;
            });
            let settings_env = match (&self.cfg.settings, self.cfg.settings_enabled()) {
                (Some(schema), true) => {
                    settings::env_vars(&settings::load(schema, &self.paths.settings_file))
                }
                _ => Vec::new(),
            };
            // Its own binding: `HostAccess` borrows the url, so it cannot be a temporary
            // inside the struct literal.
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
            let cwd = supervisor::resolve_cwd(server_cfg.cwd.as_deref(), &supervisor::base_dir());
            let log_path = self.paths.logs_dir.join("server.log");
            // tokio::process::Command::spawn() needs runtime context (it
            // registers the child with the runtime's signal/reactor driver).
            // This thread — the tao event loop — never enters the runtime
            // otherwise, so without this guard the spawn panics.
            let spawn_result = {
                let _guard = self.rt.enter();
                supervisor::spawn_server(&server_cfg, env, cwd, &log_path)
            };
            match spawn_result {
                Ok(handle) => {
                    self.log("server: spawned");
                    let status = self.status.clone();
                    let target = self.target_url();
                    let health_url = server_cfg.health_check_url.clone();
                    let timeout = Duration::from_secs(server_cfg.startup_timeout_secs);
                    let log_display = log_path.display().to_string();
                    let gen_counter = self.server_generation.clone();
                    self.rt.spawn(async move {
                        let result = supervisor::wait_healthy(&health_url, timeout).await;
                        if gen_counter.load(Ordering::SeqCst) != server_gen {
                            // A restart happened while we were polling; this
                            // server generation is gone — don't clobber the
                            // status of whatever generation replaced it.
                            return;
                        }
                        match result {
                            Ok(()) => {
                                *status.write().await = AppStatus::Ready { target_url: target }
                            }
                            Err(message) => {
                                *status.write().await = AppStatus::Error {
                                    message,
                                    log_path: Some(log_display),
                                }
                            }
                        }
                    });
                    self.rt.block_on(async {
                        self.children.lock().await.server = Some(handle);
                    });
                    self.spawn_server_watcher(server_gen);
                }
                Err(e) => {
                    dialog(
                        &self.cfg.app.name,
                        &format!(
                            "Failed to start the app's local server:\n{e}\n\nLog: {}",
                            log_path.display()
                        ),
                    );
                    std::process::exit(1);
                }
            }
        } else {
            let target = self.target_url();
            self.rt.block_on(async {
                *self.status.write().await = AppStatus::Ready { target_url: target };
            });
        }

        let chrome_log = self.chrome_log();
        let launch_result = {
            let _guard = self.rt.enter();
            chrome::launch(
                &self.chrome_exe,
                &self.initial_chrome_url(),
                &self.paths.chrome_profile,
                self.cfg.window.width,
                self.cfg.window.height,
                chrome_log,
            )
        };
        match launch_result {
            Ok(child) => {
                self.log("chrome: launched");
                self.rt.block_on(async {
                    self.children.lock().await.chrome = Some(child);
                });
                self.spawn_chrome_watcher(chrome_gen);
            }
            Err(e) => {
                dialog(
                    &self.cfg.app.name,
                    &format!("Failed to launch Google Chrome:\n{e}"),
                );
                // The server (if configured) is already running at this
                // point. std::process::exit runs no destructors, so without
                // this the server's kill_on_drop never fires and it survives
                // as an orphan holding the listening port for the next launch.
                self.kill_children();
                std::process::exit(1);
            }
        }
    }

    fn kill_children(&mut self) {
        // bump generations so watchers' exit events are recognized as intentional
        self.chrome_generation.fetch_add(1, Ordering::SeqCst);
        self.server_generation.fetch_add(1, Ordering::SeqCst);
        self.rt.block_on(async {
            let mut guard = self.children.lock().await;
            if let Some(mut chrome) = guard.chrome.take() {
                let _ = chrome.start_kill();
            }
            if let Some(mut server) = guard.server.take() {
                let _ = server.child.start_kill();
            }
        });
        self.log("children: killed");
    }

    pub(crate) fn restart(&mut self) {
        self.log("restart requested");
        self.kill_children();
        self.in_tray_mode = false;
        self.start_children();
    }

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
        if let (Some(git), Some(timeout)) = (&self.git, quit_sync_timeout(&self.cfg)) {
            git.run_quit_syncs(timeout);
        }
        std::process::exit(0);
    }

    /// Relaunch only Chrome, pointing at the current target — used by tray
    /// "Open" when the host is in tray mode (server, if any, is still
    /// running).
    ///
    /// In tray mode the only Chrome child we can own is a secondary window we
    /// launched ourselves (tray "Settings…" with no main window open), never
    /// the app window the user is working in — the host reaches tray mode
    /// precisely because that window closed. So "Open" *replaces* any owned
    /// child with the app window rather than no-opping, which would otherwise
    /// leave the user with a settings window and no way back to the app.
    /// The replaced child is killed intentionally: the Chrome generation is
    /// bumped first, so its watcher retires and any exit event already in
    /// flight is discarded instead of being reported as a user close. Only
    /// the Chrome generation moves — the server child keeps running, watched,
    /// under its own untouched generation.
    ///
    /// Clears `in_tray_mode` only if Chrome actually launched; the caller
    /// refreshes the menu from that flag afterwards.
    pub(crate) fn restart_chrome_only(&mut self) {
        let generation = self.chrome_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.rt.block_on(async {
            if let Some(mut chrome) = self.children.lock().await.chrome.take() {
                let _ = chrome.start_kill();
            }
        });
        let url = self.rt.block_on(async {
            match &*self.status.read().await {
                AppStatus::Ready { target_url } => target_url.clone(),
                _ => format!("http://127.0.0.1:{}/loading", self.port),
            }
        });
        let chrome_log = self.chrome_log();
        let launch_result = {
            let _guard = self.rt.enter();
            chrome::launch(
                &self.chrome_exe,
                &url,
                &self.paths.chrome_profile,
                self.cfg.window.width,
                self.cfg.window.height,
                chrome_log,
            )
        };
        match launch_result {
            Ok(child) => {
                self.rt.block_on(async {
                    self.children.lock().await.chrome = Some(child);
                });
                self.spawn_chrome_watcher(generation);
                self.in_tray_mode = false;
            }
            // Stay in tray mode on failure: there is still no app window, so
            // the menu must keep offering "Open" for another try.
            Err(e) => self.log(&format!("chrome: relaunch failed: {e}")),
        }
    }

    /// Tray "Settings…": `chrome::open_extra_window`'s contract requires a
    /// Chrome instance we already own (same profile dir, so the new process
    /// hands the URL off and exits on its own). That precondition doesn't
    /// hold in tray mode, where `children.chrome` is `None` — falling back
    /// to it there spawns an untracked, unwatched, un-killable browser
    /// process holding the profile dir. Branch on ownership instead: launch
    /// (and track) a Chrome instance when none is owned, hand off to the
    /// existing one otherwise.
    ///
    /// The launched window is a *secondary* window (settings), sized like the
    /// one `open_extra_window` would have produced — it is emphatically not
    /// the app's main window, so `in_tray_mode` stays `true` and the tray
    /// keeps its "Open <app>" entry. Otherwise the settings window would
    /// claim the slot of the main window and the user would have no way left
    /// to get the app back.
    pub(crate) fn open_settings(&mut self) {
        let url = format!("http://127.0.0.1:{}/settings", self.port);
        let already_running = self
            .rt
            .block_on(async { self.children.lock().await.chrome.is_some() });
        if already_running {
            let chrome_log = self.chrome_log();
            if let Err(e) = chrome::open_extra_window(
                &self.chrome_exe,
                &url,
                &self.paths.chrome_profile,
                chrome_log,
            ) {
                self.log(&format!("settings: failed to open extra window: {e}"));
            }
            return;
        }
        let generation = self.chrome_generation();
        let chrome_log = self.chrome_log();
        let launch_result = {
            let _guard = self.rt.enter();
            chrome::launch(
                &self.chrome_exe,
                &url,
                &self.paths.chrome_profile,
                chrome::EXTRA_WINDOW_SIZE.0,
                chrome::EXTRA_WINDOW_SIZE.1,
                chrome_log,
            )
        };
        match launch_result {
            Ok(child) => {
                self.rt.block_on(async {
                    self.children.lock().await.chrome = Some(child);
                });
                self.spawn_chrome_watcher(generation);
            }
            Err(e) => self.log(&format!("settings: failed to launch chrome: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    #[test]
    fn restart_debounce_drops_a_second_restart_inside_the_window() {
        let t0 = Instant::now();
        let mut last = None;
        assert!(
            restart_debounce_ok(&mut last, t0),
            "the first restart is always allowed"
        );
        assert!(
            !restart_debounce_ok(
                &mut last,
                t0 + Duration::from_millis(RESTART_DEBOUNCE_MS - 1)
            ),
            "a restart 1ms inside the window must be dropped"
        );
        assert!(
            restart_debounce_ok(&mut last, t0 + Duration::from_millis(RESTART_DEBOUNCE_MS)),
            "the window is closed, not open, at exactly RESTART_DEBOUNCE_MS"
        );
        // An accepted restart re-arms the window from its own timestamp, so a
        // steady drip of events can never restart faster than the debounce.
        assert!(
            !restart_debounce_ok(
                &mut last,
                t0 + Duration::from_millis(RESTART_DEBOUNCE_MS + 1)
            ),
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
        assert!(
            notice_is_new(&mut seen, "def456"),
            "a different merge is a different event"
        );
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
        assert_eq!(
            seen.len(),
            GIT_NOTICE_HISTORY,
            "the set must not grow past its bound"
        );
        // commit0 was evicted, so it reports as new again — the price of a
        // bounded set, and 16 dismissals of distinct merges in one session is
        // far outside anything a user does.
        assert!(notice_is_new(&mut seen, "commit0"));
        // …while a recent id is still remembered.
        assert!(!notice_is_new(
            &mut seen,
            &format!("commit{}", GIT_NOTICE_HISTORY - 1)
        ));
    }

    fn cfg(toml: &str) -> AppConfig {
        AppConfig::from_str(&format!(
            "[app]\nname = \"T\"\nidentifier = \"com.example.t\"\n{toml}"
        ))
        .unwrap()
    }

    #[test]
    fn quit_sync_timeout_is_none_without_git_and_none_at_zero() {
        // No `[git]` at all: `quit()` must not even reach the git branch.
        assert_eq!(quit_sync_timeout(&cfg("")), None);
        // `0` is the author's explicit "never hold quit for a sync", not "no ceiling".
        assert_eq!(
            quit_sync_timeout(&cfg("[git]\nquit_sync_timeout_secs = 0\n")),
            None
        );
        assert_eq!(
            quit_sync_timeout(&cfg("[git]\nquit_sync_timeout_secs = 10\n")),
            Some(Duration::from_secs(10))
        );
        // And the shipped default is a bounded number of seconds, not zero and not
        // something that would make quit feel hung.
        let default = quit_sync_timeout(&cfg("[git]\n")).expect("the default must be a ceiling");
        assert!(
            (Duration::from_secs(1)..=Duration::from_secs(60)).contains(&default),
            "{default:?} is outside the range a user will wait through"
        );
    }
}
