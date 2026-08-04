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
    /// Git wants the children restarted (a pull moved HEAD, or settings changed).
    ///
    /// This MUST travel the channel rather than calling `App::restart()` directly:
    /// `restart()` calls `kill_children()`, which calls `rt.block_on`, and `block_on`
    /// panics when called from inside a runtime worker thread. The hop is load-bearing.
    GitRestartChildren {
        repo_id: String,
        reason: &'static str,
    },
    GitFailed {
        repo_id: String,
        op: String,
        code: String,
        message: String,
    },
    /// A sync succeeded but resolved conflicts in favour of the local copy — it
    /// overwrote someone else's edit. Carries the merge commit so the consumer can
    /// de-duplicate: one data-loss event, at most one dialog, ever.
    GitConflictsResolved {
        repo_id: String,
        merge_commit: String,
        paths: Vec<String>,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum AppStatus {
    Starting,
    Ready {
        target_url: String,
    },
    Error {
        message: String,
        log_path: Option<String>,
    },
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
        &[
            ("{{APP_NAME}}", &html_escape(&s.app_name)),
            ("{{TOKEN}}", &s.token),
        ],
    ))
}

async fn placeholder(State(s): State<HostState>) -> Html<String> {
    Html(page(
        PLACEHOLDER_HTML,
        &[("{{APP_NAME}}", &html_escape(&s.app_name))],
    ))
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
            format!("<label>{label}<input type=\"text\" data-key=\"{key}\" value=\"{v}\"></label>")
        }
        FieldType::Boolean => {
            let checked = if current.as_bool().unwrap_or(false) {
                " checked"
            } else {
                ""
            };
            format!("<label><input type=\"checkbox\" data-key=\"{key}\"{checked}> {label}</label>")
        }
        FieldType::Select => {
            let options: String = f
                .options
                .iter()
                .map(|o| {
                    let sel = if current.as_str() == Some(o) {
                        " selected"
                    } else {
                        ""
                    };
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
        return (
            StatusCode::NOT_FOUND,
            "settings are not enabled for this app",
        )
            .into_response();
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
        return (
            StatusCode::UNAUTHORIZED,
            "missing or invalid token".to_string(),
        );
    }
    let Some(schema) = &s.schema else {
        return (
            StatusCode::NOT_FOUND,
            "settings are not enabled".to_string(),
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use serde_json::json;
    use std::sync::Arc;

    async fn spawn_state(
        schema: bool,
    ) -> (
        HostState,
        u16,
        tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
    ) {
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
        *state.status.write().await = AppStatus::Ready {
            target_url: "http://x/".to_string(),
        };
        let body: serde_json::Value = reqwest::get(&url).await.unwrap().json().await.unwrap();
        assert_eq!(body["state"], "ready");
        assert_eq!(body["target_url"], "http://x/");
    }

    #[tokio::test]
    async fn pages_render_with_app_name() {
        let (_state, port, _rx) = spawn_state(true).await;
        for path in ["/loading", "/placeholder", "/settings"] {
            let html = reqwest::get(format!("http://127.0.0.1:{port}{path}"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            assert!(html.contains("TestApp"), "{path} missing app name");
            assert!(!html.contains("{{"), "{path} left template markers: {html}");
        }
        let css = reqwest::get(format!("http://127.0.0.1:{port}/style.css"))
            .await
            .unwrap();
        assert_eq!(css.status(), 200);
    }

    #[tokio::test]
    async fn settings_page_404_without_schema() {
        let (_state, port, _rx) = spawn_state(false).await;
        let r = reqwest::get(format!("http://127.0.0.1:{port}/settings"))
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
    }

    #[tokio::test]
    async fn settings_post_requires_token_and_validates() {
        let (state, port, _rx) = spawn_state(true).await;
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/api/settings");
        let no_token = client
            .post(&url)
            .json(&json!({"theme": "dark"}))
            .send()
            .await
            .unwrap();
        assert_eq!(no_token.status(), 401);
        let bad = client
            .post(&url)
            .header("x-host-token", "tok123")
            .json(&json!({"theme": "purple"}))
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status(), 422);
        let ok = client
            .post(&url)
            .header("x-host-token", "tok123")
            .json(&json!({"theme": "dark"}))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), 204);
        let saved = std::fs::read_to_string(&state.settings_file).unwrap();
        assert!(saved.contains("dark"));
    }

    #[tokio::test]
    async fn settings_post_404_with_valid_token_but_no_schema() {
        // A valid token gets past the auth gate; the missing schema is what
        // decides the response.
        let (_state, port, _rx) = spawn_state(false).await;
        let r = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/api/settings"))
            .header("x-host-token", "tok123")
            .json(&json!({"theme": "dark"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
    }

    #[tokio::test]
    async fn restart_endpoint_sends_event() {
        let (_state, port, mut rx) = spawn_state(false).await;
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/api/restart");
        assert_eq!(client.post(&url).send().await.unwrap().status(), 401);
        assert_eq!(
            client
                .post(&url)
                .header("x-host-token", "tok123")
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        assert_eq!(rx.recv().await, Some(HostEvent::RestartRequested));
    }
}
