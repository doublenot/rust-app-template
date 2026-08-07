### Task 4: Registry and state files

Two leaf modules, both pure `std` + `serde`. `src/git/registry.rs` owns `repos.json` — the
hand-writable, credential-bearing list of repository *definitions*. `src/git/state.rs` owns
`git-state.json` — the host-written cache of *volatile* facts (last sync outcome, learned SSH
fingerprint). Splitting them is the whole point: the definitions file is written only by `PUT`
and `DELETE`, so a repo syncing every 30 s never rewrites the file that holds its token, and the
author can keep `repos.json` under version control without it churning.

Neither file may name `git2`, `axum`, `tokio`, `App`, `Children` or `HostEvent`. If you find
yourself wanting any of them, the code belongs in a later slice.

> **Amended by the post-execution audit (F2, F3, F4).** Four blocks in this task shipped defects
> and have been **corrected in place**; the amendment notes at each site say what they used to be.
> Read them before re-executing, because two of the four are security defects and the old code is
> the kind that looks right:
> - **Step 26's `Registry::load`** read with `read_to_string`, which collapses "could not read the
>   file" and "the file is not UTF-8" into one `io::Error` — and those two need opposite answers.
>   Now `fs::read` + `String::from_utf8`, and a third load tier (below).
> - **Step 27's `assembled`, `writable` and `quarantine`**, plus the `put`/`remove` preambles:
>   the scrub at the funnel (F4), and the write latch with `write_block`/`blocks_writes`/
>   `ensure_writable` (F3).
> - **Step 43's carry-forward predicate** treated an absent `bound_host` as a wildcard — a
>   credential-theft primitive. Now a plain equality including `None`.
> - **`StateStore::load`** got the same one-line scrub as `assembled`.

**The load-bearing decisions in this task:**

1. **Tolerant load, three tiers.** A file that fails at the *file* level with its bytes in hand
   (not UTF-8, bad JSON, wrong `version`, `repos` not an array, an unknown top-level key) is
   **renamed** to `repos.json.corrupt-<epoch_ms>` — never deleted, never truncated — and the host
   starts with an empty, still-writable registry. A file we could **not read at all**, or one
   whose quarantine rename failed, is the third tier: it is left exactly where it is and every
   registry write is refused until it can be read or moved aside. Renaming a file we never read is
   a bigger act than refusing to write one — one EACCES from a mis-chmod would otherwise retire a
   registry full of definitions and stored PATs on the first `PUT` after boot, with no `.corrupt-`
   copy to recover from. A file that parses but contains a *bad entry* keeps its good entries; the bad
   ones are skipped, reported, **and retained verbatim as `serde_json::Value` so that every later
   rewrite writes them back unchanged**. Fixing one typo must never cost the author the rest of
   their file. Either way the host starts normally: an optional feature must not brick the app.
2. **Redaction is structural, not disciplinary.** `Secret` has no `Serialize`, so `RepoDef` cannot
   derive `Serialize` — the only path from a stored token to JSON is the hand-written
   `to_wire` (to disk) or `CredentialView` (to HTTP), and `CredentialView` has no
   secret-shaped field to forget to skip. Step 21 is the canary that proves it. The *text* this
   module produces needs the same treatment for the same reason, and it does not get it from
   `GitError`: every note and every `RegistryError` is `format!`ed over the contents of
   `repos.json` and goes into a log file created under the ordinary umask. `Registry::assembled`
   is the single funnel all of them pass through, so the scrub lives there rather than at eleven
   return paths.
3. **The temp file gets mode `0600` at creation, before a single byte is written.** Writing then
   chmod-ing leaves a window in which the token is world-readable. `create_new(true).mode(0o600)`
   closes it, and `sync_all()` before the `rename` means a power cut can leave the old file or
   the new one but never an empty one.

**Two places where this task deliberately departs from the spec prose** (both follow the
contract, which is authoritative):

- §4.4 lists *duplicate ids* as a file-level failure, but contract §3.12 lists `duplicate_id` as a
  `rejected[].reason`. Only the latter can be true of a single input, and a stable string that
  nothing can ever emit is a contract violation. **Duplicates are entry-level:** the first entry
  with a given id wins, each later one is rejected with `reason: "duplicate_id"`.
- §4.4 lists an out-of-range `auto_sync_secs` as an entry-level rejection, but contract §5.4 makes
  `Registry::put` responsible for *clamping* it into `AUTO_SYNC_MIN_SECS..=AUTO_SYNC_MAX_SECS`
  with a `Warning{code:"auto_sync_clamped"}`. If `validate_def` rejected the value, `put` could
  never reach the clamp. **Out-of-range clamps, it does not reject** — in `put` and, through the
  same shared normaliser, at load.

**Files:**
- Create: `src/git/registry.rs` (~640 lines including tests)
- Create: `src/git/state.rs` (~250 lines including tests)
- Modify: `src/git/mod.rs:1-8` — add `pub mod registry;` and `pub mod state;` to the module block
  at the top of the file, keeping it alphabetical. After task 3 that block reads
  `pub mod error;` / `pub mod secret;` / `pub mod util;`; after this task it reads
  `pub mod error;` / `pub mod registry;` / `pub mod secret;` / `pub mod state;` / `pub mod util;`.

**Interfaces:**

- **Consumes** (hard prerequisites — this task does not compile without tasks 2 and 3):

```rust
// src/config.rs — task 2
pub fn validate_branch_name(name: &str) -> bool;

// src/git/secret.rs — task 3
pub struct Secret(String);            // Clone + PartialEq + Eq + Deserialize(transparent)
impl Secret { pub fn expose_for_disk(&self) -> &str; }
impl std::fmt::Debug for Secret;      // writes exactly `Secret(***)`; no Serialize, no Display

// src/git/error.rs — task 3
pub enum GitErrorCode { InvalidRepoId, InvalidRequest, InsecureRemote, SettingsSyncUnavailable,
                        RepoNotFound, RegistryReadOnly, RegistryWriteFailed, /* …and the rest */ }
pub struct GitError { pub code: GitErrorCode, pub message: String, pub repo_id: Option<String>,
                      pub path: Option<String>, pub field: Option<String>, pub git2: Option<Git2Hint> }
impl GitError {
    pub fn code(&self) -> GitErrorCode;
    pub fn invalid_request(field: &str, message: impl Into<String>) -> GitError;
    pub fn invalid_repo_id(id: &str) -> GitError;
    pub fn insecure_remote(remote: &str, why: &str) -> GitError;
    pub fn repo_not_found(id: &str) -> GitError;
    pub fn registry_read_only() -> GitError;
    pub fn registry_write_failed(e: impl std::fmt::Display) -> GitError;
    pub fn settings_sync_unavailable() -> GitError;
}
impl std::fmt::Display for GitError;

// src/git/util.rs — task 3
pub fn now_ms() -> u64;               // epoch millis, saturating
```

- **Produces** — everything below is exactly as the contract specifies it (§2 constants, §4.5,
  §4.6, §5.4, §5.5), **plus three additions listed at the end**, which tasks 6/7/9 may rely on:

