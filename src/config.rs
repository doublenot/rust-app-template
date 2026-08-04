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
    #[serde(default)]
    pub git: Option<GitSection>,
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
        Self {
            width: default_width(),
            height: default_height(),
            on_close: OnClose::Quit,
        }
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
    // Not read at runtime (tray_icon assigns its own MenuId); kept for config
    // schema completeness and future menu-editing tooling.
    #[allow(dead_code)]
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
// Consumed by the git service and the host wiring, neither of which exists yet.
// The section is parsed and validated here regardless of whether anything reads
// it, so that turning a git feature on later can never surface a *new* config
// error at an awkward moment.
#[allow(dead_code)]
pub struct GitSection {
    #[serde(default)]
    pub tray_sync: bool,
    #[serde(default)]
    pub error_dialogs: bool,
    #[serde(default)]
    pub status_api: bool,
    #[serde(default = "default_registry_writes")]
    pub registry_writes: bool,
    #[serde(default = "default_branch_name")]
    pub default_branch: String,
    #[serde(default)]
    pub author_name: String,
    #[serde(default)]
    pub author_email: String,
    #[serde(default = "default_network_timeout_secs")]
    pub network_timeout_secs: u64,
    #[serde(default = "default_quit_sync_timeout_secs")]
    pub quit_sync_timeout_secs: u64,
    #[serde(default)]
    pub allow_http: bool,
    #[serde(default)]
    pub ssh_host_key_policy: SshHostKeyPolicy,
}

fn default_registry_writes() -> bool {
    true
}

fn default_branch_name() -> String {
    "main".to_string()
}

fn default_network_timeout_secs() -> u64 {
    120
}

