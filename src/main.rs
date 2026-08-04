#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod chrome;
mod config;
mod git;
mod host;
mod internal_server;
mod settings;
mod supervisor;
mod tray;

use config::{AppConfig, OnClose, RuntimePaths};
use host::{App, Children};
use internal_server::{AppStatus, HostEvent, HostState};
use rand::Rng;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tokio::sync::{mpsc, Mutex, RwLock};
use tray_icon::menu::{MenuEvent, MenuId};

#[derive(Debug)]
pub(crate) enum UserEvent {
    Menu(MenuId),
    Host(HostEvent),
    ChromeExited { generation: u64 },
    ServerExited { generation: u64 },
}

pub(crate) fn dialog(title: &str, description: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(title)
        .set_description(description)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// Non-error modal: same shape as `dialog` at warning level, for outcomes the
/// user should see that did not stop the app from working.
///
/// `expect` rather than `allow` so the attribute cannot outlive its reason —
/// the first caller turns the unfulfilled expectation back into a warning.
#[expect(dead_code)]
pub(crate) fn notice(title: &str, description: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(title)
        .set_description(description)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

pub(crate) fn missing_chrome_dialog(app_name: &str) {
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
        git: None,
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
        chrome_generation: Arc::new(AtomicU64::new(1)),
        server_generation: Arc::new(AtomicU64::new(1)),
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
                    // menu) while in tray mode. The guard is what keeps
                    // `restart_chrome_only`'s replace-the-owned-child
                    // behavior safe: outside tray mode the owned child is the
                    // user's app window, and a stray/duplicate event must not
                    // kill and relaunch it underneath them.
                    Some(tray::TrayAction::Open) if app.in_tray_mode => {
                        app.restart_chrome_only();
                        app.refresh_tray(&mut tray_handle, app.in_tray_mode);
                    }
                    Some(tray::TrayAction::Open) => {
                        app.log("tray: ignored Open action while not in tray mode");
                    }
                    // Settings never changes tray mode — a settings window is
                    // not the app window — so the menu keeps its "Open" entry
                    // when we were in tray mode. Refreshed anyway so the menu
                    // is always rebuilt from the (unchanged) source of truth.
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
                if generation != app.chrome_generation() {
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
                if generation != app.server_generation() {
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
