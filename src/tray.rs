use crate::config::MenuSection;
use std::collections::HashMap;
use tray_icon::menu::{Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Debug, Clone, PartialEq)]
pub enum TrayAction {
    Open,
    OpenUrl(String),
    Settings,
    GitSync,
    Restart,
    Quit,
}

#[derive(Debug, PartialEq)]
pub enum Entry {
    Action(TrayAction, String),
    Separator,
}

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

pub struct Tray {
    pub tray: TrayIcon,
    pub actions: HashMap<MenuId, TrayAction>,
}

fn build_menu(model: &[Entry]) -> anyhow::Result<(Menu, HashMap<MenuId, TrayAction>)> {
    let menu = Menu::new();
    let mut actions = HashMap::new();
    for entry in model {
        match entry {
            Entry::Separator => menu.append(&PredefinedMenuItem::separator())?,
            Entry::Action(action, label) => {
                let item = MenuItem::new(label, true, None);
                actions.insert(item.id().clone(), action.clone());
                menu.append(&item)?;
            }
        }
    }
    Ok((menu, actions))
}

/// Side length of the tray icon. Small because tray icons are drawn tiny, and
/// `icons/icon.png` is 512x512 — handing that to the tray wastes memory and, on
/// some desktops, gets scaled worse than doing it here.
const TRAY_PX: u32 = 32;

/// The tray icon, decoded from the same `icons/icon.png` the installers use, so
/// replacing that one file rebrands the app everywhere it appears.
///
/// Embedded with `include_bytes!` rather than read at runtime, to match how
/// `app.toml` is embedded: the binary stays self-contained and there is no file
/// to lose between build and install.
fn tray_icon() -> Icon {
    // A generated square is a poor icon but a missing tray is worse -- the tray
    // carries Settings, Sync now and Restart, so failing to build it would take
    // the app's only controls with it. Degrade rather than propagate.
    decode_png_icon(include_bytes!("../icons/icon.png")).unwrap_or_else(default_icon)
}

fn decode_png_icon(bytes: &[u8]) -> Option<Icon> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    let scaled = image::imageops::resize(
        &img.to_rgba8(),
        TRAY_PX,
        TRAY_PX,
        image::imageops::FilterType::Lanczos3,
    );
    Icon::from_rgba(scaled.into_raw(), TRAY_PX, TRAY_PX).ok()
}

/// 32x32 solid rounded-square RGBA icon, used only when the PNG cannot be
/// decoded. Kept so the tray can always be built.
fn default_icon() -> Icon {
    const S: u32 = 32;
    let mut rgba = Vec::with_capacity((S * S * 4) as usize);
    for y in 0..S {
        for x in 0..S {
            let dx = x as i32 - 16;
            let dy = y as i32 - 16;
            let inside = dx * dx + dy * dy < 15 * 15;
            if inside {
                rgba.extend_from_slice(&[0x4a, 0x7d, 0xfc, 0xff]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    Icon::from_rgba(rgba, S, S).expect("static icon is valid")
}

pub fn build(model: &[Entry], tooltip: &str) -> anyhow::Result<Tray> {
    let (menu, actions) = build_menu(model)?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(tooltip)
        .with_icon(tray_icon())
        .build()?;
    Ok(Tray { tray, actions })
}

pub fn rebuild_menu(tray: &mut Tray, model: &[Entry]) -> anyhow::Result<()> {
    let (menu, actions) = build_menu(model)?;
    tray.tray.set_menu(Some(Box::new(menu)));
    tray.actions = actions;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MenuItemCfg, MenuSection};

    fn menu_cfg() -> MenuSection {
        MenuSection {
            settings: true,
            items: vec![MenuItemCfg {
                id: "docs".to_string(),
                label: "Docs".to_string(),
                open_url: "https://example.com".to_string(),
            }],
        }
    }

    #[test]
    fn the_shipped_icon_decodes_rather_than_falling_back() {
        // The fallback exists so a broken PNG cannot take the tray down with it,
        // which also means a broken PNG is silent. This is what makes it loud:
        // replace icons/icon.png with something undecodable and this fails,
        // instead of the app shipping a blue square nobody chose.
        assert!(
            decode_png_icon(include_bytes!("../icons/icon.png")).is_some(),
            "icons/icon.png must decode as PNG -- the tray would silently fall \
             back to the generated placeholder square"
        );
    }

    #[test]
    fn a_broken_icon_falls_back_instead_of_panicking() {
        assert!(decode_png_icon(b"not a png at all").is_none());
        assert!(decode_png_icon(&[]).is_none());
        // The PNG magic number followed by nothing usable: past the sniffing
        // stage, so it exercises the decode failure rather than format detection.
        assert!(decode_png_icon(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).is_none());
    }

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
}
