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

    /// Spawn server (if configured) + Chrome; watch both for exit.
    fn start_children(&mut self) {
        let generation = self.generation.load(Ordering::SeqCst);
        let rt = self.rt.clone();

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
            match supervisor::spawn_server(&server_cfg, env, cwd, &log_path) {
                Ok(handle) => {
                    self.log("server: spawned");
                    let status = self.status.clone();
                    let target = self.target_url();
                    let health_url = server_cfg.health_check_url.clone();
                    let timeout = Duration::from_secs(server_cfg.startup_timeout_secs);
                    let log_display = log_path.display().to_string();
                    rt.spawn(async move {
                        match supervisor::wait_healthy(&health_url, timeout).await {
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
                    // watch for unexpected exit
                    let children = self.children.clone();
                    let proxy = self.proxy.clone();
                    rt.spawn(async move {
                        // poll: take child's wait future without holding the lock forever
                        loop {
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            let mut guard = children.lock().await;
                            let Some(server) = guard.server.as_mut() else {
                                break;
                            };
                            match server.child.try_wait() {
                                Ok(Some(_)) => {
                                    guard.server = None;
                                    drop(guard);
                                    let _ =
                                        proxy.send_event(UserEvent::ServerExited { generation });
                                    break;
                                }
                                Ok(None) => {}
                                Err(_) => break,
                            }
                        }
                    });
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

        match chrome::launch(
            &self.chrome_exe,
            &self.initial_chrome_url(),
            &self.paths.chrome_profile,
            self.cfg.window.width,
            self.cfg.window.height,
        ) {
            Ok(child) => {
                self.log("chrome: launched");
                self.rt.block_on(async {
                    self.children.lock().await.chrome = Some(child);
                });
                let proxy = self.proxy.clone();
                let children = self.children.clone();
                rt.spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        let mut guard = children.lock().await;
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
            Err(e) => {
                dialog(
                    &self.cfg.app.name,
                    &format!("Failed to launch Google Chrome:\n{e}"),
                );
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

    fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Relaunch only Chrome, pointing at the current target — used by tray
    /// "Open" when the host is in tray mode (server, if any, is still running).
    fn restart_chrome_only(&mut self) {
        let generation = self.current_generation();
        let url = self.rt.block_on(async {
            match &*self.status.read().await {
                AppStatus::Ready { target_url } => target_url.clone(),
                _ => format!("http://127.0.0.1:{}/loading", self.port),
            }
        });
        match chrome::launch(
            &self.chrome_exe,
            &url,
            &self.paths.chrome_profile,
            self.cfg.window.width,
            self.cfg.window.height,
        ) {
            Ok(child) => {
                self.rt.block_on(async {
                    self.children.lock().await.chrome = Some(child);
                });
                let proxy = self.proxy.clone();
                let children = self.children.clone();
                self.rt.spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        let mut guard = children.lock().await;
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
            Err(e) => self.log(&format!("chrome: relaunch failed: {e}")),
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
        return;
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

    let settings_enabled = cfg.settings_enabled();
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
                let model =
                    tray::menu_model(&app.cfg.menu, &app.cfg.app.name, settings_enabled, false);
                match tray::build(&model, &app.cfg.app.name) {
                    Ok(t) => tray_handle = Some(t),
                    Err(e) => app.log(&format!("tray: failed to build: {e}")),
                }
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
                        if let Some(t) = tray_handle.as_mut() {
                            let model = tray::menu_model(
                                &app.cfg.menu,
                                &app.cfg.app.name,
                                settings_enabled,
                                false,
                            );
                            let _ = tray::rebuild_menu(t, &model);
                        }
                    }
                    Some(tray::TrayAction::Open) => {
                        app.in_tray_mode = false;
                        app.restart_chrome_only();
                        if let Some(t) = tray_handle.as_mut() {
                            let model = tray::menu_model(
                                &app.cfg.menu,
                                &app.cfg.app.name,
                                settings_enabled,
                                false,
                            );
                            let _ = tray::rebuild_menu(t, &model);
                        }
                    }
                    Some(tray::TrayAction::Settings) => {
                        let url = format!("http://127.0.0.1:{}/settings", app.port);
                        let _ = chrome::open_extra_window(
                            &app.chrome_exe,
                            &url,
                            &app.paths.chrome_profile,
                        );
                    }
                    Some(tray::TrayAction::OpenUrl(url)) => {
                        let _ = open::that(url);
                    }
                    None => {}
                }
            }
            Event::UserEvent(UserEvent::Host(HostEvent::RestartRequested)) => {
                app.restart();
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
                        if let Some(t) = tray_handle.as_mut() {
                            let model = tray::menu_model(
                                &app.cfg.menu,
                                &app.cfg.app.name,
                                settings_enabled,
                                true,
                            );
                            let _ = tray::rebuild_menu(t, &model);
                        }
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
                    rfd::MessageDialogResult::Custom(l) if l == "Restart" => app.restart(),
                    _ => app.quit(),
                }
            }
            _ => {}
        }
    })
}