fn default_quit_sync_timeout_secs() -> u64 {
    10
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SshHostKeyPolicy {
    #[default]
    Tofu,
    Accept,
}

pub const MAX_BRANCH_NAME_LEN: usize = 200;

/// Single source of truth for every branch name this host will accept:
/// `[git].default_branch`, `repos[].branch` in `repos.json`, and the body of
/// `POST /api/git/repos/<id>/branch`.
///
/// Deliberately stricter than git's own `check_ref_format`, and a whitelist rather
/// than a blacklist: `[A-Za-z0-9._/-]` already excludes ASCII control characters,
/// whitespace, `@`, `~`, `^`, `:`, `?`, `*`, `[`, `\` and every shell metacharacter,
/// so a future git relaxing its own rules cannot widen ours by accident.
pub fn validate_branch_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_BRANCH_NAME_LEN {
        return false;
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
    {
        return false;
    }
    if name.contains("..") || name.contains("//") {
        return false;
    }
    if name.starts_with('-') || name.starts_with('/') || name.ends_with('/') {
        return false;
    }
    !name.ends_with(".lock")
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

    // Called by the git service constructor and by `supervisor::build_env`, neither
    // of which exists yet.
    #[allow(dead_code)]
    pub fn git_enabled(&self) -> bool {
        self.git.is_some()
    }

    fn validate(&self) -> Result<(), String> {
        if self.app.name.trim().is_empty() {
            return Err("app.name must not be empty".into());
        }
        let id = &self.app.identifier;
        let parts: Vec<&str> = id.split('.').collect();
        if parts.len() < 2
            || parts
                .iter()
                .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
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
                        "menu.settings = true requires at least one [[settings.fields]]".into(),
                    )
                }
            };
            let mut seen = std::collections::BTreeSet::new();
            for f in fields {
                if f.key.is_empty() || !f.key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    return Err(format!("settings key {:?} must match [A-Za-z0-9_]+", f.key));
                }
                // Compared case-insensitively: `settings::env_vars` projects
                // every key to `APP_SETTING_<KEY>` by uppercasing it, so
                // "theme" and "Theme" would collide into one env var whose
                // value depends on map iteration order.
                if !seen.insert(f.key.to_uppercase()) {
                    return Err(format!(
                        "settings key {:?} collides with another key (settings keys must be \
                         unique ignoring case, because each is exported as APP_SETTING_{})",
                        f.key,
                        f.key.to_uppercase()
                    ));
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
        if let Some(git) = &self.git {
            if !validate_branch_name(&git.default_branch) {
                return Err(format!(
                    "git.default_branch {:?} is not a valid branch name",
                    git.default_branch
                ));
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
    fn settings_keys_collide_case_insensitively() {
        // "theme" and "Theme" both project to APP_SETTING_THEME, so they must
        // be rejected even though they differ as raw strings.
        let base = format!("{}[menu]\nsettings = true\n", minimal());
        let s = format!(
            "{base}[[settings.fields]]\nkey = \"theme\"\nlabel = \"A\"\ntype = \"text\"\ndefault = \"\"\n[[settings.fields]]\nkey = \"Theme\"\nlabel = \"B\"\ntype = \"text\"\ndefault = \"\"\n"
        );
        let err = AppConfig::from_str(&s).unwrap_err();
        assert!(err.contains("APP_SETTING_THEME"), "unexpected error: {err}");
        // differing keys that do not collide are still accepted
        let ok = format!(
            "{base}[[settings.fields]]\nkey = \"theme\"\nlabel = \"A\"\ntype = \"text\"\ndefault = \"\"\n[[settings.fields]]\nkey = \"Theme_2\"\nlabel = \"B\"\ntype = \"text\"\ndefault = \"\"\n"
        );
        assert!(AppConfig::from_str(&ok).is_ok());
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

    #[test]
    fn git_section_defaults() {
        let c = AppConfig::from_str(&format!("{}[git]\n", minimal())).unwrap();
        let g = c.git.as_ref().unwrap();
        assert!(!g.tray_sync);
        assert!(!g.error_dialogs);
        assert!(!g.status_api);
        assert!(g.registry_writes);
        assert_eq!(g.default_branch, "main");
        assert_eq!(g.author_name, "");
        assert_eq!(g.author_email, "");
        assert_eq!(g.network_timeout_secs, 120);
        assert_eq!(g.quit_sync_timeout_secs, 10);
        assert!(!g.allow_http);
        assert_eq!(g.ssh_host_key_policy, SshHostKeyPolicy::Tofu);
        assert!(c.git_enabled());
    }

    #[test]
    fn absent_git_section_is_none() {
        let c = AppConfig::from_str(minimal()).unwrap();
        assert!(c.git.is_none());
        assert!(!c.git_enabled());
    }

    #[test]
    fn git_section_rejects_unknown_key() {
        let s = format!("{}[git]\ntray_synk = true\n", minimal());
        let err = AppConfig::from_str(&s).unwrap_err();
        assert!(err.contains("tray_synk"), "unexpected error: {err}");
    }

    #[test]
    fn ssh_host_key_policy_parses_and_rejects_unknown() {
        let s = format!("{}[git]\nssh_host_key_policy = \"accept\"\n", minimal());
        let c = AppConfig::from_str(&s).unwrap();
        assert_eq!(c.git.unwrap().ssh_host_key_policy, SshHostKeyPolicy::Accept);
        let s = format!("{}[git]\nssh_host_key_policy = \"strict\"\n", minimal());
        assert!(AppConfig::from_str(&s).is_err());
    }

    #[test]
    fn branch_names() {
        for good in [
            "main",
            "a",
            "feature/x",
            "v1.2.3",
            "a-b_c",
            "release/2026.08",
        ] {
            assert!(validate_branch_name(good), "rejected {good:?}");
        }
        for bad in [
            "", "-x", "/x", "x/", "a..b", "a//b", "@", "x.lock", "a b", "a~b", "héllo", "a\tb",
        ] {
            assert!(!validate_branch_name(bad), "accepted {bad:?}");
        }
        assert!(validate_branch_name(&"a".repeat(MAX_BRANCH_NAME_LEN)));
        assert!(!validate_branch_name(&"a".repeat(MAX_BRANCH_NAME_LEN + 1)));
    }

    #[test]
    fn git_default_branch_message() {
        let s = format!("{}[git]\ndefault_branch = \"bad branch\"\n", minimal());
        assert_eq!(
            AppConfig::from_str(&s).unwrap_err(),
            "git.default_branch \"bad branch\" is not a valid branch name"
        );
    }
}
