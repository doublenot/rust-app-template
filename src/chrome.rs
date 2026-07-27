use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const DOWNLOAD_URL: &str = "https://www.google.com/chrome/";

/// Size of a secondary window (settings, etc.), used both by
/// `open_extra_window` and by the host when it has to *launch* Chrome for a
/// secondary window because no instance is owned yet (tray mode). Keeping the
/// two paths on one constant means a settings window looks the same however
/// it was opened.
pub const EXTRA_WINDOW_SIZE: (u32, u32) = (700, 560);

#[cfg(target_os = "windows")]
pub fn candidates() -> Vec<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    let mut out = Vec::new();
    const APP_PATHS: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\chrome.exe";
    for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        if let Ok(key) = RegKey::predef(hive).open_subkey(APP_PATHS) {
            if let Ok(path) = key.get_value::<String, _>("") {
                out.push(PathBuf::from(path));
            }
        }
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
        if let Some(base) = std::env::var_os(var) {
            out.push(PathBuf::from(base).join(r"Google\Chrome\Application\chrome.exe"));
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

pub fn find_first_existing(paths: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
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
        .args(launch_args(
            url,
            profile_dir,
            EXTRA_WINDOW_SIZE.0,
            EXTRA_WINDOW_SIZE.1,
        ))
        .spawn()
        .map(drop)
}

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
        let paths = vec![
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            PathBuf::from("/c"),
        ];
        let found = find_first_existing(&paths, |p| p.ends_with("b") || p.ends_with("c"));
        assert_eq!(found, Some(PathBuf::from("/b")));
        assert_eq!(find_first_existing(&paths, |_| false), None);
    }

    #[test]
    fn launch_args_contract() {
        let args = launch_args(
            "http://127.0.0.1:9/loading",
            Path::new("/data/profile"),
            1280,
            900,
        );
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--app=http://127.0.0.1:9/loading".to_string()));
        assert!(args.iter().any(|a| a.starts_with("--user-data-dir=")));
        assert!(args.contains(&"--no-first-run".to_string()));
        assert!(args.contains(&"--no-default-browser-check".to_string()));
        assert!(args.contains(&"--window-size=1280,900".to_string()));
    }

    /// The host launches a *tracked* Chrome for a secondary window when it
    /// owns no instance yet (tray "Settings…" with no app window open). That
    /// path goes through `launch` + `EXTRA_WINDOW_SIZE` rather than
    /// `open_extra_window`, so both must produce the same geometry.
    #[test]
    fn extra_window_size_is_shared_by_both_secondary_window_paths() {
        let args = launch_args(
            "http://127.0.0.1:9/settings",
            Path::new("/data/profile"),
            EXTRA_WINDOW_SIZE.0,
            EXTRA_WINDOW_SIZE.1,
        );
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--window-size=700,560".to_string()));
    }

    #[test]
    #[ignore = "requires Google Chrome installed; run locally with cargo test -- --ignored"]
    fn real_chrome_reports_version() {
        let exe = find_chrome().expect("Chrome not installed");
        let out = std::process::Command::new(exe)
            .arg("--version")
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("Chrome"));
    }
}
