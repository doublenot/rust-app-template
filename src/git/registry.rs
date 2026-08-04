//! `repos.json` — the repository registry.
//!
//! **Definitions only.** This file is written by `PUT`/`DELETE` and by nothing else: not by a
//! sync, not by a timer, not by a job. Every host-written volatile value lives in
//! `git-state.json` (`super::state`). That is what makes `repos.json` safe to hand-write, safe
//! to commit, and keeps a file that can hold a personal access token from being rewritten 2 880
//! times a day.

use crate::config::validate_branch_name;
use crate::git::error::{GitError, GitErrorCode};
use crate::git::secret::Secret;
use crate::git::util::now_ms;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Below this an auto-sync would hammer the remote and never let a user finish an edit;
/// above `MAX` the timer is indistinguishable from "off" and `sync_on_start` is the honest
/// way to ask for it.
pub const AUTO_SYNC_MIN_SECS: u64 = 30;
pub const AUTO_SYNC_MAX_SECS: u64 = 86_400;
/// An id becomes a directory name under `<data-dir>/repos/`; 64 bytes leaves room for the
/// tree beneath it on every filesystem we ship to.
pub const MAX_REPO_ID_LEN: usize = 64;
pub const REGISTRY_VERSION: u32 = 1;

/// One entry of `repos.json`.
///
/// Deliberately **not** `Serialize`: it holds `Secret`, which has no `Serialize` impl, so
/// deriving it here would not compile. The two legal ways out are `to_wire` (to disk, with the
/// secret) and `RepoView` (to HTTP, without it).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoDef {
    pub id: String,
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default = "default_remote_name")]
    pub remote_name: String,
    /// `""` at parse time, filled from `[git].default_branch` by `normalise_def`.
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub credential: Option<CredentialSpec>,
    #[serde(default)]
    pub author: Option<AuthorSpec>,
    #[serde(default)]
    pub sync_settings: bool,
    #[serde(default = "default_settings_path")]
    pub settings_path: String,
    #[serde(default)]
    pub auto_sync_secs: Option<u64>,
    #[serde(default)]
    pub sync_on_start: bool,
    #[serde(default)]
    pub sync_on_quit: bool,
    #[serde(default)]
    pub restart_children_on_pull: bool,
    /// `0` means "hand-written, never stamped"; `normalise_def` fills it with now.
    #[serde(default)]
    pub created_at_ms: u64,
    #[serde(default)]
    pub updated_at_ms: u64,
}

fn default_remote_name() -> String {
    "origin".to_string()
}

fn default_settings_path() -> String {
    "settings.json".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialSpec {
    None,
    Token {
        #[serde(default = "default_token_username")]
        username: String,
        token: Secret,
        #[serde(default)]
        bound_host: Option<String>,
    },
    SshKey {
        #[serde(default = "default_ssh_username")]
        username: String,
        #[serde(default)]
        private_key_path: Option<PathBuf>,
        #[serde(default)]
        private_key: Option<Secret>,
        #[serde(default)]
        public_key_path: Option<PathBuf>,
        #[serde(default)]
        public_key: Option<String>,
        #[serde(default)]
        passphrase: Option<Secret>,
        #[serde(default)]
        bound_host: Option<String>,
    },
}

fn default_token_username() -> String {
    "x-access-token".to_string()
}

fn default_ssh_username() -> String {
    "git".to_string()
}

impl CredentialSpec {
    /// The host this credential is allowed to be transmitted to, lowercased. `None` for
    /// `kind: "none"` and for a hand-written credential that has not been bound yet.
    pub fn bound_host(&self) -> Option<&str> {
        match self {
            CredentialSpec::None => None,
            CredentialSpec::Token { bound_host, .. }
            | CredentialSpec::SshKey { bound_host, .. } => bound_host.as_deref(),
        }
    }

    pub fn with_bound_host(self, host: Option<String>) -> Self {
        match self {
            CredentialSpec::None => CredentialSpec::None,
            CredentialSpec::Token {
                username, token, ..
            } => CredentialSpec::Token {
                username,
                token,
                bound_host: host,
            },
            CredentialSpec::SshKey {
                username,
                private_key_path,
                private_key,
                public_key_path,
                public_key,
                passphrase,
                ..
            } => CredentialSpec::SshKey {
                username,
                private_key_path,
                private_key,
                public_key_path,
                public_key,
                passphrase,
                bound_host: host,
            },
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, CredentialSpec::None)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorSpec {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

/// `^[a-z0-9][a-z0-9._-]{0,63}$`, minus traversal and Windows device names.
///
/// **Lowercase only.** The id becomes a directory name under `<data-dir>/repos/`, and macOS and
/// Windows filesystems are case-insensitive: `Notes` and `notes` would be one tree on half of all
/// machines and two trees on the other half. Making the collision impossible beats making it
/// platform-dependent, and relaxing this later is cheap where tightening it would not be.
pub fn valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > MAX_REPO_ID_LEN {
        return false;
    }
    if !id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit()) {
        return false;
    }
    if !id.bytes().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-'
    }) {
        return false;
    }
    // `..` escapes `<data-dir>/repos/`. A trailing dot or space is silently stripped by Windows
    // when it creates the directory, so two ids that differ only there would share one tree.
    // Both are already impossible given the character class above; they are restated because a
    // future relaxation of that class must not quietly re-open them.
    if id.contains("..") || id.ends_with('.') || id.ends_with(' ') || id == "." {
        return false;
    }
    !is_windows_reserved(id)
}

/// Creating a directory with one of these names fails on Windows with a bewildering error.
fn is_windows_reserved(id: &str) -> bool {
    const RESERVED: [&str; 22] = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    RESERVED.iter().any(|r| id.eq_ignore_ascii_case(r))
}

pub fn validate_id(id: &str) -> Result<(), GitError> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(GitError::invalid_repo_id(id))
    }
}

fn before(s: &str, sep: char) -> &str {
    match s.find(sep) {
        Some(i) => &s[..i],
        None => s,
    }
}

