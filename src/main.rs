#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod chrome;
mod config;
mod internal_server;
mod settings;
mod supervisor;
mod tray;

use config::{AppConfig, OnClose, RuntimePaths};
use internal_server::{AppStatus, HostEvent, HostState};
use rand::Rng;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tokio::sync::{mpsc, Mutex, RwLock};
use tray_icon::menu::{MenuEvent, MenuId};

#[derive(Debug)]
enum UserEvent {
    Menu(MenuId),
    Host(HostEvent),
    ChromeExited { generation: u64 },
    ServerExited { generation: u64 },
}

struct Children {
    chrome: Option<tokio::process::Child>,
    server: Option<supervisor::ServerHandle>,
}

struct App {
    cfg: AppConfig,
    paths: RuntimePaths,
    chrome_exe: PathBuf,
    port: u16,
    status: Arc<RwLock<AppStatus>>,
    children: Arc<Mutex<Children>>,
    generation: Arc<AtomicU64>,
    host_log: std::fs::File,
    rt: tokio::runtime::Handle,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
    in_tray_mode: bool,
}

fn dialog(title: &str, description: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(title)
        .set_description(description)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

fn missing_chrome_dialog(app_name: &str) {
    let choice = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(app_name)
        .set_description(format!(
            "{app_name} requires Google Chrome, which was not found on this computer.\n\n\
             Install Google Chrome, then launch {app_name} again."
        ))
        .set_buttons(rfd::MessageButtons::OkCancelCustom(
            "Download Chrome".to_string(),
            "Quit".to_string(),
        ))
        .show();
    if let rfd::MessageDialogResult::Custom(label) = choice {
        if label == "Download Chrome" {
            let _ = open::that(chrome::DOWNLOAD_URL);
        }
    }
}

impl App {
    fn log(&mut self, msg: &str) {
        let _ = writeln!(self.host_log, "{msg}");
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

    fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Build (first call) or rebuild (later calls) the tray menu so it always
    /// matches current state. Centralizing this keeps every state transition
    /// — restart, tray actions, chrome-exit handling — in sync with
    /// `in_tray_mode`, instead of each call site carrying its own copy of the
    /// menu-model + rebuild boilerplate (which is how the "Open" entry
    /// previously went stale after `HostEvent::RestartRequested`).
    fn refresh_tray(&mut self, tray_handle: &mut Option<tray::Tray>, show_open: bool) {
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
        let gen_counter = self.generation.clone();
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
        let gen_counter = self.generation.clone();
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
    fn start_children(&mut self) {
        let generation = self.current_generation();

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
                    let gen_counter = self.generation.clone();
                    self.rt.spawn(async move {
                        let result = supervisor::wait_healthy(&health_url, timeout).await;
                        if gen_counter.load(Ordering::SeqCst) != generation {
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
                    self.spawn_server_watcher(generation);
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

        let launch_result = {
            let _guard = self.rt.enter();
            chrome::launch(
                &self.chrome_exe,
                &self.initial_chrome_url(),
                &self.paths.chrome_profile,
                self.cfg.window.width,
                self.cfg.window.height,
            )
        };
        match launch_result {
            Ok(child) => {
                self.log("chrome: launched");
                self.rt.block_on(async {
                    self.children.lock().await.chrome = Some(child);
                });
                self.spawn_chrome_watcher(generation);
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
        // bump generation so watchers' exit events are recognized as intentional
        self.generation.fetch_add(1, Ordering::SeqCst);
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

    fn restart(&mut self) {
        self.log("restart requested");
        self.kill_children();
        self.in_tray_mode = false;
        self.start_children();
    }

    fn quit(&mut self) -> ! {
        self.log("quit");
        self.kill_children();
        std::process::exit(0);
    }

    /// Relaunch only Chrome, pointing at the current target — used by tray
    /// "Open" when the host is in tray mode (server, if any, is still
    /// running). No-ops if a Chrome child is already owned: displacing it
    /// here would drop (and kill_on_drop) a window the user is still using.
    fn restart_chrome_only(&mut self) {
        let already_running = self
            .rt
            .block_on(async { self.children.lock().await.chrome.is_some() });
        if already_running {
            self.log("open: ignored, a chrome window is already owned");
            return;
        }
        let generation = self.current_generation();
        let url = self.rt.block_on(async {
            match &*self.status.read().await {
                AppStatus::Ready { target_url } => target_url.clone(),
                _ => format!("http://127.0.0.1:{}/loading", self.port),
            }
        });
        let launch_result = {
            let _guard = self.rt.enter();
            chrome::launch(
                &self.chrome_exe,
                &url,
                &self.paths.chrome_profile,
                self.cfg.window.width,
                self.cfg.window.height,
            )
        };
        match launch_result {
            Ok(child) => {
                self.rt.block_on(async {
                    self.children.lock().await.chrome = Some(child);
                });
                self.spawn_chrome_watcher(generation);
            }
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
    fn open_settings(&mut self) {
        let url = format!("http://127.0.0.1:{}/settings", self.port);
        let already_running = self
            .rt
            .block_on(async { self.children.lock().await.chrome.is_some() });
        if already_running {
            if let Err(e) =
                chrome::open_extra_window(&self.chrome_exe, &url, &self.paths.chrome_profile)
            {
                self.log(&format!("settings: failed to open extra window: {e}"));
            }
            return;
        }
        let generation = self.current_generation();
        let launch_result = {
            let _guard = self.rt.enter();
            chrome::launch(
                &self.chrome_exe,
                &url,
                &self.paths.chrome_profile,
                self.cfg.window.width,
                self.cfg.window.height,
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
            Err(e) => self.log(&format!("settings: failed to launch chrome: {e}")),
        }
    }
}

fn main() {
    let cfg = match AppConfig::load() {
        Ok(c) => c,
        Err(e) => {
            dialog("Configuration error", &e);
            std::process::exit(1);
        }
    };
    let paths = RuntimePaths::resolve(&cfg.app.identifier);
    if let Err(e) = paths.ensure() {
        dialog(
            &cfg.app.name,
            &format!("Cannot create data directory:\n{e}"),
        );
        std::process::exit(1);
    }

    // single instance
    let lock_file = match std::fs::File::create(&paths.lock_file) {
        Ok(f) => f,
        Err(e) => {
            dialog(&cfg.app.name, &format!("Cannot open lock file:\n{e}"));
            std::process::exit(1);
        }
    };
    let mut lock = fd_lock::RwLock::new(lock_file);
    let guard = match lock.try_write() {
        Ok(g) => g,
        Err(_) => {
            rfd::MessageDialog::new()
                .set_title(&cfg.app.name)
                .set_description(format!("{} is already running.", cfg.app.name))
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
            std::process::exit(0);
        }
    };

    let Some(chrome_exe) = chrome::find_chrome() else {
        missing_chrome_dialog(&cfg.app.name);
        std::process::exit(1);
    };

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let host_log =
        supervisor::open_log(&paths.logs_dir.join("host.log"), supervisor::LOG_MAX_BYTES)
            .expect("host log");

    let token: String = {
        let mut rng = rand::rng();
        format!("{:032x}", rng.random::<u128>())
    };
    let status = Arc::new(RwLock::new(AppStatus::Starting));
    let (host_tx, host_rx) = mpsc::unbounded_channel::<HostEvent>();
    let state = HostState {
        app_name: cfg.app.name.clone(),
        token,
        status: status.clone(),
        schema: if cfg.settings_enabled() {
            cfg.settings.clone()
        } else {
            None
        },
        settings_file: paths.settings_file.clone(),
        events: host_tx,
    };
    let port = rt
        .block_on(internal_server::start(state))
        .expect("internal server");

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        // no host-owned windows: keep the host out of the Dock
        let mut event_loop = event_loop; // shadow for set_activation_policy(&mut)
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        run(
            event_loop, cfg, paths, chrome_exe, port, status, rt, guard, host_log, host_rx,
        );
    }
    #[cfg(not(target_os = "macos"))]
    run(
        event_loop, cfg, paths, chrome_exe, port, status, rt, guard, host_log, host_rx,
    );
}

#[allow(clippy::too_many_arguments)]
fn run(
    event_loop: tao::event_loop::EventLoop<UserEvent>,
    cfg: AppConfig,
    paths: RuntimePaths,
    chrome_exe: PathBuf,
    port: u16,
    status: Arc<RwLock<AppStatus>>,
    rt: tokio::runtime::Runtime,
    _lock_guard: fd_lock::RwLockWriteGuard<'_, std::fs::File>,
    host_log: std::fs::File,
    mut host_rx: mpsc::UnboundedReceiver<HostEvent>,
) -> ! {
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some({
        let proxy = proxy.clone();
        move |e: MenuEvent| {
            let _ = proxy.send_event(UserEvent::Menu(e.id().clone()));
        }
    }));
    // bridge internal-server events into the tao loop
    rt.spawn({
        let proxy = proxy.clone();
        async move {
            while let Some(ev) = host_rx.recv().await {
                let _ = proxy.send_event(UserEvent::Host(ev));
            }
        }
    });

    let mut app = App {
        chrome_exe,
        port,
        status,
        children: Arc::new(Mutex::new(Children {
            chrome: None,
            server: None,
        })),
        generation: Arc::new(AtomicU64::new(1)),
        host_log,
        rt: rt.handle().clone(),
        proxy,
        in_tray_mode: false,
        cfg,
        paths,
    };
    let mut tray_handle: Option<tray::Tray> = None;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                // tray must be created after the loop starts (macOS requirement)
                app.refresh_tray(&mut tray_handle, app.in_tray_mode);
                app.start_children();
            }
            Event::UserEvent(UserEvent::Menu(id)) => {
                let action = tray_handle
                    .as_ref()
                    .and_then(|t| t.actions.get(&id))
                    .cloned();
                match action {
                    Some(tray::TrayAction::Quit) => app.quit(),
                    Some(tray::TrayAction::Restart) => {
                        app.restart();
                        app.refresh_tray(&mut tray_handle, app.in_tray_mode);
                    }
                    // "Open" only makes sense (and is only advertised in the
                    // menu) while in tray mode; a stray/duplicate event must
                    // not be allowed to relaunch Chrome and potentially
                    // displace an already-owned window.
                    Some(tray::TrayAction::Open) if app.in_tray_mode => {
                        app.in_tray_mode = false;
                        app.restart_chrome_only();
                        app.refresh_tray(&mut tray_handle, app.in_tray_mode);
                    }
                    Some(tray::TrayAction::Open) => {
                        app.log("tray: ignored Open action while not in tray mode");
                    }
                    Some(tray::TrayAction::Settings) => {
                        app.open_settings();
                        app.refresh_tray(&mut tray_handle, app.in_tray_mode);
                    }
                    Some(tray::TrayAction::OpenUrl(url)) => {
                        let _ = open::that(url);
                    }
                    None => {}
                }
            }
            Event::UserEvent(UserEvent::Host(HostEvent::RestartRequested)) => {
                app.restart();
                app.refresh_tray(&mut tray_handle, app.in_tray_mode);
            }
            Event::UserEvent(UserEvent::ChromeExited { generation }) => {
                if generation != app.current_generation() {
                    return; // intentional kill
                }
                app.log("chrome: window closed by user");
                match app.cfg.window.on_close {
                    OnClose::Quit => app.quit(),
                    OnClose::Tray => {
                        app.in_tray_mode = true;
                        app.refresh_tray(&mut tray_handle, app.in_tray_mode);
                    }
                }
            }
            Event::UserEvent(UserEvent::ServerExited { generation }) => {
                if generation != app.current_generation() {
                    return; // intentional kill
                }
                app.log("server: exited unexpectedly");
                let log_path = app.paths.logs_dir.join("server.log");
                app.rt.block_on(async {
                    *app.status.write().await = AppStatus::Error {
                        message: "The app's local server stopped unexpectedly.".to_string(),
                        log_path: Some(log_path.display().to_string()),
                    };
                });
                let choice = rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Error)
                    .set_title(&app.cfg.app.name)
                    .set_description(format!(
                        "The app's local server stopped unexpectedly.\n\nLog: {}",
                        log_path.display()
                    ))
                    .set_buttons(rfd::MessageButtons::OkCancelCustom(
                        "Restart".to_string(),
                        "Quit".to_string(),
                    ))
                    .show();
                match choice {
                    rfd::MessageDialogResult::Custom(l) if l == "Restart" => {
                        app.restart();
                        app.refresh_tray(&mut tray_handle, app.in_tray_mode);
                    }
                    _ => app.quit(),
                }
            }
            _ => {}
        }
    })
}
