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
    /// Rendered masked, never echoed back to the page, and stored in a file the
    /// host keeps at 0600. For API keys and tokens the user supplies.
    Password,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
// Consumed by `crate::git::GitService` and the host wiring. The section is parsed and
// validated here regardless of whether a given field is read on this run, so that
// turning a git feature on later can never surface a *new* config error at an awkward
// moment.
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
/// A whitelist rather than a blacklist: `[A-Za-z0-9._/-]` already excludes ASCII
/// control characters, whitespace, `@`, `~`, `^`, `:`, `?`, `*`, `[`, `\` and every
/// shell metacharacter, so a future git relaxing its own rules cannot widen ours by
/// accident. It is additionally stricter than libgit2 on a leading `-` (an argv
/// hazard), which libgit2 itself allows.
///
/// The invariant that matters is one-directional and machine-checked by
/// `git::tests::validate_branch_name_never_admits_a_name_libgit2_refuses`: every name
/// this accepts must be a name libgit2 accepts. The reverse is deliberately false.
///
/// The dot rules are **per path component**, not per name. libgit2 refuses a component
/// that begins with `.`, ends with `.`, or ends with `.lock`, so `a/.b`, `x/.` and
/// `a.lock/b` are all invalid refs even though the name as a whole neither begins with
/// a dot nor ends in `.lock`. Checking only the whole string let all three through.
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
    name.split('/')
        .all(|part| !part.starts_with('.') && !part.ends_with('.') && !part.ends_with(".lock"))
}

impl AppConfig {
    pub fn from_str(s: &str) -> Result<Self, String> {
        let cfg: AppConfig = toml::from_str(s).map_err(|e| format!("app.toml parse error: {e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Release builds always use the copy compiled in by `include_str!`, so a
    /// shipped binary is self-contained and cannot be reconfigured by dropping a
    /// file next to it.
    ///
    /// Debug builds read `app.toml` from the source tree instead, because every
    /// change to it otherwise costs a full rebuild — and iterating on a settings
    /// schema or a menu means changing it constantly. `CARGO_MANIFEST_DIR` is
    /// baked in at compile time, so this reads the same file that would have
    /// been embedded, not something in the current directory.
    ///
    /// A debug build with an unreadable or absent `app.toml` falls back to the
    /// embedded copy rather than failing: the embedded one is what a release
    /// would have used, so falling back can only make debug behave *more* like
    /// release.
    pub fn load() -> Result<Self, String> {
        Self::from_str(&Self::source())
    }

    #[cfg(debug_assertions)]
    fn source() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("app.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                if text != EMBEDDED_CONFIG {
                    // Worth saying out loud: the running config is not the one
                    // compiled in, so anything reported about it refers to disk.
                    eprintln!(
                        "config: reloaded {} (differs from the embedded copy)",
                        path.display()
                    );
                }
                text
            }
            Err(e) => {
                eprintln!(
                    "config: {} unreadable ({e}), using the embedded copy",
                    path.display()
                );
                EMBEDDED_CONFIG.to_string()
            }
        }
    }

    #[cfg(not(debug_assertions))]
    fn source() -> String {
        EMBEDDED_CONFIG.to_string()
    }

    pub fn settings_enabled(&self) -> bool {
        self.menu.settings && self.settings.is_some()
    }