/// The lowercased network host a remote points at, or `None` when it has none.
///
/// This is the value `bound_host` is compared against, so a wrong answer here means a stored
/// token could travel to a host it was never meant for. Every ambiguous shape therefore returns
/// `None`, which fails closed: an unbound credential is never transmitted.
pub fn remote_host(remote: &str) -> Option<String> {
    let authority = match remote.find("://") {
        Some(i) => before(&remote[i + 3..], '/'),
        None => {
            // scp form: [user@]host:path. A colon that comes after a separator belongs to the
            // path, not to a host.
            let colon = remote.find(':')?;
            let head = &remote[..colon];
            if head.contains('/') || head.contains('\\') {
                return None;
            }
            head
        }
    };
    let host = match authority.rsplit_once('@') {
        Some((_, h)) => h,
        None => authority,
    };
    let host = if host.starts_with('[') {
        // Bracketed IPv6 literal; the port, if any, follows the ']'.
        match host.find(']') {
            Some(j) => &host[..=j],
            None => return None,
        }
    } else {
        before(host, ':')
    };
    // A one-character host is a Windows drive letter (`C:\repos\notes`), and an empty one is
    // `file:///path`. Neither is somewhere a credential can be sent.
    if host.len() < 2 || host.contains(char::is_whitespace) {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

pub fn validate_remote(remote: &str, allow_http: bool) -> Result<(), GitError> {
    if remote.is_empty() {
        return Err(GitError::invalid_request(
            "remote",
            "remote must not be empty (omit it entirely for a local-only repo)",
        ));
    }
    if remote.chars().any(char::is_control) {
        return Err(GitError::invalid_request(
            "remote",
            "remote must not contain control characters",
        ));
    }
    let password_refused = |authority: &str| -> Result<(), GitError> {
        match authority.rsplit_once('@') {
            // libgit2 quotes the URL into its error strings, so a password here would be copied
            // into git.log and into every API error body the moment anything goes wrong.
            Some((userinfo, _)) if userinfo.contains(':') => Err(GitError::insecure_remote(
                remote,
                "a password in the remote URL would be copied into log lines and error messages; \
                 store a credential on the repo instead",
            )),
            _ => Ok(()),
        }
    };

    if let Some(i) = remote.find("://") {
        let scheme = remote[..i].to_ascii_lowercase();
        password_refused(before(&remote[i + 3..], '/'))?;
        match scheme.as_str() {
            "https" | "ssh" | "git" | "file" => {}
            "http" if allow_http => {}
            "http" => {
                return Err(GitError::insecure_remote(
                    remote,
                    "http:// sends credentials in clear text; set [git].allow_http = true if this \
                     is a trusted host on a private network",
                ))
            }
            other => {
                return Err(GitError::invalid_request(
                    "remote",
                    format!(
                        "unsupported remote scheme {other:?} (expected https, ssh, git or file)"
                    ),
                ))
            }
        }
        if scheme != "file" && remote_host(remote).is_none() {
            return Err(GitError::invalid_request(
                "remote",
                format!("remote {remote:?} has no host"),
            ));
        }
        return Ok(());
    }

    if remote_host(remote).is_some() {
        password_refused(before(remote, ':'))?;
        return Ok(());
    }
    if Path::new(remote).is_absolute() {
        return Ok(());
    }
    Err(GitError::invalid_request(
        "remote",
        format!(
            "remote {remote:?} must be a URL (https://, ssh://, git://, file://), git@host:path, \
             or an absolute local path"
        ),
    ))
}

/// The `[git]` values a repo definition is completed and judged against. Held by `Registry` so
/// that `load` and `put` cannot drift apart on what a valid entry is.
#[derive(Debug, Clone)]
pub struct RegistryDefaults {
    pub default_branch: String,
    pub settings_enabled: bool,
    pub allow_http: bool,
    pub registry_writes: bool,
}

pub fn validate_settings_path(path: &str) -> Result<(), GitError> {
    let bad = |why: &str| -> Result<(), GitError> {
        Err(GitError::invalid_request(
            "settings_path",
            format!("settings_path {path:?} {why}"),
        ))
    };
    if path.is_empty() {
        return bad("must not be empty");
    }
    if path.contains('\\') {
        return bad("must use forward slashes, even on Windows");
    }
    if path.starts_with('/') || Path::new(path).is_absolute() {
        return bad("must be relative to the repository root");
    }
    if path.ends_with('/') {
        return bad("must name a file, not a directory");
    }
    if path.chars().any(char::is_control) {
        return bad("must not contain control characters");
    }
    for part in path.split('/') {
        if part.is_empty() {
            return bad("must not contain an empty path segment");
        }
        // The settings file is written by us inside the worktree; `..` would let a registry
        // entry name a file anywhere on disk.
        if part == ".." {
            return bad("must not contain '..'");
        }
    }
    Ok(())
}

/// Everything that makes an entry unacceptable, in one place, so that a hand-written
/// `repos.json` and a `PUT` body are judged identically.
///
/// Note what is *not* here: `auto_sync_secs` range. That is clamped by `normalise_def`, not
/// rejected — a value of 5 is a typo worth correcting, not a reason to drop the repo.
pub fn validate_def(def: &RepoDef, d: &RegistryDefaults) -> Result<(), GitError> {
    validate_id(&def.id)?;

    let branch = if def.branch.is_empty() {
        d.default_branch.as_str()
    } else {
        def.branch.as_str()
    };
    if !validate_branch_name(branch) {
        return Err(GitError::invalid_request(
            "branch",
            format!("branch {branch:?} is not a valid branch name"),
        ));
    }

    if def.remote_name.is_empty()
        || def.remote_name.len() > 64
        || !def
            .remote_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
    {
        return Err(GitError::invalid_request(
            "remote_name",
            format!(
                "remote_name {:?} must be 1-64 characters of [A-Za-z0-9._-]",
                def.remote_name
            ),
        ));
    }

    if let Some(remote) = &def.remote {
        validate_remote(remote, d.allow_http)?;
    }

    if let Some(author) = &def.author {
        if let Some(name) = &author.name {
            if name.contains(['<', '>', '\n']) {
                return Err(GitError::invalid_request(
                    "author.name",
                    "author.name must not contain '<', '>' or newlines (git signatures cannot \
                     represent them)",
                ));
            }
        }
        if let Some(email) = &author.email {
            if !email.is_empty()
                && (!email.contains('@')
                    || email.contains(['<', '>'])
                    || email.chars().any(char::is_whitespace))
            {
                return Err(GitError::invalid_request(
                    "author.email",
                    format!(
                        "author.email {email:?} must look like a plain address, e.g. app@example.com"
                    ),
                ));
            }
        }
    }

    // Refusing this is the point: sync_settings that silently does nothing is how people
    // discover, days later, that their settings were never being synced.
    if def.sync_settings && !d.settings_enabled {
        return Err(GitError::settings_sync_unavailable());
    }
    validate_settings_path(&def.settings_path)?;

    if let Some(CredentialSpec::SshKey {
        private_key_path,
        private_key,
        ..
    }) = &def.credential
    {
        // Neither means there is nothing to authenticate with; both means the resolver would
        // have to invent a precedence rule nobody asked for.
        if private_key_path.is_some() == private_key.is_some() {
            return Err(GitError::invalid_request(
                "credential.private_key",
                "an ssh_key credential needs exactly one of private_key_path or private_key",
            ));
        }
    }

    Ok(())
}

/// A repo definition as the HTTP API sees it.
///
/// The defence against leaking a token is that **there is no field to leak it through** — not a
/// `#[serde(skip)]` somebody can forget, not a `Secret` somebody can make `Serialize`. Adding a
/// secret-shaped field here is the only way to break it.
#[derive(Debug, Clone, Serialize)]
pub struct RepoView {
    pub id: String,
    pub path: String,
    pub exists: bool,
    pub remote: Option<String>,
    pub remote_name: String,
    pub branch: String,
    pub credential: Option<CredentialView>,
    pub has_credentials: bool,
    pub author: Option<AuthorSpec>,
    pub sync_settings: bool,
    pub settings_path: String,
    pub auto_sync_secs: Option<u64>,
    pub sync_on_start: bool,
    pub sync_on_quit: bool,
    pub restart_children_on_pull: bool,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CredentialView {
    None,
    Token {
        username: String,
        token_stored: bool,
        bound_host: Option<String>,
    },
    SshKey {
        username: String,
        private_key_path: Option<String>,
        private_key_stored: bool,
        public_key_path: Option<String>,
        passphrase_stored: bool,
        bound_host: Option<String>,
    },
}

impl CredentialView {
    pub fn of(spec: &CredentialSpec) -> CredentialView {
        match spec {
            CredentialSpec::None => CredentialView::None,
            CredentialSpec::Token {
                username,
                bound_host,
                ..
            } => CredentialView::Token {
                username: username.clone(),
                // `token` is a required field of the variant, so its presence is the shape,
                // not a value that has to be read.
                token_stored: true,
                bound_host: bound_host.clone(),
            },
            CredentialSpec::SshKey {
                username,
                private_key_path,
                private_key,
                public_key_path,
                passphrase,
                bound_host,
                ..
            } => CredentialView::SshKey {
                username: username.clone(),
                private_key_path: private_key_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                private_key_stored: private_key.is_some(),
                public_key_path: public_key_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                passphrase_stored: passphrase.is_some(),
                bound_host: bound_host.clone(),
            },
        }
    }
}

impl RepoView {
    pub fn new(def: &RepoDef, repos_dir: &Path) -> RepoView {
        let path = repos_dir.join(&def.id);
        let credential = def.credential.as_ref().map(CredentialView::of);
        RepoView {
            id: def.id.clone(),
            exists: path.exists(),
            path: path.display().to_string(),
            remote: def.remote.clone(),
            remote_name: def.remote_name.clone(),
            branch: def.branch.clone(),
            has_credentials: !matches!(&credential, None | Some(CredentialView::None)),
            credential,
            author: def.author.clone(),
            sync_settings: def.sync_settings,
            settings_path: def.settings_path.clone(),
            auto_sync_secs: def.auto_sync_secs,
            sync_on_start: def.sync_on_start,
            sync_on_quit: def.sync_on_quit,
            restart_children_on_pull: def.restart_children_on_pull,
            created_at_ms: def.created_at_ms,
            updated_at_ms: def.updated_at_ms,
        }
    }
}

/// The whole of `repos.json`, in memory.
#[derive(Debug, Clone)]
pub struct RegistryFile {
    pub version: u32,
    pub repos: Vec<RepoDef>,
    /// Entries we refused to load, kept exactly as they were read. They are written back on
    /// every save so that fixing one typo never costs the author the rest of their file.
    pub rejected_raw: Vec<Value>,
}

/// The only place a stored secret is turned back into JSON.
///
/// `RepoDef` cannot derive `Serialize` (it holds `Secret`, which has none), which is what forces
/// every write through this one function — and makes "who can persist a token?" answerable by
/// reading forty lines instead of auditing a derive.
fn to_wire(file: &RegistryFile) -> Value {
    let mut repos: Vec<Value> = file.repos.iter().map(def_to_wire).collect();
    // Appended rather than re-interleaved: their original indices no longer exist once the good
    // entries have been rewritten, and a stable tail is easier to explain than a shuffled file.
    repos.extend(file.rejected_raw.iter().cloned());
    json!({ "version": file.version, "repos": repos })
}

fn def_to_wire(def: &RepoDef) -> Value {
    json!({
        "id": def.id,
        "remote": def.remote,
        "remote_name": def.remote_name,
        "branch": def.branch,
        "credential": def.credential.as_ref().map(cred_to_wire),
        "author": def.author,
        "sync_settings": def.sync_settings,
        "settings_path": def.settings_path,
        "auto_sync_secs": def.auto_sync_secs,
        "sync_on_start": def.sync_on_start,
        "sync_on_quit": def.sync_on_quit,
        "restart_children_on_pull": def.restart_children_on_pull,
        "created_at_ms": def.created_at_ms,
        "updated_at_ms": def.updated_at_ms,
    })
}

fn cred_to_wire(spec: &CredentialSpec) -> Value {
    match spec {
        CredentialSpec::None => json!({ "kind": "none" }),
        CredentialSpec::Token {
            username,
            token,
            bound_host,
        } => json!({
            "kind": "token",
            "username": username,
            "token": token.expose_for_disk(),
            "bound_host": bound_host,
        }),
        CredentialSpec::SshKey {
            username,
            private_key_path,
            private_key,
            public_key_path,
            public_key,
            passphrase,
            bound_host,
        } => json!({
            "kind": "ssh_key",
            "username": username,
            "private_key_path": private_key_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "private_key": private_key.as_ref().map(Secret::expose_for_disk),
            "public_key_path": public_key_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "public_key": public_key,
            "passphrase": passphrase.as_ref().map(Secret::expose_for_disk),
            "bound_host": bound_host,
        }),
    }
}

/// `create_new` + mode `0600` **at creation**, before a single byte exists.
///
/// Writing first and chmod-ing after leaves a window in which the token is world-readable, and
/// that window is exactly long enough for a backup daemon to notice. On Windows the file
/// inherits the `%APPDATA%` ACL; README §6 documents that as the weaker posture rather than
/// implying it away.
pub(crate) fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

pub(crate) fn save(path: &Path, file: &RegistryFile) -> Result<(), GitError> {
    use std::io::Write;

    let tmp = path.with_extension("json.tmp");
    // A leftover from an interrupted save would make `create_new` fail forever.
    let _ = std::fs::remove_file(&tmp);

    let bytes =
        serde_json::to_string_pretty(&to_wire(file)).map_err(GitError::registry_write_failed)?;
    let mut f = open_private(&tmp).map_err(GitError::registry_write_failed)?;
    f.write_all(bytes.as_bytes())
        .map_err(GitError::registry_write_failed)?;
    // Durable BEFORE the rename: otherwise a power cut can publish an empty repos.json.
    f.sync_all().map_err(GitError::registry_write_failed)?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(GitError::registry_write_failed)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        // fsync the directory so the rename itself survives the same power cut. Best effort:
        // the data is already durable, and no filesystem we ship to fails this in practice.
        let _ = std::fs::File::open(parent).and_then(|d| d.sync_all());
    }
    Ok(())
}

/// Why a `repos.json` could not be used in full. Never fatal: it is reported through
/// `GET /api/git`, through `/api/status`, and in `git.log` at startup, and the host starts
/// normally either way — an optional feature must not brick the app.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegistryError {
    /// `"registry_corrupt"` (file-level, quarantined) or `"registry_entries_rejected"`.
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quarantined_to: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<RejectedEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RejectedEntry {
    pub index: usize,
    /// `null` when the entry was too malformed to read an id out of.
    pub id: Option<String>,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Warning {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug)]
pub struct PutOutcome {
    pub created: bool,
    pub repo: RepoDef,
    pub warnings: Vec<Warning>,
}

/// The in-memory registry. Read once at startup, written only by `put`/`remove`.
pub struct Registry {
    path: PathBuf,
    repos_dir: PathBuf,
    defaults: RegistryDefaults,
    file: RwLock<RegistryFile>,
    error: Option<RegistryError>,
    notes: Vec<String>,
}

impl Registry {
    /// Never fails. A file-level problem quarantines the file and yields an empty registry; an
    /// entry-level problem keeps the good entries and retains the bad ones verbatim.
    pub fn load(path: &Path, repos_dir: &Path, defaults: RegistryDefaults) -> Registry {
        let mut notes = Vec::new();

        // A leftover temp file means we died between `sync_all` and `rename`. The real file is
        // intact, so this is noise — but noise that would make the next `create_new` fail.
        let tmp = path.with_extension("json.tmp");
        if tmp.exists() {
            match std::fs::remove_file(&tmp) {
                Ok(()) => notes.push(format!("removed stale {}", tmp.display())),
                Err(e) => notes.push(format!("cannot remove stale {}: {e}", tmp.display())),
            }
        }
        #[cfg(unix)]
        tighten_mode(path, &mut notes);

        let raw = match std::fs::read_to_string(path) {
            Ok(t) => t,
            // The normal first-run state, not a problem.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Registry::assembled(path, repos_dir, defaults, empty_file(), None, notes)
            }
            Err(e) => {
                let error = RegistryError {
                    code: "registry_corrupt",
                    message: format!("repos.json cannot be read ({e})"),
                    quarantined_to: None,
                    rejected: Vec::new(),
                };
                return Registry::assembled(
                    path,
                    repos_dir,
                    defaults,
                    empty_file(),
                    Some(error),
                    notes,
                );
            }
        };

        let value: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                return quarantine(
                    path,
                    repos_dir,
                    defaults,
                    notes,
                    format!("repos.json is not valid JSON ({e}); quarantined"),
                )
            }
        };
        let Some(obj) = value.as_object() else {
            return quarantine(
                path,
                repos_dir,
                defaults,
                notes,
                "repos.json top level must be a JSON object; quarantined".to_string(),
            );
        };
        if let Some(key) = obj
            .keys()
            .find(|k| k.as_str() != "version" && k.as_str() != "repos")
        {
            return quarantine(
                path,
                repos_dir,
                defaults,
                notes,
                format!("repos.json has an unknown top-level key {key:?}; quarantined"),
            );
        }
        match obj.get("version").and_then(Value::as_u64) {
            Some(v) if v == u64::from(REGISTRY_VERSION) => {}
            Some(v) => {
                return quarantine(
                    path,
                    repos_dir,
                    defaults,
                    notes,
                    format!("repos.json has version {v}, expected {REGISTRY_VERSION}; quarantined"),
                )
            }
            None => {
                return quarantine(
                    path,
                    repos_dir,
                    defaults,
                    notes,
                    "repos.json is missing its \"version\" number; quarantined".to_string(),
                )
            }
        }
        let Some(entries) = obj.get("repos").and_then(Value::as_array).cloned() else {
            return quarantine(
                path,
                repos_dir,
                defaults,
                notes,
                "repos.json must have a \"repos\" array; quarantined".to_string(),
            );
        };

        let now = now_ms();
        let total = entries.len();
        let mut repos: Vec<RepoDef> = Vec::new();
        let mut rejected_raw: Vec<Value> = Vec::new();
        let mut rejected: Vec<RejectedEntry> = Vec::new();

        for (index, entry) in entries.into_iter().enumerate() {
            let id_hint = entry.get("id").and_then(Value::as_str).map(str::to_string);

            // Every rejection does the same three things: report it, keep the raw entry so a
            // later save can write it back, and move on. Spelled out rather than hidden in a
            // closure, because a closure would have to capture two of the three vectors.
            let mut def: RepoDef = match serde_json::from_value(entry.clone()) {
                Ok(d) => d,
                Err(e) => {
                    notes.push(format!("repos[{index}] rejected: {e}"));
                    rejected.push(RejectedEntry {
                        index,
                        id: id_hint,
                        reason: "invalid_request",
                    });
                    rejected_raw.push(entry);
                    continue;
                }
            };
            if repos.iter().any(|r| r.id == def.id) {
                notes.push(format!(
                    "repos[{index}] rejected: duplicate id {:?}",
                    def.id
                ));
                rejected.push(RejectedEntry {
                    index,
                    id: id_hint,
                    reason: "duplicate_id",
                });
                rejected_raw.push(entry);
                continue;
            }
            if let Err(e) = validate_def(&def, &defaults) {
                notes.push(format!("repos[{index}] rejected: {e}"));
                rejected.push(RejectedEntry {
                    index,
                    id: id_hint,
                    reason: reject_reason(&e),
                });
                rejected_raw.push(entry);
                continue;
            }
            // Warnings have nowhere to go at load; the clamped value itself is the report, and
            // `notes` carries the detail into git.log.
            for w in normalise_def(&mut def, &defaults, now) {
                notes.push(format!("repos[{index}]: {}", w.message));
            }
            repos.push(def);
        }

        let error = if rejected.is_empty() {
            None
        } else {
            Some(RegistryError {
                code: "registry_entries_rejected",
                message: format!(
                    "{} of {total} repo definitions were rejected",
                    rejected.len()
                ),
                quarantined_to: None,
                rejected,
            })
        };
        let file = RegistryFile {
            version: REGISTRY_VERSION,
            repos,
            rejected_raw,
        };
        Registry::assembled(path, repos_dir, defaults, file, error, notes)
    }

    fn assembled(
        path: &Path,
        repos_dir: &Path,
        defaults: RegistryDefaults,
        file: RegistryFile,
        error: Option<RegistryError>,
        notes: Vec<String>,
    ) -> Registry {
        Registry {
            path: path.to_path_buf(),
            repos_dir: repos_dir.to_path_buf(),
            defaults,
            file: RwLock::new(file),
            error,
            notes,
        }
    }

    /// A poisoned lock means another thread panicked while holding it. The registry is only ever
    /// replaced wholesale (`*guard = next`), so no half-updated state can be observed and
    /// recovering beats making every subsequent read fail forever.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, RegistryFile> {
        self.file.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, RegistryFile> {
        self.file.write().unwrap_or_else(|e| e.into_inner())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn repos_dir(&self) -> &Path {
        &self.repos_dir
    }

    pub fn writable(&self) -> bool {
        self.defaults.registry_writes
    }

    pub fn error(&self) -> Option<RegistryError> {
        self.error.clone()
    }

    /// Non-fatal observations from `load`, for `git.log`. `registry.rs` has no logger.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    pub fn count(&self) -> usize {
        self.read().repos.len()
    }

    pub fn ids(&self) -> Vec<String> {
        self.read().repos.iter().map(|r| r.id.clone()).collect()
    }

    pub fn list(&self) -> Vec<RepoDef> {
        self.read().repos.clone()
    }

    pub fn snapshot(&self, id: &str) -> Result<RepoDef, GitError> {
        self.read()
            .repos
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or_else(|| GitError::repo_not_found(id))
    }

    pub fn contains(&self, id: &str) -> bool {
        self.read().repos.iter().any(|r| r.id == id)
    }

    pub fn auto_sync_secs(&self, id: &str) -> Option<u64> {
        self.read()
            .repos
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.auto_sync_secs)
    }

    /// Create or replace one entry, then persist. A `PUT` is a whole-definition write: fields
    /// absent from `def` take their serde defaults, they are not merged with what was stored.
    pub fn put(&self, def: RepoDef) -> Result<PutOutcome, GitError> {
        if !self.defaults.registry_writes {
            return Err(GitError::registry_read_only());
        }
        validate_id(&def.id)?;

        let mut def = def;
        let mut warnings = Vec::new();
        let mut guard = self.write();
        let existing = guard.repos.iter().position(|r| r.id == def.id);

        if def.credential.is_none() {
            if let Some(stored) = existing.and_then(|i| guard.repos[i].credential.clone()) {
                let remote_host_now = def.remote.as_deref().and_then(remote_host);
                let bound = stored.bound_host().map(str::to_string);
                // Carrying the stored credential forward keeps the token off the loopback socket
                // on every unrelated edit — but only while it is still bound to the host the
                // remote points at. Repointing a repo at an attacker's host and re-PUTting is the
                // cheapest way to steal a PAT out of this service; dropping the credential here
                // is what makes that attack yield nothing.
                let keep = stored.is_none()
                    || match (&bound, &remote_host_now) {
                        (Some(b), Some(h)) => b == h,
                        // Never bound (hand-written); `normalise_def` binds it below.
                        (None, _) => true,
                        // The remote was removed, so there is no host to stay bound to.
                        (Some(_), None) => false,
                    };
                if keep {
                    def.credential = Some(stored);
                } else {
                    warnings.push(Warning {
                        code: "auth_unbound",
                        message: format!(
                            "the stored credential was bound to {} but this remote is {}; it was \
                             dropped — send a replacement credential with this PUT",
                            bound.as_deref().unwrap_or("(nothing)"),
                            remote_host_now.as_deref().unwrap_or("(no remote)")
                        ),
                    });
                }
            }
        }

        let now = now_ms();
        def.created_at_ms = match existing.map(|i| guard.repos[i].created_at_ms) {
            Some(prior) if prior != 0 => prior,
            _ if def.created_at_ms != 0 => def.created_at_ms,
            _ => now,
        };
        def.updated_at_ms = now;

        validate_def(&def, &self.defaults)?;
        warnings.extend(normalise_def(&mut def, &self.defaults, now));

        // Build the whole next file, persist it, and only then swap it in. An in-memory
        // registry that disagrees with repos.json would silently un-persist itself at the next
        // restart, so the disk write is what commits the change.
        let mut next = guard.clone();
        match existing {
            Some(i) => next.repos[i] = def.clone(),
            None => next.repos.push(def.clone()),
        }
        save(&self.path, &next)?;
        *guard = next;

        Ok(PutOutcome {
            created: existing.is_none(),
            repo: def,
            warnings,
        })
    }

    /// Removes one entry and persists. The worktree under `<data-dir>/repos/<id>` is **not**
    /// touched here — deleting a user's files is `GitService::delete_repo`'s explicit
    /// `?purge=true` decision, not a side effect of editing the registry.
    pub fn remove(&self, id: &str) -> Result<RepoDef, GitError> {
        if !self.defaults.registry_writes {
            return Err(GitError::registry_read_only());
        }
        let mut guard = self.write();
        let index = guard
            .repos
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| GitError::repo_not_found(id))?;

        let mut next = guard.clone();
        let removed = next.repos.remove(index);
        save(&self.path, &next)?;
        *guard = next;
        Ok(removed)
    }
}