```rust
// src/git/registry.rs
pub const AUTO_SYNC_MIN_SECS: u64 = 30;
pub const AUTO_SYNC_MAX_SECS: u64 = 86_400;
pub const MAX_REPO_ID_LEN: usize = 64;
pub const REGISTRY_VERSION: u32 = 1;

pub struct RepoDef { /* 14 pub fields, Deserialize only — never Serialize */ }
pub enum CredentialSpec { None, Token{..}, SshKey{..} }
pub struct AuthorSpec { pub name: Option<String>, pub email: Option<String> }
pub struct RegistryFile { pub version: u32, pub repos: Vec<RepoDef>, pub rejected_raw: Vec<serde_json::Value> }
pub struct RegistryError { pub code: &'static str, pub message: String,
                           pub quarantined_to: Option<String>, pub rejected: Vec<RejectedEntry> }
pub struct RejectedEntry { pub index: usize, pub id: Option<String>, pub reason: &'static str }
pub struct Warning { pub code: &'static str, pub message: String }
pub struct RegistryDefaults { pub default_branch: String, pub settings_enabled: bool,
                              pub allow_http: bool, pub registry_writes: bool }
pub struct PutOutcome { pub created: bool, pub repo: RepoDef, pub warnings: Vec<Warning> }
pub struct RepoView { /* 17 pub fields, Serialize */ }
pub enum CredentialView { None, Token{..}, SshKey{..} }
pub struct Registry { /* private */ }

pub fn valid_id(id: &str) -> bool;
pub fn validate_id(id: &str) -> Result<(), GitError>;
pub fn remote_host(remote: &str) -> Option<String>;
pub fn validate_remote(remote: &str, allow_http: bool) -> Result<(), GitError>;
pub fn validate_settings_path(path: &str) -> Result<(), GitError>;
pub fn validate_def(def: &RepoDef, d: &RegistryDefaults) -> Result<(), GitError>;
impl RepoView { pub fn new(def: &RepoDef, repos_dir: &std::path::Path) -> RepoView; }
impl CredentialView { pub fn of(spec: &CredentialSpec) -> CredentialView; }
impl CredentialSpec {
    pub fn bound_host(&self) -> Option<&str>;
    pub fn with_bound_host(self, host: Option<String>) -> Self;
    pub fn is_none(&self) -> bool;
}
impl Registry {
    pub fn load(path: &std::path::Path, repos_dir: &std::path::Path, defaults: RegistryDefaults) -> Registry;
    pub fn path(&self) -> &std::path::Path;
    pub fn writable(&self) -> bool;
    pub fn ensure_writable(&self) -> Result<(), GitError>;
    pub fn error(&self) -> Option<RegistryError>;
    pub fn notes(&self) -> &[String];
    pub fn count(&self) -> usize;
    pub fn ids(&self) -> Vec<String>;
    pub fn list(&self) -> Vec<RepoDef>;
    pub fn snapshot(&self, id: &str) -> Result<RepoDef, GitError>;
    pub fn contains(&self, id: &str) -> bool;
    pub fn auto_sync_secs(&self, id: &str) -> Option<u64>;
    pub fn put(&self, def: RepoDef) -> Result<PutOutcome, GitError>;
    pub fn remove(&self, id: &str) -> Result<RepoDef, GitError>;
}
pub(crate) fn save(path: &std::path::Path, file: &RegistryFile) -> Result<(), GitError>;
pub(crate) fn open_private(path: &std::path::Path) -> std::io::Result<std::fs::File>;

// src/git/state.rs
pub const STATE_VERSION: u32 = 1;
pub struct StateFile { pub version: u32, pub repos: std::collections::BTreeMap<String, RepoState> }
pub struct RepoState { pub last_sync: Option<LastSync>, pub ssh_host_fingerprint: Option<String> }
pub struct LastSync { pub at_ms: u64, pub ok: bool, pub op: String, pub job_id: String,
                      pub outcome: Option<String>, pub head: Option<String>,
                      pub code: Option<String>, pub message: Option<String> }
pub struct StateStore { /* private */ }
impl StateStore {
    pub fn load(path: &std::path::Path) -> StateStore;
    pub fn path(&self) -> &std::path::Path;
    pub fn fingerprint(&self, repo_id: &str) -> Option<String>;
    pub fn record_fingerprint(&self, repo_id: &str, fingerprint: &str);
    pub fn last_sync(&self, repo_id: &str) -> Option<LastSync>;
    pub fn record_last_sync(&self, repo_id: &str, last: LastSync);
    pub fn forget(&self, repo_id: &str);
    pub fn snapshot(&self) -> StateFile;
}
```

**Three additions the contract does not name.** All three exist because the contract's own
signatures need them; task 9 is the consumer:

```rust
impl Registry {
    /// `Registry::load` is handed `repos_dir` and must keep it: `RepoView::new` needs it and
    /// `GitService` would otherwise carry a second, independently-computed copy.
    pub fn repos_dir(&self) -> &std::path::Path;
    /// Non-fatal observations made during `load` (a stale `repos.json.tmp` removed, a loose
    /// unix mode tightened, the parse error behind each rejected entry). `registry.rs` has no
    /// logger — §4.4 requires these to be logged, so it returns them and `GitService::start`
    /// writes them to `git.log`.
    pub fn notes(&self) -> &[String];
}
impl StateStore {
    /// Same reason: §4.5 requires a corrupt `git-state.json` to be logged, and `StateStore`
    /// cannot log. `None` on a clean or absent file.
    pub fn load_error(&self) -> Option<&str>;
}
```

**Invariant every later task may rely on:** a `RepoDef` obtained from a `Registry` (via `list`,
`snapshot`, or `PutOutcome.repo`) always has a non-empty `branch`, non-zero `created_at_ms` and
`updated_at_ms`, an `auto_sync_secs` that is either `None` or inside
`AUTO_SYNC_MIN_SECS..=AUTO_SYNC_MAX_SECS`, and a `credential` whose `bound_host` is `Some`
whenever `remote` is `Some` and the credential is not `CredentialSpec::None`.

---

**Orientation for the engineer.**

`hitch` is a single binary with **no lib target** — unit tests live in the binary, in a
`#[cfg(test)] mod tests` at the bottom of the file they test, and the test filter is a module path:
`cargo test --bin hitch git::registry::tests::<name>`. Never `cargo test --lib`.
`tempfile` is already a dev-dependency; `serde_json` and `serde` are normal dependencies.

House rules that will be checked in review: comments explain **why** (the invariant, the race, the
attack), never what; no `unwrap()` outside `#[cfg(test)]` unless a panic is the correct behaviour
and a comment says so; `cargo fmt` and `cargo clippy -- -D warnings` clean at every commit.

**Why both files start with `#![allow(dead_code)]`.** These are leaf modules landing ahead of
their only consumer. Until `GitService` exists (task 9) nothing reachable from `main` calls a
single item in either file, and because this crate is a binary, `pub` does not exempt an item
from the `dead_code` lint — `cargo clippy -- -D warnings` would fail on roughly forty items.
Task 9 deletes both attributes as it wires the modules up. A per-item `#[allow]` was rejected:
forty attributes is noise nobody will remember to remove.

---

- [ ] **Step 1: Write the failing tests for the on-disk shapes**

