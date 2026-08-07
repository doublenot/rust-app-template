# Chrome-Host Desktop App Template Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A clone-and-configure Rust template that launches Google Chrome in `--app` mode on a configured URL, with tray menu, optional local-server supervision + loading screen, and schema-driven settings.

**Architecture:** One Rust binary. `tao` event loop on the main thread hosts a `tray-icon`/`muda` tray menu; a `tokio` runtime on background threads runs an `axum` internal server (loading/placeholder/settings pages + status API, bound to 127.0.0.1) and supervises two child processes: the configured app server and the Chrome instance (forced into a dedicated `--user-data-dir` so we own its lifetime).

**Tech Stack:** Rust (stable), tao, tray-icon, rfd, axum, tokio, reqwest, serde/toml/serde_json, fd-lock, dirs, open, rand; cargo-packager + GitHub Actions for packaging.

**Spec:** `docs/superpowers/specs/2026-07-27-rust-app-template-design.md` — read it before starting.

## Global Constraints

- Rust stable, `edition = "2021"`, `rust-version = "1.85"`.
- Package/binary name: `hitch`. Config file: `app.toml` at repo root, embedded via `include_str!`.
- Google Chrome only — no Chromium/Edge/Brave fallback. Download URL: `https://www.google.com/chrome/`.
- Internal server binds `127.0.0.1` only; mutating endpoints require header `x-host-token` matching a per-launch random token.
- Settings env var contract: `APP_SETTING_<KEY uppercased>` per field + `APP_SETTINGS_FILE=<path to settings.json>`.
- Runtime data dir: `<OS data dir>/<identifier>/` containing `chrome-profile/`, `logs/`, `settings.json`, `app.lock`.
- Log files size-capped at 5 MB: on open, an oversized log is renamed to `<name>.old`.
- Every commit message ends with trailer: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- After each task: `cargo fmt` and `cargo clippy --all-targets -- -D warnings` must be clean before committing.
- Crate versions given below are minimums from the spec era; if a listed version fails to resolve, use the latest semver-compatible release and note it in the commit body.

---

### Task 1: Project scaffold

**Files:**
- Create: `Cargo.toml`, `app.toml`, `.gitignore`, `src/main.rs`

**Interfaces:**
- Consumes: nothing (first task).
- Produces: a compiling crate named `hitch`; `app.toml` in placeholder mode (empty `url`, no `[server]`) that later tasks embed.

- [ ] **Step 1: Write `Cargo.toml`**

```toml
[package]
name = "hitch"
version = "0.1.0"
edition = "2021"
rust-version = "1.85"
description = "Desktop app template: Rust host rendering a configured URL in Google Chrome"
license = "MIT"

[dependencies]
anyhow = "1"
axum = "0.8"
dirs = "6"
fd-lock = "4"
open = "5"
rand = "0.9"
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }
rfd = { version = "0.15", default-features = false, features = ["gtk3"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tao = "0.31"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "process", "net", "time", "sync"] }
toml = "0.8"
tray-icon = "0.21"

[target.'cfg(windows)'.dependencies]
winreg = "0.55"

[dev-dependencies]
tempfile = "3"

[package.metadata.packager]
product-name = "Hitch"
identifier = "com.example.hitch"
icons = ["icons/icon.png"]
```

Note: `rfd`'s `gtk3` feature only affects Linux; on Windows/macOS it uses native dialogs regardless.

- [ ] **Step 2: Write `app.toml` (placeholder mode — the file app developers edit)**

```toml
# ── Hitch configuration ─────────────────────────────
# Edit this file, then `cargo build`. Full reference in README.md.

[app]
name = "Hitch"                # window title, tray tooltip
identifier = "com.example.hitch"   # reverse-domain; names the app-data dir
url = ""                                # target URL; empty → built-in placeholder page

# Uncomment to start a local server before showing the app.
# [server]
# command = ["node", "server.js"]       # argv array, no shell
# cwd = "server"                        # relative: exe dir (release) / repo root (debug)
# env = { PORT = "8080" }
# health_check_url = "http://127.0.0.1:8080/"
# startup_timeout_secs = 60

[window]
width = 1200
height = 800
on_close = "quit"                       # "quit" | "tray"

[menu]
settings = false                        # true requires [[settings.fields]] below
items = []                              # e.g. [{ id = "docs", label = "Docs", open_url = "https://example.com" }]

# [[settings.fields]]
# key = "theme"
# label = "Theme"
# type = "select"                       # "text" | "boolean" | "select"
# default = "light"
# options = ["light", "dark"]
```

- [ ] **Step 3: Write `.gitignore`**

```
/target
```

- [ ] **Step 4: Write stub `src/main.rs`**

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    println!("hitch scaffold");
}
```

- [ ] **Step 5: Verify it builds**

Run: `cargo check`
Expected: success (warnings acceptable only in this task).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock app.toml .gitignore src/main.rs
git commit -m "feat: scaffold hitch crate with default app.toml"
```

---

### Task 2: Config module (`config.rs`)

**Files:**
- Create: `src/config.rs`
- Modify: `src/main.rs` (add `mod config;`)

**Interfaces:**
- Consumes: `app.toml` from Task 1.
- Produces (exact, used by every later task):
  - `pub struct AppConfig { pub app: AppSection, pub server: Option<ServerSection>, pub window: WindowSection, pub menu: MenuSection, pub settings: Option<SettingsSection> }`
  - `pub struct AppSection { pub name: String, pub identifier: String, pub url: String }`
  - `pub struct ServerSection { pub command: Vec<String>, pub cwd: Option<String>, pub env: BTreeMap<String, String>, pub health_check_url: String, pub startup_timeout_secs: u64 }`
  - `pub struct WindowSection { pub width: u32, pub height: u32, pub on_close: OnClose }`; `pub enum OnClose { Quit, Tray }` (Copy, PartialEq, Default = Quit)
  - `pub struct MenuSection { pub settings: bool, pub items: Vec<MenuItemCfg> }`; `pub struct MenuItemCfg { pub id: String, pub label: String, pub open_url: String }`
  - `pub struct SettingsSection { pub fields: Vec<SettingsField> }`; `pub struct SettingsField { pub key: String, pub label: String, pub field_type: FieldType, pub default: toml::Value, pub options: Vec<String> }`; `pub enum FieldType { Text, Boolean, Select }` (Copy, PartialEq)
  - All config structs derive `Debug + Clone + Deserialize`.
  - `impl AppConfig { pub fn from_str(s: &str) -> Result<Self, String>; pub fn load() -> Result<Self, String>; pub fn settings_enabled(&self) -> bool }`
  - `pub struct RuntimePaths { pub data_dir: PathBuf, pub chrome_profile: PathBuf, pub logs_dir: PathBuf, pub settings_file: PathBuf, pub lock_file: PathBuf }`
  - `impl RuntimePaths { pub fn under(base: &Path, identifier: &str) -> Self; pub fn resolve(identifier: &str) -> Self; pub fn ensure(&self) -> std::io::Result<()> }`