fn empty_file() -> RegistryFile {
    RegistryFile {
        version: REGISTRY_VERSION,
        repos: Vec::new(),
        rejected_raw: Vec::new(),
    }
}

/// Rename, never delete and never truncate: a hand-written file deserves better than "we
/// deleted it", and the author needs the bytes back to find their typo.
fn quarantine(
    path: &Path,
    repos_dir: &Path,
    defaults: RegistryDefaults,
    mut notes: Vec<String>,
    message: String,
) -> Registry {
    let stamp = now_ms();
    let mut target = path.with_extension(format!("json.corrupt-{stamp}"));
    // Two corruptions inside one millisecond would otherwise overwrite the first quarantine.
    let mut n = 1u32;
    while target.exists() {
        target = path.with_extension(format!("json.corrupt-{stamp}-{n}"));
        n += 1;
    }
    let quarantined_to = match std::fs::rename(path, &target) {
        Ok(()) => Some(target.display().to_string()),
        Err(e) => {
            notes.push(format!("cannot quarantine {}: {e}", path.display()));
            None
        }
    };
    let error = RegistryError {
        code: "registry_corrupt",
        message,
        quarantined_to,
        rejected: Vec::new(),
    };
    Registry::assembled(path, repos_dir, defaults, empty_file(), Some(error), notes)
}

