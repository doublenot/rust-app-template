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
    std::fs::write(
        path,
        serde_json::to_string_pretty(&Value::Object(values.clone()))?,
    )
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
        let v = validate_incoming(&schema(), &json!({ "theme": "dark", "notify": false })).unwrap();
        let env = env_vars(&v);
        assert!(env.contains(&("APP_SETTING_THEME".into(), "dark".into())));
        assert!(env.contains(&("APP_SETTING_NOTIFY".into(), "false".into())));
    }
}
