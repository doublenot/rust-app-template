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
}

impl App {
    pub(crate) fn log(&mut self, msg: &str) {
        let _ = writeln!(self.host_log, "{msg}");
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
        let model = tray::menu_model(
            &self.cfg.menu,
            &self.cfg.app.name,
            settings_enabled,
            show_open,
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
            let env =
                supervisor::build_env(&server_cfg.env, &settings_env, &self.paths.settings_file);
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
        self.kill_children();
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