- [ ] **Step 1: Write the failing tests** (bottom of `src/config.rs`; the module body above them can start empty)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> &'static str {
        "[app]\nname = \"X\"\nidentifier = \"com.example.x\"\n"
    }

    #[test]
    fn minimal_config_parses_with_defaults() {
        let c = AppConfig::from_str(minimal()).unwrap();
        assert_eq!(c.app.url, "");
        assert!(c.server.is_none());
        assert_eq!(c.window.width, 1200);
        assert_eq!(c.window.height, 800);
        assert_eq!(c.window.on_close, OnClose::Quit);
        assert!(!c.settings_enabled());
    }

    #[test]
    fn embedded_default_config_is_valid() {
        AppConfig::load().unwrap();
    }

    #[test]
    fn identifier_must_be_reverse_domain() {
        for bad in ["", "nodots", "trailing.", ".leading", "a..b"] {
            let s = format!("[app]\nname = \"X\"\nidentifier = \"{bad}\"\n");
            assert!(AppConfig::from_str(&s).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn server_requires_command_and_health_url() {
        let no_health = format!("{}[server]\ncommand = [\"x\"]\n", minimal());
        assert!(AppConfig::from_str(&no_health).is_err());
        let empty_cmd = format!(
            "{}[server]\ncommand = []\nhealth_check_url = \"http://127.0.0.1:1/\"\n",
            minimal()
        );
        assert!(AppConfig::from_str(&empty_cmd).is_err());
        let ok = format!(
            "{}[server]\ncommand = [\"x\"]\nhealth_check_url = \"http://127.0.0.1:1/\"\n",
            minimal()
        );
        let c = AppConfig::from_str(&ok).unwrap();
        assert_eq!(c.server.as_ref().unwrap().startup_timeout_secs, 60);
    }

    #[test]
    fn settings_flag_requires_fields() {
        let s = format!("{}[menu]\nsettings = true\n", minimal());
        assert!(AppConfig::from_str(&s).is_err());
    }

    #[test]
    fn settings_field_validation() {
        let base = format!("{}[menu]\nsettings = true\n", minimal());
        // select requires options
        let s = format!(
            "{base}[[settings.fields]]\nkey = \"k\"\nlabel = \"K\"\ntype = \"select\"\ndefault = \"a\"\n"
        );
        assert!(AppConfig::from_str(&s).is_err());
        // select default must be a member of options
        let s = format!(
            "{base}[[settings.fields]]\nkey = \"k\"\nlabel = \"K\"\ntype = \"select\"\ndefault = \"z\"\noptions = [\"a\", \"b\"]\n"
        );
        assert!(AppConfig::from_str(&s).is_err());
        // boolean default must be bool
        let s = format!(
            "{base}[[settings.fields]]\nkey = \"k\"\nlabel = \"K\"\ntype = \"boolean\"\ndefault = \"yes\"\n"
        );
        assert!(AppConfig::from_str(&s).is_err());
        // bad key characters
        let s = format!(
            "{base}[[settings.fields]]\nkey = \"bad-key\"\nlabel = \"K\"\ntype = \"text\"\ndefault = \"\"\n"
        );
        assert!(AppConfig::from_str(&s).is_err());
        // duplicate keys
        let s = format!(
            "{base}[[settings.fields]]\nkey = \"k\"\nlabel = \"A\"\ntype = \"text\"\ndefault = \"\"\n[[settings.fields]]\nkey = \"k\"\nlabel = \"B\"\ntype = \"text\"\ndefault = \"\"\n"
        );
        assert!(AppConfig::from_str(&s).is_err());
        // valid select passes
        let s = format!(
            "{base}[[settings.fields]]\nkey = \"theme\"\nlabel = \"Theme\"\ntype = \"select\"\ndefault = \"light\"\noptions = [\"light\", \"dark\"]\n"
        );
        let c = AppConfig::from_str(&s).unwrap();
        assert!(c.settings_enabled());
        assert_eq!(c.settings.unwrap().fields[0].field_type, FieldType::Select);
    }

    #[test]
    fn on_close_rejects_unknown_values() {
        let s = format!("{}[window]\non_close = \"minimize\"\n", minimal());
        assert!(AppConfig::from_str(&s).is_err());
    }

    #[test]
    fn runtime_paths_layout() {
        let p = RuntimePaths::under(std::path::Path::new("/base"), "com.example.x");
        assert_eq!(p.data_dir, std::path::PathBuf::from("/base/com.example.x"));
        assert_eq!(p.chrome_profile, p.data_dir.join("chrome-profile"));
        assert_eq!(p.logs_dir, p.data_dir.join("logs"));
        assert_eq!(p.settings_file, p.data_dir.join("settings.json"));
        assert_eq!(p.lock_file, p.data_dir.join("app.lock"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test config -- --nocapture` (after adding `mod config;` to `main.rs`)
Expected: compile error — types not defined.

- [ ] **Step 3: Implement `src/config.rs`**

```rust
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const EMBEDDED_CONFIG: &str = include_str!("../app.toml");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub app: AppSection,
    #[serde(default)]
    pub server: Option<ServerSection>,
    #[serde(default)]
    pub window: WindowSection,
    #[serde(default)]
    pub menu: MenuSection,
    #[serde(default)]
    pub settings: Option<SettingsSection>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSection {
    pub name: String,
    pub identifier: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    pub command: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub health_check_url: String,
    #[serde(default = "default_timeout")]
    pub startup_timeout_secs: u64,
}

fn default_timeout() -> u64 {
    60
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowSection {
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default)]
    pub on_close: OnClose,
}

impl Default for WindowSection {
    fn default() -> Self {
        Self { width: default_width(), height: default_height(), on_close: OnClose::Quit }
    }
}

fn default_width() -> u32 {
    1200
}

fn default_height() -> u32 {
    800
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnClose {
    #[default]
    Quit,
    Tray,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MenuSection {
    #[serde(default)]
    pub settings: bool,
    #[serde(default)]
    pub items: Vec<MenuItemCfg>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MenuItemCfg {
    pub id: String,
    pub label: String,
    pub open_url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsSection {
    pub fields: Vec<SettingsField>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    pub default: toml::Value,
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    Text,
    Boolean,
    Select,
}

impl AppConfig {
    pub fn from_str(s: &str) -> Result<Self, String> {
        let cfg: AppConfig = toml::from_str(s).map_err(|e| format!("app.toml parse error: {e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load() -> Result<Self, String> {
        Self::from_str(EMBEDDED_CONFIG)
    }

    pub fn settings_enabled(&self) -> bool {
        self.menu.settings && self.settings.is_some()
    }

    fn validate(&self) -> Result<(), String> {
        if self.app.name.trim().is_empty() {
            return Err("app.name must not be empty".into());
        }
        let id = &self.app.identifier;
        let parts: Vec<&str> = id.split('.').collect();
        if parts.len() < 2
            || parts.iter().any(|p| {
                p.is_empty() || !p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
        {
            return Err(format!(
                "app.identifier {id:?} must be reverse-domain, e.g. com.example.myapp"
            ));
        }
        if let Some(server) = &self.server {
            if server.command.is_empty() {
                return Err("server.command must have at least one element".into());
            }
            if server.health_check_url.trim().is_empty() {
                return Err("server.health_check_url is required when [server] is present".into());
            }
        }
        if self.menu.settings {
            let fields = match &self.settings {
                Some(s) if !s.fields.is_empty() => &s.fields,
                _ => {
                    return Err(
                        "menu.settings = true requires at least one [[settings.fields]]".into()
                    )
                }
            };
            let mut seen = std::collections::BTreeSet::new();
            for f in fields {
                if f.key.is_empty()
                    || !f.key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    return Err(format!("settings key {:?} must match [A-Za-z0-9_]+", f.key));
                }
                if !seen.insert(&f.key) {
                    return Err(format!("duplicate settings key {:?}", f.key));
                }
                match f.field_type {
                    FieldType::Select => {
                        if f.options.is_empty() {
                            return Err(format!("select field {:?} requires options", f.key));
                        }
                        match f.default.as_str() {
                            Some(d) if f.options.iter().any(|o| o == d) => {}
                            _ => {
                                return Err(format!(
                                    "select field {:?} default must be one of its options",
                                    f.key
                                ))
                            }
                        }
                    }
                    FieldType::Boolean => {
                        if !f.default.is_bool() {
                            return Err(format!(
                                "boolean field {:?} default must be true/false",
                                f.key
                            ));
                        }
                        if !f.options.is_empty() {
                            return Err(format!("field {:?} must not have options", f.key));
                        }
                    }
                    FieldType::Text => {
                        if !f.default.is_str() {
                            return Err(format!("text field {:?} default must be a string", f.key));
                        }
                        if !f.options.is_empty() {
                            return Err(format!("field {:?} must not have options", f.key));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct RuntimePaths {
    pub data_dir: PathBuf,
    pub chrome_profile: PathBuf,
    pub logs_dir: PathBuf,
    pub settings_file: PathBuf,
    pub lock_file: PathBuf,
}

impl RuntimePaths {
    pub fn under(base: &Path, identifier: &str) -> Self {
        let data_dir = base.join(identifier);
        Self {
            chrome_profile: data_dir.join("chrome-profile"),
            logs_dir: data_dir.join("logs"),
            settings_file: data_dir.join("settings.json"),
            lock_file: data_dir.join("app.lock"),
            data_dir,
        }
    }

    pub fn resolve(identifier: &str) -> Self {
        let base = dirs::data_dir().expect("no OS data directory available");
        Self::under(&base, identifier)
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.chrome_profile)?;
        std::fs::create_dir_all(&self.logs_dir)
    }
}
```

Also change `src/main.rs` to:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;

fn main() {
    println!("hitch scaffold");
}
```

(`mod config;` will warn as unused — add `#[allow(dead_code)]` above `mod config;` temporarily; Task 8 removes it.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test config`
Expected: all 8 tests PASS.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/config.rs src/main.rs
git commit -m "feat: config parsing, validation, and runtime paths"
```

---

### Task 3: Settings module (`settings.rs`)

**Files:**
- Create: `src/settings.rs`
- Modify: `src/main.rs` (add `mod settings;` under the same temporary `#[allow(dead_code)]` pattern)

**Interfaces:**
- Consumes: `config::{SettingsSection, SettingsField, FieldType}` (Task 2).
- Produces (exact):
  - `pub fn defaults(schema: &SettingsSection) -> serde_json::Map<String, serde_json::Value>`
  - `pub fn load(schema: &SettingsSection, path: &Path) -> serde_json::Map<String, serde_json::Value>` — defaults overlaid with valid values from `settings.json`; unknown keys and type-mismatched values ignored.
  - `pub fn validate_incoming(schema: &SettingsSection, incoming: &serde_json::Value) -> Result<serde_json::Map<String, serde_json::Value>, String>` — rejects unknown keys, wrong types, select values not in options; missing keys fall back to defaults.
  - `pub fn save(path: &Path, values: &serde_json::Map<String, serde_json::Value>) -> std::io::Result<()>`
  - `pub fn env_vars(values: &serde_json::Map<String, serde_json::Value>) -> Vec<(String, String)>` — `("APP_SETTING_" + key.to_uppercase(), string form)`; bools as `"true"`/`"false"`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use serde_json::json;

    fn schema() -> crate::config::SettingsSection {
        let s = r#"
[app]
name = "X"
identifier = "com.example.x"
[menu]
settings = true
[[settings.fields]]
key = "theme"
label = "Theme"
type = "select"
default = "light"
options = ["light", "dark"]
[[settings.fields]]
key = "notify"
label = "Notifications"
type = "boolean"
default = true
[[settings.fields]]
key = "nickname"
label = "Nickname"
type = "text"
default = ""
"#;
        AppConfig::from_str(s).unwrap().settings.unwrap()
    }

    #[test]
    fn defaults_come_from_schema() {
        let d = defaults(&schema());
        assert_eq!(d["theme"], json!("light"));
        assert_eq!(d["notify"], json!(true));
        assert_eq!(d["nickname"], json!(""));
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let d = load(&schema(), &dir.path().join("settings.json"));
        assert_eq!(d["theme"], json!("light"));
    }

    #[test]
    fn save_then_load_round_trip_ignoring_junk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let incoming = json!({ "theme": "dark", "notify": false, "nickname": "dn" });
        let validated = validate_incoming(&schema(), &incoming).unwrap();
        save(&path, &validated).unwrap();
        // corrupt with junk: unknown key + wrong type
        let mut on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        on_disk["junk"] = json!(1);
        on_disk["notify"] = json!("not-a-bool");
        std::fs::write(&path, serde_json::to_string(&on_disk).unwrap()).unwrap();
        let loaded = load(&schema(), &path);
        assert_eq!(loaded["theme"], json!("dark"));
        assert_eq!(loaded["notify"], json!(true)); // junk value fell back to default
        assert!(!loaded.contains_key("junk"));
    }

    #[test]
    fn validate_rejects_bad_input() {
        assert!(validate_incoming(&schema(), &json!({ "unknown": 1 })).is_err());
        assert!(validate_incoming(&schema(), &json!({ "theme": "purple" })).is_err());
        assert!(validate_incoming(&schema(), &json!({ "notify": "yes" })).is_err());
        assert!(validate_incoming(&schema(), &json!([1, 2])).is_err());
        // missing keys fall back to defaults
        let v = validate_incoming(&schema(), &json!({ "theme": "dark" })).unwrap();
        assert_eq!(v["notify"], json!(true));
    }

    #[test]
    fn env_vars_contract() {
        let v = validate_incoming(&schema(), &json!({ "theme": "dark", "notify": false }))
            .unwrap();
        let env = env_vars(&v);
        assert!(env.contains(&("APP_SETTING_THEME".into(), "dark".into())));
        assert!(env.contains(&("APP_SETTING_NOTIFY".into(), "false".into())));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test settings`
Expected: compile error — functions not defined.

- [ ] **Step 3: Implement `src/settings.rs`**

```rust
use crate::config::{FieldType, SettingsField, SettingsSection};
use serde_json::{Map, Value};
use std::path::Path;

fn default_json(f: &SettingsField) -> Value {
    match f.field_type {
        FieldType::Boolean => Value::Bool(f.default.as_bool().unwrap_or(false)),
        FieldType::Text | FieldType::Select => {
            Value::String(f.default.as_str().unwrap_or_default().to_string())
        }
    }
}

fn value_fits(f: &SettingsField, v: &Value) -> bool {
    match f.field_type {
        FieldType::Boolean => v.is_boolean(),
        FieldType::Text => v.is_string(),
        FieldType::Select => match v.as_str() {
            Some(s) => f.options.iter().any(|o| o == s),
            None => false,
        },
    }
}

pub fn defaults(schema: &SettingsSection) -> Map<String, Value> {
    schema
        .fields
        .iter()
        .map(|f| (f.key.clone(), default_json(f)))
        .collect()
}

pub fn load(schema: &SettingsSection, path: &Path) -> Map<String, Value> {
    let mut values = defaults(schema);
    let Ok(text) = std::fs::read_to_string(path) else {
        return values;
    };
    let Ok(Value::Object(on_disk)) = serde_json::from_str::<Value>(&text) else {
        return values;
    };
    for f in &schema.fields {
        if let Some(v) = on_disk.get(&f.key) {
            if value_fits(f, v) {
                values.insert(f.key.clone(), v.clone());
            }
        }
    }
    values
}

pub fn validate_incoming(
    schema: &SettingsSection,
    incoming: &Value,
) -> Result<Map<String, Value>, String> {
    let Value::Object(obj) = incoming else {
        return Err("settings payload must be a JSON object".into());
    };
    for key in obj.keys() {
        if !schema.fields.iter().any(|f| &f.key == key) {
            return Err(format!("unknown settings key {key:?}"));
        }
    }
    let mut values = Map::new();
    for f in &schema.fields {
        match obj.get(&f.key) {
            None => {
                values.insert(f.key.clone(), default_json(f));
            }
            Some(v) if value_fits(f, v) => {
                values.insert(f.key.clone(), v.clone());
            }
            Some(v) => return Err(format!("invalid value {v} for settings key {:?}", f.key)),
        }
    }
    Ok(values)
}

pub fn save(path: &Path, values: &Map<String, Value>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&Value::Object(values.clone()))?)
}

pub fn env_vars(values: &Map<String, Value>) -> Vec<(String, String)> {
    values
        .iter()
        .map(|(k, v)| {
            let s = match v {
                Value::String(s) => s.clone(),
                Value::Bool(b) => b.to_string(),
                other => other.to_string(),
            };
            (format!("APP_SETTING_{}", k.to_uppercase()), s)
        })
        .collect()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test settings`
Expected: all 5 tests PASS.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/settings.rs src/main.rs
git commit -m "feat: schema-driven settings load/validate/save and env-var contract"
```

---

### Task 4: Chrome module (`chrome.rs`)

**Files:**
- Create: `src/chrome.rs`
- Modify: `src/main.rs` (add `mod chrome;`)

**Interfaces:**
- Consumes: nothing new.
- Produces (exact):
  - `pub const DOWNLOAD_URL: &str = "https://www.google.com/chrome/";`
  - `pub fn candidates() -> Vec<PathBuf>` — platform-specific candidate executables (registry-derived paths included on Windows).
  - `pub fn find_first_existing(paths: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Option<PathBuf>`
  - `pub fn find_chrome() -> Option<PathBuf>`
  - `pub fn launch_args(url: &str, profile_dir: &Path, width: u32, height: u32) -> Vec<OsString>`
  - `pub fn launch(exe: &Path, url: &str, profile_dir: &Path, width: u32, height: u32) -> std::io::Result<tokio::process::Child>` — `kill_on_drop(true)`.
  - `pub fn open_extra_window(exe: &Path, url: &str, profile_dir: &Path) -> std::io::Result<()>` — fire-and-forget `std::process::Command` (no kill_on_drop; the process delegates to the owned Chrome and exits), used for the settings window.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn candidates_is_never_empty() {
        assert!(!candidates().is_empty());
    }

    #[test]
    fn find_first_existing_respects_order_and_checker() {
        let paths = vec![PathBuf::from("/a"), PathBuf::from("/b"), PathBuf::from("/c")];
        let found = find_first_existing(&paths, |p| p.ends_with("b") || p.ends_with("c"));
        assert_eq!(found, Some(PathBuf::from("/b")));
        assert_eq!(find_first_existing(&paths, |_| false), None);
    }

    #[test]
    fn launch_args_contract() {
        let args = launch_args("http://127.0.0.1:9/loading", Path::new("/data/profile"), 1280, 900);
        let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.contains(&"--app=http://127.0.0.1:9/loading".to_string()));
        assert!(args.iter().any(|a| a.starts_with("--user-data-dir=")));
        assert!(args.contains(&"--no-first-run".to_string()));
        assert!(args.contains(&"--no-default-browser-check".to_string()));
        assert!(args.contains(&"--window-size=1280,900".to_string()));
    }

    #[test]
    #[ignore = "requires Google Chrome installed; run locally with cargo test -- --ignored"]
    fn real_chrome_reports_version() {
        let exe = find_chrome().expect("Chrome not installed");
        let out = std::process::Command::new(exe).arg("--version").output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("Chrome"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test chrome`
Expected: compile error — functions not defined.

- [ ] **Step 3: Implement `src/chrome.rs`**

```rust
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const DOWNLOAD_URL: &str = "https://www.google.com/chrome/";

#[cfg(target_os = "windows")]
pub fn candidates() -> Vec<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    let mut out = Vec::new();
    const APP_PATHS: &str =
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\chrome.exe";
    for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        if let Ok(key) = RegKey::predef(hive).open_subkey(APP_PATHS) {
            if let Ok(path) = key.get_value::<String, _>("") {
                out.push(PathBuf::from(path));
            }
        }
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
        if let Some(base) = std::env::var_os(var) {
            out.push(
                PathBuf::from(base).join(r"Google\Chrome\Application\chrome.exe"),
            );
        }
    }
    out
}

#[cfg(target_os = "macos")]
pub fn candidates() -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from(
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    )];
    if let Some(home) = dirs::home_dir() {
        out.push(home.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
    }
    out
}

#[cfg(target_os = "linux")]
pub fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            out.push(dir.join("google-chrome"));
            out.push(dir.join("google-chrome-stable"));
        }
    }
    out.push(PathBuf::from("/opt/google/chrome/chrome"));
    out
}

pub fn find_first_existing(
    paths: &[PathBuf],
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    paths.iter().find(|p| exists(p)).cloned()
}

pub fn find_chrome() -> Option<PathBuf> {
    find_first_existing(&candidates(), |p| p.is_file())
}

pub fn launch_args(url: &str, profile_dir: &Path, width: u32, height: u32) -> Vec<OsString> {
    let mut user_data_dir = OsString::from("--user-data-dir=");
    user_data_dir.push(profile_dir.as_os_str());
    vec![
        format!("--app={url}").into(),
        user_data_dir,
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        format!("--window-size={width},{height}").into(),
    ]
}

pub fn launch(
    exe: &Path,
    url: &str,
    profile_dir: &Path,
    width: u32,
    height: u32,
) -> std::io::Result<tokio::process::Child> {
    tokio::process::Command::new(exe)
        .args(launch_args(url, profile_dir, width, height))
        .kill_on_drop(true)
        .spawn()
}

pub fn open_extra_window(exe: &Path, url: &str, profile_dir: &Path) -> std::io::Result<()> {
    // Same profile dir → this process hands the URL to the Chrome instance we
    // already own and exits on its own; never kill_on_drop it.
    std::process::Command::new(exe)
        .args(launch_args(url, profile_dir, 700, 560))
        .spawn()
        .map(drop)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test chrome`
Expected: 3 PASS, 1 ignored. If Chrome is installed locally, also run `cargo test chrome -- --ignored` and expect PASS.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/chrome.rs src/main.rs
git commit -m "feat: Chrome detection and app-mode launch with owned profile dir"
```

---

### Task 5: Server supervisor (`supervisor.rs`)

**Files:**
- Create: `src/supervisor.rs`
- Modify: `src/main.rs` (add `mod supervisor;`)

**Interfaces:**
- Consumes: `config::ServerSection` (Task 2); settings `env_vars` output shape (Task 3).
- Produces (exact):
  - `pub struct ServerHandle { pub child: tokio::process::Child, pub log_path: PathBuf }`
  - `pub fn resolve_cwd(cwd: Option<&str>, base_dir: &Path) -> PathBuf` — `None` → base; absolute → as-is; relative → joined to base. (Caller picks base: exe dir in release, `CARGO_MANIFEST_DIR` in debug.)
  - `pub fn base_dir() -> PathBuf` — debug builds: `env!("CARGO_MANIFEST_DIR")`; release: executable's directory.
  - `pub fn build_env(cfg_env: &BTreeMap<String, String>, settings_env: &[(String, String)], settings_file: &Path) -> Vec<(String, String)>` — cfg env first, then settings vars, then `APP_SETTINGS_FILE`.
  - `pub fn open_log(path: &Path, max_bytes: u64) -> std::io::Result<std::fs::File>` — append handle; if the file already exceeds `max_bytes`, rename it to `<name>.old` (replacing any previous `.old`) first.
  - `pub fn spawn_server(cfg: &ServerSection, env: Vec<(String, String)>, cwd: PathBuf, log_path: &Path) -> std::io::Result<ServerHandle>` — stdout+stderr appended to the log, `kill_on_drop(true)`.
  - `pub async fn wait_healthy(url: &str, timeout: Duration) -> Result<(), String>` — GET every 500 ms; ready when status < 400; `Err` describes the timeout.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    #[test]
    fn resolve_cwd_rules() {
        let base = Path::new("/base");
        assert_eq!(resolve_cwd(None, base), PathBuf::from("/base"));
        assert_eq!(resolve_cwd(Some("server"), base), PathBuf::from("/base/server"));
        let abs = if cfg!(windows) { r"C:\abs" } else { "/abs" };
        assert_eq!(resolve_cwd(Some(abs), base), PathBuf::from(abs));
    }

    #[test]
    fn build_env_layers_and_settings_file() {
        let mut cfg_env = BTreeMap::new();
        cfg_env.insert("PORT".to_string(), "8080".to_string());
        let settings = vec![("APP_SETTING_THEME".to_string(), "dark".to_string())];
        let env = build_env(&cfg_env, &settings, Path::new("/data/settings.json"));
        assert!(env.contains(&("PORT".into(), "8080".into())));
        assert!(env.contains(&("APP_SETTING_THEME".into(), "dark".into())));
        assert!(env
            .iter()
            .any(|(k, v)| k == "APP_SETTINGS_FILE" && v.ends_with("settings.json")));
    }

    #[test]
    fn open_log_rotates_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.log");
        std::fs::write(&path, vec![b'x'; 64]).unwrap();
        let _f = open_log(&path, 16).unwrap();
        assert!(dir.path().join("server.log.old").exists());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    }

    #[tokio::test]
    async fn wait_healthy_succeeds_once_server_responds() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (seen, mut requests) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                // Drain the request before answering it. Closing a socket whose receive
                // buffer still holds unread bytes is an *abortive* close on Windows: the
                // teardown outruns the response we just wrote, hyper fails the exchange in
                // SendRequest, and every poll `wait_healthy` makes fails until the 5s budget
                // expires. This is the whole of why the test failed on windows-latest and
                // nowhere else; the real child servers this stands in for read their requests.
                //
                // Measured on the runner rather than reasoned, because this machine cannot
                // reproduce it -- all four combinations below pass on Linux. 5 requests each,
                // `os=windows`:
                //
                //     drain  shutdown            result
                //     no     no                  0/5, os error 10053
                //     yes    no                  5/5
                //     no     yes                 0/5, os error 10053
                //     yes    yes                 5/5
                //
                // So the drain is the entire fix and a graceful `shutdown()` is worth nothing
                // here -- the abort comes from the unread bytes, not from skipping the FIN,
                // and 10053 is WSAECONNABORTED (the local stack tore it down), not the
                // WSAECONNRESET a peer's RST would raise. Do not "simplify" this by keeping
                // a shutdown and dropping the read loop; that is the combination that fails.
                let mut req = Vec::new();
                loop {
                    let mut buf = [0u8; 1024];
                    match sock.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            req.extend_from_slice(&buf[..n]);
                            if req.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let _ = seen.send(String::from_utf8_lossy(&req).into_owned());
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                    .await;
            }
        });
        wait_healthy(&format!("http://{addr}/"), Duration::from_secs(5))
            .await
            .unwrap();
        // Pins the drain above rather than trusting it: the read loop has to have consumed a
        // whole request for this to arrive, which is the behaviour whose absence made Windows
        // reset the connection instead of answering it.
        let req = requests
            .recv()
            .await
            .expect("the health check sent a request");
        assert!(req.starts_with("GET / HTTP/1.1\r\n"), "got: {req:?}");
    }

    #[tokio::test]
    async fn wait_healthy_times_out_when_nothing_listens() {
        // bind then drop to get a port that refuses connections
        let addr = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap()
        };
        let err = wait_healthy(&format!("http://{addr}/"), Duration::from_millis(1200))
            .await
            .unwrap_err();
        assert!(err.contains("did not become healthy"), "got: {err}");
    }

    #[tokio::test]
    async fn spawn_server_writes_log() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("server.log");
        let cfg = crate::config::ServerSection {
            command: vec!["cargo".to_string(), "--version".to_string()],
            cwd: None,
            env: BTreeMap::new(),
            health_check_url: "http://127.0.0.1:1/".to_string(),
            startup_timeout_secs: 5,
        };
        let mut handle = spawn_server(
            &cfg,
            vec![],
            std::env::current_dir().unwrap(),
            &log_path,
        )
        .unwrap();
        handle.child.wait().await.unwrap();
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.contains("cargo"), "log was: {log:?}");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test supervisor`
Expected: compile error — functions not defined.

- [ ] **Step 3: Implement `src/supervisor.rs`**

```rust
use crate::config::ServerSection;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct ServerHandle {
    pub child: tokio::process::Child,
    pub log_path: PathBuf,
}

pub fn base_dir() -> PathBuf {
    if cfg!(debug_assertions) {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    } else {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

pub fn resolve_cwd(cwd: Option<&str>, base_dir: &Path) -> PathBuf {
    match cwd {
        None => base_dir.to_path_buf(),
        Some(c) => {
            let p = Path::new(c);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                base_dir.join(p)
            }
        }
    }
}

pub fn build_env(
    cfg_env: &BTreeMap<String, String>,
    settings_env: &[(String, String)],
    settings_file: &Path,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> =
        cfg_env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    env.extend(settings_env.iter().cloned());
    env.push((
        "APP_SETTINGS_FILE".to_string(),
        settings_file.to_string_lossy().into_owned(),
    ));
    env
}

pub fn open_log(path: &Path, max_bytes: u64) -> std::io::Result<std::fs::File> {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > max_bytes {
            let mut old = path.as_os_str().to_owned();
            old.push(".old");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(path, &old);
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new().create(true).append(true).open(path)
}

pub const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;

pub fn spawn_server(
    cfg: &ServerSection,
    env: Vec<(String, String)>,
    cwd: PathBuf,
    log_path: &Path,
) -> std::io::Result<ServerHandle> {
    let stdout = open_log(log_path, LOG_MAX_BYTES)?;
    let stderr = stdout.try_clone()?;
    let child = tokio::process::Command::new(&cfg.command[0])
        .args(&cfg.command[1..])
        .envs(env)
        .current_dir(cwd)
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr))
        .kill_on_drop(true)
        .spawn()?;
    Ok(ServerHandle { child, log_path: log_path.to_path_buf() })
}

pub async fn wait_healthy(url: &str, timeout: Duration) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| e.to_string())?;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        // Carried into the timeout message. Without it the operator of a child that will not
        // come up learns only that it did not, and never whether the port refused, the TLS
        // handshake failed, or the thing answered 500 every time. Only the attempt that ran
        // up against the deadline is worth reporting, so it is scoped to the iteration.
        let last = match client.get(url).send().await {
            Ok(resp) if resp.status().as_u16() < 400 => return Ok(()),
            Ok(resp) => format!("last answered {}", resp.status()),
            Err(e) => format!("last error: {e}"),
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "server did not become healthy at {url} within {}s ({last})",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
```

> **Amended by the first CI run on Windows.** Two changes above, one in the test and one in
> `wait_healthy` itself. The test's stand-in server wrote its response and dropped the socket
> without ever reading the request; closing a socket whose receive buffer still holds unread
> bytes is an abortive close on Windows, so the teardown outran the response and every poll
> failed until the 5s budget expired — systematically, on `windows-latest` only. The server
> now drains the request before answering, and the test asserts the drained request so the fix
> is pinned on every platform rather than taken on faith about one nobody here can run.
>
> The 2×2 was then measured on the runner, because a green suite proves the fix and not the
> explanation. Five requests per configuration, `os=windows`: no drain scores **0/5** with or
> without a graceful `shutdown()`, and drain scores **5/5** with or without one. So the drain
> is the whole fix; the first version of this amendment also shut the write half down and
> credited the FIN, which measurably does nothing. The error is `os error 10053`
> (`WSAECONNABORTED`, the local stack tearing it down), not the 10054 `WSAECONNRESET` a peer's
> RST would raise. All four configurations pass on Linux, which is why this survived every
> gate that ran here.
>
> `wait_healthy` discarded the transport error, so its timeout could only ever say that the
> server did not come up and never why — the reason this took a second CI round to diagnose.
> It carries the last error or status into the message now.

Note for the `spawn_server` test: `cargo` is on PATH in dev and CI. On Windows `Command::new("cargo")` resolves `cargo.exe` — no special-casing needed.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test supervisor`
Expected: all 6 tests PASS.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/supervisor.rs src/main.rs
git commit -m "feat: app-server supervisor with env injection, log rotation, health poll"
```

---

### Task 6: Internal server + embedded pages (`internal_server.rs`, `src/assets/`)

**Files:**
- Create: `src/internal_server.rs`, `src/assets/loading.html`, `src/assets/placeholder.html`, `src/assets/settings.html`, `src/assets/style.css`
- Modify: `src/main.rs` (add `mod internal_server;`)

**Interfaces:**
- Consumes: `config::{AppConfig is not needed here — only SettingsSection}` (Task 2), `settings::{load, validate_incoming, save}` (Task 3).
- Produces (exact):
  - `#[derive(Debug, Clone, PartialEq)] pub enum HostEvent { RestartRequested }`
  - `#[derive(Debug, Clone, serde::Serialize)] #[serde(tag = "state", rename_all = "lowercase")] pub enum AppStatus { Starting, Ready { target_url: String }, Error { message: String, log_path: Option<String> } }`
  - `#[derive(Clone)] pub struct HostState { pub app_name: String, pub token: String, pub status: Arc<tokio::sync::RwLock<AppStatus>>, pub schema: Option<SettingsSection>, pub settings_file: PathBuf, pub events: tokio::sync::mpsc::UnboundedSender<HostEvent> }`
  - `pub fn router(state: HostState) -> axum::Router`
  - `pub async fn start(state: HostState) -> std::io::Result<u16>` — binds `127.0.0.1:0`, serves in a spawned task, returns the port.
  - Routes: `GET /loading`, `GET /placeholder`, `GET /settings` (404 when `schema` is `None`), `GET /style.css`, `GET /api/status`, `POST /api/settings` (401 without valid `x-host-token`, 422 on invalid payload, 204 on success), `POST /api/restart` (401 without token; sends `HostEvent::RestartRequested`, 204).

- [ ] **Step 1: Write the four asset files**

`src/assets/style.css`:

```css
:root { color-scheme: light dark; }
* { box-sizing: border-box; }
body {
  margin: 0; min-height: 100vh; display: grid; place-items: center;
  font-family: system-ui, sans-serif;
  background: light-dark(#f6f7f9, #16181d); color: light-dark(#1a1c20, #e8eaed);
}
main { text-align: center; padding: 2rem; max-width: 32rem; }
h1 { font-size: 1.4rem; margin: 0 0 0.75rem; }
p { line-height: 1.5; margin: 0.4rem 0; }
code { background: light-dark(#e8eaee, #23262d); padding: 0.15em 0.4em; border-radius: 4px; }
.spinner {
  width: 2.2rem; height: 2.2rem; margin: 0 auto 1.2rem;
  border: 3px solid light-dark(#d7dbe0, #2c3037); border-top-color: #4a7dfc;
  border-radius: 50%; animation: spin 0.9s linear infinite;
}
@keyframes spin { to { transform: rotate(360deg); } }
.error { color: #d64545; }
.muted { opacity: 0.65; font-size: 0.85rem; }
button {
  font: inherit; padding: 0.5rem 1.1rem; border-radius: 6px; border: 1px solid #4a7dfc;
  background: #4a7dfc; color: white; cursor: pointer; margin-top: 1rem;
}
form { text-align: left; display: grid; gap: 0.9rem; }
label { display: grid; gap: 0.3rem; font-size: 0.9rem; }
input[type="text"], select { font: inherit; padding: 0.45rem; border-radius: 6px;
  border: 1px solid light-dark(#c8cdd4, #3a3f47); background: transparent; color: inherit; }
```

`src/assets/loading.html`:

```html
<!doctype html>
<meta charset="utf-8">
<title>{{APP_NAME}}</title>
<link rel="stylesheet" href="/style.css">
<main>
  <div class="spinner" id="spinner"></div>
  <h1>Starting {{APP_NAME}}…</h1>
  <div id="err" hidden>
    <p class="error" id="err-msg"></p>
    <p class="muted" id="err-log"></p>
    <button onclick="retry()">Retry</button>
  </div>
</main>
<script>
  const TOKEN = "{{TOKEN}}";
  async function tick() {
    try {
      const s = await (await fetch("/api/status")).json();
      if (s.state === "ready") { location.replace(s.target_url); return; }
      if (s.state === "error") {
        document.getElementById("spinner").hidden = true;
        document.getElementById("err").hidden = false;
        document.getElementById("err-msg").textContent = s.message;
        document.getElementById("err-log").textContent = s.log_path ? "Log: " + s.log_path : "";
        return;
      }
    } catch (e) { /* host not ready yet; keep polling */ }
    setTimeout(tick, 500);
  }
  async function retry() {
    await fetch("/api/restart", { method: "POST", headers: { "x-host-token": TOKEN } });
  }
  tick();
</script>
```

`src/assets/placeholder.html`:

```html
<!doctype html>
<meta charset="utf-8">
<title>{{APP_NAME}}</title>
<link rel="stylesheet" href="/style.css">
<main>
  <h1>{{APP_NAME}} is ready to be configured</h1>
  <p>This app was built from the Chrome-host template but has no URL configured yet.</p>
  <p>Edit <code>app.toml</code> in the project root — set <code>[app] url</code>,
     or add a <code>[server]</code> section to launch a local server — then rebuild.</p>
  <p class="muted">See README.md for the full configuration reference.</p>
</main>
```

`src/assets/settings.html`:

```html
<!doctype html>
<meta charset="utf-8">
<title>{{APP_NAME}} — Settings</title>
<link rel="stylesheet" href="/style.css">
<main>
  <h1>{{APP_NAME}} Settings</h1>
  <form id="f" onsubmit="return save(event)">
    {{FIELDS_HTML}}
    <button type="submit">Save</button>
  </form>
  <div id="saved" hidden>
    <p>Saved — restart the app to apply.</p>
    <button onclick="restartApp()">Restart App</button>
  </div>
  <p class="error" id="save-err" hidden></p>
</main>
<script>
  const TOKEN = "{{TOKEN}}";
  function collect() {
    const out = {};
    for (const el of document.querySelectorAll("[data-key]")) {
      out[el.dataset.key] = el.type === "checkbox" ? el.checked : el.value;
    }
    return out;
  }
  async function save(e) {
    e.preventDefault();
    const r = await fetch("/api/settings", {
      method: "POST",
      headers: { "content-type": "application/json", "x-host-token": TOKEN },
      body: JSON.stringify(collect()),
    });
    const err = document.getElementById("save-err");
    if (r.ok) {
      document.getElementById("saved").hidden = false;
      err.hidden = true;
    } else {
      err.textContent = await r.text();
      err.hidden = false;
    }
    return false;
  }
  async function restartApp() {
    await fetch("/api/restart", { method: "POST", headers: { "x-host-token": TOKEN } });
  }
</script>
```

- [ ] **Step 2: Write the failing tests** (bottom of `src/internal_server.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use serde_json::json;
    use std::sync::Arc;

    async fn spawn_state(schema: bool) -> (HostState, u16, tokio::sync::mpsc::UnboundedReceiver<HostEvent>) {
        let dir = tempfile::tempdir().unwrap();
        let settings_file = dir.path().join("settings.json");
        std::mem::forget(dir); // keep tempdir alive for the test process
        let schema = if schema {
            let s = r#"
[app]
name = "X"
identifier = "com.example.x"
[menu]
settings = true
[[settings.fields]]
key = "theme"
label = "Theme"
type = "select"
default = "light"
options = ["light", "dark"]
"#;
            AppConfig::from_str(s).unwrap().settings
        } else {
            None
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let state = HostState {
            app_name: "TestApp".to_string(),
            token: "tok123".to_string(),
            status: Arc::new(tokio::sync::RwLock::new(AppStatus::Starting)),
            schema,
            settings_file,
            events: tx,
        };
        let port = start(state.clone()).await.unwrap();
        (state, port, rx)
    }

    #[tokio::test]
    async fn status_endpoint_reports_states() {
        let (state, port, _rx) = spawn_state(false).await;
        let url = format!("http://127.0.0.1:{port}/api/status");
        let body: serde_json::Value = reqwest::get(&url).await.unwrap().json().await.unwrap();
        assert_eq!(body["state"], "starting");
        *state.status.write().await =
            AppStatus::Ready { target_url: "http://x/".to_string() };
        let body: serde_json::Value = reqwest::get(&url).await.unwrap().json().await.unwrap();
        assert_eq!(body["state"], "ready");
        assert_eq!(body["target_url"], "http://x/");
    }

    #[tokio::test]
    async fn pages_render_with_app_name() {
        let (_state, port, _rx) = spawn_state(true).await;
        for path in ["/loading", "/placeholder", "/settings"] {
            let html = reqwest::get(format!("http://127.0.0.1:{port}{path}"))
                .await.unwrap().text().await.unwrap();
            assert!(html.contains("TestApp"), "{path} missing app name");
            assert!(!html.contains("{{"), "{path} left template markers: {html}");
        }
        let css = reqwest::get(format!("http://127.0.0.1:{port}/style.css"))
            .await.unwrap();
        assert_eq!(css.status(), 200);
    }

    #[tokio::test]
    async fn settings_page_404_without_schema() {
        let (_state, port, _rx) = spawn_state(false).await;
        let r = reqwest::get(format!("http://127.0.0.1:{port}/settings")).await.unwrap();
        assert_eq!(r.status(), 404);
    }

    #[tokio::test]
    async fn settings_post_requires_token_and_validates() {
        let (state, port, _rx) = spawn_state(true).await;
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/api/settings");
        let no_token = client.post(&url).json(&json!({"theme": "dark"})).send().await.unwrap();
        assert_eq!(no_token.status(), 401);
        let bad = client.post(&url).header("x-host-token", "tok123")
            .json(&json!({"theme": "purple"})).send().await.unwrap();
        assert_eq!(bad.status(), 422);
        let ok = client.post(&url).header("x-host-token", "tok123")
            .json(&json!({"theme": "dark"})).send().await.unwrap();
        assert_eq!(ok.status(), 204);
        let saved = std::fs::read_to_string(&state.settings_file).unwrap();
        assert!(saved.contains("dark"));
    }

    #[tokio::test]
    async fn restart_endpoint_sends_event() {
        let (_state, port, mut rx) = spawn_state(false).await;
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/api/restart");
        assert_eq!(client.post(&url).send().await.unwrap().status(), 401);
        assert_eq!(
            client.post(&url).header("x-host-token", "tok123").send().await.unwrap().status(),
            204
        );
        assert_eq!(rx.recv().await, Some(HostEvent::RestartRequested));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test internal_server`
Expected: compile error — module empty.

- [ ] **Step 4: Implement `src/internal_server.rs`**

```rust
use crate::config::{FieldType, SettingsSection};
use crate::settings;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

const LOADING_HTML: &str = include_str!("assets/loading.html");
const PLACEHOLDER_HTML: &str = include_str!("assets/placeholder.html");
const SETTINGS_HTML: &str = include_str!("assets/settings.html");
const STYLE_CSS: &str = include_str!("assets/style.css");

#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    RestartRequested,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum AppStatus {
    Starting,
    Ready { target_url: String },
    Error { message: String, log_path: Option<String> },
}

#[derive(Clone)]
pub struct HostState {
    pub app_name: String,
    pub token: String,
    pub status: Arc<RwLock<AppStatus>>,
    pub schema: Option<SettingsSection>,
    pub settings_file: PathBuf,
    pub events: mpsc::UnboundedSender<HostEvent>,
}

pub fn router(state: HostState) -> Router {
    Router::new()
        .route("/loading", get(loading))
        .route("/placeholder", get(placeholder))
        .route("/settings", get(settings_page))
        .route("/style.css", get(style))
        .route("/api/status", get(api_status))
        .route("/api/settings", post(api_settings))
        .route("/api/restart", post(api_restart))
        .with_state(state)
}

pub async fn start(state: HostState) -> std::io::Result<u16> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let app = router(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(port)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn page(template: &str, replacements: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (marker, value) in replacements {
        out = out.replace(marker, value);
    }
    out
}

async fn loading(State(s): State<HostState>) -> Html<String> {
    Html(page(
        LOADING_HTML,
        &[("{{APP_NAME}}", &html_escape(&s.app_name)), ("{{TOKEN}}", &s.token)],
    ))
}

async fn placeholder(State(s): State<HostState>) -> Html<String> {
    Html(page(PLACEHOLDER_HTML, &[("{{APP_NAME}}", &html_escape(&s.app_name))]))
}

async fn style() -> impl IntoResponse {
    ([("content-type", "text/css")], STYLE_CSS)
}

fn field_html(f: &crate::config::SettingsField, current: &Value) -> String {
    let key = html_escape(&f.key);
    let label = html_escape(&f.label);
    match f.field_type {
        FieldType::Text => {
            let v = html_escape(current.as_str().unwrap_or_default());
            format!(
                "<label>{label}<input type=\"text\" data-key=\"{key}\" value=\"{v}\"></label>"
            )
        }
        FieldType::Boolean => {
            let checked = if current.as_bool().unwrap_or(false) { " checked" } else { "" };
            format!(
                "<label><input type=\"checkbox\" data-key=\"{key}\"{checked}> {label}</label>"
            )
        }
        FieldType::Select => {
            let options: String = f
                .options
                .iter()
                .map(|o| {
                    let sel = if current.as_str() == Some(o) { " selected" } else { "" };
                    let o = html_escape(o);
                    format!("<option{sel}>{o}</option>")
                })
                .collect();
            format!("<label>{label}<select data-key=\"{key}\">{options}</select></label>")
        }
    }
}

async fn settings_page(State(s): State<HostState>) -> impl IntoResponse {
    let Some(schema) = &s.schema else {
        return (StatusCode::NOT_FOUND, "settings are not enabled for this app").into_response();
    };
    let current = settings::load(schema, &s.settings_file);
    let fields: String = schema
        .fields
        .iter()
        .map(|f| field_html(f, current.get(&f.key).unwrap_or(&Value::Null)))
        .collect();
    Html(page(
        SETTINGS_HTML,
        &[
            ("{{APP_NAME}}", &html_escape(&s.app_name)),
            ("{{TOKEN}}", &s.token),
            ("{{FIELDS_HTML}}", &fields),
        ],
    ))
    .into_response()
}

async fn api_status(State(s): State<HostState>) -> Json<AppStatus> {
    Json(s.status.read().await.clone())
}

fn authorized(headers: &HeaderMap, state: &HostState) -> bool {
    headers
        .get("x-host-token")
        .and_then(|v| v.to_str().ok())
        .map(|t| t == state.token)
        .unwrap_or(false)
}

async fn api_settings(
    State(s): State<HostState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !authorized(&headers, &s) {
        return (StatusCode::UNAUTHORIZED, "missing or invalid token".to_string());
    }
    let Some(schema) = &s.schema else {
        return (StatusCode::NOT_FOUND, "settings are not enabled".to_string());
    };
    match settings::validate_incoming(schema, &body) {
        Ok(values) => match settings::save(&s.settings_file, &values) {
            Ok(()) => (StatusCode::NO_CONTENT, String::new()),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        },
        Err(e) => (StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

async fn api_restart(State(s): State<HostState>, headers: HeaderMap) -> StatusCode {
    if !authorized(&headers, &s) {
        return StatusCode::UNAUTHORIZED;
    }
    let _ = s.events.send(HostEvent::RestartRequested);
    StatusCode::NO_CONTENT
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test internal_server`
Expected: all 5 tests PASS.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/internal_server.rs src/assets src/main.rs
git commit -m "feat: internal axum server with loading/placeholder/settings pages and token-gated API"
```

---

### Task 7: Tray menu (`tray.rs`)

**Files:**
- Create: `src/tray.rs`
- Modify: `src/main.rs` (add `mod tray;`)

**Interfaces:**
- Consumes: `config::MenuSection` (Task 2).
- Produces (exact):
  - `#[derive(Debug, Clone, PartialEq)] pub enum TrayAction { Open, OpenUrl(String), Settings, Restart, Quit }`
  - `#[derive(Debug, PartialEq)] pub enum Entry { Action(TrayAction, String /* label */), Separator }`
  - `pub fn menu_model(menu: &MenuSection, app_name: &str, settings_enabled: bool, show_open: bool) -> Vec<Entry>` — order: Open (only when `show_open`), custom items, Settings (only when enabled), Restart App, Separator, Quit.
  - `pub struct Tray { pub tray: tray_icon::TrayIcon, pub actions: std::collections::HashMap<tray_icon::menu::MenuId, TrayAction> }`
  - `pub fn build(model: &[Entry], tooltip: &str) -> anyhow::Result<Tray>`
  - `pub fn rebuild_menu(tray: &mut Tray, model: &[Entry]) -> anyhow::Result<()>` — swaps the menu (used when entering/leaving tray mode).

- [ ] **Step 1: Write the failing tests** (model only — constructing a real tray needs a running GTK/event loop and cannot run headless in CI)

```rust
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
            Entry::Action(TrayAction::OpenUrl("https://example.com".to_string()), "Docs".to_string()),
            Entry::Action(TrayAction::Settings, "Settings…".to_string()),
            Entry::Action(TrayAction::Restart, "Restart App".to_string()),
            Entry::Separator,
            Entry::Action(TrayAction::Quit, "Quit".to_string()),
        ];
        assert_eq!(m, expected);
    }

    #[test]
    fn minimal_model_hides_open_and_settings() {
        let empty = MenuSection { settings: false, items: vec![] };
        let m = menu_model(&empty, "X", false, false);
        let expected = vec![
            Entry::Action(TrayAction::Restart, "Restart App".to_string()),
            Entry::Separator,
            Entry::Action(TrayAction::Quit, "Quit".to_string()),
        ];
        assert_eq!(m, expected);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test tray`
Expected: compile error.

- [ ] **Step 3: Implement `src/tray.rs`**

```rust
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
    out.push(Entry::Action(TrayAction::Restart, "Restart App".to_string()));
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test tray`
Expected: both tests PASS. (On Linux, `cargo clippy` needs GTK dev headers — `sudo apt-get install libgtk-3-dev libayatana-appindicator3-dev libxdo-dev` if not present.)

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/tray.rs src/main.rs
git commit -m "feat: config-driven tray menu model and tray construction"
```

---

### Task 8: Main wiring (`main.rs`) — lifecycle, event loop, dialogs

**Files:**
- Modify: `src/main.rs` (full rewrite; remove all temporary `#[allow(dead_code)]`)

**Interfaces:**
- Consumes everything produced by Tasks 2–7, exactly as declared there.
- Produces: the runnable app. No new public API.

Behavior checklist (from the spec — implement all):
1. Invalid config → error dialog → exit. 2. Single instance via `fd_lock` on `app.lock` → "already running" dialog → exit. 3. Missing Chrome → dialog [Download Chrome]/[Quit]; download opens `chrome::DOWNLOAD_URL` via `open::that`. 4. Internal server on ephemeral port; token from `rand`. 5. Server configured → spawn + health poll task → status transitions Starting→Ready/Error; Chrome opens `/loading`. No server → Chrome opens `app.url`, or `/placeholder` when empty. Target URL after health = `app.url` if non-empty else `health_check_url`. 6. Tray actions: Open (tray mode only), OpenUrl → `open::that`, Settings → `chrome::open_extra_window` at `/settings`, Restart → kill+respawn children, Quit → kill children + exit. 7. Chrome exit → `on_close` Quit: cleanup+exit; Tray: rebuild menu with Open entry. 8. Server unexpected exit → status Error + dialog offering Restart/Quit. 9. Host log written to `logs/host.log`; server log to `logs/server.log`. 10. Intentional kills must not fire "unexpected exit" — use a generation counter.

- [ ] **Step 1: Rewrite `src/main.rs`**

```rust
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
        .set_description(&format!(
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
            let env = supervisor::build_env(
                &server_cfg.env,
                &settings_env,
                &self.paths.settings_file,
            );
            let cwd = supervisor::resolve_cwd(
                server_cfg.cwd.as_deref(),
                &supervisor::base_dir(),
            );
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
                            let Some(server) = guard.server.as_mut() else { break };
                            match server.child.try_wait() {
                                Ok(Some(_)) => {
                                    guard.server = None;
                                    drop(guard);
                                    let _ = proxy
                                        .send_event(UserEvent::ServerExited { generation });
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
            Ok(mut child) => {
                self.log("chrome: launched");
                let proxy = self.proxy.clone();
                let children = self.children.clone();
                rt.spawn(async move {
                    let _ = child.wait().await;
                    let mut guard = children.lock().await;
                    guard.chrome = None;
                    drop(guard);
                    let _ = proxy.send_event(UserEvent::ChromeExited { generation });
                });
                // Note: the waiter owns the child; nothing to store. Chrome kill on
                // shutdown happens via taskkill-free path: we store no handle, so
                // instead keep a second handle? No — we DO need to kill Chrome on
                // Quit. Store the child and poll, mirroring the server watcher.
            }
            Err(e) => {
                dialog(&self.cfg.app.name, &format!("Failed to launch Google Chrome:\n{e}"));
                std::process::exit(1);
            }
        }
    }
}
```

**Correction to the Chrome block above (use this, not the comment-flagged version):** store the child in `children.chrome` and poll it like the server watcher, so Quit/Restart can kill it:

```rust
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
                        let Some(chrome) = guard.chrome.as_mut() else { break };
                        match chrome.try_wait() {
                            Ok(Some(_)) => {
                                guard.chrome = None;
                                drop(guard);
                                let _ =
                                    proxy.send_event(UserEvent::ChromeExited { generation });
                                break;
                            }
                            Ok(None) => {}
                            Err(_) => break,
                        }
                    }
                });
            }
```

Continue `main.rs`:

```rust
impl App {
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
        dialog(&cfg.app.name, &format!("Cannot create data directory:\n{e}"));
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
                .set_description(&format!("{} is already running.", cfg.app.name))
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
    let host_log = supervisor::open_log(
        &paths.logs_dir.join("host.log"),
        supervisor::LOG_MAX_BYTES,
    )
    .expect("host log");

    let token: String = {
        let mut rng = rand::rng();
        format!("{:032x}", rng.random::<u128>())
    };
    let status = Arc::new(RwLock::new(AppStatus::Starting));
    let (host_tx, mut host_rx) = mpsc::unbounded_channel::<HostEvent>();
    let state = HostState {
        app_name: cfg.app.name.clone(),
        token,
        status: status.clone(),
        schema: if cfg.settings_enabled() { cfg.settings.clone() } else { None },
        settings_file: paths.settings_file.clone(),
        events: host_tx,
    };
    let port = rt.block_on(internal_server::start(state)).expect("internal server");

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        // no host-owned windows: keep the host out of the Dock
        let mut event_loop = event_loop; // shadow for set_activation_policy(&mut)
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        run(event_loop, cfg, paths, chrome_exe, port, status, rt, guard, host_log, host_rx);
        return;
    }
    #[cfg(not(target_os = "macos"))]
    run(event_loop, cfg, paths, chrome_exe, port, status, rt, guard, host_log, host_rx);
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
        children: Arc::new(Mutex::new(Children { chrome: None, server: None })),
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
                    .set_description(&format!(
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
```

Add the missing `restart_chrome_only` to `impl App` (relaunches only Chrome, pointing at the current target — used by tray "Open" in tray mode):

```rust
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
                        let Some(chrome) = guard.chrome.as_mut() else { break };
                        match chrome.try_wait() {
                            Ok(Some(_)) => {
                                guard.chrome = None;
                                drop(guard);
                                let _ =
                                    proxy.send_event(UserEvent::ChromeExited { generation });
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
```

Implementation notes for this task (API details to verify against the installed crate versions — adjust mechanically if signatures moved, do not redesign):
- `MenuEvent::set_event_handler` takes `Option<impl Fn(MenuEvent)>`.
- `tao` macOS `set_activation_policy` needs `&mut EventLoop` **before** `run`; the `cfg` block above shadows it mutably for that call.
- `rfd::MessageDialogResult::Custom(String)` is returned for `OkCancelCustom` — if the installed rfd returns `MessageDialogResult::Ok` for the custom-ok button instead, match on that.
- The struct-field orderings in `App { .. }` literals must match your final struct declaration order only where required by borrow timing (`cfg`/`paths` moved last).
- `start_children` uses `self.rt.block_on` on the event-loop thread only for sub-millisecond lock/status writes — never for waits.

- [ ] **Step 2: Verify it compiles clean**

Run: `cargo check && cargo clippy --all-targets -- -D warnings && cargo fmt`
Expected: clean. Iterate on crate-API mismatches here (signatures, not structure).

- [ ] **Step 3: Run the full test suite**

Run: `cargo test`
Expected: all tests from Tasks 2–7 still PASS.

- [ ] **Step 4: Manual smoke test (requires Chrome + a desktop session)**

Run: `cargo run`
Expected: Chrome app-mode window opens showing "Hitch is ready to be configured"; tray icon appears with Restart App / Quit; Quit closes window and exits; a second `cargo run` while one is running shows "already running". Record what you observed in the commit body. If no desktop session is available, state that explicitly in the commit body instead.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: host lifecycle wiring — event loop, children supervision, dialogs"
```

---

### Task 9: Icons, packager config, README

**Files:**
- Create: `src/bin/gen_icons.rs`, `icons/icon.png` (generated), `README.md`
- Modify: `Cargo.toml` (add `image` dependency)

**Interfaces:**
- Consumes: `[package.metadata.packager]` stub from Task 1.
- Produces: `icons/icon.png` (512×512) committed; README documenting the full template contract.

- [ ] **Step 1: Add the `image` dependency**

In `Cargo.toml` `[dependencies]`:

```toml
image = { version = "0.25", default-features = false, features = ["png"] }
```

- [ ] **Step 2: Write `src/bin/gen_icons.rs`**

```rust
//! Regenerates icons/icon.png. Replace that file with real branding per app;
//! rerun this only to restore the placeholder: cargo run --bin gen_icons
use image::{Rgba, RgbaImage};

fn main() {
    const S: u32 = 512;
    let mut img = RgbaImage::new(S, S);
    let c = S as i32 / 2;
    let r = (S as i32 / 2) - 24;
    for (x, y, px) in img.enumerate_pixels_mut() {
        let dx = x as i32 - c;
        let dy = y as i32 - c;
        *px = if dx * dx + dy * dy < r * r {
            Rgba([0x4a, 0x7d, 0xfc, 0xff])
        } else {
            Rgba([0, 0, 0, 0])
        };
    }
    std::fs::create_dir_all("icons").unwrap();
    img.save("icons/icon.png").unwrap();
    println!("wrote icons/icon.png");
}
```

- [ ] **Step 3: Generate the icon**

Run: `cargo run --bin gen_icons`
Expected: `icons/icon.png` exists.

- [ ] **Step 4: Write `README.md`**

Cover exactly these sections (write real prose, not stubs):
1. **What this is** — one paragraph: Rust host + Google Chrome app-mode rendering; what a fresh clone does (placeholder page).
2. **Quick start** — prerequisites (Rust stable ≥1.85; Linux: `sudo apt-get install libgtk-3-dev libayatana-appindicator3-dev libxdo-dev` for building, `libayatana-appindicator3-1` at runtime; Chrome required at runtime); `cargo run`.
3. **Configuration reference** — every `app.toml` key with type, default, and behavior, matching Task 1's file and Task 2's validation exactly (including `on_close` modes, menu items, settings field types, select options rules).
4. **Local server contract** — health-check readiness, `startup_timeout_secs`, cwd resolution (debug = repo root, release = exe dir), env received: `[server].env` + `APP_SETTING_<KEY>` + `APP_SETTINGS_FILE`; logs at `<data-dir>/logs/server.log`.
5. **Settings** — how the schema renders, where `settings.json` lives, restart-to-apply.
6. **Data locations** — the `RuntimePaths` table per OS (`%APPDATA%`, `~/Library/Application Support`, `~/.local/share`).
7. **Packaging** — `cargo packager --release` locally (`cargo install cargo-packager`); formats per OS; icon replacement; signing/notarization marked as per-app TODO with links to Apple notarization and Windows signing docs.
8. **Releasing** — tag `v*` → GitHub Actions builds installers (Task 10 workflow).

- [ ] **Step 5: Verify and commit**

Run: `cargo check && cargo clippy --all-targets -- -D warnings && cargo fmt && cargo test`
Expected: clean, all tests pass.

```bash
git add Cargo.toml Cargo.lock src/bin/gen_icons.rs icons/icon.png README.md
git commit -m "feat: placeholder icon generator, packager metadata, README"
```

---

### Task 10: CI and release workflows

**Files:**
- Create: `.github/workflows/ci.yml`, `.github/workflows/release.yml`

**Interfaces:**
- Consumes: the full crate from Tasks 1–9; `[package.metadata.packager]` from Task 1/9.
- Produces: green CI on push; installer artifacts attached to GitHub releases on `v*` tags.

- [ ] **Step 1: Write `.github/workflows/ci.yml`**

```yaml
name: CI
on:
  push:
    branches: [main]
  pull_request:

jobs:
  test:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - name: Install Linux system deps
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y libgtk-3-dev libayatana-appindicator3-dev libxdo-dev
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo test
```

- [ ] **Step 2: Write `.github/workflows/release.yml`**

```yaml
name: Release
on:
  push:
    tags: ["v*"]

permissions:
  contents: write

jobs:
  package:
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: ubuntu-latest
            formats: deb,appimage
          - os: macos-latest
            formats: dmg
          - os: windows-latest
            formats: nsis
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - name: Install Linux system deps
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y libgtk-3-dev libayatana-appindicator3-dev libxdo-dev
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Install cargo-packager
        run: cargo install cargo-packager --locked
      - name: Package
        run: cargo packager --release --formats ${{ matrix.formats }} --verbose
      - name: Upload to release
        uses: softprops/action-gh-release@v2
        with:
          files: |
            target/release/*.deb
            target/release/*.AppImage
            target/release/*.dmg
            target/release/*-setup.exe
```

Note: cargo-packager writes bundles under `target/release/`; if a format lands in a subdirectory on some version, widen the globs (`target/release/**/*.dmg`) rather than restructuring.

- [ ] **Step 3: Validate workflow syntax locally**

Run: `cargo check` (sanity) and, if available, `actionlint .github/workflows/*.yml`
Expected: no errors (skip actionlint gracefully if not installed).

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml .github/workflows/release.yml
git commit -m "ci: 3-OS test matrix and tag-triggered installer packaging"
```

- [ ] **Step 5: Full-suite final check**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: clean and green. This is the definition of done for the template.

---

## Self-Review Notes (already applied)

- Spec coverage: config/validation (T2), settings + env contract (T3), Chrome detect/launch/profile-dir (T4), supervisor + health poll + log rotation (T5), internal server routes + token auth + pages (T6), tray model/order (T7), lifecycle incl. single-instance, on_close modes, all dialog paths, generation-counter kill semantics (T8), icons/packager/README (T9), CI + release (T10). Placeholder default `app.toml` ships in T1.
- Target-URL rule (server present but `app.url` empty → fall back to `health_check_url`) is spec'd in T8's `target_url()` and must be documented in the README (T9 §3).
- Known verify-at-implementation points are called out inline in Task 8 (rfd result variant, tao macOS activation-policy mutability, MenuEvent handler signature) — adjust mechanically, don't redesign.
