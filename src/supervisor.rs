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
    let mut env: Vec<(String, String)> = cfg_env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
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
        if let Ok(resp) = client.get(url).send().await {
            if resp.status().as_u16() < 400 {
                return Ok(());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "server did not become healthy at {url} within {}s",
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
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                use tokio::io::AsyncWriteExt;
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                    .await;
            }
        });
        wait_healthy(&format!("http://{addr}/"), Duration::from_secs(5))
            .await
            .unwrap();
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
