use crate::config::MenuSection;
use std::collections::HashMap;
use tray_icon::menu::{Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Debug, Clone, PartialEq)]
pub enum TrayAction {
    Open,
    OpenUrl(String),
    Settings,
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

/// 32x32 solid rounded-square RGBA icon so the template needs no binary assets.
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
        .with_icon(default_icon())
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
    fn full_model_order() {
        let m = menu_model(&menu_cfg(), "My App", true, true);
        let expected = vec![
            Entry::Action(TrayAction::Open, "Open My App".to_string()),
            Entry::Action(
                TrayAction::OpenUrl("https://example.com".to_string()),
                "Docs".to_string(),
            ),
            Entry::Action(TrayAction::Settings, "Settings…".to_string()),
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
        let m = menu_model(&empty, "X", false, false);
        let expected = vec![
            Entry::Action(TrayAction::Restart, "Restart App".to_string()),
            Entry::Separator,
            Entry::Action(TrayAction::Quit, "Quit".to_string()),
        ];
        assert_eq!(m, expected);
    }
}
