use crate::config::ServerSection;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct ServerHandle {
    pub child: tokio::process::Child,
    // Not read back by the host (main.rs recomputes the log path from
    // RuntimePaths); kept on the handle for API completeness/callers that
    // don't have RuntimePaths in scope.
    #[allow(dead_code)]
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

/// The `[server]` child's ticket back into the host: the loopback base URL and
/// the bearer token the internal server checks as `x-host-token`.
///
/// This reaches the child process and NOTHING else. `chrome::launch` inherits
/// the host's own environment, which never contains the token — so no page
/// rendered in the browser can read it out of a child's `process.env`, and a
/// compromised page cannot drive `/api/restart` or `/api/git/*`.
pub struct HostAccess<'a> {
    pub url: &'a str,
    pub token: &'a str,
    pub git_enabled: bool,
}

pub fn build_env(
    cfg_env: &BTreeMap<String, String>,
    settings_env: &[(String, String)],
    settings_file: &Path,
    host: &HostAccess<'_>,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = cfg_env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    env.extend(settings_env.iter().cloned());
    env.push((
        "APP_SETTINGS_FILE".to_string(),
        settings_file.to_string_lossy().into_owned(),
    ));
    env.push(("APP_HOST_URL".to_string(), host.url.to_string()));
    env.push(("APP_HOST_TOKEN".to_string(), host.token.to_string()));
    // Pushed only when [git] is present, and absent (not "0") otherwise, so a
    // child can branch on `"APP_GIT_ENABLED" in process.env` alone.
    if host.git_enabled {
        env.push(("APP_GIT_ENABLED".to_string(), "1".to_string()));
    }
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
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
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
    Ok(ServerHandle {
        child,
        log_path: log_path.to_path_buf(),
    })
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
        assert_eq!(
            resolve_cwd(Some("server"), base),
            PathBuf::from("/base/server")
        );
        let abs = if cfg!(windows) { r"C:\abs" } else { "/abs" };
        assert_eq!(resolve_cwd(Some(abs), base), PathBuf::from(abs));
    }

    #[test]
    fn build_env_layers_and_settings_file() {
        let mut cfg_env = BTreeMap::new();
        cfg_env.insert("PORT".to_string(), "8080".to_string());
        let settings = vec![("APP_SETTING_THEME".to_string(), "dark".to_string())];
        let host = HostAccess {
            url: "http://127.0.0.1:5051",
            token: "tok",
            git_enabled: false,
        };
        let env = build_env(&cfg_env, &settings, Path::new("/data/settings.json"), &host);
        assert!(env.contains(&("PORT".into(), "8080".into())));
        assert!(env.contains(&("APP_SETTING_THEME".into(), "dark".into())));
        assert!(env
            .iter()
            .any(|(k, v)| k == "APP_SETTINGS_FILE" && v.ends_with("settings.json")));
    }

    #[test]
    fn build_env_exports_host_access_and_gates_the_git_flag() {
        let cfg_env = BTreeMap::new();
        let off = HostAccess {
            url: "http://127.0.0.1:5051",
            token: "s3cret",
            git_enabled: false,
        };
        let env = build_env(&cfg_env, &[], Path::new("/d/settings.json"), &off);
        assert!(env.contains(&("APP_HOST_URL".into(), "http://127.0.0.1:5051".into())));
        assert!(env.contains(&("APP_HOST_TOKEN".into(), "s3cret".into())));
        // Absent, not "0": a child that tests for the variable's presence and
        // one that parses its value must agree about what "off" looks like.
        assert!(
            !env.iter().any(|(k, _)| k == "APP_GIT_ENABLED"),
            "APP_GIT_ENABLED must not be exported when [git] is absent"
        );

        let on = HostAccess {
            url: "http://127.0.0.1:5051",
            token: "s3cret",
            git_enabled: true,
        };
        let env = build_env(&cfg_env, &[], Path::new("/d/settings.json"), &on);
        assert!(env.contains(&("APP_GIT_ENABLED".into(), "1".into())));
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
                // buffer still holds unread bytes is an *abortive* close on Windows -- the
                // stack sends RST instead of FIN, and the reset outruns the response we just
                // wrote, so every poll `wait_healthy` makes fails and the 5s budget expires.
                // This is the whole of why the test failed on windows-latest and nowhere
                // else; the real child servers this stands in for read their requests.
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
                // FIN, not RST: let the client read what we wrote.
                let _ = sock.shutdown().await;
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
        let mut handle =
            spawn_server(&cfg, vec![], std::env::current_dir().unwrap(), &log_path).unwrap();
        handle.child.wait().await.unwrap();
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.contains("cargo"), "log was: {log:?}");
    }
}
