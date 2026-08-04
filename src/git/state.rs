//! `git-state.json` — host-owned volatile state.
//!
//! Everything the host learns while running and wants back after a restart: the outcome of the
//! last sync, the SSH host key it pinned. It is written after every terminal job, which is why
//! it must not be `repos.json` — a repo syncing every 30 s would rewrite its own credential file
//! 2 880 times a day. **Never contains a secret**, and a corrupt one is never fatal: it is a
//! cache, so the only correct response to nonsense is to start empty.

use crate::git::registry::open_private;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StateFile {
    pub version: u32,
    /// Keyed by repo id. `BTreeMap` so the file has a stable key order and diffs are readable.
    pub repos: BTreeMap<String, RepoState>,
}

impl Default for StateFile {
    fn default() -> Self {
        StateFile {
            version: STATE_VERSION,
            repos: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepoState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<LastSync>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_host_fingerprint: Option<String>,
}

/// The outcome of the last terminal job for a repo. Survives both job eviction and a host
/// restart, which is what lets `/api/status` answer "when did this last sync, and did it work?"
/// without keeping every job record forever.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastSync {
    pub at_ms: u64,
    pub ok: bool,
    /// `JobOp::as_str()`. A `String` rather than the enum so `state.rs` stays free of `jobs.rs`.
    pub op: String,
    pub job_id: String,
    pub outcome: Option<String>,
    pub head: Option<String>,
    pub code: Option<String>,
    /// Already scrubbed by the time it lands here. This file must never contain a secret.
    pub message: Option<String>,
}

pub struct StateStore {
    path: PathBuf,
    file: Mutex<StateFile>,
    load_error: Option<String>,
}

impl StateStore {
    /// Never fails. Anything unreadable resets to empty, because this file is a cache and the
    /// alternative — refusing to start over a stale cache — is absurd.
    pub fn load(path: &Path) -> StateStore {
        let (file, load_error) = match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (StateFile::default(), None),
            Err(e) => (
                StateFile::default(),
                Some(format!(
                    "git-state.json cannot be read ({e}); starting empty"
                )),
            ),
            Ok(text) => match serde_json::from_str::<StateFile>(&text) {
                Ok(f) if f.version == STATE_VERSION => (f, None),
                Ok(f) => (
                    StateFile::default(),
                    Some(format!(
                        "git-state.json has version {}, expected {STATE_VERSION}; starting empty",
                        f.version
                    )),
                ),
                Err(e) => (
                    StateFile::default(),
                    Some(format!("git-state.json is not valid ({e}); starting empty")),
                ),
            },
        };
        StateStore {
            path: path.to_path_buf(),
            file: Mutex::new(file),
            load_error,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// For the startup line in `git.log`; `state.rs` has no logger of its own.
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    /// A poisoned lock means another thread panicked while holding it. Every mutation replaces
    /// whole values under the lock, so nothing half-written can be observed and recovering beats
    /// making the cache permanently unreadable.
    fn lock(&self) -> std::sync::MutexGuard<'_, StateFile> {
        self.file.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn snapshot(&self) -> StateFile {
        self.lock().clone()
    }

    pub fn fingerprint(&self, repo_id: &str) -> Option<String> {
        self.lock()
            .repos
            .get(repo_id)
            .and_then(|r| r.ssh_host_fingerprint.clone())
    }

    pub fn last_sync(&self, repo_id: &str) -> Option<LastSync> {
        self.lock()
            .repos
            .get(repo_id)
            .and_then(|r| r.last_sync.clone())
    }

    pub fn record_fingerprint(&self, repo_id: &str, fingerprint: &str) {
        self.mutate(|f| {
            f.repos
                .entry(repo_id.to_string())
                .or_default()
                .ssh_host_fingerprint = Some(fingerprint.to_string());
        });
    }

    pub fn record_last_sync(&self, repo_id: &str, last: LastSync) {
        self.mutate(|f| {
            f.repos.entry(repo_id.to_string()).or_default().last_sync = Some(last);
        });
    }

    pub fn forget(&self, repo_id: &str) {
        self.mutate(|f| {
            f.repos.remove(repo_id);
        });
    }

    /// **Best effort, by contract.** `git-state.json` is a cache; a failed write here must never
    /// turn a successful sync into a failed one, so the error is swallowed at this one place and
    /// nowhere else. The in-memory value is updated either way.
    fn mutate(&self, edit: impl FnOnce(&mut StateFile)) {
        let mut guard = self.lock();
        edit(&mut guard);
        let _ = persist(&self.path, &guard);
    }
}

/// Same atomic, mode-0600 dance as `repos.json`: `open_private` sets the mode at creation,
/// `sync_all` makes the bytes durable before the rename, and the rename publishes them.
/// This file holds no secret, but it sits beside one and inconsistent permissions in a data
/// directory are how a "that file is safe" assumption gets made about the wrong file.
fn persist(path: &Path, file: &StateFile) -> std::io::Result<()> {
    use std::io::Write;

    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let bytes = serde_json::to_string_pretty(file)?;
    let mut f = open_private(&tmp)?;
    f.write_all(bytes.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn last(ok: bool) -> LastSync {
        LastSync {
            at_ms: 1_785_000_004_411,
            ok,
            op: "sync".to_string(),
            job_id: "job_7c1e93aa_00002b".to_string(),
            outcome: Some("merged".to_string()),
            head: Some("b4a71dd".to_string()),
            code: None,
            message: None,
        }
    }

    #[test]
    fn a_missing_file_loads_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("git-state.json");
        let store = StateStore::load(&path);
        assert_eq!(store.path(), path);
        assert!(store.load_error().is_none());
        assert!(store.snapshot().repos.is_empty());
        assert_eq!(store.snapshot().version, STATE_VERSION);
        assert_eq!(store.last_sync("notes"), None);
        assert_eq!(store.fingerprint("notes"), None);
        assert!(!path.exists(), "loading must not create the file");
    }

    #[test]
    fn the_documented_shape_round_trips() {
        let text = r#"{"version":1,"repos":{"notes":{
            "last_sync":{"at_ms":1785000004411,"ok":true,"op":"sync",
                         "job_id":"job_7c1e93aa_00002b","outcome":"merged",
                         "head":"b4a71dd","code":null,"message":null},
            "ssh_host_fingerprint":"SHA256:nThbg6kXUpJWGl7E1IGOCspRomTxdCARLviKw6E5SY8"}}}"#;
        let file: StateFile = serde_json::from_str(text).expect("parses");
        assert_eq!(file.version, STATE_VERSION);
        let repo = file.repos.get("notes").expect("notes");
        assert_eq!(
            repo.last_sync
                .as_ref()
                .expect("last_sync")
                .outcome
                .as_deref(),
            Some("merged")
        );
        assert!(repo
            .ssh_host_fingerprint
            .as_deref()
            .expect("fp")
            .starts_with("SHA256:"));

