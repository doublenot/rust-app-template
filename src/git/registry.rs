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
}