Create `src/git/registry.rs` with exactly this content — a module header and a test module, no
implementation yet. Three tests, one behaviour ("`repos.json` entries deserialize the way the
contract says") from three angles: defaults, `deny_unknown_fields`, and the tagged credential enum.

```rust
//! `repos.json` — the repository registry.
//!
//! **Definitions only.** This file is written by `PUT`/`DELETE` and by nothing else: not by a
//! sync, not by a timer, not by a job. Every host-written volatile value lives in
//! `git-state.json` (`super::state`). That is what makes `repos.json` safe to hand-write, safe
//! to commit, and keeps a file that can hold a personal access token from being rewritten 2 880
//! times a day.

// This module lands ahead of its only consumer (`GitService`, a later slice). Until that exists
// nothing reachable from `main` calls anything here, and in a binary crate `pub` does not exempt
// an item from `dead_code`. Delete this attribute when `GitService` starts calling in.
#![allow(dead_code)]

#[cfg(test)]
mod tests {
    use super::*;

    fn def(json: &str) -> RepoDef {
        serde_json::from_str(json).expect("test fixture must parse")
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
            Some(CredentialSpec::Token { ref username, ref token, ref bound_host }) => {
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
            Some(CredentialSpec::SshKey { ref username, ref private_key_path, ref public_key, .. }) => {
                assert_eq!(username, "git");
                assert_eq!(private_key_path.as_deref(), Some(std::path::Path::new("/k/id_ed25519")));
                assert_eq!(public_key.as_deref(), Some("ssh-ed25519 AAAA"));
            }
            other => panic!("expected an ssh_key credential, got {other:?}"),
        }

        let e = serde_json::from_str::<CredentialSpec>(r#"{"kind":"token","token":"t","nope":1}"#)
            .expect_err("deny_unknown_fields must reach inside the variant");
        assert!(e.to_string().contains("unknown field `nope`"), "{e}");
        let e = serde_json::from_str::<CredentialSpec>(r#"{"kind":"weird"}"#).expect_err("bad kind");
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
}
```

Then add the module to `src/git/mod.rs` — insert `pub mod registry;` into the `pub mod` block at
the top, alphabetically (after `pub mod error;`).

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::registry::tests`

Expected: FAIL to compile — the types do not exist yet.

```
error[E0412]: cannot find type `RepoDef` in this scope
  --> src/git/registry.rs:19:38
   |
19 |     fn def(json: &str) -> RepoDef {
   |                           ^^^^^^^ not found in this scope

error[E0433]: failed to resolve: use of undeclared type `CredentialSpec`
  --> src/git/registry.rs:47:29
```

- [ ] **Step 3: Define the on-disk types**

Insert above the `#[cfg(test)] mod tests` block in `src/git/registry.rs` (i.e. between the
`#![allow(dead_code)]` line and the tests):

```rust
use crate::git::secret::Secret;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
            CredentialSpec::Token { bound_host, .. } | CredentialSpec::SshKey { bound_host, .. } => {
                bound_host.as_deref()
            }
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
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::registry::tests`

Expected: PASS — `4 passed`.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -- -D warnings
git add src/git/registry.rs src/git/mod.rs
git commit -m "feat(git): repos.json entry types with structural secret redaction"
```

- [ ] **Step 6: Write the failing tests for repo-id validation**

Append inside `mod tests` in `src/git/registry.rs`:

```rust
    #[test]
    fn valid_ids_are_lowercase_and_filesystem_safe() {
        let longest = "n".repeat(MAX_REPO_ID_LEN);
        for good in ["notes", "a", "0", "my-repo", "my_repo", "app.v2", longest.as_str()] {
            assert!(valid_id(good), "rejected {good:?}");
        }
        for bad in [
            "",                       // empty
            "Notes",                  // uppercase: macOS and Windows would collide it with "notes"
            "-lead",                  // must start alphanumeric
            ".hidden",
            "../evil",                // traversal
            "a..b",                   // traversal, spelled differently
            "sub/dir",                // one path segment only
            "sp ace",
            "trail.",                 // Windows silently strips a trailing dot
            "trail ",
            ".",
            "café",                   // ASCII only
            "con",                    // Windows reserved device names
            "PRN",
            "aux",
            "nul",
            "com1",
            "LPT9",
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
```

- [ ] **Step 7: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::registry::tests::valid_ids_are_lowercase_and_filesystem_safe`

Expected: FAIL to compile.

```
error[E0425]: cannot find function `valid_id` in this scope
  --> src/git/registry.rs:NNN:25
   |
   |             assert!(valid_id(good), "rejected {good:?}");
   |                     ^^^^^^^^ not found in this scope

error[E0433]: failed to resolve: use of undeclared type `GitErrorCode`
```

- [ ] **Step 8: Implement id validation**

Add `use crate::git::error::{scrub, GitError, GitErrorCode};` to the imports at the top of
`src/git/registry.rs` — `scrub` is used by `Registry::assembled` and `scrub_registry_error` in
step 33 — then append after the `AuthorSpec` definition:

```rust
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
    if !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-')
    {
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
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
        "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
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
```

- [ ] **Step 9: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::registry::tests`

Expected: PASS — `6 passed`.

- [ ] **Step 10: Commit**

```bash
cargo fmt && cargo clippy -- -D warnings
git add src/git/registry.rs
git commit -m "feat(git): validate repo ids as lowercase, single-segment, non-reserved"
```

- [ ] **Step 11: Write the failing tests for remote parsing and validation**

> **Amended by the first CI run on Windows (third round).** Two changes below, both in the
> blocks this step and Step 13 write:
>
> - `validate_remote_accepts_the_documented_forms` asserted that `/srv/repos/x.git` is
>   accepted, which is true only off Windows. `Path::is_absolute` wants a drive or UNC
>   prefix there, so a leading `/` is drive-*relative* and the validator refuses it —
>   correctly, since a remote is stored now and resolved later, possibly against a different
>   current drive. The row is `cfg!(windows)`-selected and the test now also asserts that the
>   *other* platform's form is refused, so the platform-dependence is pinned in both
>   directions instead of assumed away.
> - `remote_host`'s bracket branch is a `?` rather than a `match`, which is what
>   `clippy::question_mark` requires as of Rust 1.97 — CI floats on `@stable` and the lint
>   turned all three matrix legs red. Behaviour is unchanged; the table gains a row for the
>   unterminated bracket so the arm is not vacuous, because `Some("[::1")` would be a host a
>   stored credential could bind to that is not the host we would connect to.

Append inside `mod tests`:

```rust
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
            // An unterminated bracket is not a host we can name, so it fails closed too.
            ("https://[::1/x.git", None),
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
            // "Absolute" is the host filesystem's own answer, not POSIX's: `Path::is_absolute`
            // wants a drive or UNC prefix on Windows, so a leading `/` is merely
            // drive-relative there. Only the native form is accepted.
            if cfg!(windows) {
                r"C:\repos\x.git"
            } else {
                "/srv/repos/x.git"
            },
        ] {
            assert!(validate_remote(good, false).is_ok(), "rejected {good:?}");
        }
        // And the other platform's form is refused rather than half-understood. A remote is
        // stored now and resolved later, possibly against a different current drive, so an
        // absolute-looking path that this host cannot resolve must not be stored at all.
        let foreign = if cfg!(windows) {
            "/srv/repos/x.git"
        } else {
            r"C:\repos\x.git"
        };
        let e = validate_remote(foreign, false).expect_err("not absolute on this host");
        assert_eq!(e.code(), GitErrorCode::InvalidRequest);
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
```

- [ ] **Step 12: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::registry::tests::remote_host_is_lowercased_and_scp_aware`

Expected: FAIL to compile.

```
error[E0425]: cannot find function `remote_host` in this scope
error[E0425]: cannot find function `validate_remote` in this scope
```

- [ ] **Step 13: Implement `remote_host` and `validate_remote`**

Change the path import at the top of the file to `use std::path::{Path, PathBuf};`, then append
after `validate_id`:

```rust
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
        &host[..=host.find(']')?]
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
    // The username position is a secret too, on http(s) and only there. A PAT is routinely
    // carried as `https://ghp_x@host/...` with no colon in sight, which is why
    // `error::strip_userinfo` scrubs a bare `user@` as well — the scrubber has always
    // treated this shape as secret-bearing and the validator did not. Refusing it here is
    // the filesystem half of what F4 closed for logs and API bodies: `ops::init` and
    // `reconcile_remote` write this string verbatim into `<data-dir>/repos/<id>/.git/config`,
    // created 0664 under a 0775 `repos/`, right beside a `repos.json` that `open_private`
    // deliberately creates 0600.
    //
    // ssh and git are exempt because their username is a transport identity rather than a
    // credential — `ssh://git@host/a.git` and the scp form `git@host:a.git` are the ordinary
    // way to write those, and SSH authenticates by key. file:// has no userinfo to speak of.
    let token_userinfo_refused = |authority: &str| -> Result<(), GitError> {
        match authority.rsplit_once('@') {
            Some((userinfo, _)) if !userinfo.is_empty() => Err(GitError::insecure_remote(
                remote,
                "a username in an http(s) remote URL is where a token is usually carried, and it \
                 would be written into this repo's .git/config in clear text; put it on the repo \
                 as credential {\"kind\":\"token\"} instead",
            )),
            _ => Ok(()),
        }
    };

    if let Some(i) = remote.find("://") {
        let scheme = remote[..i].to_ascii_lowercase();
        let authority = before(&remote[i + 3..], '/');
        password_refused(authority)?;
        if matches!(scheme.as_str(), "http" | "https") {
            token_userinfo_refused(authority)?;
        }
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
```

- [ ] **Step 14: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::registry::tests`

Expected: PASS — `9 passed`.

- [ ] **Step 15: Commit**

```bash
cargo fmt && cargo clippy -- -D warnings
git add src/git/registry.rs
git commit -m "feat(git): parse remote hosts and refuse http/password remotes"
```

- [ ] **Step 16: Write the failing tests for `settings_path` and whole-entry validation**

Append inside `mod tests`:

```rust
    fn defaults() -> RegistryDefaults {
        RegistryDefaults {
            default_branch: "main".to_string(),
            settings_enabled: false,
            allow_http: false,
            registry_writes: true,
        }
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
            validate_def(&both, &defaults()).expect_err("both sources").field.as_deref(),
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
            validate_def(&d, &bad_default).expect_err("bad default").field.as_deref(),
            Some("branch")
        );
    }
```

- [ ] **Step 17: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::registry::tests::validate_def_reports_the_offending_field`

Expected: FAIL to compile.

```
error[E0422]: cannot find struct, variant or union type `RegistryDefaults` in this scope
error[E0425]: cannot find function `validate_settings_path` in this scope
error[E0425]: cannot find function `validate_def` in this scope
```

- [ ] **Step 18: Implement `RegistryDefaults`, `validate_settings_path` and `validate_def`**

Add `use crate::config::validate_branch_name;` to the imports, then append after
`validate_remote`:

```rust
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
        || !def.remote_name.bytes().all(|b| {
            b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-'
        })
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
                    format!("author.email {email:?} must look like a plain address, e.g. app@example.com"),
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
```

- [ ] **Step 19: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::registry::tests`

Expected: PASS — `14 passed`.

- [ ] **Step 20: Commit**

```bash
cargo fmt && cargo clippy -- -D warnings
git add src/git/registry.rs
git commit -m "feat(git): validate a whole repo definition against the [git] defaults"
```

- [ ] **Step 21: Write the failing leak-canary and view tests**

This is the mandatory canary from §10.4. It runs against all three credential kinds, because a
regression will show up in exactly one variant of the hand-written view.

Append inside `mod tests`:

```rust
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
            assert!(!json.contains("ghp_SUPERSECRET"), "leaked through RepoView: {json}");
            assert!(!format!("{d:?}").contains("ghp_SUPERSECRET"), "leaked through Debug");
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
            Some(CredentialView::Token { ref username, token_stored, ref bound_host }) => {
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
        let explicit_none = RepoView::new(&def(r#"{"id":"n","credential":{"kind":"none"}}"#), &repos_dir);
        assert!(!explicit_none.has_credentials);
        assert!(matches!(explicit_none.credential, Some(CredentialView::None)));
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
                assert_eq!(public_key_path.as_deref(), Some("/home/u/.ssh/id_ed25519.pub"));
                assert!(!private_key_stored);
                assert!(passphrase_stored);
                assert_eq!(bound_host.as_deref(), Some("github.com"));
            }
            other => panic!("expected an ssh_key view, got {other:?}"),
        }
    }
```

- [ ] **Step 22: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::registry::tests::redaction_never_emits_a_secret`

Expected: FAIL to compile.

```
error[E0433]: failed to resolve: use of undeclared type `RepoView`
error[E0433]: failed to resolve: use of undeclared type `CredentialView`
```

- [ ] **Step 23: Implement the outward-facing views**

Append after `validate_def` in `src/git/registry.rs`:

```rust
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
```

- [ ] **Step 24: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::registry::tests`

Expected: PASS — `17 passed`.

- [ ] **Step 25: Commit**

```bash
cargo fmt && cargo clippy -- -D warnings
git add src/git/registry.rs
git commit -m "feat(git): RepoView/CredentialView with no secret-shaped fields"
```

- [ ] **Step 26: Write the failing tests for the atomic private save**

Append inside `mod tests`:

```rust
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
        assert!(!dir.path().join("repos.json.tmp").exists(), "temp file survived");

        let back: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(back["version"], serde_json::json!(REGISTRY_VERSION));
        assert_eq!(back["repos"][0]["id"], serde_json::json!("notes"));
        assert_eq!(back["repos"][0]["remote_name"], serde_json::json!("origin"));
        assert_eq!(back["repos"][0]["credential"]["kind"], serde_json::json!("token"));
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
        assert_eq!(repos[1], serde_json::json!({ "id": "typo", "whoops": true }));
    }

    #[cfg(unix)]
    #[test]
    fn saved_registry_is_not_readable_by_group_or_other() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("repos.json");
        save(&path, &file_of(&[r#"{"id":"a"}"#])).expect("save");
        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o077, 0, "mode was {:o}", mode & 0o777);
    }
```

- [ ] **Step 27: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::registry::tests::save_writes_the_token_and_leaves_no_temp_file`

Expected: FAIL to compile.

```
error[E0422]: cannot find struct, variant or union type `RegistryFile` in this scope
error[E0425]: cannot find function `save` in this scope
```

- [ ] **Step 28: Implement `RegistryFile`, `to_wire` and the atomic save**

Add `use serde_json::{json, Value};` to the imports, then append after the `impl RepoView` block:

```rust
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

    let bytes = serde_json::to_string_pretty(&to_wire(file))
        .map_err(GitError::registry_write_failed)?;
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
```

- [ ] **Step 29: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::registry::tests`

Expected: PASS — `22 passed` (21 on Windows, where the mode test is `#[cfg(unix)]`).

- [ ] **Step 30: Commit**

```bash
cargo fmt && cargo clippy -- -D warnings
git add src/git/registry.rs
git commit -m "feat(git): atomic 0600 write of repos.json, secrets included"
```

- [ ] **Step 31: Write the failing tests for the tolerant load**

This is the centre of the task: ten tests, all one behaviour — "loading `repos.json` never
prevents the host from starting, and never destroys the author's file" — from ten angles. They
are one step because splitting them would mean writing a `load` in the first half that the second
half rewrites.

Append inside `mod tests`:

```rust
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
        assert_eq!(d.branch, "main", "empty branch filled from [git].default_branch");
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
        std::fs::write(dir.path().join("repos.json"), r#"{"version":1,"repos":[]}"#).expect("write");
        std::fs::write(&tmp, "half-written").expect("write tmp");
        let reg = load_at(dir.path());
        assert!(!tmp.exists(), "the crash leftover must be cleaned up");
        assert!(reg.notes().iter().any(|n| n.contains("repos.json.tmp")), "{:?}", reg.notes());
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
        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode was {mode:o}");
        assert!(reg.notes().iter().any(|n| n.contains("tightened")), "{:?}", reg.notes());
    }

    #[test]
    fn invalid_json_is_quarantined_with_the_original_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("repos.json");
        let original = "{ this is not json";
        std::fs::write(&path, original).expect("write");

        let reg = load_at(dir.path());
        assert_eq!(reg.count(), 0, "the host still starts");
        assert!(!path.exists(), "repos.json must be renamed away, never left in place");
        let err = reg.error().expect("registry_error");
        assert_eq!(err.code, "registry_corrupt");
        assert!(err.rejected.is_empty());
        let quarantine = err.quarantined_to.expect("quarantined_to");
        assert!(quarantine.contains("repos.json.corrupt-"), "{quarantine}");
        // Never deleted, never truncated: the author gets their file back.
        assert_eq!(std::fs::read_to_string(&quarantine).expect("read"), original);
    }

    #[test]
    fn file_level_failures_are_all_quarantined() {
        for body in [
            r#"{"version":2,"repos":[]}"#,               // wrong version
            r#"[]"#,                                      // top level not an object
            r#"{"version":1}"#,                           // no repos array
            r#"{"version":1,"repos":{}}"#,                // repos not an array
            r#"{"version":1,"repos":[],"extra":true}"#,   // unknown top-level key
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
        assert!(err.quarantined_to.is_none(), "entry failures never quarantine the file");
        assert_eq!(err.message, "6 of 7 repo definitions were rejected");
        assert_eq!(
            err.rejected,
            vec![
                RejectedEntry { index: 1, id: Some("../evil".to_string()), reason: "invalid_repo_id" },
                RejectedEntry { index: 2, id: Some("typo".to_string()), reason: "invalid_request" },
                RejectedEntry { index: 3, id: Some("cfg".to_string()), reason: "settings_sync_unavailable" },
                RejectedEntry { index: 4, id: Some("branchy".to_string()), reason: "invalid_branch_name" },
                RejectedEntry { index: 5, id: Some("leaky".to_string()), reason: "insecure_remote" },
                // The first entry with an id wins; the later one is the duplicate.
                RejectedEntry { index: 6, id: Some("good".to_string()), reason: "duplicate_id" },
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
```

- [ ] **Step 32: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::registry::tests::a_missing_file_loads_an_empty_writable_registry`

Expected: FAIL to compile.

```
error[E0433]: failed to resolve: use of undeclared type `Registry`
  --> src/git/registry.rs:NNN:9
   |
   |         Registry::load(&dir.join("repos.json"), &dir.join("repos"), defaults())
   |         ^^^^^^^^ use of undeclared type `Registry`

error[E0422]: cannot find struct, variant or union type `RejectedEntry` in this scope
```

- [ ] **Step 33: Implement `Registry::load` and the reporting types**

> **Amended by the post-execution audit (F3, F4).** Four blocks below changed. `load`'s read was
> `std::fs::read_to_string`, whose one `io::Error` cannot distinguish "the host could not read
> this file" from "these bytes are not UTF-8" — the first must be left alone and latch writes
> shut, the second is a corruption to quarantine like any other. `assembled` returned `error` and
> `notes` unscrubbed, so the `insecure_remote` refusal that warns about passwords in remote URLs
> was printing one into `git.log` on every launch. `writable()` read only the config key, so the
> first `PUT` after an unreadable load renamed a temp file over a `repos.json` full of other
> definitions and stored PATs. And `quarantine`'s failed-rename arm left every caller's
> "; quarantined" message standing while the file was still on disk. `blocks_writes`,
> `write_block`, `ensure_writable` and `scrub_registry_error` are all new here; `ensure_writable`
> is what steps 40 and 55 call instead of reading `defaults.registry_writes` themselves.

Add to the imports at the top of the file:

```rust
use crate::git::util::now_ms;
use std::sync::RwLock;
```

Append at the end of the implementation section (before `#[cfg(test)] mod tests`):

```rust
/// Why a `repos.json` could not be used in full. Never fatal: it is reported through
/// `GET /api/git`, through `/api/status`, and in `git.log` at startup, and the host starts
/// normally either way — an optional feature must not brick the app.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegistryError {
    /// `"registry_corrupt"` (file-level) or `"registry_entries_rejected"`. A `registry_corrupt`
    /// with `quarantined_to: None` is the third tier — bytes we never got — and latches writes
    /// shut; see `blocks_writes`.
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

/// The in-memory registry. Read once at startup, written only by `put`/`remove`.
pub struct Registry {
    path: PathBuf,
    repos_dir: PathBuf,
    defaults: RegistryDefaults,
    file: RwLock<RegistryFile>,
    error: Option<RegistryError>,
    notes: Vec<String>,
}

> **Amended by the post-execution audit (second round).** Three changes in `validate_remote`
> and `load`, all about a secret reaching a file that is not 0600:
>
> - `password_refused` fired only on a userinfo containing `:`, so `https://ghp_x@host/…` —
>   the way a PAT is actually written — was accepted, and `ops::init` then copied it into a
>   0664 `.git/config`. `token_userinfo_refused` refuses any non-empty userinfo on http(s);
>   ssh and git are exempt because their username is a transport identity, not a credential.
> - `tighten_mode` ran *before* the read, so the read-failure arm's "it was left exactly as
>   it is" was false — and for a directory planted where the file belongs it stripped the
>   search bit. It runs after the successful read now.
> - serde echoes the value it choked on, so `{"credential": "<token>"}` put the token into
>   `git.log` on every launch. The per-entry rejection goes through `error::scrub_serde`.

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
        // Bytes, not `read_to_string`: the string form collapses "the file could not be read" and
        // "the file is not UTF-8" into one `io::Error`, and those two need opposite answers.
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            // The normal first-run state, not a problem.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Registry::assembled(path, repos_dir, defaults, empty_file(), None, notes)
            }
            Err(e) => {
                // Deliberately NOT quarantined. Renaming a file we could not read is a bigger act
                // than refusing to write one: one EACCES from a mis-chmod would retire the whole
                // registry, credentials included. Leave it; `blocks_writes` latches writes shut.
                let error = RegistryError {
                    code: "registry_corrupt",
                    message: format!(
                        "{} cannot be read ({e}); it was left exactly as it is, and registry \
                         writes are refused until it can be read or moved aside",
                        path.display()
                    ),
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
        // After the read, not before it. `tighten_mode` exists to protect a file whose
        // contents are secret, and on the read-failure arm above there is nothing here we
        // could read, so nothing to protect — while chmodding it 0600 anyway made that
        // arm's promise ("it was left exactly as it is") false, and, for the
        // directory-in-place case, took the search bit with it and locked the operator
        // out of their own contents.
        #[cfg(unix)]
        tighten_mode(path, &mut notes);

        // Bytes we *have* and cannot use are a corruption like any other, so the author gets them
        // back under a `.corrupt-` name and the registry stays writable.
        let raw = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(e) => {
                return quarantine(
                    path,
                    repos_dir,
                    defaults,
                    notes,
                    format!("repos.json is not UTF-8 text ({e}); quarantined"),
                )
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
                    // `scrub_serde`, not `scrub`: serde quotes the value it choked on, and
                    // the value most likely to be hand-written into this file wrong is
                    // `credential`. See its doc comment.
                    notes.push(crate::git::error::scrub_serde(&format!(
                        "repos[{index}] rejected: {e}"
                    )));
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
                notes.push(format!("repos[{index}] rejected: duplicate id {:?}", def.id));
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
                message: format!("{} of {total} repo definitions were rejected", rejected.len()),
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

    /// The one funnel every `load` return path and `quarantine` pass through, which is why the
    /// scrub lives here: every note and every `RegistryError` above is `format!`ed over the
    /// contents of `repos.json`, and they go straight into a log file created under the ordinary
    /// umask and into `GET /api/git`. Scrubbing at the funnel is what stops a twelfth return path
    /// from re-opening the leak.
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
            error: error.map(scrub_registry_error),
            // `&[]`: nothing here has resolved a credential, so `strip_userinfo` is the whole of
            // the available defence — and it is the pass that matters, because a stored token
            // reaches a note only by being quoted back inside a remote.
            notes: notes.into_iter().map(|n| scrub(&n, &[])).collect(),
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

    /// `Some(why)` when a `repos.json` we could not account for is still sitting at `path`.
    /// Derived from `error` rather than stored, so no construction path can set one without the
    /// other — a `writes_blocked: bool` written at one site is free to drift from the error it
    /// came from, and `error` is immutable after `assembled`.
    fn write_block(&self) -> Option<&RegistryError> {
        self.error.as_ref().filter(|e| blocks_writes(e))
    }

    pub fn writable(&self) -> bool {
        self.defaults.registry_writes && self.write_block().is_none()
    }

    /// The gate every write goes through, and the only place either refusal is worded. `put` and
    /// `remove` call this instead of reading `defaults.registry_writes` directly, so a registry
    /// that latched shut at load cannot be written by a caller that reached `Registry` without
    /// going through `GitService`.
    ///
    /// The latch is reported first: `registry_writes = false` is a choice the author already
    /// knows about, while an unaccounted-for `repos.json` is a fault only this message will name.
    /// Both are the same wire code deliberately — to a client they mean the same thing ("this
    /// host will not persist registry edits, and retrying will not help"), and `GET /api/git`
    /// already tells them apart without a second code: `registry_writable: false` **with** a
    /// `registry_error` is the fault, without one it is the config key.
    pub fn ensure_writable(&self) -> Result<(), GitError> {
        if let Some(why) = self.write_block() {
            return Err(GitError::new(
                GitErrorCode::RegistryReadOnly,
                format!(
                    "repos.json cannot be written while its contents are unaccounted for: {}",
                    why.message
                ),
            ));
        }
        if !self.defaults.registry_writes {
            return Err(GitError::registry_read_only());
        }
        Ok(())
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
}

fn empty_file() -> RegistryFile {
    RegistryFile {
        version: REGISTRY_VERSION,
        repos: Vec::new(),
        rejected_raw: Vec::new(),
    }
}

/// The one rule that decides whether the registry may write itself back: is there still a
/// `repos.json` on disk whose contents we could not account for?
///
/// `save` publishes by `rename`, so the first `PUT` after such a load replaces that file
/// wholesale — every other definition and every stored PAT in it, with no `.corrupt-` copy to
/// recover from. A quarantined file is accounted for: the bytes moved aside.
/// `registry_entries_rejected` is accounted for too: those entries were read, and `rejected_raw`
/// writes them back verbatim. What is left is a `registry_corrupt` with nowhere to point —
/// either the read failed, or the quarantine rename did.
fn blocks_writes(error: &RegistryError) -> bool {
    error.code == "registry_corrupt" && error.quarantined_to.is_none()
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
    let (quarantined_to, message) = match std::fs::rename(path, &target) {
        Ok(()) => (Some(target.display().to_string()), message),
        Err(e) => {
            notes.push(format!("cannot quarantine {}: {e}", path.display()));
            // Every caller's message ends "; quarantined", which is now untrue — and the operator
            // has to know the file is still theirs to move, because until it is moved
            // `ensure_writable` refuses every PUT and DELETE.
            (
                None,
                format!(
                    "{message} — but the rename failed ({e}); {} is still there and registry \
                     writes are refused until it is moved aside",
                    path.display()
                ),
            )
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
        Ok(()) => notes.push(format!(
            "tightened {} from {mode:o} to 600",
            path.display()
        )),
        Err(e) => notes.push(format!("cannot tighten {}: {e}", path.display())),
    }
}

/// `RegistryError` crosses two surfaces that outlive the request — `git.log` and the
/// `registry_error` field of `GET /api/git` and `/api/status` — and both of its text fields are
/// quoted out of `repos.json`. `message` carries serde's complaint, and `serde_json::from_value`
/// echoes the offending value verbatim (`invalid type: string "https://user:pat@host/x.git",
/// expected u64`); `rejected[].id` is the raw `id` field of an entry too broken to have been
/// validated yet.
///
/// Scrubbing `id` cannot cost a caller an identifying value: `valid_id` admits neither `:` nor
/// `/`, so the only ids this rewrites are ones that were rejected anyway, and `rejected[].index`
/// is the precise locator regardless. `quarantined_to` is a path this host built rather than file
/// content, and is deliberately left intact.
fn scrub_registry_error(mut error: RegistryError) -> RegistryError {
    error.message = scrub(&error.message, &[]);
    for entry in &mut error.rejected {
        entry.id = entry.id.take().map(|id| scrub(&id, &[]));
    }
    error
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
```

- [ ] **Step 34: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::registry::tests`

Expected: PASS — `32 passed` (30 on Windows, where the two `#[cfg(unix)]` mode tests are absent).

If `bad_entries_are_skipped_and_the_good_ones_load` fails on the `reason` of index 4, check that
task 2's `validate_branch_name` rejects `bad..branch` — the rule is "no `..`".

- [ ] **Step 35: Commit**

```bash
cargo fmt && cargo clippy -- -D warnings
git add src/git/registry.rs
git commit -m "feat(git): tolerant repos.json load with quarantine and entry rejection"
```

- [ ] **Step 36: Write the failing tests for `Registry::put`**

Append inside `mod tests`:

```rust
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
        assert_eq!(repos.len(), 3, "the rejected entries were written back: {repos:?}");
        assert!(repos.iter().any(|r| r["id"] == serde_json::json!("../evil")));
        assert!(repos.iter().any(|r| r["whoops"] == serde_json::json!(true)));
    }
```

- [ ] **Step 37: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::registry::tests::put_creates_then_updates_and_persists`

Expected: FAIL to compile.

```
error[E0599]: no method named `put` found for struct `Registry` in the current scope
  --> src/git/registry.rs:NNN:29
   |
   |         let first = reg.put(def(r#"{"id":"notes"}"#)).expect("create");
   |                         ^^^ method not found in `Registry`
```

- [ ] **Step 38: Implement `PutOutcome` and `Registry::put`**

Add before `impl Registry` (next to `RegistryDefaults` is fine):

```rust
#[derive(Debug)]
pub struct PutOutcome {
    pub created: bool,
    pub repo: RepoDef,
    pub warnings: Vec<Warning>,
}
```

Add inside `impl Registry`, after `auto_sync_secs`:

```rust
    /// Create or replace one entry, then persist. A `PUT` is a whole-definition write: fields
    /// absent from `def` take their serde defaults, they are not merged with what was stored.
    pub fn put(&self, def: RepoDef) -> Result<PutOutcome, GitError> {
        self.ensure_writable()?;
        validate_id(&def.id)?;

        let mut def = def;
        let mut warnings = Vec::new();
        let mut guard = self.write();
        let existing = guard.repos.iter().position(|r| r.id == def.id);

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
```

Note what is deliberately **missing**: nothing carries a stored credential forward yet, so a
`PUT` that omits `credential` currently wipes it. Step 41 writes the tests that prove that is
wrong, and step 43 fixes it.

- [ ] **Step 39: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::registry::tests`

Expected: PASS — `37 passed` (35 on Windows).

- [ ] **Step 40: Commit**

```bash
cargo fmt && cargo clippy -- -D warnings
git add src/git/registry.rs
git commit -m "feat(git): Registry::put persists before it commits in memory"
```

- [ ] **Step 41: Write the failing tests for credential binding and clamping through `put`**

Append inside `mod tests`:

```rust
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
            out.repo.credential.as_ref().and_then(CredentialSpec::bound_host),
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
            out.repo.credential.as_ref().and_then(CredentialSpec::bound_host),
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
            .put(def(r#"{"id":"notes","remote":"https://evil.example/acme/notes.git"}"#))
            .expect("repointed put");
        assert!(
            out.repo.credential.is_none(),
            "a repointed remote must not inherit the old host's token"
        );
        assert_eq!(out.warnings.len(), 1);
        assert_eq!(out.warnings[0].code, "auth_unbound");
        assert!(out.warnings[0].message.contains("github.com"), "{:?}", out.warnings[0]);

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

        let low = reg.put(def(r#"{"id":"a","auto_sync_secs":5}"#)).expect("put");
        assert_eq!(low.repo.auto_sync_secs, Some(AUTO_SYNC_MIN_SECS));
        assert_eq!(low.warnings[0].code, "auto_sync_clamped");

        let high = reg
            .put(def(r#"{"id":"b","auto_sync_secs":999999}"#))
            .expect("put");
        assert_eq!(high.repo.auto_sync_secs, Some(AUTO_SYNC_MAX_SECS));
        assert_eq!(high.warnings[0].code, "auto_sync_clamped");

        // 0 is "off", not a value to clamp, and produces no warning.
        let off = reg.put(def(r#"{"id":"c","auto_sync_secs":0}"#)).expect("put");
        assert_eq!(off.repo.auto_sync_secs, None);
        assert!(off.warnings.is_empty());

        let fine = reg.put(def(r#"{"id":"d","auto_sync_secs":300}"#)).expect("put");
        assert_eq!(fine.repo.auto_sync_secs, Some(300));
        assert!(fine.warnings.is_empty());
    }
```

- [ ] **Step 42: Run the tests and watch two of them fail**

Run: `cargo test --bin hitch git::registry::tests`

Expected: FAIL — three of the five new tests pass already (binding and clamping are
`normalise_def`'s job and landed in step 33); the two that describe credential *carry-forward*
fail on an assertion.

```
---- git::registry::tests::put_carries_a_stored_credential_forward stdout ----
thread 'git::registry::tests::put_carries_a_stored_credential_forward' panicked at src/git/registry.rs:NNN:9:
assertion failed: out.repo.credential.is_some()

---- git::registry::tests::put_drops_a_credential_whose_host_changed stdout ----
thread 'git::registry::tests::put_drops_a_credential_whose_host_changed' panicked at src/git/registry.rs:NNN:9:
assertion `left == right` failed
  left: 0
 right: 1

test result: FAILED. 40 passed; 2 failed; 0 ignored
```

- [ ] **Step 43: Carry a stored credential forward, but only while it stays bound**

> **Amended by the post-execution audit (F2).** This step originally wrote the predicate as a
> three-arm `match (&bound, &remote_host_now)` whose `(None, _) => true` arm — commented "Never
> bound (hand-written); `normalise_def` binds it below" — treated an absent `bound_host` as
> permission to bind to **any** host. That is a credential-theft primitive: a loopback PUT
> repointing a repo at an attacker's remote kept the stored PAT, and `normalise_def` three lines
> below then rebound it to the attacker's host. The warning text was also built with
> `unwrap_or("(nothing)")` / `unwrap_or("(no remote)")`, which rendered as "was bound to (nothing)"
> and "this remote is (no remote)". Both are corrected below;
> `registry::tests::put_drops_an_unbound_credential_when_the_remote_gains_a_host` asserts the drop
> and asserts the message does **not** contain `"(nothing)"`, and
> `::put_keeps_an_unbound_credential_while_the_remote_still_has_no_host` is the guard against the
> tempting over-correction `bound.is_some() && bound == remote_host_now`.

Insert into `Registry::put`, between `let existing = …;` and `let now = now_ms();`:

```rust
        if def.credential.is_none() {
            if let Some(stored) = existing.and_then(|i| guard.repos[i].credential.clone()) {
                let remote_host_now = def.remote.as_deref().and_then(remote_host);
                let bound = stored.bound_host().map(str::to_string);
                // Carrying the stored credential forward keeps the token off the loopback socket
                // on every unrelated edit — but only while it is still bound to the host the
                // remote points at. Repointing a repo at an attacker's host and re-PUTting is the
                // cheapest way to steal a PAT out of this service; dropping the credential here
                // is what makes that attack yield nothing.
                //
                // Deliberately the same equality `creds::resolve` transmits by, `None` included.
                // `normalise_def` binds every credential it stores, so an unbound one can only
                // have been written next to a remote with no host at all — `file://`, a local
                // path, or no remote — and it is keepable only while that is still true. Carrying
                // it onto a remote that does have a host would hand it to `normalise_def` three
                // lines below, which binds it to that host: the same theft, with the rebinding
                // done for the attacker.
                //
                // `kind: "none"` short-circuits because its `bound_host` is always `None`:
                // without that it would now "drop" to an identical value and warn the user to
                // rebind a credential that does not exist.
                let keep = stored.is_none() || bound == remote_host_now;
                if keep {
                    def.credential = Some(stored);
                } else {
                    // Spelled out on both sides because either can be absent, and "(nothing)"
                    // mid-sentence read like a placeholder that had failed to expand.
                    let was = match &bound {
                        Some(b) => format!("was bound to {b}"),
                        None => "was never bound to a host".to_string(),
                    };
                    let points_at = match &remote_host_now {
                        Some(h) => format!("this remote is {h}"),
                        None => "this remote has no host".to_string(),
                    };
                    warnings.push(Warning {
                        code: "auth_unbound",
                        message: format!(
                            "the stored credential {was} but {points_at}; it was dropped — send a \
                             replacement credential with this PUT"
                        ),
                    });
                }
            }
        }
```

- [ ] **Step 44: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::registry::tests`

Expected: PASS — `42 passed` (40 on Windows).

- [ ] **Step 45: Commit**

```bash
cargo fmt && cargo clippy -- -D warnings
git add src/git/registry.rs
git commit -m "feat(git): carry a stored credential forward only while it stays bound"
```

- [ ] **Step 46: Write the failing tests for `Registry::remove`**

Append inside `mod tests`:

```rust
    #[test]
    fn remove_deletes_the_entry_and_persists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = load_at(dir.path());
        reg.put(def(r#"{"id":"notes","sync_on_start":true}"#)).expect("put");
        reg.put(def(r#"{"id":"other"}"#)).expect("put");

        let removed = reg.remove("notes").expect("remove");
        assert_eq!(removed.id, "notes");
        assert!(removed.sync_on_start, "the removed definition is returned in full");
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
```

- [ ] **Step 47: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::registry::tests::remove_deletes_the_entry_and_persists`

Expected: FAIL to compile.

```
error[E0599]: no method named `remove` found for struct `Registry` in the current scope
```

- [ ] **Step 48: Implement `Registry::remove`**

Add inside `impl Registry`, after `put`:

```rust
    /// Removes one entry and persists. The worktree under `<data-dir>/repos/<id>` is **not**
    /// touched here — deleting a user's files is `GitService::delete_repo`'s explicit
    /// `?purge=true` decision, not a side effect of editing the registry.
    pub fn remove(&self, id: &str) -> Result<RepoDef, GitError> {
        self.ensure_writable()?;
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
```

- [ ] **Step 49: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::registry::tests`

Expected: PASS — `44 passed` (42 on Windows).

- [ ] **Step 50: Commit**

```bash
cargo fmt && cargo clippy -- -D warnings
git add src/git/registry.rs
git commit -m "feat(git): Registry::remove without touching the worktree"
```

- [ ] **Step 51: Write the failing tests for `git-state.json` loading**

Create `src/git/state.rs` with only the header and a test module:

```rust
//! `git-state.json` — host-owned volatile state.
//!
//! Everything the host learns while running and wants back after a restart: the outcome of the
//! last sync, the SSH host key it pinned. It is written after every terminal job, which is why
//! it must not be `repos.json` — a repo syncing every 30 s would rewrite its own credential file
//! 2 880 times a day. **Never contains a secret**, and a corrupt one is never fatal: it is a
//! cache, so the only correct response to nonsense is to start empty.

// Same reason as `registry.rs`: this module lands ahead of `GitService`, and in a binary crate
// `pub` does not exempt an item from `dead_code`. Delete when `GitService` starts calling in.
#![allow(dead_code)]

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
        assert_eq!(repo.last_sync.as_ref().expect("last_sync").outcome.as_deref(), Some("merged"));
        assert!(repo.ssh_host_fingerprint.as_deref().expect("fp").starts_with("SHA256:"));

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
}
```

Then add `pub mod state;` to `src/git/mod.rs`, alphabetically after `pub mod secret;`.
(Step 56 appends five more tests immediately before that closing `}`.)

- [ ] **Step 52: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::state::tests`

Expected: FAIL to compile.

```
error[E0412]: cannot find type `LastSync` in this scope
error[E0433]: failed to resolve: use of undeclared type `StateStore`
error[E0425]: cannot find value `STATE_VERSION` in this scope
```

- [ ] **Step 53: Implement the state types and `StateStore::load`**

Insert between `#![allow(dead_code)]` and the test module:

```rust
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
                Some(format!("git-state.json cannot be read ({e}); starting empty")),
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
            // The same reason `Registry::assembled` scrubs: this string quotes serde's complaint
            // about a file a human may have hand-edited, serde echoes the offending value
            // verbatim, and its only reader is a `git.log` line in a file created under the
            // ordinary umask. A cache documented as never containing a secret must also never
            // republish one it was handed. Needs `use crate::git::error::scrub;`.
            load_error: load_error.map(|e| scrub(&e, &[])),
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
        self.lock().repos.get(repo_id).and_then(|r| r.last_sync.clone())
    }
}
```

- [ ] **Step 54: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::state::tests`

Expected: PASS — `4 passed`.

- [ ] **Step 55: Commit**

```bash
cargo fmt && cargo clippy -- -D warnings
git add src/git/state.rs src/git/mod.rs
git commit -m "feat(git): git-state.json types and non-fatal load"
```

- [ ] **Step 56: Write the failing tests for the state mutators**

Append inside `git::state`'s `mod tests`:

```rust
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
        assert_eq!(StateStore::load(&path).fingerprint("other").as_deref(), Some("SHA256:def"));
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
        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o077, 0, "mode was {:o}", mode & 0o777);
    }
```

- [ ] **Step 57: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::state::tests::recorded_state_survives_a_reload`

Expected: FAIL to compile.

```
error[E0599]: no method named `record_last_sync` found for struct `StateStore` in the current scope
  --> src/git/state.rs:NNN:15
   |
   |         store.record_last_sync("notes", last(true));
   |               ^^^^^^^^^^^^^^^^ method not found in `StateStore`
```

- [ ] **Step 58: Implement the mutators and the atomic persist**

Add `use crate::git::registry::open_private;` to the imports at the top of `src/git/state.rs`,
then add inside `impl StateStore`, after `last_sync`:

```rust
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
```

And add at the end of the file, before `#[cfg(test)] mod tests`:

```rust
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
```

- [ ] **Step 59: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::state::tests`

Expected: PASS — `9 passed` (8 on Windows).

- [ ] **Step 60: Commit**

```bash
cargo fmt && cargo clippy -- -D warnings
git add src/git/state.rs
git commit -m "feat(git): best-effort atomic writes for git-state.json"
```

- [ ] **Step 61: Run the whole gate and confirm the module boundary**

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: all green. Then confirm neither new file *uses* a forbidden crate or host type — this is
the invariant tasks 5 through 12 build on, and it is much cheaper to keep than to restore:

```bash
grep -nE '(^use .*(git2|axum|tokio))|git2::|axum::|tokio::|HostEvent|Children|crate::host' \
  src/git/registry.rs src/git/state.rs
```

Expected: no output. (Plain prose mentions of "libgit2" and "git.log" in comments are fine — the
pattern above deliberately does not match them.) If `grep` prints anything, the offending code
belongs in a later slice.

Also confirm the two `#![allow(dead_code)]` attributes are the only lint suppressions added:

```bash
grep -n 'allow(' src/git/registry.rs src/git/state.rs
```

Expected: exactly two lines, one per file. Both are deleted by task 9 when `GitService` starts
calling into these modules.

- [ ] **Step 62: Commit the final state**

Nothing should be left uncommitted after step 60; if `cargo fmt` changed anything in step 61:

```bash
git add -A src/git
git commit -m "style(git): cargo fmt after registry and state"
```