        // An absent RepoState serialises to `{}`, not to a wall of nulls.
        let empty = serde_json::to_string(&RepoState::default()).expect("serialize");
        assert_eq!(empty, "{}");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(serde_json::from_str::<StateFile>(r#"{"version":1,"repos":{},"x":1}"#).is_err());
        assert!(serde_json::from_str::<RepoState>(r#"{"x":1}"#).is_err());
    }

    #[test]
    fn corruption_is_never_fatal() {
        for body in [
            "not json at all",
            r#"{"version":9,"repos":{}}"#,
            r#"{"version":1,"repos":{"notes":{"last_sync":{"at_ms":1}}}}"#, // partial last_sync
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("git-state.json");
            std::fs::write(&path, body).expect("write");
            let store = StateStore::load(&path);
            assert!(store.snapshot().repos.is_empty(), "{body}");
            assert!(store.load_error().is_some(), "{body}");
            // A cache is regenerated, not quarantined: there is nothing here to hand back.
            assert!(path.exists(), "{body}");
        }
    }

    #[test]
    fn recorded_state_survives_a_reload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("git-state.json");
        let store = StateStore::load(&path);
        store.record_last_sync("notes", last(true));
        store.record_fingerprint("notes", "SHA256:abc");
        store.record_fingerprint("other", "SHA256:def");

        assert_eq!(store.last_sync("notes"), Some(last(true)));
        assert_eq!(store.fingerprint("notes").as_deref(), Some("SHA256:abc"));

        let reloaded = StateStore::load(&path);
        assert!(reloaded.load_error().is_none());
        assert_eq!(reloaded.last_sync("notes"), Some(last(true)));
        assert_eq!(reloaded.fingerprint("other").as_deref(), Some("SHA256:def"));
        assert_eq!(reloaded.snapshot().repos.len(), 2);
        assert!(!dir.path().join("git-state.json.tmp").exists());
    }

    #[test]
    fn a_later_record_replaces_the_earlier_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StateStore::load(&dir.path().join("git-state.json"));
        store.record_last_sync("notes", last(true));
        store.record_last_sync("notes", last(false));
        assert!(!store.last_sync("notes").expect("present").ok);
        // Recording a sync must not disturb the pinned host key, and vice versa.
        store.record_fingerprint("notes", "SHA256:abc");
        store.record_last_sync("notes", last(true));
        assert_eq!(store.fingerprint("notes").as_deref(), Some("SHA256:abc"));
    }

    #[test]
    fn forget_drops_one_repo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("git-state.json");
        let store = StateStore::load(&path);
        store.record_fingerprint("notes", "SHA256:abc");
        store.record_fingerprint("other", "SHA256:def");
        store.forget("notes");
        assert_eq!(store.fingerprint("notes"), None);
        assert_eq!(
            StateStore::load(&path).fingerprint("other").as_deref(),
            Some("SHA256:def")
        );
        // Forgetting something absent is a no-op, not a panic.
        store.forget("never-existed");
    }

    #[test]
    fn a_write_failure_is_swallowed() {
        // The parent directory does not exist, so no write can succeed. A cache write failure
        // must never turn a successful sync into a failed one.
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StateStore::load(&dir.path().join("nope").join("git-state.json"));
        store.record_fingerprint("notes", "SHA256:abc");
        assert_eq!(
            store.fingerprint("notes").as_deref(),
            Some("SHA256:abc"),
            "the in-memory value still updates"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_state_file_is_not_readable_by_group_or_other() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("git-state.json");
        let store = StateStore::load(&path);
        store.record_fingerprint("notes", "SHA256:abc");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "mode was {:o}", mode & 0o777);
    }
}