    // Read by `host::App::start_children`, to fill `HostAccess.git_enabled`. (Not by
    // `GitService::new`, which destructures `cfg.git` directly, and not by
    // `supervisor::build_env`, which receives the pre-computed bool.)
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
                    FieldType::Password => {
                        let Some(d) = f.default.as_str() else {
                            return Err(format!(
                                "password field {:?} default must be a string",
                                f.key
                            ));
                        };
                        // app.toml is committed. A non-empty default is a secret
                        // in the repository, so refuse it at load time rather
                        // than trusting anyone to notice.
                        if !d.is_empty() {
                            return Err(format!(
                                "password field {:?} must default to \"\" -- app.toml is \
                                 committed to the repository, so a non-empty default \
                                 would publish the secret",
                                f.key
                            ));
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
            if git.author_name.contains(['<', '>', '\n']) {
                return Err(
                    "git.author_name must not contain '<', '>' or newlines (git \
                            signatures cannot represent them)"
                        .into(),
                );
            }
            // `""` is the documented sentinel for "fall back to [app].name". A name
            // that is merely *blank* is not the sentinel, so task 9 hands it straight
            // to `git2::Signature::now`, which refuses it — measured: `" "` yields
            // "Signature cannot have an empty name or email", and an embedded NUL
            // yields "data contained a nul byte". Catching both here turns a one-space
            // typo in app.toml into a startup config error instead of a libgit2 error
            // on every commit, sync and merge for the life of the install.
            if !git.author_name.is_empty()
                && (git.author_name.trim().is_empty()
                    || git.author_name.chars().any(char::is_control))
            {
                return Err(
                    "git.author_name must not be blank or contain control characters (git \
                     signatures cannot represent them)"
                        .into(),
                );
            }
            // Same reasoning, plus: a bare "@" passes a naive `contains('@')` and
            // produces the signature `App <@>` — syntactically valid, attributable to
            // nobody. Require a non-empty local part and domain.
            let email_shaped = match git.author_email.split_once('@') {
                Some((local, domain)) => !local.is_empty() && !domain.is_empty(),
                None => false,
            };
            if !git.author_email.is_empty()
                && (!email_shaped
                    || git.author_email.contains(['<', '>'])
                    || git
                        .author_email
                        .chars()
                        .any(|c| c.is_whitespace() || c.is_control()))
            {
                return Err(format!(
                    "git.author_email {:?} must look like a plain address, e.g. app@example.com",
                    git.author_email
                ));
            }
            if !(5..=3600).contains(&git.network_timeout_secs) {
                return Err(format!(
                    "git.network_timeout_secs must be between 5 and 3600 (got {})",
                    git.network_timeout_secs
                ));
            }
            if git.quit_sync_timeout_secs > 120 {
                return Err(
                    "git.quit_sync_timeout_secs must be 120 or less (0 disables sync_on_quit)"
                        .into(),
                );
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
    // Computed unconditionally — they are only paths. Nothing here creates them:
    // `ensure()` deliberately ignores all three so that a host with no `[git]`
    // section leaves no git files on disk at all. The mkdir for `repos_dir`
    // belongs to `GitService::start`, and that placement is the entire reason
    // the "nothing on disk when git is off" guarantee holds.
    pub repos_dir: PathBuf,
    pub registry_file: PathBuf,
    pub git_state_file: PathBuf,
}

impl RuntimePaths {
    pub fn under(base: &Path, identifier: &str) -> Self {
        let data_dir = base.join(identifier);
        Self {
            chrome_profile: data_dir.join("chrome-profile"),
            logs_dir: data_dir.join("logs"),
            settings_file: data_dir.join("settings.json"),
            lock_file: data_dir.join("app.lock"),
            repos_dir: data_dir.join("repos"),
            registry_file: data_dir.join("repos.json"),
            git_state_file: data_dir.join("git-state.json"),
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
        assert_eq!(p.repos_dir, p.data_dir.join("repos"));
        assert_eq!(p.registry_file, p.data_dir.join("repos.json"));
        assert_eq!(p.git_state_file, p.data_dir.join("git-state.json"));
    }

    #[test]
    fn ensure_does_not_create_git_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let p = RuntimePaths::under(tmp.path(), "com.example.x");
        p.ensure().unwrap();
        assert!(p.data_dir.is_dir());
        assert!(p.chrome_profile.is_dir());
        assert!(p.logs_dir.is_dir());
        assert!(!p.repos_dir.exists());
        assert!(!p.registry_file.exists());
        assert!(!p.git_state_file.exists());
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
            "",
            "-x",
            "/x",
            "x/",
            "a..b",
            "a//b",
            "@",
            "x.lock",
            "a b",
            "a~b",
            "héllo",
            "a\tb",
            // Per-component dot rules. Every one of these is refused by libgit2
            // 1.9.6 (measured); the whole-name `.lock` check alone let the last
            // four through.
            ".",
            ".x",
            "a.",
            "x.lock.",
            "a/.b",
            "x/.",
            "a.lock/b",
            "a/b.lock/c",
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

    #[test]
    fn git_author_messages() {
        let s = format!("{}[git]\nauthor_name = \"A <a@b.c>\"\n", minimal());
        assert_eq!(
            AppConfig::from_str(&s).unwrap_err(),
            "git.author_name must not contain '<', '>' or newlines (git signatures cannot \
             represent them)"
        );
        // Blank and control-bearing names are NOT the "" sentinel: measured against
        // libgit2 1.9.6, each one makes `Signature::now` fail outright. These are
        // TOML escapes (`\u0000`, four hex digits) — not Rust's `\u{0}`.
        for bad in [" ", "  ", r"\t", r"App\u0000"] {
            let s = format!("{}[git]\nauthor_name = \"{bad}\"\n", minimal());
            assert_eq!(
                AppConfig::from_str(&s).unwrap_err(),
                "git.author_name must not be blank or contain control characters (git \
                 signatures cannot represent them)",
                "accepted author_name {bad:?}"
            );
        }
        // A NUL in the address fails `Signature::now` with "data contained a nul
        // byte". Asserted separately because the message quotes the *parsed* value,
        // whose Debug form is `"a\u{0}@e.com"`, not the TOML source spelling.
        let nul_email = "a\u{0}@e.com";
        let s = format!("{}[git]\nauthor_email = \"a\\u0000@e.com\"\n", minimal());
        assert_eq!(
            AppConfig::from_str(&s).unwrap_err(),
            // Built with the same `{:?}` the validator uses. Spelling it out by hand
            // would pin a rustc detail: Debug for NUL renders `\0` today and `\u{0}`
            // in older toolchains.
            format!("git.author_email {nul_email:?} must look like a plain address, e.g. app@example.com")
        );
        // "@" alone yields the signature `App <@>` — valid to libgit2, attributable
        // to nobody; a missing local part or domain is the same defect.
        for bad in ["nobody", "a b@c.d", "<a@b.c>", "@", "@e.com", "a@"] {
            let s = format!("{}[git]\nauthor_email = \"{bad}\"\n", minimal());
            assert_eq!(
                AppConfig::from_str(&s).unwrap_err(),
                format!(
                    "git.author_email \"{bad}\" must look like a plain address, \
                     e.g. app@example.com"
                )
            );
        }
        let ok = format!(
            "{}[git]\nauthor_name = \"App\"\nauthor_email = \"app@example.com\"\n",
            minimal()
        );
        assert!(AppConfig::from_str(&ok).is_ok());
    }

    #[test]
    fn git_timeout_messages() {
        for bad in [0u64, 4, 3601] {
            let s = format!("{}[git]\nnetwork_timeout_secs = {bad}\n", minimal());
            assert_eq!(
                AppConfig::from_str(&s).unwrap_err(),
                format!("git.network_timeout_secs must be between 5 and 3600 (got {bad})")
            );
        }
        let s = format!("{}[git]\nquit_sync_timeout_secs = 121\n", minimal());
        assert_eq!(
            AppConfig::from_str(&s).unwrap_err(),
            "git.quit_sync_timeout_secs must be 120 or less (0 disables sync_on_quit)"
        );
        let ok = format!(
            "{}[git]\nnetwork_timeout_secs = 5\nquit_sync_timeout_secs = 0\n",
            minimal()
        );
        assert!(AppConfig::from_str(&ok).is_ok());
    }

    #[test]
    fn the_two_manifests_agree_on_identity() {
        // The app's display name and identifier are declared twice: app.toml
        // for the running host, Cargo.toml's packager metadata for the
        // installers. Nothing can interpolate between them -- cargo-packager
        // reads Cargo.toml, the host reads app.toml -- so the duplication is
        // structural. This is what stops it drifting: rename them together
        // (`cargo run --bin rename`) or this fails.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let cargo = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let app = AppConfig::from_str(&std::fs::read_to_string(root.join("app.toml")).unwrap())
            .expect("app.toml parses");

        let packager = cargo
            .split("[package.metadata.packager]")
            .nth(1)
            .expect("Cargo.toml has a [package.metadata.packager] table");
        let value_of = |key: &str| -> String {
            packager
                .lines()
                .take_while(|l| !l.starts_with("[package.metadata.packager."))
                .find_map(|l| l.trim().strip_prefix(&format!("{key} = ")))
                .unwrap_or_else(|| panic!("no {key} in [package.metadata.packager]"))
                .trim()
                .trim_matches('"')
                .to_string()
        };

        assert_eq!(
            value_of("product-name"),
            app.app.name,
            "Cargo.toml product-name and app.toml [app].name disagree"
        );
        assert_eq!(
            value_of("identifier"),
            app.app.identifier,
            "Cargo.toml identifier and app.toml [app].identifier disagree -- \
             the installer and the data directory would use different names"
        );
    }

    #[test]
    fn a_debug_build_reads_app_toml_from_disk() {
        // The whole point of the reload: in a debug build the running config
        // comes from the file, so editing it does not need a rebuild. In a
        // release build it must come from the embedded copy, so a shipped binary
        // cannot be reconfigured by dropping a file beside it.
        let on_disk = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("app.toml"),
        )
        .expect("app.toml is readable from the manifest dir");

        let src = AppConfig::source();
        if cfg!(debug_assertions) {
            assert_eq!(src, on_disk, "debug builds must read app.toml from disk");
        } else {
            assert_eq!(
                src, EMBEDDED_CONFIG,
                "release builds must use the embedded copy"
            );
        }
        // Either way it has to parse -- a reload that produced an invalid config
        // would turn a typo into a failure to start.
        AppConfig::from_str(&src).expect("the config source parses");
    }

    #[test]
    fn the_embedded_copy_matches_the_file_on_disk() {
        // include_str! and the disk file are the same bytes, so debug and
        // release builds agree unless someone has edited app.toml since the last
        // compile -- which is exactly the situation the reload exists to serve.
        // Asserting it here means CI, which always builds fresh, catches a
        // committed app.toml that fails to parse or drifts from what ships.
        let on_disk = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("app.toml"),
        )
        .unwrap();
        assert_eq!(on_disk, EMBEDDED_CONFIG);
    }

    #[test]
    fn a_password_field_may_not_ship_a_default() {
        let with_default = |d: &str| {
            format!(
                "[app]\nname = \"X\"\nidentifier = \"com.example.x\"\n\
                 [menu]\nsettings = true\n\
                 [[settings.fields]]\nkey = \"api_key\"\nlabel = \"API key\"\n\
                 type = \"password\"\ndefault = \"{d}\"\n"
            )
        };
        // app.toml is committed to the repository. A non-empty default is a
        // secret published to everyone who can read the repo, so it is refused
        // at load time rather than left for someone to notice in review.
        let err = AppConfig::from_str(&with_default("sk-live-abc")).unwrap_err();
        assert!(err.contains("api_key"), "unexpected error: {err}");
        assert!(err.contains("committed"), "the error must say why: {err}");

        // Empty is the only acceptable default, and it must still be a string.
        assert!(AppConfig::from_str(&with_default("")).is_ok());
    }

    #[test]
    fn the_windows_icon_resource_is_a_valid_multi_size_ico() {
        // build.rs compiles this into the .exe, and Explorer takes a desktop
        // shortcut's icon from there. The taskbar and title bar do not -- those
        // come from the page favicon -- so a broken .ico is invisible everywhere
        // except a Windows desktop, on a platform this machine cannot check.
        let ico = include_bytes!("../icons/icon.ico");
        assert_eq!(&ico[0..2], &[0, 0], "reserved field must be zero");
        assert_eq!(
            u16::from_le_bytes([ico[2], ico[3]]),
            1,
            "type must be 1; 2 would make it a cursor"
        );
        let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
        assert!(
            count >= 4,
            "want several sizes so Explorer picks rather than rescales; got {count}"
        );

        for i in 0..count {
            let e = 6 + 16 * i;
            assert_eq!(ico[e], ico[e + 1], "entry {i} is not square");
            // 0 means 256: the field is one byte, so 256 does not fit in it.
            let dim = if ico[e] == 0 { 256u32 } else { ico[e] as u32 };
            let len = u32::from_le_bytes(ico[e + 8..e + 12].try_into().unwrap()) as usize;
            let off = u32::from_le_bytes(ico[e + 12..e + 16].try_into().unwrap()) as usize;
            assert!(off + len <= ico.len(), "entry {i} points past the file");

            let payload = &ico[off..off + len];
            assert_eq!(
                &payload[..8],
                b"\x89PNG\r\n\x1a\n",
                "entry {i} is not a PNG payload"
            );
            // The directory and the PNG must agree. If they disagree Windows
            // believes the directory, picks this entry for a size it is not, and
            // draws a rescaled mess.
            let w = u32::from_be_bytes(payload[16..20].try_into().unwrap());
            let h = u32::from_be_bytes(payload[20..24].try_into().unwrap());
            assert_eq!(
                (w, h),
                (dim, dim),
                "entry {i}: directory claims {dim}x{dim}, PNG is {w}x{h}"
            );
        }
    }

    #[test]
    fn the_crate_names_an_author_so_the_deb_gets_a_maintainer() {
        // cargo-packager writes the .deb's `Maintainer:` from CARGO_PKG_AUTHORS.
        // Omitting `authors` does NOT omit the field: cargo reports an absent
        // list as an empty one, so the control file ships `Maintainer:` with
        // nothing after it. dpkg warns on install, copies the malformed record
        // into /var/lib/dpkg/status, and then warns on every later dpkg run on
        // that machine -- a defect the user carries around, not a one-off.
        let authors = env!("CARGO_PKG_AUTHORS");
        assert!(
            !authors.trim().is_empty(),
            "Cargo.toml [package].authors must not be empty"
        );
        // Debian's format is `Name <email>`. An author string without one is
        // accepted by cargo and rejected by anything that lints a package.
        assert!(
            authors.contains('<') && authors.contains('@') && authors.contains('>'),
            "authors must be `Name <email>` for the deb Maintainer; got {authors:?}"
        );
    }

    #[test]
    fn shipped_git_block_is_commented_out_but_valid() {
        assert!(!AppConfig::load().unwrap().git_enabled());
        // The shipped `[git]` block is documentation users uncomment. Rename a key
        // without updating the comment and `deny_unknown_fields` bites the user on
        // their first run, not us. Uncomment it here and parse it instead.
        let mut out = String::new();
        let mut in_git = false;
        for line in EMBEDDED_CONFIG.lines() {
            let body = line.strip_prefix("# ").unwrap_or("").trim();
            if body == "[git]" {
                in_git = true;
            } else if in_git && !line.starts_with('#') {
                in_git = false;
            }
            if in_git && !body.is_empty() && !body.starts_with('#') {
                out.push_str(body);
                out.push('\n');
            }
        }
        assert!(
            out.starts_with("[git]\n"),
            "app.toml has no commented-out [git] block"
        );
        // `[git]` plus one line per GitSection field. Every shipped value is its own
        // serde default, so a block that *lost* keys would deserialize into a
        // GitSection indistinguishable from a complete one and every assertion below
        // would still pass. The count is what catches that direction.
        assert_eq!(out.lines().count(), 12, "expected [git] + 11 keys:\n{out}");
        let s = format!("{}{out}", minimal());
        let c = AppConfig::from_str(&s).unwrap_or_else(|e| panic!("{e}\n--- built from ---\n{s}"));
        // Exhaustive destructuring is the other direction, and it is a *compile*
        // error: add a field to GitSection and this stops building (E0027) until
        // someone names it here and documents it in app.toml. A hardcoded count
        // alone cannot do that — 11 documented keys still equal 11 when the struct
        // grows a twelfth field.
        let GitSection {
            tray_sync,
            error_dialogs,
            status_api,
            registry_writes,
            default_branch,
            author_name,
            author_email,
            network_timeout_secs,
            quit_sync_timeout_secs,
            allow_http,
            ssh_host_key_policy,
        } = c.git.unwrap();
        // Every shipped value must equal the real default. `allow_http` matters most:
        // its comment is the operator's only statement of the secure default.
        assert!(!tray_sync);
        assert!(!error_dialogs);
        assert!(!status_api);
        assert!(registry_writes);
        assert_eq!(default_branch, "main");
        assert_eq!(author_name, "");
        assert_eq!(author_email, "");
        assert_eq!(network_timeout_secs, 120);
        assert_eq!(quit_sync_timeout_secs, 10);
        assert!(!allow_http);
        assert_eq!(ssh_host_key_policy, SshHostKeyPolicy::Tofu);
    }
}
