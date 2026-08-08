use crate::config::{FieldType, SettingsField, SettingsSection};
use serde_json::{Map, Value};
use std::path::Path;

fn default_json(f: &SettingsField) -> Value {
    match f.field_type {
        FieldType::Boolean => Value::Bool(f.default.as_bool().unwrap_or(false)),
        FieldType::Text | FieldType::Select | FieldType::Password => {
            Value::String(f.default.as_str().unwrap_or_default().to_string())
        }
    }
}

fn value_fits(f: &SettingsField, v: &Value) -> bool {
    match f.field_type {
        FieldType::Boolean => v.is_boolean(),
        FieldType::Text | FieldType::Password => v.is_string(),
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

/// `current` is what is on disk, and it exists for password fields only.
///
/// The settings page is never sent a stored password (that would put the secret
/// in the page source and make "masked" cosmetic), so an untouched password
/// input posts an empty string. Treating that as "clear it" would wipe the
/// secret on every unrelated save, so it means "leave it alone" instead. The
/// consequence, which is the right trade: a password cannot be cleared from the
/// settings page, only replaced. Deleting one means editing `settings.json`.
pub fn validate_incoming(
    schema: &SettingsSection,
    incoming: &Value,
    current: &Map<String, Value>,
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
        let keep_existing = || {
            current
                .get(&f.key)
                .filter(|v| value_fits(f, v))
                .cloned()
                .unwrap_or_else(|| default_json(f))
        };
        match obj.get(&f.key) {
            None => {
                // An omitted password keeps its value for the same reason an
                // empty one does; any other omitted field resets to its default.
                if f.field_type == FieldType::Password {
                    values.insert(f.key.clone(), keep_existing());
                } else {
                    values.insert(f.key.clone(), default_json(f));
                }
            }
            Some(v)
                if f.field_type == FieldType::Password && v.as_str().is_some_and(str::is_empty) =>
            {
                values.insert(f.key.clone(), keep_existing());
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
    std::fs::write(
        path,
        serde_json::to_string_pretty(&Value::Object(values.clone()))?,
    )?;
    restrict(path);
    Ok(())
}

/// 0600 on the settings file, matching what the git registry already does for
/// `repos.json` (`registry.rs`). This file can hold a `password` field's value,
/// and the default umask would otherwise leave it world-readable — every other
/// user on the machine could read an API key out of it.
///
/// Best-effort on purpose: a settings file that saved but could not be tightened
/// is better than a save that fails. The permission is re-applied on every write
/// because `fs::write` truncates an existing file rather than recreating it, so
/// a file someone loosened stays loose otherwise.
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// Windows has no mode bits. Files under `%APPDATA%` are already scoped to the
/// user's profile, which is the closest equivalent, and doing better would mean
/// ACL work well beyond what this template takes on.
#[cfg(not(unix))]
fn restrict(_path: &Path) {}

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

    fn schema_with_password() -> crate::config::SettingsSection {
        let s = r#"
[app]
name = "X"
identifier = "com.example.x"
[menu]
settings = true
[[settings.fields]]
key = "nickname"
label = "Nickname"
type = "text"
default = ""
[[settings.fields]]
key = "api_key"
label = "API key"
type = "password"
default = ""
"#;
        AppConfig::from_str(s).unwrap().settings.unwrap()
    }

    #[test]
    fn an_empty_password_keeps_the_stored_secret() {
        // The settings page never receives the stored value, so an untouched
        // password input posts "". If that cleared the field, saving any other
        // setting would silently destroy the user's API key.
        let sc = schema_with_password();
        let mut current = Map::new();
        current.insert("api_key".into(), json!("sk-secret"));

        let v =
            validate_incoming(&sc, &json!({ "nickname": "dn", "api_key": "" }), &current).unwrap();
        assert_eq!(v["api_key"], json!("sk-secret"), "the secret was cleared");
        assert_eq!(v["nickname"], json!("dn"));

        // Omitted entirely behaves the same way, for the same reason.
        let v = validate_incoming(&sc, &json!({ "nickname": "dn" }), &current).unwrap();
        assert_eq!(v["api_key"], json!("sk-secret"));
    }

    #[test]
    fn a_non_empty_password_replaces_the_stored_secret() {
        let sc = schema_with_password();
        let mut current = Map::new();
        current.insert("api_key".into(), json!("old"));
        let v = validate_incoming(&sc, &json!({ "api_key": "new" }), &current).unwrap();
        assert_eq!(v["api_key"], json!("new"));
    }

    #[test]
    fn a_text_field_is_still_cleared_by_an_empty_value() {
        // Keep-on-empty is a password-only rule. If it leaked into text fields
        // nobody could ever blank one out.
        let sc = schema_with_password();
        let mut current = Map::new();
        current.insert("nickname".into(), json!("previous"));
        let v = validate_incoming(&sc, &json!({ "nickname": "" }), &current).unwrap();
        assert_eq!(v["nickname"], json!(""));
    }

    #[cfg(unix)]
    #[test]
    fn the_settings_file_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut values = Map::new();
        values.insert("api_key".into(), json!("sk-secret"));
        save(&path, &values).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "settings.json is {mode:o}, want 600");

        // Re-applied on rewrite: fs::write truncates rather than recreating, so
        // a file someone loosened would otherwise stay loose forever.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        save(&path, &values).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "loosened file was not re-tightened: {mode:o}");
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
        let validated = validate_incoming(&schema(), &incoming, &Map::new()).unwrap();
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
        assert!(validate_incoming(&schema(), &json!({ "unknown": 1 }), &Map::new()).is_err());
        assert!(validate_incoming(&schema(), &json!({ "theme": "purple" }), &Map::new()).is_err());
        assert!(validate_incoming(&schema(), &json!({ "notify": "yes" }), &Map::new()).is_err());
        assert!(validate_incoming(&schema(), &json!([1, 2]), &Map::new()).is_err());
        // missing keys fall back to defaults
        let v = validate_incoming(&schema(), &json!({ "theme": "dark" }), &Map::new()).unwrap();
        assert_eq!(v["notify"], json!(true));
    }

    #[test]
    fn env_vars_contract() {
        let v = validate_incoming(
            &schema(),
            &json!({ "theme": "dark", "notify": false }),
            &Map::new(),
        )
        .unwrap();
        let env = env_vars(&v);
        assert!(env.contains(&("APP_SETTING_THEME".into(), "dark".into())));
        assert!(env.contains(&("APP_SETTING_NOTIFY".into(), "false".into())));
    }
}