/// `repos.json` can hold a personal access token, so group and other bits on it are a finding.
/// Tightening in place is silent-but-logged rather than fatal: refusing to start over a mode
/// bit would be worse than fixing it.
#[cfg(unix)]
fn tighten_mode(path: &Path, notes: &mut Vec<String>) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 == 0 {
        return;
    }
    match std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        Ok(()) => notes.push(format!("tightened {} from {mode:o} to 600", path.display())),
        Err(e) => notes.push(format!("cannot tighten {}: {e}", path.display())),
    }
}

/// Maps a validation failure onto the stable `rejected[].reason` vocabulary. `invalid_branch_name`
/// has no `GitErrorCode` of its own — it is an `invalid_request` on the `branch` field.
fn reject_reason(err: &GitError) -> &'static str {
    match err.code() {
        GitErrorCode::InvalidRepoId => "invalid_repo_id",
        GitErrorCode::InsecureRemote => "insecure_remote",
        GitErrorCode::SettingsSyncUnavailable => "settings_sync_unavailable",
        GitErrorCode::InvalidRequest if err.field.as_deref() == Some("branch") => {
            "invalid_branch_name"
        }
        _ => "invalid_request",
    }
}

/// Completes a definition in place. Shared by `load` and `put` so the two can never disagree
/// about what a normalised entry looks like.
fn normalise_def(def: &mut RepoDef, d: &RegistryDefaults, now: u64) -> Vec<Warning> {
    let mut warnings = Vec::new();

    if def.branch.is_empty() {
        def.branch = d.default_branch.clone();
    }
    if def.created_at_ms == 0 {
        def.created_at_ms = now;
    }
    if def.updated_at_ms == 0 {
        def.updated_at_ms = now;
    }

    // A hand-written credential may omit `bound_host`. Binding it to the remote it was written
    // next to is the only reading that is not a downgrade: an absent bound_host would otherwise
    // have to mean "send this token to whatever host the remote points at today".
    if let Some(cred) = def.credential.take() {
        let cred = if cred.is_none() || cred.bound_host().is_some() {
            cred
        } else {
            cred.with_bound_host(def.remote.as_deref().and_then(remote_host))
        };
        def.credential = Some(cred);
    }

    match def.auto_sync_secs {
        // 0 means off, and off is not a value to clamp.
        None | Some(0) => def.auto_sync_secs = None,
        Some(v) if v < AUTO_SYNC_MIN_SECS => {
            def.auto_sync_secs = Some(AUTO_SYNC_MIN_SECS);
            warnings.push(Warning {
                code: "auto_sync_clamped",
                message: format!(
                    "auto_sync_secs {v} is below the {AUTO_SYNC_MIN_SECS}s minimum and was raised \
                     to {AUTO_SYNC_MIN_SECS}"
                ),
            });
        }
        Some(v) if v > AUTO_SYNC_MAX_SECS => {
            def.auto_sync_secs = Some(AUTO_SYNC_MAX_SECS);
            warnings.push(Warning {
                code: "auto_sync_clamped",
                message: format!(
                    "auto_sync_secs {v} is above the {AUTO_SYNC_MAX_SECS}s maximum and was lowered \
                     to {AUTO_SYNC_MAX_SECS}"
                ),
            });
        }
        Some(_) => {}
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(json: &str) -> RepoDef {
        serde_json::from_str(json).expect("test fixture must parse")
    }

    fn defaults() -> RegistryDefaults {
        RegistryDefaults {
            default_branch: "main".to_string(),
            settings_enabled: false,
            allow_http: false,
            registry_writes: true,
        }
    }

    #[test]
    fn repo_def_parses_with_documented_defaults() {
        let d = def(r#"{"id":"notes"}"#);
        assert_eq!(d.id, "notes");
        assert_eq!(d.remote, None);
        assert_eq!(d.remote_name, "origin");
        // "" is the sentinel for "fill from [git].default_branch"; normalisation, not parsing,
        // resolves it, so `ctx.def.branch` can stay a plain &str at every call site.
        assert_eq!(d.branch, "");
        assert!(d.credential.is_none());
        assert!(d.author.is_none());
        assert!(!d.sync_settings);
        assert_eq!(d.settings_path, "settings.json");
        assert_eq!(d.auto_sync_secs, None);
        assert!(!d.sync_on_start);
        assert!(!d.sync_on_quit);
        assert!(!d.restart_children_on_pull);
        assert_eq!(d.created_at_ms, 0);
        assert_eq!(d.updated_at_ms, 0);
    }

    #[test]
    fn repo_def_rejects_an_unknown_key() {
        let e = serde_json::from_str::<RepoDef>(r#"{"id":"notes","whoops":true}"#)
            .expect_err("deny_unknown_fields must be on");
        assert!(e.to_string().contains("unknown field `whoops`"), "{e}");
    }

    #[test]
    fn credential_kinds_parse_and_never_print_their_secret() {
        let none = def(r#"{"id":"a","credential":{"kind":"none"}}"#);
        assert!(matches!(none.credential, Some(CredentialSpec::None)));

        let tok = def(r#"{"id":"a","credential":{"kind":"token","token":"ghp_SUPERSECRET"}}"#);
        match tok.credential {
            Some(CredentialSpec::Token {
                ref username,
                ref token,
                ref bound_host,
            }) => {
                assert_eq!(username, "x-access-token");
                assert_eq!(bound_host, &None);
                // Secret's Debug is the last line of defence against a stray dbg!/panic.
                assert_eq!(format!("{token:?}"), "Secret(***)");
            }
            other => panic!("expected a token credential, got {other:?}"),
        }
        assert!(!format!("{tok:?}").contains("ghp_SUPERSECRET"));

        let ssh = def(
            r#"{"id":"a","credential":{"kind":"ssh_key","private_key_path":"/k/id_ed25519",
                 "public_key":"ssh-ed25519 AAAA","bound_host":"github.com"}}"#,
        );
        match ssh.credential {
            Some(CredentialSpec::SshKey {
                ref username,
                ref private_key_path,
                ref public_key,
                ..
            }) => {
                assert_eq!(username, "git");
                assert_eq!(
                    private_key_path.as_deref(),
                    Some(std::path::Path::new("/k/id_ed25519"))
                );
                assert_eq!(public_key.as_deref(), Some("ssh-ed25519 AAAA"));
            }
            other => panic!("expected an ssh_key credential, got {other:?}"),
        }

        let e = serde_json::from_str::<CredentialSpec>(r#"{"kind":"token","token":"t","nope":1}"#)
            .expect_err("deny_unknown_fields must reach inside the variant");
        assert!(e.to_string().contains("unknown field `nope`"), "{e}");
        let e =
            serde_json::from_str::<CredentialSpec>(r#"{"kind":"weird"}"#).expect_err("bad kind");
        assert!(e.to_string().contains("unknown variant `weird`"), "{e}");
    }

    #[test]
    fn bound_host_helpers_round_trip() {
        let spec: CredentialSpec =
            serde_json::from_str(r#"{"kind":"token","token":"t"}"#).expect("parses");
        assert_eq!(spec.bound_host(), None);
        assert!(!spec.is_none());
        let bound = spec.with_bound_host(Some("github.com".to_string()));
        assert_eq!(bound.bound_host(), Some("github.com"));
        assert!(CredentialSpec::None.is_none());
        assert_eq!(CredentialSpec::None.bound_host(), None);
    }

    #[test]
    fn valid_ids_are_lowercase_and_filesystem_safe() {
        let longest = "n".repeat(MAX_REPO_ID_LEN);
        for good in [
            "notes",
            "a",
            "0",
            "my-repo",
            "my_repo",
            "app.v2",
            longest.as_str(),
        ] {
            assert!(valid_id(good), "rejected {good:?}");
        }
        for bad in [
            "",        // empty
            "Notes",   // uppercase: macOS and Windows would collide it with "notes"
            "-lead",   // must start alphanumeric
            ".hidden", //
            "../evil", // traversal
            "a..b",    // traversal, spelled differently
            "sub/dir", // one path segment only
            "sp ace", "trail.", // Windows silently strips a trailing dot
            "trail ", ".", "café", // ASCII only
            "con",  // Windows reserved device names
            "PRN", "aux", "nul", "com1", "LPT9",
        ] {
            assert!(!valid_id(bad), "accepted {bad:?}");
        }
        assert!(!valid_id(&"n".repeat(MAX_REPO_ID_LEN + 1)));
    }

    #[test]
    fn validate_id_reports_invalid_repo_id() {
        assert!(validate_id("notes").is_ok());
        let e = validate_id("../evil").expect_err("must reject");
        assert_eq!(e.code(), GitErrorCode::InvalidRepoId);
        assert!(e.to_string().contains("../evil"), "{e}");
    }

    #[test]
    fn remote_host_is_lowercased_and_scp_aware() {
        for (remote, want) in [
            ("https://github.com/acme/notes.git", Some("github.com")),
            ("https://user@Example.COM:8443/x.git", Some("example.com")),
            ("https://token:pw@evil.example/x.git", Some("evil.example")),
            ("ssh://git@github.com/acme/x.git", Some("github.com")),
            ("git@github.com:acme/notes.git", Some("github.com")),
            ("github.com:acme/notes.git", Some("github.com")),
            ("https://[::1]:8443/x.git", Some("[::1]")),
            // No network host: nothing to bind a credential to, so `None` fails closed.
            ("file:///srv/repos/x.git", None),
            ("/srv/repos/x.git", None),
            ("C:\\repos\\notes", None),
            ("", None),
        ] {
            assert_eq!(
                remote_host(remote).as_deref(),
                want,
                "remote_host({remote:?})"
            );
        }
    }

    #[test]
    fn validate_remote_accepts_the_documented_forms() {
        for good in [
            "https://github.com/acme/notes.git",
            "ssh://git@github.com/acme/x.git",
            "git://git.example.org/x.git",
            "file:///srv/repos/x.git",
            "git@github.com:acme/notes.git",
            "/srv/repos/x.git",
        ] {
            assert!(validate_remote(good, false).is_ok(), "rejected {good:?}");
        }
    }

    #[test]
    fn validate_remote_refuses_http_and_passwords() {
        let e = validate_remote("http://intranet/x.git", false).expect_err("http is refused");
        assert_eq!(e.code(), GitErrorCode::InsecureRemote);
        assert!(e.to_string().contains("allow_http"), "{e}");
        assert!(validate_remote("http://intranet/x.git", true).is_ok());

        // libgit2 quotes the URL into its error strings, so a password in the remote would be
        // copied into git.log and into every API error body.
        let e = validate_remote("https://user:hunter2@github.com/x.git", false)
            .expect_err("password is refused");
        assert_eq!(e.code(), GitErrorCode::InsecureRemote);
        let e = validate_remote("git@github.com", false).expect_err("no path, not scp form");
        assert_eq!(e.code(), GitErrorCode::InvalidRequest);
        let e = validate_remote("wat://github.com/x.git", false).expect_err("unknown scheme");
        assert_eq!(e.code(), GitErrorCode::InvalidRequest);
        let e = validate_remote("relative/path", false).expect_err("relative path");
        assert_eq!(e.code(), GitErrorCode::InvalidRequest);
        let e = validate_remote("", false).expect_err("empty");
        assert_eq!(e.code(), GitErrorCode::InvalidRequest);
    }

    #[test]
    fn settings_path_must_stay_inside_the_tree() {
        assert!(validate_settings_path("settings.json").is_ok());
        assert!(validate_settings_path("config/app/settings.json").is_ok());
        for bad in [
            "",
            "/etc/passwd",
            "../settings.json",
            "a/../../b.json",
            "config\\settings.json",
            "config//settings.json",
            "config/",
        ] {
            let e = validate_settings_path(bad).expect_err("must reject");
            assert_eq!(e.code(), GitErrorCode::InvalidRequest, "{bad:?}");
            assert_eq!(e.field.as_deref(), Some("settings_path"), "{bad:?}");
        }
    }

    #[test]
    fn validate_def_accepts_a_minimal_entry_and_the_full_example() {
        assert!(validate_def(&def(r#"{"id":"notes"}"#), &defaults()).is_ok());
        let full = def(
            r#"{"id":"notes","remote":"https://github.com/acme/notes.git","remote_name":"origin",
                "branch":"main","author":{"name":"Notes App","email":"notes@acme.dev"},
                "settings_path":"settings.json","auto_sync_secs":300,"sync_on_start":true,
                "created_at_ms":1785000000000,"updated_at_ms":1785000000000}"#,
        );
        assert!(validate_def(&full, &defaults()).is_ok());
    }

    #[test]
    fn validate_def_reports_the_offending_field() {
        let cases: [(&str, GitErrorCode, Option<&str>); 6] = [
            (r#"{"id":"../evil"}"#, GitErrorCode::InvalidRepoId, None),
            (
                r#"{"id":"a","branch":"bad..branch"}"#,
                GitErrorCode::InvalidRequest,
                Some("branch"),
            ),
            (
                r#"{"id":"a","remote_name":"or igin"}"#,
                GitErrorCode::InvalidRequest,
                Some("remote_name"),
            ),
            (
                r#"{"id":"a","author":{"name":"A <b>"}}"#,
                GitErrorCode::InvalidRequest,
                Some("author.name"),
            ),
            (
                r#"{"id":"a","author":{"email":"not an address"}}"#,
                GitErrorCode::InvalidRequest,
                Some("author.email"),
            ),
            (
                r#"{"id":"a","credential":{"kind":"ssh_key"}}"#,
                GitErrorCode::InvalidRequest,
                Some("credential.private_key"),
            ),
        ];
        for (json, code, field) in cases {
            let e = validate_def(&def(json), &defaults()).expect_err("must reject");
            assert_eq!(e.code(), code, "{json}");
            assert_eq!(e.field.as_deref(), field, "{json}");
        }
        // Exactly one key source, not both.
        let both = def(
            r#"{"id":"a","credential":{"kind":"ssh_key","private_key_path":"/k","private_key":"k"}}"#,
        );
        assert_eq!(
            validate_def(&both, &defaults())
                .expect_err("both sources")
                .field
                .as_deref(),
            Some("credential.private_key")
        );
    }

    #[test]
    fn sync_settings_without_a_schema_is_refused() {
        // A silently no-op data-sync feature is how people lose data.
        let d = def(r#"{"id":"a","sync_settings":true}"#);
        let e = validate_def(&d, &defaults()).expect_err("no schema configured");
        assert_eq!(e.code(), GitErrorCode::SettingsSyncUnavailable);
        let mut with_schema = defaults();
        with_schema.settings_enabled = true;
        assert!(validate_def(&d, &with_schema).is_ok());
    }

    #[test]
    fn an_empty_branch_is_validated_against_the_default() {
        // "" means "use [git].default_branch", so the effective name is what must be legal.
        let d = def(r#"{"id":"a"}"#);
        assert!(validate_def(&d, &defaults()).is_ok());
        let mut bad_default = defaults();
        bad_default.default_branch = "..".to_string();
        assert_eq!(
            validate_def(&d, &bad_default)
                .expect_err("bad default")
                .field
                .as_deref(),
            Some("branch")
        );
    }

    #[test]
    fn redaction_never_emits_a_secret() {
        let dir = tempfile::tempdir().expect("tempdir");
        for cred in [
            r#"{"kind":"none"}"#,
            r#"{"kind":"token","token":"ghp_SUPERSECRET","bound_host":"github.com"}"#,
            r#"{"kind":"ssh_key","private_key":"ghp_SUPERSECRET","passphrase":"ghp_SUPERSECRET",
                 "public_key":"ssh-ed25519 AAAA","bound_host":"github.com"}"#,
        ] {
            let d = def(&format!(r#"{{"id":"notes","credential":{cred}}}"#));
            let view = RepoView::new(&d, dir.path());
            let json = serde_json::to_string(&view).expect("view serializes");
            assert!(
                !json.contains("ghp_SUPERSECRET"),
                "leaked through RepoView: {json}"
            );
            assert!(
                !format!("{d:?}").contains("ghp_SUPERSECRET"),
                "leaked through Debug"
            );
        }
    }

    #[test]
    fn repo_view_reports_stored_credentials_without_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repos_dir = dir.path().join("repos");
        std::fs::create_dir_all(repos_dir.join("notes")).expect("tree");

        let d = def(
            r#"{"id":"notes","remote":"https://github.com/acme/notes.git","branch":"main",
                "credential":{"kind":"token","token":"ghp_SUPERSECRET","bound_host":"github.com"},
                "auto_sync_secs":300,"created_at_ms":7,"updated_at_ms":8}"#,
        );
        let v = RepoView::new(&d, &repos_dir);
        assert_eq!(v.id, "notes");
        assert_eq!(v.path, repos_dir.join("notes").display().to_string());
        assert!(v.exists);
        assert!(v.has_credentials);
        assert_eq!(v.auto_sync_secs, Some(300));
        assert_eq!(v.created_at_ms, 7);
        assert_eq!(v.updated_at_ms, 8);
        match v.credential {
            Some(CredentialView::Token {
                ref username,
                token_stored,
                ref bound_host,
            }) => {
                assert_eq!(username, "x-access-token");
                assert!(token_stored);
                assert_eq!(bound_host.as_deref(), Some("github.com"));
            }
            ref other => panic!("expected a token view, got {other:?}"),
        }

        // A missing tree is reported, not hidden: `exists: false` is how a UI knows to clone.
        let absent = RepoView::new(&def(r#"{"id":"other"}"#), &repos_dir);
        assert!(!absent.exists);
        assert!(!absent.has_credentials);
        assert!(absent.credential.is_none());

        // `kind: "none"` is a credential in the file but not a credential in effect.
        let explicit_none = RepoView::new(
            &def(r#"{"id":"n","credential":{"kind":"none"}}"#),
            &repos_dir,
        );
        assert!(!explicit_none.has_credentials);
        assert!(matches!(
            explicit_none.credential,
            Some(CredentialView::None)
        ));
    }

    #[test]
    fn ssh_credential_view_shows_paths_but_never_material() {
        let d = def(
            r#"{"id":"a","credential":{"kind":"ssh_key","username":"git",
                 "private_key_path":"/home/u/.ssh/id_ed25519",
                 "public_key_path":"/home/u/.ssh/id_ed25519.pub",
                 "passphrase":"ghp_SUPERSECRET","bound_host":"github.com"}}"#,
        );
        match CredentialView::of(d.credential.as_ref().expect("credential")) {
            CredentialView::SshKey {
                username,
                private_key_path,
                private_key_stored,
                public_key_path,
                passphrase_stored,
                bound_host,
            } => {
                assert_eq!(username, "git");
                // Paths are not secrets, and hiding them makes misconfiguration undebuggable.
                assert_eq!(private_key_path.as_deref(), Some("/home/u/.ssh/id_ed25519"));
                assert_eq!(
                    public_key_path.as_deref(),
                    Some("/home/u/.ssh/id_ed25519.pub")
                );
                assert!(!private_key_stored);
                assert!(passphrase_stored);
                assert_eq!(bound_host.as_deref(), Some("github.com"));
            }
            other => panic!("expected an ssh_key view, got {other:?}"),
        }
    }

    fn file_of(defs: &[&str]) -> RegistryFile {
        RegistryFile {
            version: REGISTRY_VERSION,
            repos: defs.iter().copied().map(def).collect(),
            rejected_raw: Vec::new(),
        }
    }

    #[test]
    fn save_writes_the_token_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("repos.json");
        let file = file_of(&[
            r#"{"id":"notes","remote":"https://github.com/acme/notes.git","branch":"main",
                "credential":{"kind":"token","token":"ghp_SUPERSECRET","bound_host":"github.com"},
                "created_at_ms":7,"updated_at_ms":8}"#,
        ]);
        save(&path, &file).expect("save");

        let text = std::fs::read_to_string(&path).expect("read back");
        // The registry is the credential store; the token belongs on disk, redaction is for
        // the HTTP surface. If this ever fails, `to_wire` stopped persisting credentials.
        assert!(text.contains("ghp_SUPERSECRET"), "{text}");
        assert!(
            !dir.path().join("repos.json.tmp").exists(),
            "temp file survived"
        );

        let back: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(back["version"], serde_json::json!(REGISTRY_VERSION));
        assert_eq!(back["repos"][0]["id"], serde_json::json!("notes"));
        assert_eq!(back["repos"][0]["remote_name"], serde_json::json!("origin"));
        assert_eq!(
            back["repos"][0]["credential"]["kind"],
            serde_json::json!("token")
        );
        assert_eq!(back["repos"][0]["auto_sync_secs"], serde_json::Value::Null);

        // Round-trips through the parser it will be read back with.
        let reparsed: RepoDef =
            serde_json::from_value(back["repos"][0].clone()).expect("round trip");
        assert_eq!(reparsed.id, "notes");
        assert_eq!(reparsed.created_at_ms, 7);
    }

    #[test]
    fn save_replaces_a_stale_temp_file_and_overwrites_atomically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("repos.json");
        // A crash between sync_all and rename leaves this behind; create_new would fail on it.
        std::fs::write(dir.path().join("repos.json.tmp"), "junk").expect("stale tmp");
        save(&path, &file_of(&[r#"{"id":"a"}"#])).expect("first save");
        save(&path, &file_of(&[r#"{"id":"b"}"#])).expect("second save over an existing file");
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains("\"b\""), "{text}");
        assert!(!text.contains("\"a\""), "{text}");
        assert!(!dir.path().join("repos.json.tmp").exists());
    }

    #[test]
    fn save_reports_registry_write_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("no-such-dir").join("repos.json");
        let e = save(&path, &file_of(&[])).expect_err("parent directory does not exist");
        assert_eq!(e.code(), GitErrorCode::RegistryWriteFailed);
    }

    #[test]
    fn rejected_raw_entries_are_written_back_verbatim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("repos.json");
        let mut file = file_of(&[r#"{"id":"good"}"#]);
        file.rejected_raw
            .push(serde_json::json!({ "id": "typo", "whoops": true }));
        save(&path, &file).expect("save");
        let back: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        let repos = back["repos"].as_array().expect("array");
        assert_eq!(repos.len(), 2);
        assert_eq!(
            repos[1],
            serde_json::json!({ "id": "typo", "whoops": true })
        );
    }

    #[cfg(unix)]
    #[test]
    fn saved_registry_is_not_readable_by_group_or_other() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("repos.json");
        save(&path, &file_of(&[r#"{"id":"a"}"#])).expect("save");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "mode was {:o}", mode & 0o777);
    }

    fn load_at(dir: &std::path::Path) -> Registry {
        Registry::load(&dir.join("repos.json"), &dir.join("repos"), defaults())
    }

    #[test]
    fn a_missing_file_loads_an_empty_writable_registry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = load_at(dir.path());
        assert_eq!(reg.count(), 0);
        assert!(reg.error().is_none());
        assert!(reg.writable());
        assert!(reg.ids().is_empty());
        assert!(!reg.contains("notes"));
        assert_eq!(reg.path(), dir.path().join("repos.json"));
        assert_eq!(reg.repos_dir(), dir.path().join("repos"));
        // A host with [git] present and no repos never creates the file.
        assert!(!dir.path().join("repos.json").exists());
    }

    #[test]
    fn a_well_formed_file_loads_and_is_normalised_in_memory_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("repos.json");
        let original = r#"{"version":1,"repos":[
            {"id":"notes","remote":"https://GitHub.com/acme/notes.git",
             "credential":{"kind":"token","token":"ghp_SUPERSECRET"},
             "auto_sync_secs":5}
        ]}"#;
        std::fs::write(&path, original).expect("write");

        let reg = load_at(dir.path());
        assert!(reg.error().is_none());
        let d = reg.snapshot("notes").expect("present");
        assert_eq!(
            d.branch, "main",
            "empty branch filled from [git].default_branch"
        );
        assert_ne!(d.created_at_ms, 0, "timestamps stamped on load");
        assert_ne!(d.updated_at_ms, 0);
        assert_eq!(d.auto_sync_secs, Some(AUTO_SYNC_MIN_SECS), "5s clamped up");
        assert_eq!(
            d.credential.as_ref().and_then(CredentialSpec::bound_host),
            Some("github.com"),
            "a hand-written credential is bound to the remote it sits next to"
        );
        assert_eq!(reg.auto_sync_secs("notes"), Some(AUTO_SYNC_MIN_SECS));
        assert_eq!(reg.auto_sync_secs("absent"), None);
        assert_eq!(reg.list().len(), 1);
        assert_eq!(
            reg.snapshot("absent").expect_err("absent").code(),
            GitErrorCode::RepoNotFound
        );

        // Normalisation is in memory. Loading must never rewrite the author's file — an
        // updated_at_ms bump on every launch would make repos.json unusable in version control.
        assert_eq!(std::fs::read_to_string(&path).expect("read"), original);
    }

    #[test]
    fn a_stale_temp_file_is_removed_at_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp = dir.path().join("repos.json.tmp");
        std::fs::write(dir.path().join("repos.json"), r#"{"version":1,"repos":[]}"#)
            .expect("write");
        std::fs::write(&tmp, "half-written").expect("write tmp");
        let reg = load_at(dir.path());
        assert!(!tmp.exists(), "the crash leftover must be cleaned up");
        assert!(
            reg.notes().iter().any(|n| n.contains("repos.json.tmp")),
            "{:?}",
            reg.notes()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_loose_mode_is_tightened_at_load() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("repos.json");
        std::fs::write(&path, r#"{"version":1,"repos":[]}"#).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let reg = load_at(dir.path());
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "mode was {mode:o}");
        assert!(
            reg.notes().iter().any(|n| n.contains("tightened")),
            "{:?}",
            reg.notes()
        );
    }

    #[test]
    fn invalid_json_is_quarantined_with_the_original_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("repos.json");
        let original = "{ this is not json";
        std::fs::write(&path, original).expect("write");

        let reg = load_at(dir.path());
        assert_eq!(reg.count(), 0, "the host still starts");
        assert!(
            !path.exists(),
            "repos.json must be renamed away, never left in place"
        );
        let err = reg.error().expect("registry_error");
        assert_eq!(err.code, "registry_corrupt");
        assert!(err.rejected.is_empty());
        let quarantine = err.quarantined_to.expect("quarantined_to");
        assert!(quarantine.contains("repos.json.corrupt-"), "{quarantine}");
        // Never deleted, never truncated: the author gets their file back.
        assert_eq!(
            std::fs::read_to_string(&quarantine).expect("read"),
            original
        );
    }

    #[test]
    fn file_level_failures_are_all_quarantined() {
        for body in [
            r#"{"version":2,"repos":[]}"#,              // wrong version
            r#"[]"#,                                    // top level not an object
            r#"{"version":1}"#,                         // no repos array
            r#"{"version":1,"repos":{}}"#,              // repos not an array
            r#"{"version":1,"repos":[],"extra":true}"#, // unknown top-level key
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::write(dir.path().join("repos.json"), body).expect("write");
            let reg = load_at(dir.path());
            let err = reg.error().unwrap_or_else(|| panic!("no error for {body}"));
            assert_eq!(err.code, "registry_corrupt", "{body}");
            assert!(err.quarantined_to.is_some(), "{body}");
            assert_eq!(reg.count(), 0, "{body}");
        }
    }

    #[test]
    fn bad_entries_are_skipped_and_the_good_ones_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("repos.json"),
            r#"{"version":1,"repos":[
                {"id":"good"},
                {"id":"../evil"},
                {"id":"typo","whoops":true},
                {"id":"cfg","sync_settings":true},
                {"id":"branchy","branch":"bad..branch"},
                {"id":"leaky","remote":"http://intranet/x.git"},
                {"id":"good"}
            ]}"#,
        )
        .expect("write");

        let reg = load_at(dir.path());
        assert_eq!(reg.ids(), vec!["good".to_string()]);
        let err = reg.error().expect("registry_error");
        assert_eq!(err.code, "registry_entries_rejected");
        assert!(
            err.quarantined_to.is_none(),
            "entry failures never quarantine the file"
        );
        assert_eq!(err.message, "6 of 7 repo definitions were rejected");
        assert_eq!(
            err.rejected,
            vec![
                RejectedEntry {
                    index: 1,
                    id: Some("../evil".to_string()),
                    reason: "invalid_repo_id"
                },
                RejectedEntry {
                    index: 2,
                    id: Some("typo".to_string()),
                    reason: "invalid_request"
                },
                RejectedEntry {
                    index: 3,
                    id: Some("cfg".to_string()),
                    reason: "settings_sync_unavailable"
                },
                RejectedEntry {
                    index: 4,
                    id: Some("branchy".to_string()),
                    reason: "invalid_branch_name"
                },
                RejectedEntry {
                    index: 5,
                    id: Some("leaky".to_string()),
                    reason: "insecure_remote"
                },
                // The first entry with an id wins; the later one is the duplicate.
                RejectedEntry {
                    index: 6,
                    id: Some("good".to_string()),
                    reason: "duplicate_id"
                },
            ]
        );
    }

    #[test]
    fn an_unreadable_id_is_reported_as_null() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("repos.json"),
            r#"{"version":1,"repos":[{"id":42},"nonsense"]}"#,
        )
        .expect("write");
        let reg = load_at(dir.path());
        let err = reg.error().expect("registry_error");
        assert_eq!(err.code, "registry_entries_rejected");
        assert_eq!(err.rejected.len(), 2);
        assert_eq!(err.rejected[0].id, None);
        assert_eq!(err.rejected[0].reason, "invalid_request");
        assert_eq!(err.rejected[1].id, None);
    }

    #[test]
    fn a_missing_private_key_path_is_accepted() {
        // An unplugged USB key must surface as a per-job auth failure, not brick the registry.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("repos.json"),
            r#"{"version":1,"repos":[{"id":"notes","remote":"git@github.com:acme/notes.git",
                 "credential":{"kind":"ssh_key","private_key_path":"/nope/does/not/exist"}}]}"#,
        )
        .expect("write");
        let reg = load_at(dir.path());
        assert!(reg.error().is_none(), "{:?}", reg.error());
        assert!(reg.contains("notes"));
    }

    #[test]
    fn a_read_only_registry_reports_itself_as_such() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut d = defaults();
        d.registry_writes = false;
        let reg = Registry::load(&dir.path().join("repos.json"), &dir.path().join("repos"), d);
        assert!(!reg.writable());
    }

    #[test]
    fn put_creates_then_updates_and_persists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("repos.json");
        let reg = load_at(dir.path());

        let first = reg.put(def(r#"{"id":"notes"}"#)).expect("create");
        assert!(first.created);
        assert!(first.warnings.is_empty());
        assert_eq!(first.repo.branch, "main");
        assert_ne!(first.repo.created_at_ms, 0);
        assert!(path.exists(), "the first PUT is what creates repos.json");

        let second = reg
            .put(def(r#"{"id":"notes","sync_on_start":true}"#))
            .expect("update");
        assert!(!second.created);
        assert!(second.repo.sync_on_start);
        assert_eq!(
            second.repo.created_at_ms, first.repo.created_at_ms,
            "created_at_ms is preserved across updates"
        );
        assert!(second.repo.updated_at_ms >= first.repo.updated_at_ms);
        assert_eq!(reg.count(), 1);

        // Survives a reload, which is the only thing that proves it reached the disk.
        let reloaded = load_at(dir.path());
        assert!(reloaded.snapshot("notes").expect("present").sync_on_start);
        assert!(reloaded.error().is_none());
    }

    #[test]
    fn put_refuses_an_invalid_definition_without_touching_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = load_at(dir.path());
        let e = reg
            .put(def(r#"{"id":"../evil"}"#))
            .expect_err("bad id must not be stored");
        assert_eq!(e.code(), GitErrorCode::InvalidRepoId);
        let e = reg
            .put(def(r#"{"id":"a","branch":"bad..branch"}"#))
            .expect_err("bad branch");
        assert_eq!(e.code(), GitErrorCode::InvalidRequest);
        assert_eq!(reg.count(), 0);
        assert!(!dir.path().join("repos.json").exists());
    }

    #[test]
    fn put_is_refused_when_registry_writes_is_off() {
        // registry_writes = false means repos.json is author-owned; the API must not edit it.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut d = defaults();
        d.registry_writes = false;
        let reg = Registry::load(&dir.path().join("repos.json"), &dir.path().join("repos"), d);
        let e = reg.put(def(r#"{"id":"notes"}"#)).expect_err("read only");
        assert_eq!(e.code(), GitErrorCode::RegistryReadOnly);
        assert!(!dir.path().join("repos.json").exists());
    }

    #[test]
    fn a_failed_write_leaves_memory_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The parent directory does not exist, so the temp file cannot be created.
        let reg = Registry::load(
            &dir.path().join("nope").join("repos.json"),
            &dir.path().join("repos"),
            defaults(),
        );
        let e = reg.put(def(r#"{"id":"notes"}"#)).expect_err("cannot write");
        assert_eq!(e.code(), GitErrorCode::RegistryWriteFailed);
        // An in-memory registry that disagrees with repos.json would silently un-persist
        // itself at the next restart, which is worse than a visible 500.
        assert_eq!(reg.count(), 0);
        assert!(!reg.contains("notes"));
    }

    #[test]
    fn rejected_entries_survive_a_later_put() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("repos.json");
        std::fs::write(
            &path,
            r#"{"version":1,"repos":[{"id":"good"},{"id":"../evil"},{"id":"typo","whoops":true}]}"#,
        )
        .expect("write");
        let reg = load_at(dir.path());
        assert_eq!(reg.ids(), vec!["good".to_string()]);

        reg.put(def(r#"{"id":"good","sync_on_quit":true}"#))
            .expect("put");

        let back: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        let repos = back["repos"].as_array().expect("array");
        assert_eq!(
            repos.len(),
            3,
            "the rejected entries were written back: {repos:?}"
        );
        assert!(repos
            .iter()
            .any(|r| r["id"] == serde_json::json!("../evil")));
        assert!(repos.iter().any(|r| r["whoops"] == serde_json::json!(true)));
    }

    fn token_def(id: &str, remote: &str) -> RepoDef {
        def(&format!(
            r#"{{"id":"{id}","remote":"{remote}",
                 "credential":{{"kind":"token","token":"ghp_SUPERSECRET"}}}}"#
        ))
    }

    #[test]
    fn put_binds_a_new_credential_to_its_remote() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = load_at(dir.path());
        let out = reg
            .put(token_def("notes", "https://GitHub.com/acme/notes.git"))
            .expect("put");
        assert_eq!(
            out.repo
                .credential
                .as_ref()
                .and_then(CredentialSpec::bound_host),
            Some("github.com")
        );
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn put_carries_a_stored_credential_forward() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = load_at(dir.path());
        reg.put(token_def("notes", "https://github.com/acme/notes.git"))
            .expect("first put");

        // No credential in the body: an edit to an unrelated field must not require the caller
        // to re-send the token over the loopback socket.
        let out = reg
            .put(def(
                r#"{"id":"notes","remote":"https://github.com/acme/notes.git","sync_on_start":true}"#,
            ))
            .expect("second put");
        assert!(out.repo.credential.is_some());
        assert!(out.warnings.is_empty());
        assert_eq!(
            out.repo
                .credential
                .as_ref()
                .and_then(CredentialSpec::bound_host),
            Some("github.com")
        );
    }

    #[test]
    fn put_drops_a_credential_whose_host_changed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = load_at(dir.path());
        reg.put(token_def("notes", "https://github.com/acme/notes.git"))
            .expect("first put");

        let out = reg
            .put(def(
                r#"{"id":"notes","remote":"https://evil.example/acme/notes.git"}"#,
            ))
            .expect("repointed put");
        assert!(
            out.repo.credential.is_none(),
            "a repointed remote must not inherit the old host's token"
        );
        assert_eq!(out.warnings.len(), 1);
        assert_eq!(out.warnings[0].code, "auth_unbound");
        assert!(
            out.warnings[0].message.contains("github.com"),
            "{:?}",
            out.warnings[0]
        );

        // And the token is really gone from disk.
        let text = std::fs::read_to_string(dir.path().join("repos.json")).expect("read");
        assert!(!text.contains("ghp_SUPERSECRET"), "{text}");
    }

    #[test]
    fn put_replaces_rather_than_merges_a_credential() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = load_at(dir.path());
        reg.put(token_def("notes", "https://github.com/acme/notes.git"))
            .expect("first put");
        // An explicit `kind: "none"` is how a caller turns a stored credential off.
        let out = reg
            .put(def(
                r#"{"id":"notes","remote":"https://github.com/acme/notes.git",
                     "credential":{"kind":"none"}}"#,
            ))
            .expect("put none");
        assert!(matches!(out.repo.credential, Some(CredentialSpec::None)));
        let text = std::fs::read_to_string(dir.path().join("repos.json")).expect("read");
        assert!(!text.contains("ghp_SUPERSECRET"), "{text}");
    }

    #[test]
    fn put_clamps_auto_sync_secs_and_says_so() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = load_at(dir.path());

        let low = reg
            .put(def(r#"{"id":"a","auto_sync_secs":5}"#))
            .expect("put");
        assert_eq!(low.repo.auto_sync_secs, Some(AUTO_SYNC_MIN_SECS));
        assert_eq!(low.warnings[0].code, "auto_sync_clamped");

        let high = reg
            .put(def(r#"{"id":"b","auto_sync_secs":999999}"#))
            .expect("put");
        assert_eq!(high.repo.auto_sync_secs, Some(AUTO_SYNC_MAX_SECS));
        assert_eq!(high.warnings[0].code, "auto_sync_clamped");

        // 0 is "off", not a value to clamp, and produces no warning.
        let off = reg
            .put(def(r#"{"id":"c","auto_sync_secs":0}"#))
            .expect("put");
        assert_eq!(off.repo.auto_sync_secs, None);
        assert!(off.warnings.is_empty());

        let fine = reg
            .put(def(r#"{"id":"d","auto_sync_secs":300}"#))
            .expect("put");
        assert_eq!(fine.repo.auto_sync_secs, Some(300));
        assert!(fine.warnings.is_empty());
    }

    #[test]
    fn remove_deletes_the_entry_and_persists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = load_at(dir.path());
        reg.put(def(r#"{"id":"notes","sync_on_start":true}"#))
            .expect("put");
        reg.put(def(r#"{"id":"other"}"#)).expect("put");

        let removed = reg.remove("notes").expect("remove");
        assert_eq!(removed.id, "notes");
        assert!(
            removed.sync_on_start,
            "the removed definition is returned in full"
        );
        assert_eq!(reg.ids(), vec!["other".to_string()]);

        let reloaded = load_at(dir.path());
        assert_eq!(reloaded.ids(), vec!["other".to_string()]);
    }

    #[test]
    fn remove_reports_repo_not_found_and_read_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = load_at(dir.path());
        assert_eq!(
            reg.remove("nope").expect_err("absent").code(),
            GitErrorCode::RepoNotFound
        );

        let mut d = defaults();
        d.registry_writes = false;
        let ro = Registry::load(&dir.path().join("repos.json"), &dir.path().join("repos"), d);
        assert_eq!(
            ro.remove("anything").expect_err("read only").code(),
            GitErrorCode::RegistryReadOnly,
            "read-only is checked before existence, so a probe cannot enumerate ids"
        );
    }
}
