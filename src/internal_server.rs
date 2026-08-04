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
    pub git: Option<Arc<crate::git::GitService>>,
}

pub fn router(state: HostState) -> Router {
    let router = Router::new()
        .route("/loading", get(loading))
        .route("/placeholder", get(placeholder))
        .route("/settings", get(settings_page))
        .route("/style.css", get(style))
        .route("/api/status", get(api_status))
        .route("/api/settings", post(api_settings))
        .route("/api/restart", post(api_restart));
    // Nested before `with_state` so the sub-router's handlers resolve against the same
    // state. Which of the two is nested is the on/off switch for the whole feature.
    let router = match &state.git {
        Some(_) => router.nest("/api/git", crate::git::api::router(state.clone())),
        None => router.nest("/api/git", crate::git::api::disabled_router()),
    };
    router.with_state(state)
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

pub(crate) fn authorized(headers: &HeaderMap, state: &HostState) -> bool {
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
            git: None,
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

    const GIT_ROUTES: [(&str, &str); 17] = [
        ("GET", "/api/git"),
        ("GET", "/api/git/repos"),
        ("PUT", "/api/git/repos/notes"),
        ("GET", "/api/git/repos/notes"),
        ("DELETE", "/api/git/repos/notes"),
        ("GET", "/api/git/repos/notes/status"),
        ("GET", "/api/git/repos/notes/branches"),
        ("POST", "/api/git/repos/notes/init"),
        ("POST", "/api/git/repos/notes/clone"),
        ("POST", "/api/git/repos/notes/pull"),
        ("POST", "/api/git/repos/notes/push"),
        ("POST", "/api/git/repos/notes/sync"),
        ("POST", "/api/git/repos/notes/commit"),
        ("POST", "/api/git/repos/notes/branch"),
        ("POST", "/api/git/repos/notes/reset"),
        ("GET", "/api/git/jobs"),
        ("GET", "/api/git/jobs/job_deadbeef_000001"),
    ];

    #[tokio::test]
    async fn every_git_route_is_git_disabled_without_the_section() {
        let (_state, port, _rx) = spawn_state(false).await;
        let client = reqwest::Client::new();
        for (method, path) in GIT_ROUTES {
            let r = client
                .request(
                    method.parse().unwrap(),
                    format!("http://127.0.0.1:{port}{path}"),
                )
                .header("x-host-token", "tok123")
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 404, "{method} {path}");
            let body: serde_json::Value = r.json().await.unwrap();
            // A bare 404 with an empty body is indistinguishable from a typo in the
            // path; the child has to be able to tell that the *host* has no git.
            assert_eq!(body["error"]["code"], "git_disabled", "{method} {path}");
        }
        // The rest of the server is untouched.
        let r = reqwest::get(format!("http://127.0.0.1:{port}/api/status"))
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }

    async fn spawn_git_state_cfg(
        git_toml: &str,
        ops: Arc<dyn crate::git::GitOps>,
    ) -> (HostState, u16, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::config::RuntimePaths::under(dir.path(), "com.example.git");
        paths.ensure().unwrap();
        let cfg = AppConfig::from_str(&format!(
            "[app]\nname = \"TestApp\"\nidentifier = \"com.example.git\"\n{git_toml}"
        ))
        .unwrap();
        // The receiver is dropped on purpose: every `events.send` in the git subsystem
        // ignores a closed channel, and these tests assert over HTTP, not over events.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let status = Arc::new(tokio::sync::RwLock::new(AppStatus::Ready {
            target_url: "http://x/".to_string(),
        }));
        let git = cfg.git.as_ref().map(|_| {
            let svc = crate::git::GitService::with_ops(
                &cfg,
                &paths,
                tokio::runtime::Handle::current(),
                tx.clone(),
                status.clone(),
                ops,
            );
            svc.start();
            svc
        });
        let state = HostState {
            app_name: "TestApp".to_string(),
            token: "tok123".to_string(),
            status,
            schema: None,
            settings_file: paths.settings_file.clone(),
            events: tx,
            git,
        };
        let port = start(state.clone()).await.unwrap();
        (state, port, dir)
    }

    /// Mirrors `spawn_state`, with a git service wired in.
    async fn spawn_git_state(
        enabled: bool,
        ops: Arc<dyn crate::git::GitOps>,
    ) -> (HostState, u16, tempfile::TempDir) {
        spawn_git_state_cfg(if enabled { "[git]\n" } else { "" }, ops).await
    }

    /// A `GitOps` that answers instantly, so the HTTP tests never wait on libgit2.
    struct StubOps;

    impl crate::git::GitOps for StubOps {
        fn run(
            &self,
            _op: crate::git::jobs::JobOp,
            _ctx: &crate::git::ops::OpCtx,
        ) -> Result<crate::git::ops::OpOutcome, crate::git::error::GitError> {
            Ok(crate::git::ops::OpOutcome::new("up_to_date", "main"))
        }
    }

    fn git(port: u16, path: &str) -> String {
        format!("http://127.0.0.1:{port}/api/git{path}")
    }

    #[tokio::test]
    async fn service_info_and_the_repo_list_answer_only_with_the_token() {
        let (_state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        let client = reqwest::Client::new();

        for path in ["", "/repos"] {
            let bare = client.get(git(port, path)).send().await.unwrap();
            assert_eq!(bare.status(), 401, "{path} answered without a token");
            let body: serde_json::Value = bare.json().await.unwrap();
            assert_eq!(body["error"]["code"], "unauthorized");
        }

        let info: serde_json::Value = client
            .get(git(port, ""))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(info["host_instance"].as_str().unwrap().len(), 8);
        assert_eq!(info["job_retention"], 50);
        assert_eq!(info["repo_count"], 0);
        assert_eq!(info["registry_writable"], true);

        let repos: serde_json::Value = client
            .get(git(port, "/repos"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(repos["repos"].as_array().unwrap().len(), 0);
    }

    fn authed() -> reqwest::Client {
        reqwest::Client::new()
    }

    async fn put_notes(port: u16, body: serde_json::Value) -> reqwest::Response {
        authed()
            .put(git(port, "/repos/notes"))
            .header("x-host-token", "tok123")
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn put_creates_then_replaces_and_never_echoes_a_secret() {
        let (_state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        let body = json!({
            "remote": "https://github.com/acme/notes.git",
            "branch": "main",
            "credential": {"kind": "token", "username": "x-access-token",
                           "token": "github_pat_SUPERSECRET"},
            "auto_sync_secs": 300
        });

        let created = put_notes(port, body.clone()).await;
        assert_eq!(created.status(), 201);
        let text = created.text().await.unwrap();
        // Blunt canary: no path from a stored token to any response body may exist.
        assert!(!text.contains("github_pat"), "leaked a token: {text}");
        let first: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(first["repo"]["id"], "notes");
        assert_eq!(first["repo"]["branch"], "main");
        assert_eq!(first["repo"]["credential"]["kind"], "token");
        assert_eq!(first["repo"]["credential"]["token_stored"], true);
        assert_eq!(first["repo"]["credential"]["bound_host"], "github.com");
        assert_eq!(first["repo"]["has_credentials"], true);
        assert_eq!(first["warnings"].as_array().unwrap().len(), 0);

        let replaced = put_notes(port, body).await;
        assert_eq!(replaced.status(), 200, "PUT is create-or-replace");
        let second: serde_json::Value = replaced.json().await.unwrap();
        assert!(
            second["repo"]["updated_at_ms"].as_u64() >= first["repo"]["updated_at_ms"].as_u64()
        );

        let err = authed()
            .put(git(port, "/repos/../evil"))
            .header("x-host-token", "tok123")
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        // axum normalises `..` out of the path, so this can only ever be a 404 or a 422
        // — never a write outside `repos/`.
        assert!(
            err.status() == 404 || err.status() == 422,
            "{}",
            err.status()
        );

        for bad in ["A", &"n".repeat(200)] {
            let r = authed()
                .put(git(port, &format!("/repos/{bad}")))
                .header("x-host-token", "tok123")
                .json(&json!({}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 422, "accepted id {bad:?}");
            let body: serde_json::Value = r.json().await.unwrap();
            assert_eq!(body["error"]["code"], "invalid_repo_id");
        }
    }

    #[tokio::test]
    async fn get_and_delete_a_repo() {
        let (state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        put_notes(port, json!({})).await;

        for path in ["/repos/notes", "/repos/notes/status"] {
            let body: serde_json::Value = authed()
                .get(git(port, path))
                .header("x-host-token", "tok123")
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            // Both shapes are `{repo, status}` — §5.5's worked example, not §5.1's cell.
            assert_eq!(body["repo"]["id"], "notes", "{path}");
            assert_eq!(body["status"]["state"], "absent", "{path}");
        }

        let unknown = authed()
            .get(git(port, "/repos/ghost"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(unknown.status(), 404);
        let body: serde_json::Value = unknown.json().await.unwrap();
        assert_eq!(body["error"]["code"], "repo_not_found");
        assert!(body["error"]["host_instance"].is_string());

        let tree = state.git.as_ref().unwrap().tree_path("notes");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("inbox.md"), b"hi").unwrap();
        let deleted: serde_json::Value = authed()
            .delete(git(port, "/repos/notes?purge=true"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(deleted["deleted"], true);
        assert_eq!(deleted["purged"], true);
        assert!(!tree.exists());
    }

    #[tokio::test]
    async fn a_read_only_registry_still_serves_reads() {
        let (_state, port, _dir) =
            spawn_git_state_cfg("[git]\nregistry_writes = false\n", Arc::new(StubOps)).await;
        let r = put_notes(port, json!({})).await;
        assert_eq!(r.status(), 403);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["code"], "registry_read_only");

        let r = authed()
            .delete(git(port, "/repos/notes"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);

        // Reads are unaffected: `registry_writes = false` means the author owns
        // repos.json, not that the API is off.
        let r = authed()
            .get(git(port, "/repos"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let r = authed()
            .get(git(port, ""))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let info: serde_json::Value = r.json().await.unwrap();
        assert_eq!(info["registry_writable"], false);
    }

    async fn post(port: u16, path: &str, body: serde_json::Value) -> reqwest::Response {
        authed()
            .post(git(port, path))
            .header("x-host-token", "tok123")
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    /// Blocks the first job it is given until the test drops the sender. That is what
    /// makes the 409 deterministic: `admit` marks the repo busy synchronously, so the
    /// second request conflicts whether or not the blocking thread has started.
    struct GatedOps {
        gate: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl crate::git::GitOps for GatedOps {
        fn run(
            &self,
            _op: crate::git::jobs::JobOp,
            _ctx: &crate::git::ops::OpCtx,
        ) -> Result<crate::git::ops::OpOutcome, crate::git::error::GitError> {
            let gate = self.gate.lock().unwrap().take();
            if let Some(rx) = gate {
                let _ = rx.recv();
            }
            Ok(crate::git::ops::OpOutcome::new("up_to_date", "main"))
        }
    }

    #[tokio::test]
    async fn sync_returns_202_with_a_location_and_replays_a_repeated_request_id() {
        let (_state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        put_notes(port, json!({"remote": "https://github.com/acme/notes.git"})).await;

        let accepted = post(port, "/repos/notes/sync", json!({"request_id": "boot-1"})).await;
        assert_eq!(accepted.status(), 202);
        let location = accepted.headers()["location"].to_str().unwrap().to_string();
        let body: serde_json::Value = accepted.json().await.unwrap();
        let id = body["job"]["id"].as_str().unwrap().to_string();
        assert_eq!(location, format!("/api/git/jobs/{id}"));
        assert!(id.starts_with("job_"));
        assert_eq!(body["job"]["repo_id"], "notes");
        assert_eq!(body["job"]["op"], "sync");
        assert_eq!(body["job"]["request_id"], "boot-1");
        assert_eq!(body["job"]["stalled"], false);
        assert!(body["job"]["host_instance"].is_string());

        // The dropped-response case: same id, so the answer is the same job and never
        // a conflict.
        let replay = post(port, "/repos/notes/sync", json!({"request_id": "boot-1"})).await;
        assert_eq!(replay.status(), 200);
        assert!(replay.headers().get("location").is_none());
        let body: serde_json::Value = replay.json().await.unwrap();
        assert_eq!(body["job"]["id"], id);
    }

    #[tokio::test]
    async fn a_busy_repo_answers_409_with_the_running_job_embedded() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let ops = Arc::new(GatedOps {
            gate: std::sync::Mutex::new(Some(rx)),
        });
        let (_state, port, _dir) = spawn_git_state(true, ops).await;
        put_notes(port, json!({})).await;

        assert_eq!(
            post(port, "/repos/notes/commit", json!({})).await.status(),
            202
        );
        let conflict = post(port, "/repos/notes/commit", json!({})).await;
        assert_eq!(conflict.status(), 409);
        assert_eq!(conflict.headers()["retry-after"], "1");
        let body: serde_json::Value = conflict.json().await.unwrap();
        assert_eq!(body["error"]["code"], "repo_busy");
        assert_eq!(body["error"]["repo_id"], "notes");
        assert_eq!(body["error"]["job"]["op"], "commit");
        assert_eq!(body["error"]["job"]["state"], "running");

        // A DELETE mid-job is the same conflict: the running job snapshotted its def.
        let conflict = authed()
            .delete(git(port, "/repos/notes"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(conflict.status(), 409);

        drop(tx); // release the blocked worker rather than leaking a pool thread
    }

    #[tokio::test]
    async fn the_mutating_routes_refuse_impossible_requests() {
        let (_state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        put_notes(port, json!({})).await;

        for verb in ["clone", "pull", "push", "sync"] {
            let r = post(port, &format!("/repos/notes/{verb}"), json!({})).await;
            assert_eq!(r.status(), 422, "{verb} with no remote");
            let body: serde_json::Value = r.json().await.unwrap();
            assert_eq!(body["error"]["code"], "remote_missing", "{verb}");
        }

        let r = post(port, "/repos/notes/reset", json!({"to": "head"})).await;
        assert_eq!(r.status(), 422);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["code"], "confirm_required");

        // create:false, checkout:false, no upstream asks for nothing at all.
        let r = post(
            port,
            "/repos/notes/branch",
            json!({"name": "dev", "checkout": false}),
        )
        .await;
        assert_eq!(r.status(), 422);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["code"], "invalid_request");

        let r = post(port, "/repos/notes/branch", json!({"name": "../evil"})).await;
        assert_eq!(r.status(), 422);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["field"], "name");

        let r = post(
            port,
            "/repos/notes/init",
            json!({"request_id": "x".repeat(200)}),
        )
        .await;
        assert_eq!(r.status(), 422);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["field"], "request_id");

        let r = post(port, "/repos/notes/commit", json!({"messsage": "typo"})).await;
        assert_eq!(r.status(), 422, "a silently-ignored key is a data-loss bug");
    }

    #[tokio::test]
    async fn jobs_can_be_listed_filtered_and_fetched_by_id() {
        let (_state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        put_notes(port, json!({})).await;
        authed()
            .put(git(port, "/repos/other"))
            .header("x-host-token", "tok123")
            .json(&json!({}))
            .send()
            .await
            .unwrap();

        let first: serde_json::Value = post(port, "/repos/notes/init", json!({}))
            .await
            .json()
            .await
            .unwrap();
        let id = first["job"]["id"].as_str().unwrap().to_string();
        post(port, "/repos/other/init", json!({})).await;

        let all: serde_json::Value = authed()
            .get(git(port, "/jobs"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(all["jobs"].as_array().unwrap().len(), 2);
        // Newest first, so a caller reading the head of the list gets the latest.
        assert_eq!(all["jobs"][0]["repo_id"], "other");

        let filtered: serde_json::Value = authed()
            .get(git(port, "/jobs?repo_id=notes&limit=1"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(filtered["jobs"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["jobs"][0]["id"], id);

        let bad = authed()
            .get(git(port, "/jobs?state=sleeping"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status(), 422);

        let one: serde_json::Value = authed()
            .get(git(port, &format!("/jobs/{id}")))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(one["job"]["id"], id);
        assert_eq!(one["job"]["op"], "init");
    }

    #[tokio::test]
    async fn an_unknown_job_id_404s_and_names_the_host_instance() {
        let (state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        let r = authed()
            .get(git(port, "/jobs/job_deadbeef_000001"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["code"], "job_not_found");
        // The published recipe: a *different* host_instance means the host restarted and
        // the caller should read `last_sync` rather than retry the poll.
        assert_eq!(
            body["error"]["host_instance"],
            state.git.as_ref().unwrap().host_instance()
        );
    }

    #[tokio::test]
    async fn no_route_under_api_git_answers_without_the_token() {
        let (_state, port, _dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        put_notes(port, json!({})).await;
        let client = reqwest::Client::new();
        for (method, path) in GIT_ROUTES {
            let r = client
                .request(
                    method.parse().unwrap(),
                    format!("http://127.0.0.1:{port}{path}"),
                )
                .json(&json!({}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 401, "{method} {path} answered without a token");
            let body: serde_json::Value = r.json().await.unwrap();
            assert_eq!(body["error"]["code"], "unauthorized", "{method} {path}");
            // A wrong token is the same answer as no token.
            let r = client
                .request(
                    method.parse().unwrap(),
                    format!("http://127.0.0.1:{port}{path}"),
                )
                .header("x-host-token", "not-the-token")
                .json(&json!({}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 401, "{method} {path} accepted a wrong token");
        }
    }

    /// A `GitOps` whose `status` blocks until the test releases it — the wedged
    /// filesystem `STATUS_READ_TIMEOUT_MS` exists for. The unit-level twin lives in
    /// `git::tests`; this one proves the 504 survives the whole axum stack.
    struct StallingOps {
        gate: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl crate::git::GitOps for StallingOps {
        fn run(
            &self,
            _op: crate::git::jobs::JobOp,
            _ctx: &crate::git::ops::OpCtx,
        ) -> Result<crate::git::ops::OpOutcome, crate::git::error::GitError> {
            Ok(crate::git::ops::OpOutcome::new("up_to_date", "main"))
        }

        fn status(&self, ctx: &crate::git::ops::ReadCtx) -> crate::git::ops::RepoStatus {
            if let Some(rx) = self.gate.lock().unwrap().take() {
                let _ = rx.recv();
            }
            crate::git::ops::status(ctx)
        }
    }

    #[tokio::test]
    async fn a_wedged_status_read_answers_504_over_http() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let ops = Arc::new(StallingOps {
            gate: std::sync::Mutex::new(Some(rx)),
        });
        let (_state, port, _dir) = spawn_git_state(true, ops).await;
        put_notes(port, json!({})).await;

        // This test pays the whole STATUS_READ_TIMEOUT_MS once; `status_timeout` is the
        // only code in the host that answers 504, so it is the only way to prove the
        // mapping end to end.
        let r = authed()
            .get(git(port, "/repos/notes/status"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 504);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["code"], "status_timeout");
        assert_eq!(body["error"]["repo_id"], "notes");
        // `retryable` belongs to the *job*-error shape, not the HTTP envelope; the
        // status code is what tells an HTTP caller to come back.
        assert!(body["error"]["retryable"].is_null());

        drop(tx); // release the blocking-pool thread rather than hanging shutdown
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn purging_a_tree_that_escapes_the_repos_root_is_refused_over_http() {
        let (state, port, dir) = spawn_git_state(true, Arc::new(StubOps)).await;
        put_notes(port, json!({})).await;

        // The one attack this guard exists for: point the repo's tree at something the
        // host was never asked to own, then ask the host to purge it.
        let outside = dir.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.md"), b"not yours to delete").unwrap();
        let tree = state.git.as_ref().unwrap().tree_path("notes");
        std::os::unix::fs::symlink(&outside, &tree).unwrap();

        let r = authed()
            .delete(git(port, "/repos/notes?purge=true"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["code"], "path_refused");
        // The refusal names the *resolved* target, not the link — that is the path a
        // human has to go and look at.
        assert!(
            body["error"]["path"]
                .as_str()
                .unwrap()
                .ends_with("elsewhere"),
            "path_refused must name the path it refused: {body}"
        );
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("resolves outside"));

        assert!(outside.join("keep.md").exists(), "the target was deleted");
        // A refusal leaves the definition alone, so the caller can fix the symlink and
        // retry rather than having lost the repo.
        let still = authed()
            .get(git(port, "/repos/notes"))
            .header("x-host-token", "tok123")
            .send()
            .await
            .unwrap();
        assert_eq!(still.status(), 200);
    }
}
