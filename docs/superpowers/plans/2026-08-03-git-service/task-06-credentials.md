### Task 6: Credential resolution and callbacks

Two files land here, and the split between them is the whole point of the task.

`src/git/creds.rs` is deliberately in two halves. **`resolve` is pure**: it turns *what the
request said* plus *what `repos.json` stored* into a `ResolvedCred` — a plain description of a
credential — with no libgit2 anywhere, so the precedence rule and the `bound_host` rule are
table-tested. **`callbacks` is a thin adapter**: it hands that description to libgit2 and does
nothing else. The security property this file exists to guarantee is a *negative* one — the host
never reaches for an identity the caller did not supply — and negatives get tested too, by a
source-grep test at the bottom of the file.

`src/git/ops.rs` is created here but only its **context layer**: the request, the identity, the
outcome, and the `OpCtx` that every libgit2 callback writes through. `creds::callbacks` takes
`&OpCtx`, so the two cannot land separately. The verbs (`init`, `clone`, `commit`, …) land as
stubs that later tasks replace one at a time.

**House rules that apply to every step below:** unit tests live in a `#[cfg(test)] mod tests` at
the bottom of the file they test; comments explain WHY (an invariant, a race, an attack), never
what; no `unwrap()` outside `#[cfg(test)]` unless a panic is the correct behaviour and a comment
says so; `cargo fmt` and `cargo clippy --all-targets -- -D warnings` clean at every commit.

**Neither new file needs its own `#![allow(dead_code)]`.** `src/git/mod.rs` already carries
`#![allow(dead_code)]` and `#![allow(clippy::result_large_err)]` from task 3, and lint levels
propagate down the module tree into `creds` and `ops`. Both attributes are still required — this
is a binary crate, so `pub` does *not* exempt an item from `dead_code`, and nothing reachable
from `main` calls into `src/git/` until task 9 wires up `GitService`.

**This crate has no `--lib` target** (`src/main.rs` is the only entry point, and
`src/bin/gen_icons.rs` is a second binary), so unit tests run in the `chrome-host-app` bin
target: `cargo test --bin chrome-host-app <filter>`.

---

**Files:**
- Create: `src/git/creds.rs`
- Create: `src/git/ops.rs`
- Modify: `src/git/mod.rs` — add `pub mod creds;` and `pub mod ops;` to the `pub mod` block
  (which sits after the two `#![allow(...)]` inner attributes and before `#[cfg(test)] mod
  tests`). Keep the list alphabetical: `creds`, `error`, `jobs`, `ops`, `registry`, `secret`,
  `state`, `util`.

**Interfaces:**

- **Consumes** (all already landed; signatures are exact):
  - `crate::config::SshHostKeyPolicy` — `#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]`,
    variants `Tofu` (default) and `Accept`. *(task 2)*
  - `crate::config::SettingsSection` — `#[derive(Debug, Clone, Deserialize)]`. *(existing)*
  - `crate::git::secret::Secret` — `Secret::new(String) -> Secret`, `expose(&self) -> &str`,
    `expose_for_scrub(&self) -> &str`; `Clone + PartialEq + Eq`; `Debug` prints `Secret(***)`;
    **no `Serialize`, no `Display`**. *(task 3)*
  - `crate::git::error::{GitError, GitErrorCode, AbortReason}` with
    `GitError::new(GitErrorCode, impl Into<String>) -> GitError`,
    `GitError::internal(impl Into<String>)`,
    `GitError::invalid_request(field: &str, message: impl Into<String>)`,
    `GitError::push_rejected(refname: &str, status: &str)`,
    `.with_repo(&str)`, `.with_git2(&git2::Error)`, `.scrubbed(&[&str])`, `.code() -> GitErrorCode`,
    and `error::classify(&git2::Error, AbortReason) -> GitErrorCode`. *(task 3)*
  - `crate::git::util::b64_nopad(&[u8]) -> String` — standard alphabet, `=` stripped. *(task 3)*
  - `crate::git::registry::{RepoDef, CredentialSpec, AuthorSpec, Warning}`,
    `registry::remote_host(&str) -> Option<String>` (lowercased, scp-form aware),
    `CredentialSpec::bound_host(&self) -> Option<&str>`, `CredentialSpec::is_none(&self) -> bool`.
    *(task 4)*
  - `crate::git::jobs::{JobStore, JobSlot, JobOp, Phase, Progress, AbortFlag, Admission,
    RepoLease}` with `JobStore::new(String) -> Arc<JobStore>`,
    `JobStore::admit(&Arc<Self>, repo_id: &str, op: JobOp, request_id: Option<&str>)
    -> Result<Admission, GitError>`, `JobSlot::set_phase(Phase)`, `JobSlot::phase() -> Phase`,
    `JobSlot::set_progress(Progress) -> bool`, `AbortFlag::{abort, is_aborted, reason}`,
    `JobOp::as_str(self) -> &'static str`. *(task 5)*
  - `gethostname::gethostname() -> std::ffi::OsString`, `git2` 0.21 with `https`, `ssh`,
    `vendored-libgit2`. *(task 1)*

- **Produces** (later tasks rely on all of these):
  - `src/git/creds.rs`:
    `pub const MAX_CRED_ATTEMPTS: u32 = 2`;
    `pub enum ResolvedCred { None, Token{username: String, token: Secret},
    SshPath{username: String, public: Option<PathBuf>, private: PathBuf, pass: Option<Secret>},
    SshMem{username: String, public: Option<String>, private: Secret, pass: Option<Secret>} }`
    with `username(&self) -> Option<&str>`, `is_none(&self) -> bool`, `secrets(&self) -> Vec<&str>`;
    `pub struct CredResolution { pub cred: ResolvedCred, pub unbound: bool }`;
    `pub fn resolve(per_call: Option<&CredentialSpec>, stored: Option<&CredentialSpec>,
    remote: Option<&str>) -> Result<CredResolution, GitError>`;
    `pub fn callbacks<'a>(ctx: &'a OpCtx) -> git2::RemoteCallbacks<'a>`;
    `pub fn fingerprint_sha256(raw: &[u8; 32]) -> String`;
    `pub struct HostKeyChecker { pub policy: SshHostKeyPolicy, pub pinned: Option<String>, .. }`
    with `new(SshHostKeyPolicy, Option<String>) -> HostKeyChecker`,
    `check(&self, &git2::Cert<'_>, &str) -> Result<git2::CertificateCheckStatus, git2::Error>`,
    `learned(&self) -> Option<String>`.
  - `src/git/ops.rs`: `Identity` (+ `Identity::new(app_name: &str, identifier: &str)`),
    `SettingsCtx`, `BranchRequest`, `ResetRequest`, `OpRequest` (+ `Default`, `auto()`,
    `manual()`), `MergeReport`, `SettingsRejected`, `OpOutcome` (+
    `OpOutcome::new(&'static str, &str)`), `OpScratch`, `OpCtx` and its twelve methods,
    `pub fn run(op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError>` plus the eight verb
    stubs `init clone pull push sync commit branch reset`, each
    `pub fn(&OpCtx) -> Result<OpOutcome, GitError>`.

- **Two deliberate refinements of the interface contract.** Both are called out so the
  consistency audit sees them rather than flagging them:
  1. **`OpCtx.scratch: OpScratch`** replaces the contract's three bare private fields
     (`cred_attempts`, `cred_failure`, `push_rejections`). Private fields make an `OpCtx` struct
     literal impossible from `git/mod.rs`, where task 9 builds one; the alternatives were a
     twelve-argument constructor or a twelve-field duplicate init struct. `OpScratch` keeps the
     three cells private to `ops.rs` and costs task 9 exactly one line
     (`scratch: OpScratch::default()`). Every contract-named **method** is unchanged.
  2. **`CredResolution` gets a hand-written `Debug`**, not `#[derive(Debug)]`. The contract asks
     for both `#[derive(Debug)] pub struct CredResolution` and "`ResolvedCred` derives nothing
     (`Debug` is deliberately absent so it cannot be printed)" — those cannot both hold. The
     hand-written impl prints the credential's *kind* and never its contents, which satisfies the
     intent of each.

---

- [ ] **Step 1: Write the failing tests for credential resolution**

Create `src/git/creds.rs` with exactly this content — a module header and a test module, no
implementation yet. Nine tests, one per rule in the precedence table.

```rust
//! Credential resolution and the libgit2 callback set.
//!
//! Deliberately two halves that never mix. `resolve` is pure: request credential
//! plus stored credential plus remote in, a `ResolvedCred` *description* out, with
//! no libgit2 anywhere — which is what makes the precedence and `bound_host` rules
//! table-testable. `callbacks` is the adapter: it hands that description to libgit2
//! and does nothing else.
//!
//! The most important property of this file is a negative one, and the test at the
//! bottom is what keeps it true: the host never authenticates with an identity the
//! caller did not supply.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::error::GitErrorCode;

    fn spec(json: &str) -> CredentialSpec {
        serde_json::from_str(json).expect("test fixture must parse")
    }

    const STORED_TOKEN: &str =
        r#"{"kind":"token","token":"stored-token-value","bound_host":"github.com"}"#;
    const STORED_SSH_MEM: &str = r#"{"kind":"ssh_key","private_key":"-----BEGIN KEY-----stored",
        "passphrase":"stored-passphrase","bound_host":"github.com"}"#;

    #[test]
    fn per_call_credential_replaces_the_stored_one_whole() {
        let per_call = spec(r#"{"kind":"token","username":"ci","token":"per-call-token"}"#);
        let stored = spec(STORED_SSH_MEM);
        let r = resolve(
            Some(&per_call),
            Some(&stored),
            Some("https://github.com/a/b.git"),
        )
        .expect("a well-formed token spec resolves");
        assert!(!r.unbound);
        match &r.cred {
            ResolvedCred::Token { username, token } => {
                assert_eq!(username, "ci");
                assert_eq!(token, &Secret::new("per-call-token".to_string()));
            }
            _ => panic!("the per-call token must win outright"),
        }
        // The whole point of "replaces" rather than "merges": no field of the stored
        // ssh credential may survive into the result.
        assert_eq!(r.cred.secrets(), vec!["per-call-token"]);
    }

    #[test]
    fn per_call_kind_none_disables_the_stored_credential() {
        let per_call = spec(r#"{"kind":"none"}"#);
        let stored = spec(STORED_TOKEN);
        let r = resolve(
            Some(&per_call),
            Some(&stored),
            Some("https://github.com/a/b.git"),
        )
        .expect("kind none always resolves");
        assert!(r.cred.is_none());
        assert!(
            !r.unbound,
            "the caller asked for no credential; nothing was withheld"
        );
    }

    #[test]
    fn stored_credential_is_used_when_its_bound_host_matches_the_remote() {
        let stored = spec(STORED_TOKEN);
        let r = resolve(None, Some(&stored), Some("https://github.com/a/b.git"))
            .expect("a well-formed token spec resolves");
        assert!(!r.unbound);
        match &r.cred {
            ResolvedCred::Token { username, token } => {
                assert_eq!(username, "x-access-token");
                assert_eq!(token, &Secret::new("stored-token-value".to_string()));
            }
            _ => panic!("the stored token must be used"),
        }
    }

    #[test]
    fn stored_credential_is_withheld_when_the_remote_host_changed() {
        let stored = spec(STORED_TOKEN);
        for remote in [Some("https://evil.example/a/b.git"), None] {
            let r = resolve(None, Some(&stored), remote).expect("withholding is not an error");
            assert!(
                r.cred.is_none(),
                "{remote:?} must not receive a credential bound to github.com"
            );
            assert!(r.unbound, "{remote:?} must be reported as unbound");
            assert!(r.cred.secrets().is_empty());
        }
    }

    #[test]
    fn a_credential_bound_to_nothing_matches_a_remote_with_no_host() {
        // A local-path remote has no host, so `remote_host` yields None and the
        // registry stores `bound_host: null`. Those two Nones must compare equal or
        // the entire local-path test suite authenticates as unbound.
        let stored = spec(r#"{"kind":"token","token":"local-token-value"}"#);
        let r = resolve(None, Some(&stored), Some("/srv/git/origin.git"))
            .expect("a well-formed token spec resolves");
        assert!(!r.unbound);
        assert!(!r.cred.is_none());
    }

    #[test]
    fn stored_kind_none_is_never_reported_as_unbound() {
        // `kind: "none"` holds no secret, so a host change cannot leak anything.
        // Reporting it as unbound would turn every later auth demand into a
        // misleading `auth_unbound` telling the user to rebind a credential that
        // does not exist.
        let stored = spec(r#"{"kind":"none"}"#);
        let r = resolve(None, Some(&stored), Some("https://evil.example/a/b.git"))
            .expect("kind none always resolves");
        assert!(r.cred.is_none());
        assert!(!r.unbound);
    }

    #[test]
    fn no_credential_anywhere_resolves_to_none() {
        let r = resolve(None, None, Some("https://github.com/a/b.git")).expect("None resolves");
        assert!(r.cred.is_none());
        assert!(!r.unbound);
        assert!(r.cred.username().is_none());
    }

    #[test]
    fn an_ssh_key_resolves_by_which_source_it_names() {
        let on_disk = spec(r#"{"kind":"ssh_key","private_key_path":"/home/u/.ssh/id_ed25519",
            "public_key_path":"/home/u/.ssh/id_ed25519.pub","passphrase":"disk-passphrase"}"#);
        match resolve(Some(&on_disk), None, None)
            .expect("one key source is valid")
            .cred
        {
            ResolvedCred::SshPath {
                username,
                public,
                private,
                pass,
            } => {
                assert_eq!(username, "git");
                assert_eq!(public, Some(PathBuf::from("/home/u/.ssh/id_ed25519.pub")));
                assert_eq!(private, PathBuf::from("/home/u/.ssh/id_ed25519"));
                assert_eq!(pass, Some(Secret::new("disk-passphrase".to_string())));
            }
            _ => panic!("private_key_path must resolve to SshPath"),
        }

        let in_memory = spec(r#"{"kind":"ssh_key","username":"deploy",
            "private_key":"-----BEGIN KEY-----body","public_key":"ssh-ed25519 AAAA"}"#);
        match resolve(Some(&in_memory), None, None)
            .expect("one key source is valid")
            .cred
        {
            ResolvedCred::SshMem {
                username,
                public,
                private,
                pass,
            } => {
                assert_eq!(username, "deploy");
                assert_eq!(public.as_deref(), Some("ssh-ed25519 AAAA"));
                assert_eq!(private, Secret::new("-----BEGIN KEY-----body".to_string()));
                assert!(pass.is_none());
            }
            _ => panic!("private_key must resolve to SshMem"),
        }
    }

    #[test]
    fn an_ssh_key_with_neither_or_both_sources_is_invalid_request() {
        let neither = spec(r#"{"kind":"ssh_key"}"#);
        let both = spec(r#"{"kind":"ssh_key","private_key_path":"/k","private_key":"body"}"#);
        for bad in [neither, both] {
            let e = resolve(Some(&bad), None, None).expect_err("ambiguous key source");
            assert_eq!(e.code(), GitErrorCode::InvalidRequest);
            assert_eq!(e.field.as_deref(), Some("credential.private_key"));
            assert!(
                e.message.contains("exactly one"),
                "the message must say what to fix: {}",
                e.message
            );
        }
    }

    #[test]
    fn a_resolved_credential_reports_its_username_and_its_secrets() {
        // `secrets()` feeds `error::scrub`. Anything it forgets is a value that can
        // reach a job record, `git.log`, or a dialog in clear text.
        let token = spec(r#"{"kind":"token","token":"token-body-value"}"#);
        let resolved = resolve(Some(&token), None, None).expect("valid").cred;
        assert_eq!(resolved.username(), Some("x-access-token"));
        assert_eq!(resolved.secrets(), vec!["token-body-value"]);

        let mem = spec(r#"{"kind":"ssh_key","private_key":"key-body-value",
            "passphrase":"pass-body-value"}"#);
        let resolved = resolve(Some(&mem), None, None).expect("valid").cred;
        assert_eq!(resolved.username(), Some("git"));
        assert_eq!(resolved.secrets(), vec!["key-body-value", "pass-body-value"]);

        assert!(ResolvedCred::None.username().is_none());
        assert!(ResolvedCred::None.secrets().is_empty());
        assert!(ResolvedCred::None.is_none());
    }
}
```

Then register the module. In `src/git/mod.rs`, add `pub mod creds;` to the alphabetical
`pub mod` block:

```rust
pub mod creds;
pub mod error;
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::creds`

Expected: FAIL — the module has no implementation, so it does not compile:

```
error[E0433]: failed to resolve: use of undeclared type `CredentialSpec`
  --> src/git/creds.rs:20:35
   |
20 |     fn spec(json: &str) -> CredentialSpec {
   |                            ^^^^^^^^^^^^^^ use of undeclared type `CredentialSpec`

error[E0425]: cannot find function `resolve` in this scope
  --> src/git/creds.rs:31:17
   |
31 |         let r = resolve(
   |                 ^^^^^^^ not found in this scope

error[E0433]: failed to resolve: use of undeclared type `ResolvedCred`
error[E0433]: failed to resolve: use of undeclared type `Secret`
error[E0433]: failed to resolve: use of undeclared type `PathBuf`
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 14 previous errors
```

- [ ] **Step 3: Implement `ResolvedCred`, `CredResolution` and `resolve`**

Insert this block into `src/git/creds.rs` immediately above the `#[cfg(test)]` line.

```rust
use std::path::PathBuf;

use crate::git::error::GitError;
use crate::git::registry::{self, CredentialSpec};
use crate::git::secret::Secret;

/// A credential in the shape a libgit2 callback can actually offer.
///
/// Derives nothing on purpose. No `Debug`, so it cannot be printed into a test
/// failure or a log line; no `Clone`, so there is exactly one copy per operation;
/// no `Serialize`, so it cannot reach an HTTP response.
pub enum ResolvedCred {
    None,
    Token {
        username: String,
        token: Secret,
    },
    SshPath {
        username: String,
        public: Option<PathBuf>,
        private: PathBuf,
        pass: Option<Secret>,
    },
    SshMem {
        username: String,
        public: Option<String>,
        private: Secret,
        pass: Option<Secret>,
    },
}

impl ResolvedCred {
    /// The username to answer libgit2's USERNAME probe with, and the one every
    /// credential constructor is built from.
    pub fn username(&self) -> Option<&str> {
        match self {
            ResolvedCred::None => None,
            ResolvedCred::Token { username, .. }
            | ResolvedCred::SshPath { username, .. }
            | ResolvedCred::SshMem { username, .. } => Some(username.as_str()),
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, ResolvedCred::None)
    }

    /// Every value that must be replaced by `***` before an error message reaches a
    /// job record, `git-state.json`, `git.log`, or a dialog.
    ///
    /// A private key on disk contributes only its passphrase: the path is not a
    /// secret, and blanking it out of a "no such file" message would leave the user
    /// with nothing to fix.
    pub fn secrets(&self) -> Vec<&str> {
        match self {
            ResolvedCred::None => Vec::new(),
            ResolvedCred::Token { token, .. } => vec![token.expose_for_scrub()],
            ResolvedCred::SshPath { pass, .. } => {
                pass.iter().map(Secret::expose_for_scrub).collect()
            }
            ResolvedCred::SshMem { private, pass, .. } => {
                let mut out = vec![private.expose_for_scrub()];
                out.extend(pass.iter().map(Secret::expose_for_scrub));
                out
            }
        }
    }

    /// For `Debug` only — the variant name, never its contents.
    fn kind(&self) -> &'static str {
        match self {
            ResolvedCred::None => "none",
            ResolvedCred::Token { .. } => "token",
            ResolvedCred::SshPath { .. } => "ssh_key_path",
            ResolvedCred::SshMem { .. } => "ssh_key_memory",
        }
    }
}

pub struct CredResolution {
    pub cred: ResolvedCred,
    /// A stored credential existed but its `bound_host` did not match the remote.
    /// The credential is **not** transmitted; if the remote then demands
    /// authentication the failure is `auth_unbound`, not `auth_missing`.
    pub unbound: bool,
}

/// Hand-written because `ResolvedCred` has no `Debug`: a derived impl here would
/// print the secret into the first `assert_eq!` failure that touched it.
impl std::fmt::Debug for CredResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredResolution")
            .field("cred", &self.cred.kind())
            .field("unbound", &self.unbound)
            .finish()
    }
}

/// Decide which credential this operation may offer.
///
/// The only fallible case is an `ssh_key` naming neither or both key sources.
pub fn resolve(
    per_call: Option<&CredentialSpec>,
    stored: Option<&CredentialSpec>,
    remote: Option<&str>,
) -> Result<CredResolution, GitError> {
    // A credential in the request body replaces the stored one whole. Merging them
    // field by field would build hybrids nobody asked for — a per-call token paired
    // with a stored passphrase — and would cost `{"kind":"none"}` its meaning of
    // "spend nothing on this one call".
    if let Some(spec) = per_call {
        // No `bound_host` check: the caller supplied this credential for this
        // request, so there is no stored secret being replayed at a host it was
        // never meant for. That is the only thing `bound_host` defends against.
        return Ok(CredResolution {
            cred: convert(spec)?,
            unbound: false,
        });
    }

    let Some(spec) = stored else {
        return Ok(CredResolution {
            cred: ResolvedCred::None,
            unbound: false,
        });
    };

    // `kind: "none"` carries no secret, so a host change cannot leak anything.
    // Flagging it unbound would turn a later auth demand into an `auth_unbound`
    // telling the user to rebind a credential that does not exist.
    if spec.is_none() {
        return Ok(CredResolution {
            cred: ResolvedCred::None,
            unbound: false,
        });
    }

    if spec.bound_host() != remote.and_then(registry::remote_host).as_deref() {
        // This is the defence against a loopback caller repointing a repo at a host
        // it controls, triggering a fetch, and being handed the user's token. The
        // operation proceeds unauthenticated instead of failing here, because a
        // public remote does not need the credential at all.
        return Ok(CredResolution {
            cred: ResolvedCred::None,
            unbound: true,
        });
    }

    Ok(CredResolution {
        cred: convert(spec)?,
        unbound: false,
    })
}

/// `CredentialSpec` (what is written down) → `ResolvedCred` (what a callback can
/// offer). Total except for the one ambiguous ssh shape.
fn convert(spec: &CredentialSpec) -> Result<ResolvedCred, GitError> {
    match spec {
        CredentialSpec::None => Ok(ResolvedCred::None),
        CredentialSpec::Token {
            username, token, ..
        } => Ok(ResolvedCred::Token {
            username: username.clone(),
            token: token.clone(),
        }),
        CredentialSpec::SshKey {
            username,
            private_key_path,
            private_key,
            public_key_path,
            public_key,
            passphrase,
            ..
        } => match (private_key_path, private_key) {
            (Some(path), None) => Ok(ResolvedCred::SshPath {
                username: username.clone(),
                public: public_key_path.clone(),
                private: path.clone(),
                pass: passphrase.clone(),
            }),
            (None, Some(key)) => Ok(ResolvedCred::SshMem {
                username: username.clone(),
                public: public_key.clone(),
                private: key.clone(),
                pass: passphrase.clone(),
            }),
            // Guessing which one wins would mean silently ignoring a key the user
            // wrote down and believes is in use.
            _ => Err(GitError::invalid_request(
                "credential.private_key",
                "an ssh_key credential needs exactly one of private_key_path or private_key",
            )),
        },
    }
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::creds`

Expected: PASS — `test result: ok. 10 passed; 0 failed`.

- [ ] **Step 5: Commit**

```bash
git add src/git/creds.rs src/git/mod.rs
git commit -m "feat(git): resolve credentials from the request and the registry

A credential in the request body replaces the stored one whole rather than
merging field by field, and a stored credential is withheld whenever the
remote's host no longer matches the bound_host it was recorded against."
```

---

- [ ] **Step 6: Write the failing tests for SSH host-key checking**

libgit2 constructs `git2::Cert` values from raw pointers and its `Binding` trait is private, so
a test can never build one. The decision logic therefore lives in a private `decide` that takes
what `check` extracted; `check` itself is a four-line extraction. Add these tests to the
`mod tests` block in `src/git/creds.rs`. They need no new `use` in the test module —
`SshHostKeyPolicy` and `HostKeyChecker` arrive through `use super::*` once Step 8 imports them
at module level.

```rust
    const ZERO_FP: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    #[test]
    fn fingerprints_are_unpadded_standard_base64() {
        assert_eq!(fingerprint_sha256(&[0u8; 32]), ZERO_FP);
        let mut ramp = [0u8; 32];
        for (i, b) in ramp.iter_mut().enumerate() {
            *b = i as u8;
        }
        assert_eq!(
            fingerprint_sha256(&ramp),
            "SHA256:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"
        );
        assert_eq!(
            fingerprint_sha256(&[0xffu8; 32]),
            "SHA256://////////////////////////////////////////8"
        );
        // 43 characters: 32 bytes of base64 is 44 with padding, and ssh-keygen
        // prints the unpadded form, so a pinned value copied from `ssh-keyscan`
        // compares equal.
        assert_eq!(fingerprint_sha256(&[0u8; 32]).len(), "SHA256:".len() + 43);
    }

    #[test]
    fn a_tls_certificate_passes_through_instead_of_being_approved() {
        // Returning CertificateOk for a TLS cert would OVERRIDE libgit2's own
        // verification — including a rejection it had already decided on. This is
        // the difference between "we have no opinion" and "we vouch for it".
        for policy in [SshHostKeyPolicy::Tofu, SshHostKeyPolicy::Accept] {
            let checker = HostKeyChecker::new(policy, None);
            assert!(matches!(
                checker.decide(HostKeyHash::NotSsh, "github.com"),
                Ok(git2::CertificateCheckStatus::CertificatePassthrough)
            ));
            assert!(
                checker.learned().is_none(),
                "a TLS certificate is not a host key and must never be pinned"
            );
        }
    }

    #[test]
    fn tofu_learns_an_unpinned_host_key_and_accepts_it() {
        let checker = HostKeyChecker::new(SshHostKeyPolicy::Tofu, None);
        assert!(matches!(
            checker.decide(HostKeyHash::Sha256(&[0u8; 32]), "github.com"),
            Ok(git2::CertificateCheckStatus::CertificateOk)
        ));
        assert_eq!(checker.learned().as_deref(), Some(ZERO_FP));
    }

    #[test]
    fn tofu_accepts_a_matching_pin_without_relearning_it() {
        let checker = HostKeyChecker::new(SshHostKeyPolicy::Tofu, Some(ZERO_FP.to_string()));
        assert!(matches!(
            checker.decide(HostKeyHash::Sha256(&[0u8; 32]), "github.com"),
            Ok(git2::CertificateCheckStatus::CertificateOk)
        ));
        assert!(
            checker.learned().is_none(),
            "nothing new was learned, so after_job must not rewrite git-state.json"
        );
    }

    #[test]
    fn tofu_refuses_a_changed_host_key_and_says_how_to_re_pin() {
        let checker = HostKeyChecker::new(SshHostKeyPolicy::Tofu, Some(ZERO_FP.to_string()));
        let e = checker
            .decide(HostKeyHash::Sha256(&[0xffu8; 32]), "github.com")
            .expect_err("a changed host key is a hard failure");
        assert_eq!(e.code(), git2::ErrorCode::Certificate);
        assert_eq!(e.class(), git2::ErrorClass::Ssh);
        assert!(e.message().contains("github.com"), "{}", e.message());
        assert!(e.message().contains(ZERO_FP), "{}", e.message());
        assert!(
            e.message().contains("ssh_host_fingerprint"),
            "the message is the only place the user is told how to recover: {}",
            e.message()
        );
        assert!(checker.learned().is_none());
    }

    #[test]
    fn tofu_refuses_a_host_key_with_no_sha256_hash() {
        let checker = HostKeyChecker::new(SshHostKeyPolicy::Tofu, None);
        let e = checker
            .decide(HostKeyHash::Missing, "github.com")
            .expect_err("nothing to pin means nothing to trust");
        assert!(e.message().contains("SHA-256"), "{}", e.message());
    }

    #[test]
    fn the_accept_policy_short_circuits_before_any_hashing() {
        let checker = HostKeyChecker::new(SshHostKeyPolicy::Accept, Some(ZERO_FP.to_string()));
        // Even a key that would fail the pin, and even one with no hash at all.
        assert!(matches!(
            checker.decide(HostKeyHash::Sha256(&[0xffu8; 32]), "github.com"),
            Ok(git2::CertificateCheckStatus::CertificateOk)
        ));
        assert!(matches!(
            checker.decide(HostKeyHash::Missing, "github.com"),
            Ok(git2::CertificateCheckStatus::CertificateOk)
        ));
        assert!(checker.learned().is_none());
    }
```

- [ ] **Step 7: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::creds`

Expected: FAIL — compile error:

```
error[E0425]: cannot find function `fingerprint_sha256` in this scope
  --> src/git/creds.rs:NNN:20
error[E0433]: failed to resolve: use of undeclared type `HostKeyChecker`
error[E0433]: failed to resolve: use of undeclared type `HostKeyHash`
error[E0433]: failed to resolve: use of undeclared type `SshHostKeyPolicy`
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 20 previous errors
```

- [ ] **Step 8: Implement `fingerprint_sha256` and `HostKeyChecker`**

Add to the `use` block at the top of `src/git/creds.rs`:

```rust
use std::cell::RefCell;

use crate::config::SshHostKeyPolicy;
use crate::git::util::b64_nopad;
```

Then insert this block immediately above the `#[cfg(test)]` line.

```rust
/// `SHA256:<unpadded standard base64>` — byte-identical to what `ssh-keygen -lf`
/// and `ssh-keyscan` print, so a value pasted from either compares equal.
pub fn fingerprint_sha256(raw: &[u8; 32]) -> String {
    format!("SHA256:{}", b64_nopad(raw))
}

/// What `check` managed to extract from a `git2::Cert`.
///
/// Three states, not two: "this is not an SSH host key" and "this is an SSH host
/// key with no SHA-256 hash" lead to opposite answers, and collapsing them into one
/// `Option` would make a TLS certificate look like a hashless host key. libgit2
/// builds `Cert` values from raw pointers through a private trait, so this enum is
/// also the only way the decision below can be tested.
enum HostKeyHash<'a> {
    /// A TLS certificate. libgit2 has already verified it.
    NotSsh,
    /// An SSH host key the remote offered without a SHA-256 hash.
    Missing,
    Sha256(&'a [u8; 32]),
}

/// Trust-on-first-use for SSH host keys.
///
/// With no `certificate_check` callback at all, libssh2 accepts **any** host key:
/// a silent man-in-the-middle on every SSH sync, with no error and no log line.
pub struct HostKeyChecker {
    pub policy: SshHostKeyPolicy,
    /// The fingerprint recorded in `git-state.json` for this repo, if any.
    pub pinned: Option<String>,
    /// Set when an unpinned key is accepted. `GitService::after_job` persists it;
    /// the blocking thread never writes the file itself.
    learned: RefCell<Option<String>>,
}

impl HostKeyChecker {
    pub fn new(policy: SshHostKeyPolicy, pinned: Option<String>) -> HostKeyChecker {
        HostKeyChecker {
            policy,
            pinned,
            learned: RefCell::new(None),
        }
    }

    pub fn check(
        &self,
        cert: &git2::Cert<'_>,
        host: &str,
    ) -> Result<git2::CertificateCheckStatus, git2::Error> {
        let hash = match cert.as_hostkey() {
            None => HostKeyHash::NotSsh,
            Some(hk) => match hk.hash_sha256() {
                Some(sha) => HostKeyHash::Sha256(sha),
                None => HostKeyHash::Missing,
            },
        };
        self.decide(hash, host)
    }

    pub fn learned(&self) -> Option<String> {
        self.learned.borrow().clone()
    }

    fn decide(
        &self,
        hash: HostKeyHash<'_>,
        host: &str,
    ) -> Result<git2::CertificateCheckStatus, git2::Error> {
        // libgit2 has already checked a TLS certificate against the system trust
        // store by the time this runs. `CertificateOk` would OVERRIDE that verdict,
        // turning a rejected certificate into an accepted one — passthrough hands
        // the decision straight back. There is no way to disable TLS verification
        // from the config or the API, and there is not going to be one.
        if matches!(hash, HostKeyHash::NotSsh) {
            return Ok(git2::CertificateCheckStatus::CertificatePassthrough);
        }
        if self.policy == SshHostKeyPolicy::Accept {
            return Ok(git2::CertificateCheckStatus::CertificateOk);
        }
        let HostKeyHash::Sha256(sha) = hash else {
            return Err(git2::Error::from_str(
                "remote offered no SHA-256 host key hash",
            ));
        };
        let fingerprint = fingerprint_sha256(sha);
        match &self.pinned {
            None => {
                *self.learned.borrow_mut() = Some(fingerprint);
                Ok(git2::CertificateCheckStatus::CertificateOk)
            }
            Some(pinned) if *pinned == fingerprint => Ok(git2::CertificateCheckStatus::CertificateOk),
            Some(pinned) => Err(git2::Error::new(
                git2::ErrorCode::Certificate,
                git2::ErrorClass::Ssh,
                format!(
                    "host key for {host} changed (pinned {pinned}, got {fingerprint}); clear \
                     ssh_host_fingerprint for this repo in git-state.json to re-pin"
                ),
            )),
        }
    }
}
```

- [ ] **Step 9: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::creds`

Expected: PASS — `test result: ok. 17 passed; 0 failed`.

- [ ] **Step 10: Commit**

```bash
git add src/git/creds.rs
git commit -m "feat(git): pin ssh host keys on first use

Without a certificate_check callback libssh2 accepts any host key, so every
SSH sync is silently MITM-able. TLS certificates return CertificatePassthrough
rather than CertificateOk, which would override libgit2's own verification."
```

---

- [ ] **Step 11: Write the failing tests for credential-type matching**

`allowed` is a **bitmask**. A remote that supports several mechanisms sets several bits at once,
so every arm must test it with `contains`; comparing with `==` matches only the
single-mechanism case and silently offers nothing to every server that gives a choice. Add these
tests to `mod tests` in `src/git/creds.rs`.

```rust
    fn resolved(json: &str) -> ResolvedCred {
        resolve(Some(&spec(json)), None, None)
            .expect("test fixture must resolve")
            .cred
    }

    #[test]
    fn a_token_answers_only_a_plaintext_userpass_demand() {
        let cred = resolved(r#"{"kind":"token","token":"token-body-value"}"#);
        assert!(matches!(
            choose(&cred, git2::CredentialType::USER_PASS_PLAINTEXT),
            CredChoice::Token { .. }
        ));
        assert!(matches!(
            choose(&cred, git2::CredentialType::SSH_KEY),
            CredChoice::Missing
        ));
    }

    #[test]
    fn an_in_memory_key_answers_either_ssh_demand() {
        let cred = resolved(r#"{"kind":"ssh_key","private_key":"key-body-value"}"#);
        for allowed in [
            git2::CredentialType::SSH_MEMORY,
            git2::CredentialType::SSH_KEY,
        ] {
            assert!(matches!(
                choose(&cred, allowed),
                CredChoice::SshMemory { .. }
            ));
        }
        assert!(matches!(
            choose(&cred, git2::CredentialType::USER_PASS_PLAINTEXT),
            CredChoice::Missing
        ));
    }

    #[test]
    fn an_on_disk_key_answers_only_ssh_key() {
        let cred = resolved(r#"{"kind":"ssh_key","private_key_path":"/home/u/.ssh/id_ed25519"}"#);
        assert!(matches!(
            choose(&cred, git2::CredentialType::SSH_KEY),
            CredChoice::SshPath { .. }
        ));
        // SSH_MEMORY alone means libssh2 wants the key material inline; a path
        // cannot satisfy it.
        assert!(matches!(
            choose(&cred, git2::CredentialType::SSH_MEMORY),
            CredChoice::Missing
        ));
    }

    #[test]
    fn a_multi_mechanism_demand_is_a_bitmask_not_a_value() {
        // A server offering both is the case `allowed == SSH_KEY` would miss. This
        // test is the one that fails if anybody rewrites `contains` as `==`.
        let both = git2::CredentialType::USER_PASS_PLAINTEXT | git2::CredentialType::SSH_KEY;
        assert!(matches!(
            choose(
                &resolved(r#"{"kind":"token","token":"token-body-value"}"#),
                both
            ),
            CredChoice::Token { .. }
        ));
        assert!(matches!(
            choose(
                &resolved(r#"{"kind":"ssh_key","private_key_path":"/k"}"#),
                both
            ),
            CredChoice::SshPath { .. }
        ));
    }

    #[test]
    fn nothing_answers_a_demand_when_no_credential_was_resolved() {
        let everything = git2::CredentialType::all();
        assert!(matches!(
            choose(&ResolvedCred::None, everything),
            CredChoice::Missing
        ));
    }

    #[test]
    fn the_attempt_budget_is_small_enough_to_fail_fast() {
        // libgit2 re-invokes the credentials callback up to fifteen times before it
        // gives up. Two offers is one round-trip more than any mechanism here needs.
        assert_eq!(MAX_CRED_ATTEMPTS, 2);
    }
```

- [ ] **Step 12: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::creds`

Expected: FAIL — compile error:

```
error[E0425]: cannot find function `choose` in this scope
error[E0433]: failed to resolve: use of undeclared type `CredChoice`
error[E0425]: cannot find value `MAX_CRED_ATTEMPTS` in this scope
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 9 previous errors
```

- [ ] **Step 13: Implement `MAX_CRED_ATTEMPTS`, `CredChoice` and `choose`**

Add `use std::path::Path;` to the `std::path` import at the top of `src/git/creds.rs` so it
reads `use std::path::{Path, PathBuf};`. Then insert this block immediately above the
`#[cfg(test)]` line.

```rust
/// How many credentials this host offers a remote before it gives up.
///
/// libgit2 keeps re-invoking the credentials callback until the callback itself
/// returns an error — up to fifteen times — so a wrong token becomes fifteen
/// round-trips and a generic transport failure. Two covers every mechanism here:
/// the first offer, plus one for a server that renegotiates.
pub const MAX_CRED_ATTEMPTS: u32 = 2;

/// Which resolved credential satisfies the type mask libgit2 offered.
///
/// Borrows rather than clones, so no secret is copied on the way to the callback.
#[derive(Debug, PartialEq, Eq)]
enum CredChoice<'a> {
    Token {
        username: &'a str,
        token: &'a Secret,
    },
    SshMemory {
        username: &'a str,
        public: Option<&'a str>,
        private: &'a Secret,
        pass: Option<&'a Secret>,
    },
    SshPath {
        username: &'a str,
        public: Option<&'a Path>,
        private: &'a Path,
        pass: Option<&'a Secret>,
    },
    /// Nothing resolved for this repo can answer what the remote asked for.
    Missing,
}

/// `allowed` is a **bitmask**: a remote that supports several mechanisms sets
/// several bits in one call. Every arm tests it with `contains` — `==` would match
/// only the single-mechanism case and silently offer nothing to every server that
/// gives the client a choice.
fn choose<'a>(cred: &'a ResolvedCred, allowed: git2::CredentialType) -> CredChoice<'a> {
    match cred {
        ResolvedCred::Token { username, token }
            if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) =>
        {
            CredChoice::Token {
                username: username.as_str(),
                token,
            }
        }
        // libssh2 asks for SSH_MEMORY when it wants the key inline and SSH_KEY when
        // it will read a file; material we already hold in memory satisfies both.
        ResolvedCred::SshMem {
            username,
            public,
            private,
            pass,
        } if allowed.contains(git2::CredentialType::SSH_MEMORY)
            || allowed.contains(git2::CredentialType::SSH_KEY) =>
        {
            CredChoice::SshMemory {
                username: username.as_str(),
                public: public.as_deref(),
                private,
                pass: pass.as_ref(),
            }
        }
        ResolvedCred::SshPath {
            username,
            public,
            private,
            pass,
        } if allowed.contains(git2::CredentialType::SSH_KEY) => CredChoice::SshPath {
            username: username.as_str(),
            public: public.as_deref(),
            private: private.as_path(),
            pass: pass.as_ref(),
        },
        _ => CredChoice::Missing,
    }
}
```

- [ ] **Step 14: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::creds`

Expected: PASS — `test result: ok. 23 passed; 0 failed`.

- [ ] **Step 15: Commit**

```bash
git add src/git/creds.rs
git commit -m "feat(git): match libgit2's credential-type bitmask

allowed is a set of bits, not a value: a server offering both HTTPS basic auth
and SSH sets two of them in one call. Every arm tests it with contains()."
```

---

- [ ] **Step 16: Write the failing tests for the operation request types**

`creds::callbacks` takes `&OpCtx`, so the context layer of `ops.rs` has to land before the
callback set can compile. This step starts that file with the request-shaped types.

Create `src/git/ops.rs` with exactly this content:

```rust
//! Git operations — the blocking half of the service.
//!
//! Every verb here runs inside one `spawn_blocking` closure and keeps its
//! `git2::Repository` alive for the whole job. This file holds the *context* layer:
//! the request the caller made, the identity a commit will carry, the outcome the
//! job records, and the `OpCtx` that libgit2's callbacks write through. The verbs
//! themselves land in later slices.
//!
//! Nothing under `src/git/` may name `App`, `Children`, `chrome_generation`,
//! `server_generation`, `rt.enter()`, or `tokio::process`. The only channel from
//! here back into the event loop is the job outcome.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_operation_request_pushes_by_default() {
        // `sync` means "make the remote match". A derived Default would give
        // `push: false` and turn every sync into a silent local-only commit.
        assert!(OpRequest::default().push);
        assert!(OpRequest::default().restart_children.is_none());
        assert!(!OpRequest::default().force);
        assert!(!OpRequest::default().allow_empty);
        assert!(!OpRequest::default().commit_local);
    }

    #[test]
    fn timer_and_tray_syncs_never_ask_for_a_child_restart() {
        // A background pull must not yank the UI out from under someone mid-edit.
        // A settings change still restarts children, but that is `after_job`'s
        // decision from `settings_changed`, not this flag.
        for req in [OpRequest::auto(), OpRequest::manual()] {
            assert_eq!(req.restart_children, Some(false));
            assert!(req.request_id.is_none());
            assert!(req.push, "an unattended sync still pushes");
        }
    }

    #[test]
    fn an_identity_never_leaves_the_hostname_empty_or_padded() {
        let id = Identity::new("Chrome Host App", "com.example.chromehost");
        assert_eq!(id.app_name, "Chrome Host App");
        assert_eq!(id.identifier, "com.example.chromehost");
        // The hostname is the right-hand side of the fallback author email, so an
        // empty one produces `com.example.chromehost@` and git refuses the commit.
        assert!(!id.hostname.is_empty());
        assert_eq!(id.hostname.trim(), id.hostname);
    }

    #[test]
    fn a_branch_request_distinguishes_absent_upstream_from_null_upstream() {
        // Three states, because "no upstream key in the body" (leave it alone) and
        // `"upstream": null` (unset it) are different instructions.
        let leave = BranchRequest {
            name: "main".to_string(),
            create: false,
            from: None,
            checkout: true,
            upstream: None,
        };
        let unset = BranchRequest {
            upstream: Some(None),
            ..leave.clone()
        };
        let set = BranchRequest {
            upstream: Some(Some("origin/main".to_string())),
            ..leave.clone()
        };
        assert!(leave.upstream.is_none());
        assert_eq!(unset.upstream, Some(None));
        assert_eq!(set.upstream, Some(Some("origin/main".to_string())));
    }
}
```

Then register the module. In `src/git/mod.rs`, add `pub mod ops;` to the alphabetical
`pub mod` block:

```rust
pub mod jobs;
pub mod ops;
pub mod registry;
```

- [ ] **Step 17: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::ops`

Expected: FAIL — compile error:

```
error[E0433]: failed to resolve: use of undeclared type `OpRequest`
  --> src/git/ops.rs:20:17
   |
20 |         assert!(OpRequest::default().push);
   |                 ^^^^^^^^^ use of undeclared type `OpRequest`

error[E0433]: failed to resolve: use of undeclared type `Identity`
error[E0433]: failed to resolve: use of undeclared type `BranchRequest`
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 11 previous errors
```

- [ ] **Step 18: Implement the identity and request types**

Insert this block into `src/git/ops.rs` immediately above the `#[cfg(test)]` line.

```rust
use std::path::PathBuf;

use crate::config::SettingsSection;
use crate::git::registry::{AuthorSpec, CredentialSpec};

/// Who the host says it is when it writes a commit nobody typed.
#[derive(Debug, Clone)]
pub struct Identity {
    pub app_name: String,
    pub identifier: String,
    pub hostname: String,
}

impl Identity {
    /// `gethostname` yields an `OsString`. A non-UTF-8 hostname is converted
    /// lossily rather than refused: the only thing it feeds is the fallback author
    /// email, and a mangled address beats a job that cannot commit at all.
    pub fn new(app_name: &str, identifier: &str) -> Identity {
        let host = gethostname::gethostname()
            .to_string_lossy()
            .trim()
            .to_string();
        Identity {
            app_name: app_name.to_string(),
            identifier: identifier.to_string(),
            hostname: if host.is_empty() {
                "localhost".to_string()
            } else {
                host
            },
        }
    }
}

/// What `sync_settings` needs to validate and write `settings.json`, cloned into
/// every `OpCtx` so the blocking thread never reaches back into `GitService`.
#[derive(Clone)]
pub struct SettingsCtx {
    pub schema: SettingsSection,
    pub settings_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct BranchRequest {
    pub name: String,
    pub create: bool,
    pub from: Option<String>,
    pub checkout: bool,
    /// `None` leaves the upstream alone, `Some(None)` unsets it, `Some(Some(x))`
    /// sets it to `x`. Two states would conflate "the body had no upstream key"
    /// with "the body said `upstream: null`", which are opposite instructions.
    pub upstream: Option<Option<String>>,
}

#[derive(Debug, Clone)]
pub struct ResetRequest {
    pub to: String,
    pub clean_untracked: bool,
    pub confirm: bool,
}

/// Everything one HTTP body, tray click, or timer tick can say about an operation.
/// One struct for all eight verbs: each reads the fields that apply to it.
#[derive(Debug, Clone)]
pub struct OpRequest {
    pub request_id: Option<String>,
    /// Replaces the stored credential wholly for this one call. Never persisted.
    pub credential: Option<CredentialSpec>,
    pub message: Option<String>,
    pub author: Option<AuthorSpec>,
    pub paths: Option<Vec<String>>,
    pub allow_empty: bool,
    /// `sync` only.
    pub push: bool,
    pub force: bool,
    /// `pull` only.
    pub commit_local: bool,
    /// `None` falls back to the repo's `restart_children_on_pull`.
    pub restart_children: Option<bool>,
    pub branch: Option<BranchRequest>,
    pub reset: Option<ResetRequest>,
}

impl Default for OpRequest {
    /// Hand-written for one field. `push` defaults to **true** because `sync` means
    /// "make the remote match"; a derived `false` would turn every sync into a
    /// local-only commit and nobody would notice until they needed the remote.
    fn default() -> OpRequest {
        OpRequest {
            request_id: None,
            credential: None,
            message: None,
            author: None,
            paths: None,
            allow_empty: false,
            push: true,
            force: false,
            commit_local: false,
            restart_children: None,
            branch: None,
            reset: None,
        }
    }
}

impl OpRequest {
    /// A timer-driven sync. Never asks for a child restart: a background pull must
    /// not yank the UI out from under someone mid-edit. A settings change still
    /// restarts, but that is `after_job`'s decision and not this flag.
    pub fn auto() -> OpRequest {
        OpRequest {
            restart_children: Some(false),
            ..OpRequest::default()
        }
    }

    /// Tray "Sync now" and the quit-time sync. The same shape as `auto` today, kept
    /// separate because only this one is ever attached to a user gesture and only
    /// this one is a candidate for a confirmation prompt.
    pub fn manual() -> OpRequest {
        OpRequest::auto()
    }
}
```

- [ ] **Step 19: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::ops`

Expected: PASS — `test result: ok. 4 passed; 0 failed`.

- [ ] **Step 20: Commit**

```bash
git add src/git/ops.rs src/git/mod.rs
git commit -m "feat(git): add the operation request and identity types

OpRequest::default() sets push = true by hand: a derived Default would make
every sync local-only. BranchRequest.upstream is a double Option so an absent
key and an explicit null stay distinguishable."
```

---

- [ ] **Step 21: Write the failing tests for the operation outcome**

`OpOutcome` becomes the `result` object of a finished job, and clients read it by key. Every
non-skipped field is serialized unconditionally — nulls included — so the shape never changes
between a `commit` and a `sync`. Add these tests to `mod tests` in `src/git/ops.rs`.

```rust
    #[test]
    fn an_outcome_serializes_a_fixed_shape_with_no_secrets_in_it() {
        let value = serde_json::to_value(OpOutcome::new("committed", "main"))
            .expect("OpOutcome must serialize");
        let obj = value.as_object().expect("an object");
        assert_eq!(obj["outcome"], "committed");
        assert_eq!(obj["branch"], "main");
        // Nulls are present, not omitted: a client reading `result.head_after`
        // must not have to distinguish "absent" from "null".
        assert_eq!(obj["head_after"], serde_json::Value::Null);
        assert_eq!(obj["merge"], serde_json::Value::Null);
        assert_eq!(obj["files_committed"], 0);
        assert_eq!(obj["warnings"], serde_json::json!([]));
        assert_eq!(
            obj.len(),
            19,
            "the result object has a fixed shape; adding a field is an API change: {obj:?}"
        );
        // Host-internal bookkeeping, not part of the wire contract. A learned host
        // key fingerprint in particular must not be published to every caller.
        assert!(!obj.contains_key("settings_changed"));
        assert!(!obj.contains_key("learned_fingerprint"));
    }

    #[test]
    fn a_merge_report_carries_the_hint_that_recovers_the_losing_side() {
        let report = MergeReport {
            kind: "merge_commit",
            merge_commit: Some("abc123".to_string()),
            conflicts_resolved: vec!["notes/f.md".to_string()],
            recover_hint: Some("git show abc123^2:notes/f.md".to_string()),
        };
        let value = serde_json::to_value(&report).expect("MergeReport must serialize");
        assert_eq!(value["kind"], "merge_commit");
        assert_eq!(value["conflicts_resolved"], serde_json::json!(["notes/f.md"]));
        assert_eq!(value["recover_hint"], "git show abc123^2:notes/f.md");

        let rejected = SettingsRejected {
            error: "port must be between 1 and 65535".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&rejected).expect("SettingsRejected must serialize")["error"],
            "port must be between 1 and 65535"
        );
    }
```

- [ ] **Step 22: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::ops`

Expected: FAIL — compile error:

```
error[E0433]: failed to resolve: use of undeclared type `OpOutcome`
error[E0422]: cannot find struct, variant or union type `MergeReport` in this scope
error[E0422]: cannot find struct, variant or union type `SettingsRejected` in this scope
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 5 previous errors
```

- [ ] **Step 23: Implement the outcome types**

Add to the `use` block at the top of `src/git/ops.rs`:

```rust
use serde::Serialize;

use crate::git::registry::Warning;
```

(`crate::git::registry::{AuthorSpec, CredentialSpec}` becomes
`crate::git::registry::{AuthorSpec, CredentialSpec, Warning}`.)

Then insert this block immediately above the `#[cfg(test)]` line.

```rust
/// What a merge did, in the shape the job result publishes it.
#[derive(Debug, Clone, Serialize)]
pub struct MergeReport {
    /// `up_to_date` | `no_remote_branch` | `adopted_remote` | `fast_forward` |
    /// `merge_commit` | `merge_commit_unrelated`.
    pub kind: &'static str,
    pub merge_commit: Option<String>,
    pub conflicts_resolved: Vec<String>,
    /// The command that reads back the side that lost a conflict. Prefer-local
    /// resolution is only defensible because the other version is still reachable,
    /// so the outcome says out loud how to reach it.
    pub recover_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingsRejected {
    pub error: String,
}

/// The `result` object of a finished job.
///
/// Every field that is not `#[serde(skip)]` is serialized unconditionally, nulls
/// included, so a client reading `result.head_after` never has to tell "absent"
/// from "null" and the shape does not vary between verbs.
#[derive(Debug, Clone, Serialize)]
pub struct OpOutcome {
    /// One of the values in the outcome vocabulary: `up_to_date`, `no_changes`,
    /// `committed`, `fast_forward`, `merged`, `merged_unrelated`, `cloned`,
    /// `initialized`, `pushed`, `reset`, `checked_out`, `branch_created`.
    pub outcome: &'static str,
    pub branch: String,
    pub head_before: Option<String>,
    pub head_after: Option<String>,
    pub committed: bool,
    pub commit: Option<String>,
    pub files_committed: usize,
    pub fetched: bool,
    pub fetched_head: Option<String>,
    pub merge: Option<MergeReport>,
    pub pushed: bool,
    pub force_pushed: bool,
    pub upstream_set: bool,
    pub ahead: usize,
    pub behind: usize,
    pub settings_synced: bool,
    pub settings_rejected: Option<SettingsRejected>,
    /// What `after_job` decided, not what the caller asked for.
    pub restart_requested: bool,
    pub warnings: Vec<Warning>,
    /// Drives the settings-change restart. Host-internal: publishing it would
    /// invite clients to build their own restart logic on top of it.
    #[serde(skip)]
    pub settings_changed: bool,
    /// A host key accepted on first use, on its way to `git-state.json`. Never
    /// published — it is per-host state, not a property of the operation.
    #[serde(skip)]
    pub learned_fingerprint: Option<String>,
}

impl OpOutcome {
    pub fn new(outcome: &'static str, branch: &str) -> OpOutcome {
        OpOutcome {
            outcome,
            branch: branch.to_string(),
            head_before: None,
            head_after: None,
            committed: false,
            commit: None,
            files_committed: 0,
            fetched: false,
            fetched_head: None,
            merge: None,
            pushed: false,
            force_pushed: false,
            upstream_set: false,
            ahead: 0,
            behind: 0,
            settings_synced: false,
            settings_rejected: None,
            restart_requested: false,
            warnings: Vec::new(),
            settings_changed: false,
            learned_fingerprint: None,
        }
    }
}
```

- [ ] **Step 24: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::ops`

Expected: PASS — `test result: ok. 6 passed; 0 failed`.

- [ ] **Step 25: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): add the operation outcome types

Every non-skipped field serializes unconditionally so result has a fixed shape
across verbs. settings_changed and learned_fingerprint are host-internal and
carry the #[serde(skip)] that keeps them off the wire."
```

---

- [ ] **Step 26: Write the failing tests for `OpCtx` and its progress plumbing**

`OpCtx` holds a real `JobSlot`, and `JobSlot` has no constructor outside `JobStore`, so the
tests need a small fixture. It lands here and every later `ops.rs` test reuses it. Add to
`mod tests` in `src/git/ops.rs`.

These three `use` lines are the complete list: everything else the fixture names
(`HostKeyChecker`, `ResolvedCred`, `RepoDef`, `AuthorSpec`, `Phase`, `JobOp`, `PathBuf`,
`Instant`, `AbortReason`) arrives through `use super::*` once Step 28 imports it at module
level. Importing any of them here as well would be a redundant import.

```rust
    use crate::config::SshHostKeyPolicy;
    use crate::git::jobs::{Admission, JobStore, RepoLease};
    use std::time::Duration;

    fn def(json: &str) -> RepoDef {
        serde_json::from_str(json).expect("test fixture must parse")
    }

    struct Fixture {
        ctx: OpCtx,
        /// Dropping the lease finishes the job the slot belongs to, so the test has
        /// to outlive it.
        _lease: RepoLease,
    }

    fn fixture(def_json: &str, cred: ResolvedCred, cred_unbound: bool) -> Fixture {
        let store = JobStore::new("0000dead".to_string());
        let admitted = store
            .admit("notes", JobOp::Sync, None)
            .expect("a fresh store is not draining");
        let Admission::Started(slot, lease) = admitted else {
            panic!("a fresh store must admit the first job");
        };
        let abort = slot.abort.clone();
        Fixture {
            ctx: OpCtx {
                tree: PathBuf::from("/nonexistent/repos/notes"),
                def: def(def_json),
                cred,
                cred_unbound,
                request: OpRequest::default(),
                identity: Identity {
                    app_name: "Test App".to_string(),
                    identifier: "com.example.test".to_string(),
                    hostname: "testhost".to_string(),
                },
                host_key: HostKeyChecker::new(SshHostKeyPolicy::Tofu, None),
                abort,
                slot,
                deadline: Instant::now() + Duration::from_secs(60),
                settings: None,
                default_author: AuthorSpec::default(),
                scratch: OpScratch::default(),
            },
            _lease: lease,
        }
    }

    fn plain() -> Fixture {
        fixture(r#"{"id":"notes"}"#, ResolvedCred::None, false)
    }

    #[test]
    fn a_phase_change_reaches_the_job_slot() {
        let fx = plain();
        fx.ctx.phase(Phase::Cloning);
        assert_eq!(fx.ctx.slot.phase(), Phase::Cloning);
        fx.ctx.phase(Phase::Merging);
        assert_eq!(fx.ctx.slot.phase(), Phase::Merging);
    }

    #[test]
    fn a_context_is_aborted_by_the_flag_or_by_its_own_deadline() {
        let fx = plain();
        assert!(!fx.ctx.aborted());

        let fx = plain();
        fx.ctx.abort.abort(AbortReason::Shutdown);
        assert!(fx.ctx.aborted());
        assert_eq!(fx.ctx.abort.reason(), AbortReason::Shutdown);

        // The watchdog trips the same flag from the runtime, but a transfer that
        // stalls without ever waking tokio must still stop itself — and it must
        // record `Timeout`, or `classify` reports `network_failed` for a deadline.
        let mut fx = plain();
        fx.ctx.deadline = Instant::now() - Duration::from_secs(1);
        assert!(fx.ctx.aborted());
        assert_eq!(fx.ctx.abort.reason(), AbortReason::Timeout);
    }

    #[test]
    fn transfer_percent_never_divides_by_zero_or_exceeds_a_hundred() {
        // `total_objects` is 0 until the remote finishes counting, and a server is
        // free to send more objects than it announced.
        assert_eq!(percent_of(0, 0), 0);
        assert_eq!(percent_of(17, 0), 0);
        assert_eq!(percent_of(0, 200), 0);
        assert_eq!(percent_of(50, 200), 25);
        assert_eq!(percent_of(200, 200), 100);
        assert_eq!(percent_of(500, 200), 100);
        assert_eq!(percent_of(usize::MAX, 3), 100);
    }
```

- [ ] **Step 27: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::ops`

Expected: FAIL — compile error:

```
error[E0433]: failed to resolve: use of undeclared type `OpCtx`
error[E0433]: failed to resolve: use of undeclared type `OpScratch`
error[E0433]: failed to resolve: use of undeclared type `ResolvedCred`
error[E0433]: failed to resolve: use of undeclared type `HostKeyChecker`
error[E0433]: failed to resolve: use of undeclared type `RepoDef`
error[E0433]: failed to resolve: use of undeclared type `AbortReason`
error[E0433]: failed to resolve: use of undeclared type `Phase`
error[E0433]: failed to resolve: use of undeclared type `Instant`
error[E0425]: cannot find function `percent_of` in this scope
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 24 previous errors
```

- [ ] **Step 28: Implement `OpScratch`, `OpCtx`, and the progress plumbing**

Replace the `use` block at the top of `src/git/ops.rs` with:

```rust
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use crate::config::SettingsSection;
use crate::git::creds::{HostKeyChecker, ResolvedCred};
use crate::git::error::{AbortReason, GitErrorCode};
use crate::git::jobs::{AbortFlag, JobOp, JobSlot, Phase, Progress};
use crate::git::registry::{AuthorSpec, CredentialSpec, RepoDef, Warning};
```

Then insert this block immediately above the `#[cfg(test)]` line.

```rust
/// Scratch that the libgit2 callbacks write through a shared `&OpCtx`.
///
/// `RemoteCallbacks` stores each callback as its own `Box<dyn FnMut + 'a>`, so two
/// of them can never both hold `&mut` to one context. Interior mutability is what
/// makes the whole set shareable behind a single `&`. The fields are private to
/// this module; `OpScratch::default()` is the only value any caller needs.
#[derive(Default)]
pub struct OpScratch {
    cred_attempts: Cell<u32>,
    cred_failure: Cell<Option<GitErrorCode>>,
    push_rejections: RefCell<Vec<(String, String)>>,
}

/// Everything one operation needs, moved whole into a `spawn_blocking` closure.
///
/// It deliberately holds no handle back into the host: no `App`, no `Children`, no
/// runtime, no channel. The job slot and the returned `OpOutcome` are the only ways
/// anything here reaches the event loop.
pub struct OpCtx {
    pub tree: PathBuf,
    pub def: RepoDef,
    pub cred: ResolvedCred,
    /// A stored credential existed but was bound to another host, so nothing was
    /// resolved. An auth demand becomes `auth_unbound`, not `auth_missing`.
    pub cred_unbound: bool,
    pub request: OpRequest,
    pub identity: Identity,
    pub host_key: HostKeyChecker,
    pub abort: Arc<AbortFlag>,
    pub slot: Arc<JobSlot>,
    pub deadline: Instant,
    pub settings: Option<SettingsCtx>,
    /// `[git].author_name` / `[git].author_email`, already resolved.
    pub default_author: AuthorSpec,
    pub scratch: OpScratch,
}

impl OpCtx {
    pub fn phase(&self, p: Phase) {
        self.slot.set_phase(p);
    }

    /// True once this operation must stop, for any reason.
    pub fn aborted(&self) -> bool {
        if self.abort.is_aborted() {
            return true;
        }
        // The watchdog trips the same flag from the runtime, but the blocking thread
        // cannot depend on it: a transfer that stalls without ever waking tokio
        // would run past its deadline unnoticed. Recording the reason here is what
        // lets `classify` report `timeout` rather than `network_failed`.
        if Instant::now() >= self.deadline {
            self.abort.abort(AbortReason::Timeout);
            return true;
        }
        false
    }

    /// libgit2's transfer callback. Returning `false` cancels the transfer.
    pub fn transfer(&self, p: &git2::Progress<'_>) -> bool {
        if self.aborted() {
            return false;
        }
        self.slot.set_progress(Progress {
            received_objects: p.received_objects() as u32,
            total_objects: p.total_objects() as u32,
            indexed_objects: p.indexed_objects() as u32,
            received_bytes: p.received_bytes() as u64,
            // Spec §6.4 specifies `indexed_objects * 100 / total_objects`, not received.
            // Indexing lags receipt, so the two diverge for the whole tail of a fetch and
            // a received-based percent would sit at 100% while indexing is still running.
            percent: percent_of(p.indexed_objects(), p.total_objects()),
        });
        true
    }
}

/// `total_objects` is 0 until the remote finishes counting, and a server may send
/// more objects than it announced — neither may produce a panic or a percentage
/// above 100 in a progress bar.
fn percent_of(received: usize, total: usize) -> u8 {
    if total == 0 {
        return 0;
    }
    (received.saturating_mul(100) / total).min(100) as u8
}
```

- [ ] **Step 29: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::ops`

Expected: PASS — `test result: ok. 9 passed; 0 failed`.

- [ ] **Step 30: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): add OpCtx and its progress plumbing

The libgit2 callbacks each own a separate boxed closure, so none of them can
take &mut OpCtx; OpScratch holds the interior mutability they share behind one
shared borrow. aborted() checks the deadline itself and records Timeout so a
stalled transfer is not misreported as a network failure."
```

---

- [ ] **Step 31: Write the failing test for author resolution**

Add to `mod tests` in `src/git/ops.rs`:

```rust
    #[test]
    fn the_commit_author_falls_back_through_request_repo_config_then_host() {
        let mut fx = plain();
        // Nothing configured anywhere: the host names itself, and the address is
        // obviously a machine's rather than a person's.
        assert_eq!(fx.ctx.author_name(), "Test App");
        assert_eq!(fx.ctx.author_email(), "com.example.test@testhost");

        // `[git].author_name = ""` is the config default, not a choice, so an
        // empty (or whitespace-only) string must not win over the fallback.
        fx.ctx.default_author = AuthorSpec {
            name: Some("   ".to_string()),
            email: Some(String::new()),
        };
        assert_eq!(fx.ctx.author_name(), "Test App");
        assert_eq!(fx.ctx.author_email(), "com.example.test@testhost");

        fx.ctx.default_author = AuthorSpec {
            name: Some("Config Bot".to_string()),
            email: Some("bot@config.example".to_string()),
        };
        assert_eq!(fx.ctx.author_name(), "Config Bot");
        assert_eq!(fx.ctx.author_email(), "bot@config.example");

        fx.ctx.def = def(
            r#"{"id":"notes","author":{"name":"Repo Bot","email":"bot@repo.example"}}"#,
        );
        assert_eq!(fx.ctx.author_name(), "Repo Bot");
        assert_eq!(fx.ctx.author_email(), "bot@repo.example");

        fx.ctx.request.author = Some(AuthorSpec {
            name: Some("Caller".to_string()),
            email: None,
        });
        // Name and email fall back independently: a caller may override one.
        assert_eq!(fx.ctx.author_name(), "Caller");
        assert_eq!(fx.ctx.author_email(), "bot@repo.example");
    }
```

- [ ] **Step 32: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app git::ops::tests::the_commit_author`

Expected: FAIL — compile error:

```
error[E0599]: no method named `author_name` found for struct `OpCtx` in the current scope
   --> src/git/ops.rs:NNN:24
    |
    |         assert_eq!(fx.ctx.author_name(), "Test App");
    |                           ^^^^^^^^^^^ method not found in `OpCtx`

error[E0599]: no method named `author_email` found for struct `OpCtx` in the current scope
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 2 previous errors
```

- [ ] **Step 33: Implement `author_name` and `author_email`**

Add these two methods inside the existing `impl OpCtx` block in `src/git/ops.rs`, after
`transfer`:

```rust
    /// Request author, then the repo's, then `[git].author_name`, then the app.
    pub fn author_name(&self) -> String {
        pick(self.request.author.as_ref().and_then(|a| a.name.as_ref()))
            .or_else(|| pick(self.def.author.as_ref().and_then(|a| a.name.as_ref())))
            .or_else(|| pick(self.default_author.name.as_ref()))
            .unwrap_or_else(|| self.identity.app_name.clone())
    }

    /// Same order as `author_name`, resolved independently of it, ending at
    /// `<identifier>@<hostname>` — a syntactically valid address that is obviously
    /// the machine's rather than a person's.
    pub fn author_email(&self) -> String {
        pick(self.request.author.as_ref().and_then(|a| a.email.as_ref()))
            .or_else(|| pick(self.def.author.as_ref().and_then(|a| a.email.as_ref())))
            .or_else(|| pick(self.default_author.email.as_ref()))
            .unwrap_or_else(|| format!("{}@{}", self.identity.identifier, self.identity.hostname))
    }
```

And add this free function next to `percent_of`:

```rust
/// An empty or whitespace-only author field is the config default (`author_name =
/// ""`), not a choice — treat it as absent rather than committing as "".
fn pick(value: Option<&String>) -> Option<String> {
    value
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}
```

- [ ] **Step 34: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::ops`

Expected: PASS — `test result: ok. 10 passed; 0 failed`.

- [ ] **Step 35: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): resolve the commit author from request, repo, and config

Name and email fall back independently so a caller can override one. An empty
string is the config default rather than a choice and does not win."
```

---

- [ ] **Step 36: Write the failing tests for credential bookkeeping and error classification**

Add to `mod tests` in `src/git/ops.rs`:

These tests add no `use` line: `GitError` arrives through `use super::*` once Step 38 puts it in
the module-level import.

```rust
    #[test]
    fn the_attempt_counter_returns_the_count_it_just_recorded() {
        let fx = plain();
        assert_eq!(fx.ctx.cred_attempt(), 1);
        assert_eq!(fx.ctx.cred_attempt(), 2);
        assert_eq!(fx.ctx.cred_attempt(), 3);
    }

    #[test]
    fn a_recorded_credential_failure_beats_whatever_libgit2_reported() {
        // From libgit2's side every one of our refusals is just "a callback
        // returned an error"; the code we recorded is the only specific thing
        // anybody has.
        let fx = plain();
        let raw = git2::Error::from_str("callback returned an error");
        fx.ctx.record_cred_failure(GitErrorCode::AuthUnbound);
        let err = fx.ctx.classify_err(&raw);
        assert_eq!(err.code(), GitErrorCode::AuthUnbound);
        assert_eq!(err.repo_id.as_deref(), Some("notes"));
        assert_eq!(err.message, "callback returned an error");
        let hint = err.git2.expect("classify_err always attaches the git2 hint");
        assert_eq!(hint.code, "GenericError");
    }

    #[test]
    fn a_classified_error_is_scrubbed_of_the_credential_it_was_using() {
        let fx = fixture(
            r#"{"id":"notes"}"#,
            ResolvedCred::Token {
                username: "x-access-token".to_string(),
                token: crate::git::secret::Secret::new("ghp-secret-token-value".to_string()),
            },
            false,
        );
        let raw = git2::Error::from_str(
            "failed to connect to https://x-access-token:ghp-secret-token-value@github.com/a/b",
        );
        let err = fx.ctx.classify_err(&raw);
        assert!(
            !err.message.contains("ghp-secret-token-value"),
            "libgit2 quotes URLs into its messages, and this one reaches git.log: {}",
            err.message
        );
        // The same guarantee for an error we built ourselves.
        let ours = fx.ctx.scrub_err(GitError::internal(
            "token ghp-secret-token-value rejected",
        ));
        assert!(!ours.message.contains("ghp-secret-token-value"));
    }

    #[test]
    fn the_missing_credential_message_names_the_mechanism_the_remote_wanted() {
        let fx = plain();
        let msg = fx
            .ctx
            .auth_missing_message(git2::CredentialType::USER_PASS_PLAINTEXT);
        assert!(msg.contains("notes"), "{msg}");
        assert!(msg.contains("username and password"), "{msg}");

        assert_eq!(
            describe_allowed(git2::CredentialType::SSH_KEY),
            "an ssh key"
        );
        assert_eq!(
            describe_allowed(
                git2::CredentialType::SSH_KEY | git2::CredentialType::USER_PASS_PLAINTEXT
            ),
            "a username and password or token or an ssh key"
        );
        // libgit2 can ask for NTLM/Negotiate. Saying so is better than a message
        // that reads as though a credential were configured wrongly.
        assert_eq!(
            describe_allowed(git2::CredentialType::DEFAULT),
            "an authentication method this host does not implement"
        );
    }

    #[test]
    fn the_unbound_message_names_the_host_the_credential_belongs_to() {
        let fx = fixture(
            r#"{"id":"notes","remote":"https://evil.example/a/b.git",
                "credential":{"kind":"token","token":"stored-token-value",
                              "bound_host":"github.com"}}"#,
            ResolvedCred::None,
            true,
        );
        let msg = fx
            .ctx
            .auth_missing_message(git2::CredentialType::USER_PASS_PLAINTEXT);
        assert!(msg.contains("github.com"), "{msg}");
        assert!(msg.contains("evil.example"), "{msg}");
        assert!(
            msg.contains("PUT"),
            "the message is where the user learns how to rebind: {msg}"
        );
    }

    #[test]
    fn a_learned_host_key_fingerprint_travels_out_through_the_context() {
        let fx = plain();
        assert!(fx.ctx.learned_fingerprint().is_none());
        // `after_job` is the only writer of git-state.json; the blocking thread
        // only carries the value back.
        assert!(matches!(
            fx.ctx
                .host_key
                .check_for_test_sha256(&[0u8; 32], "github.com"),
            Ok(())
        ));
        assert_eq!(
            fx.ctx.learned_fingerprint().as_deref(),
            Some("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
        );
    }
```

The last test needs one seam: `HostKeyChecker::decide` is private to `creds.rs`, and a
`git2::Cert` cannot be constructed. Add this to `impl HostKeyChecker` in `src/git/creds.rs`,
alongside `learned`:

```rust
    /// Drive the TOFU decision from a raw hash. `#[cfg(test)]` because production
    /// code only ever arrives here through `check`, from a real `git2::Cert` — which
    /// no test can build, since libgit2 constructs them behind a private trait.
    #[cfg(test)]
    pub fn check_for_test_sha256(&self, sha: &[u8; 32], host: &str) -> Result<(), git2::Error> {
        self.decide(HostKeyHash::Sha256(sha), host).map(|_| ())
    }
```

- [ ] **Step 37: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::ops`

Expected: FAIL — compile error:

```
error[E0599]: no method named `cred_attempt` found for struct `OpCtx` in the current scope
error[E0599]: no method named `record_cred_failure` found for struct `OpCtx` in the current scope
error[E0599]: no method named `classify_err` found for struct `OpCtx` in the current scope
error[E0599]: no method named `scrub_err` found for struct `OpCtx` in the current scope
error[E0599]: no method named `auth_missing_message` found for struct `OpCtx` in the current scope
error[E0599]: no method named `learned_fingerprint` found for struct `OpCtx` in the current scope
error[E0599]: no method named `check_for_test_sha256` found for struct `HostKeyChecker` ...
error[E0425]: cannot find function `describe_allowed` in this scope
error[E0433]: failed to resolve: use of undeclared type `GitError`
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 11 previous errors
```

(`check_for_test_sha256` resolves only after the `#[cfg(test)]` seam from Step 36 is added to
`src/git/creds.rs`; if you skipped that edit, add it now.)

- [ ] **Step 38: Implement the credential bookkeeping and classification methods**

In `src/git/ops.rs`, extend the error import to
`use crate::git::error::{self, AbortReason, GitError, GitErrorCode};`, then add these methods
inside the existing `impl OpCtx` block, after `author_email`:

```rust
    /// Count one authentication attempt and return the new total.
    pub fn cred_attempt(&self) -> u32 {
        let n = self.scratch.cred_attempts.get() + 1;
        self.scratch.cred_attempts.set(n);
        n
    }

    /// Remember why authentication failed.
    ///
    /// A plain overwrite is right: every path that records also returns an error to
    /// libgit2 in the same breath, which ends the operation, so at most one code is
    /// recorded per job.
    pub fn record_cred_failure(&self, code: GitErrorCode) {
        self.scratch.cred_failure.set(Some(code));
    }

    /// The message libgit2 carries back out as `git2::Error::message()`, which then
    /// becomes the job's `error.message`. It is the only place the user is told what
    /// to do about a credential, so it names the fix rather than the symptom.
    pub fn auth_missing_message(&self, allowed: git2::CredentialType) -> String {
        if self.cred_unbound {
            let bound = self
                .def
                .credential
                .as_ref()
                .and_then(CredentialSpec::bound_host)
                .unwrap_or("another host");
            let remote = self.def.remote.as_deref().unwrap_or("this remote");
            return format!(
                "the stored credential is bound to {bound} and was not sent to {remote}; \
                 PUT the repo again to rebind it, or send a credential with the request"
            );
        }
        format!(
            "no credential is configured for {}; the remote asked for {}",
            self.def.id,
            describe_allowed(allowed)
        )
    }

    /// Turn a libgit2 error into ours, preferring what our own callbacks recorded.
    pub fn classify_err(&self, e: &git2::Error) -> GitError {
        // From libgit2's side every refusal our credentials callback makes is just
        // "a callback returned an error" — the recorded code is the only thing that
        // can tell `auth_missing` from `auth_unbound` from `auth_failed`.
        let code = self
            .scratch
            .cred_failure
            .get()
            .unwrap_or_else(|| error::classify(e, self.abort.reason()));
        self.scrub_err(
            GitError::new(code, e.message())
                .with_repo(&self.def.id)
                .with_git2(e),
        )
    }

    /// libgit2 quotes remote URLs into its error strings, and those strings land in
    /// job records, `git.log`, and dialogs. Nothing leaves this file unscrubbed.
    pub fn scrub_err(&self, e: GitError) -> GitError {
        e.scrubbed(&self.cred.secrets())
    }

    pub fn learned_fingerprint(&self) -> Option<String> {
        self.host_key.learned()
    }
```

And add this free function next to `pick`:

```rust
/// Render libgit2's credential-type bitmask for the message a human reads.
fn describe_allowed(allowed: git2::CredentialType) -> String {
    let mut parts = Vec::new();
    if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
        parts.push("a username and password or token");
    }
    if allowed.contains(git2::CredentialType::SSH_KEY)
        || allowed.contains(git2::CredentialType::SSH_MEMORY)
    {
        parts.push("an ssh key");
    }
    if parts.is_empty() {
        // NTLM/Negotiate and keyboard-interactive land here. Naming the gap beats a
        // message that reads as though a credential were merely configured wrongly.
        return "an authentication method this host does not implement".to_string();
    }
    parts.join(" or ")
}
```

- [ ] **Step 39: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::ops`

Expected: PASS — `test result: ok. 16 passed; 0 failed`.

Then confirm the `#[cfg(test)]` seam did not disturb `creds.rs`:
`cargo test --bin chrome-host-app git::creds` → `23 passed; 0 failed`.

- [ ] **Step 40: Commit**

```bash
git add src/git/ops.rs src/git/creds.rs
git commit -m "feat(git): record credential failures and classify git2 errors

libgit2 reports every refusal our credentials callback makes as a generic
callback error, so the code we recorded wins over classify(). Every error
leaving here is scrubbed of the secrets the operation was holding."
```

---

- [ ] **Step 41: Write the failing test for push rejections**

Add to `mod tests` in `src/git/ops.rs`:

```rust
    #[test]
    fn a_rejected_push_is_only_visible_through_the_update_reference_callback() {
        // `Remote::push` returns Ok(()) even when the server refused every ref —
        // the refusal arrives only here. A push that does not drain this reports a
        // success it never had.
        let fx = plain();
        assert!(fx.ctx.take_push_rejections().is_ok());

        fx.ctx
            .record_push_status("refs/heads/main", Some("non-fast-forward"));
        fx.ctx.record_push_status("refs/heads/other", None);
        let e = fx
            .ctx
            .take_push_rejections()
            .expect_err("a recorded rejection must surface");
        assert_eq!(e.code(), GitErrorCode::PushRejected);
        assert!(e.message.contains("refs/heads/main"), "{}", e.message);
        assert!(e.message.contains("non-fast-forward"), "{}", e.message);
        assert_eq!(e.repo_id.as_deref(), Some("notes"));

        // Draining clears the list, so a retry inside the same job does not
        // rediscover a rejection the previous attempt already reported.
        assert!(fx.ctx.take_push_rejections().is_ok());
    }
```

- [ ] **Step 42: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app git::ops::tests::a_rejected_push`

Expected: FAIL — compile error:

```
error[E0599]: no method named `take_push_rejections` found for struct `OpCtx` in the current scope
error[E0599]: no method named `record_push_status` found for struct `OpCtx` in the current scope
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 2 previous errors
```

- [ ] **Step 43: Implement the push-status methods**

Add these two methods inside the existing `impl OpCtx` block in `src/git/ops.rs`, after
`scrub_err`:

```rust
    /// libgit2 calls this once per ref it pushed. `Some(status)` means the server
    /// refused that ref; a ref that went through reports `None`.
    pub fn record_push_status(&self, refname: &str, status: Option<&str>) {
        if let Some(status) = status {
            self.scratch
                .push_rejections
                .borrow_mut()
                .push((refname.to_string(), status.to_string()));
        }
    }

    /// Turn whatever the server refused into an error, and forget it.
    ///
    /// `Remote::push` returns `Ok(())` even when every ref was rejected — the
    /// rejection arrives only through `push_update_reference` — so a push that does
    /// not call this reports a success it never had. Draining rather than peeking
    /// keeps a retry inside the same job from rediscovering an old rejection.
    pub fn take_push_rejections(&self) -> Result<(), GitError> {
        let rejections = std::mem::take(&mut *self.scratch.push_rejections.borrow_mut());
        match rejections.into_iter().next() {
            None => Ok(()),
            Some((refname, status)) => Err(self.scrub_err(
                GitError::push_rejected(&refname, &status).with_repo(&self.def.id),
            )),
        }
    }
```

- [ ] **Step 44: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::ops`

Expected: PASS — `test result: ok. 17 passed; 0 failed`.

- [ ] **Step 45: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): collect push rejections from the update-reference callback

Remote::push returns Ok even when the server refused every ref, so the refusal
has to be drained out of the callback or the job reports a false success."
```

---

- [ ] **Step 46: Write the failing test for the operation dispatch table**

Add to `mod tests` in `src/git/ops.rs`:

```rust
    /// Scaffolding. It proves `run`'s dispatch table is total and that each arm
    /// reaches a verb of the same name; tasks 7 and 8 drop a line from the list
    /// below as each verb lands, and the whole test with the last stub.
    #[test]
    fn run_dispatches_to_a_verb_of_the_same_name() {
        let fx = plain();
        for op in [
            JobOp::Init,
            JobOp::Clone,
            JobOp::Pull,
            JobOp::Push,
            JobOp::Sync,
            JobOp::Commit,
            JobOp::Branch,
            JobOp::Reset,
        ] {
            let e = run(op, &fx.ctx).expect_err("no verb is implemented in this slice");
            assert_eq!(e.code(), GitErrorCode::Internal);
            assert!(
                e.message.starts_with(op.as_str()),
                "{op:?} reached the wrong verb: {}",
                e.message
            );
        }
    }
```

- [ ] **Step 47: Run the test and watch it fail**

Run: `cargo test --bin chrome-host-app git::ops::tests::run_dispatches`

Expected: FAIL — compile error:

```
error[E0425]: cannot find function `run` in this scope
   --> src/git/ops.rs:NNN:21
    |
    |             let e = run(op, &fx.ctx).expect_err("no verb is implemented in this slice");
    |                     ^^^ not found in this scope
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 1 previous error
```

- [ ] **Step 48: Implement `run` and the eight verb stubs**

Insert this block into `src/git/ops.rs` immediately above the `#[cfg(test)]` line.

```rust
// ---------------------------------------------------------------------------
// Verbs
//
// `run` is the single entry point `GitService` calls from its blocking thread.
// The eight verbs below are stubs in this slice; each is replaced in place, so the
// dispatch table never has to change again.
// ---------------------------------------------------------------------------

pub fn run(op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    match op {
        JobOp::Init => init(ctx),
        JobOp::Clone => clone(ctx),
        JobOp::Pull => pull(ctx),
        JobOp::Push => push(ctx),
        JobOp::Sync => sync(ctx),
        JobOp::Commit => commit(ctx),
        JobOp::Branch => branch(ctx),
        JobOp::Reset => reset(ctx),
    }
}

pub fn init(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("init is not implemented yet"))
}

pub fn clone(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("clone is not implemented yet"))
}

pub fn pull(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("pull is not implemented yet"))
}

pub fn push(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("push is not implemented yet"))
}

pub fn sync(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("sync is not implemented yet"))
}

pub fn commit(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("commit is not implemented yet"))
}

pub fn branch(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("branch is not implemented yet"))
}

pub fn reset(_ctx: &OpCtx) -> Result<OpOutcome, GitError> {
    Err(GitError::internal("reset is not implemented yet"))
}
```

- [ ] **Step 49: Run the test and watch it pass**

Run: `cargo test --bin chrome-host-app git::ops`

Expected: PASS — `test result: ok. 18 passed; 0 failed`.

- [ ] **Step 50: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): add the ops::run dispatch skeleton

Every JobOp reaches a verb of its own name. The verbs are stubs replaced in
place by later slices, so the dispatch table never changes again."
```

---

- [ ] **Step 51: Write the failing tests for the credentials callback body**

`RemoteCallbacks` keeps its closures in private fields, so a test can never call one back out of
a built callback set. The closure body therefore gets a name of its own. Add to `mod tests` in
`src/git/creds.rs`:

`OpCtx` is left out of the `use` list below on purpose: Step 53 imports it at module level and
it reaches the tests through `use super::*`.

```rust
    use crate::git::jobs::{Admission, JobOp, JobStore, RepoLease};
    use crate::git::ops::{Identity, OpRequest, OpScratch};
    use crate::git::registry::{AuthorSpec, RepoDef};
    use std::time::{Duration, Instant};

    struct Fixture {
        ctx: OpCtx,
        /// Dropping the lease finishes the job the slot belongs to.
        _lease: RepoLease,
    }

    fn fixture(def_json: &str, cred: ResolvedCred, cred_unbound: bool) -> Fixture {
        let store = JobStore::new("0000dead".to_string());
        let admitted = store
            .admit("notes", JobOp::Sync, None)
            .expect("a fresh store is not draining");
        let Admission::Started(slot, lease) = admitted else {
            panic!("a fresh store must admit the first job");
        };
        let abort = slot.abort.clone();
        Fixture {
            ctx: OpCtx {
                tree: PathBuf::from("/nonexistent/repos/notes"),
                def: serde_json::from_str::<RepoDef>(def_json).expect("fixture must parse"),
                cred,
                cred_unbound,
                request: OpRequest::default(),
                identity: Identity {
                    app_name: "Test App".to_string(),
                    identifier: "com.example.test".to_string(),
                    hostname: "testhost".to_string(),
                },
                host_key: HostKeyChecker::new(SshHostKeyPolicy::Tofu, None),
                abort,
                slot,
                deadline: Instant::now() + Duration::from_secs(60),
                settings: None,
                default_author: AuthorSpec::default(),
                scratch: OpScratch::default(),
            },
            _lease: lease,
        }
    }

    fn with_token() -> Fixture {
        fixture(
            r#"{"id":"notes","remote":"https://github.com/a/b.git"}"#,
            resolved(r#"{"kind":"token","token":"token-body-value"}"#),
            false,
        )
    }

    fn refusal(fx: &Fixture, allowed: git2::CredentialType) -> git2::Error {
        // `git2::Cred` has no Debug, so `expect_err` will not compile here.
        match credentials_for(&fx.ctx, allowed) {
            Ok(_) => panic!("the callback was expected to refuse"),
            Err(e) => e,
        }
    }

    #[test]
    fn the_username_probe_never_consumes_the_attempt_budget() {
        // libgit2 asks for USERNAME on its own before every SSH authentication
        // attempt. If that probe spent an attempt, one SSH connect would burn the
        // whole allowance before a single key was offered.
        let fx = with_token();
        for _ in 0..10 {
            let cred = credentials_for(&fx.ctx, git2::CredentialType::USERNAME)
                .expect("a username probe is always answerable");
            assert!(cred.has_username());
        }
        // Two real offers still remain.
        assert!(credentials_for(&fx.ctx, git2::CredentialType::USER_PASS_PLAINTEXT).is_ok());
        assert!(credentials_for(&fx.ctx, git2::CredentialType::USER_PASS_PLAINTEXT).is_ok());
    }

    #[test]
    fn the_third_offer_gives_up_instead_of_replaying_a_rejected_credential() {
        // libgit2 re-invokes this callback up to fifteen times before the transport
        // gives up with "too many redirects or authentication replays". Self-limit.
        let fx = with_token();
        assert!(credentials_for(&fx.ctx, git2::CredentialType::USER_PASS_PLAINTEXT).is_ok());
        assert!(credentials_for(&fx.ctx, git2::CredentialType::USER_PASS_PLAINTEXT).is_ok());
        let e = refusal(&fx, git2::CredentialType::USER_PASS_PLAINTEXT);
        assert!(e.message().contains("rejected"), "{}", e.message());
        assert_eq!(
            fx.ctx.classify_err(&e).code(),
            GitErrorCode::AuthFailed,
            "a credential the remote kept refusing is auth_failed, not auth_missing"
        );
    }

    #[test]
    fn a_demand_no_resolved_credential_can_answer_is_auth_missing() {
        let fx = fixture(
            r#"{"id":"notes","remote":"https://github.com/a/b.git"}"#,
            ResolvedCred::None,
            false,
        );
        let e = refusal(&fx, git2::CredentialType::USER_PASS_PLAINTEXT);
        assert_eq!(fx.ctx.classify_err(&e).code(), GitErrorCode::AuthMissing);
        // The message libgit2 hands back becomes the job's error.message verbatim.
        assert!(e.message().contains("notes"), "{}", e.message());
        assert!(
            e.message().contains("username and password"),
            "{}",
            e.message()
        );
    }

    #[test]
    fn a_withheld_credential_is_auth_unbound_not_auth_missing() {
        let fx = fixture(
            r#"{"id":"notes","remote":"https://evil.example/a/b.git",
                "credential":{"kind":"token","token":"stored-token-value",
                              "bound_host":"github.com"}}"#,
            ResolvedCred::None,
            true,
        );
        let e = refusal(&fx, git2::CredentialType::USER_PASS_PLAINTEXT);
        assert_eq!(fx.ctx.classify_err(&e).code(), GitErrorCode::AuthUnbound);
        assert!(e.message().contains("github.com"), "{}", e.message());
    }

    #[test]
    fn a_token_is_never_offered_to_a_demand_that_only_accepts_a_key() {
        // The mask says SSH only; offering the token anyway would put a PAT on the
        // wire in a context that cannot use it.
        let fx = with_token();
        let e = refusal(&fx, git2::CredentialType::SSH_KEY);
        assert_eq!(fx.ctx.classify_err(&e).code(), GitErrorCode::AuthMissing);
    }
```

- [ ] **Step 52: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::creds`

Expected: FAIL — compile error:

```
error[E0425]: cannot find function `credentials_for` in this scope
  --> src/git/creds.rs:NNN:15
   |
   |         match credentials_for(&fx.ctx, allowed) {
   |               ^^^^^^^^^^^^^^^ not found in this scope
error[E0433]: failed to resolve: use of undeclared type `OpCtx`
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 8 previous errors
```

- [ ] **Step 53: Implement `credentials_for`**

Add to the `use` block at the top of `src/git/creds.rs`:

```rust
use crate::git::error::GitErrorCode;
use crate::git::ops::OpCtx;
```

(`use crate::git::error::GitError;` becomes `use crate::git::error::{GitError, GitErrorCode};`.)
Then **delete `use crate::git::error::GitErrorCode;` from the test module** — Step 1 put it
there, and the module-level import above now covers it through `use super::*`.

Then insert this block immediately above the `#[cfg(test)]` line.

**Note the comment wording.** The security test in Step 56 greps this file for the names of
libgit2's ambient-credential constructors, so no comment here may spell one out. Say "the
machine's ambient identity", not the function name.

```rust
/// The body of the `credentials` callback, named rather than inlined so it can be
/// exercised directly: `RemoteCallbacks` keeps its closures in private fields, and
/// a test can never call one back out of a built callback set.
fn credentials_for(
    ctx: &OpCtx,
    allowed: git2::CredentialType,
) -> Result<git2::Cred, git2::Error> {
    // libgit2 asks for USERNAME on its own before every SSH authentication attempt.
    // That probe always precedes the real ask, so answering it must not spend the
    // budget — one SSH connect would otherwise burn the whole allowance before a
    // single key was offered. scp-style `git@host:path` skips the probe entirely,
    // which is why the budget cannot simply be doubled to compensate.
    if allowed.contains(git2::CredentialType::USERNAME) {
        return git2::Cred::username(ctx.cred.username().unwrap_or("git"));
    }
    if ctx.cred_attempt() > MAX_CRED_ATTEMPTS {
        ctx.record_cred_failure(GitErrorCode::AuthFailed);
        return Err(git2::Error::from_str(
            "authentication failed: the remote rejected the credential",
        ));
    }
    match choose(&ctx.cred, allowed) {
        CredChoice::Token { username, token } => {
            git2::Cred::userpass_plaintext(username, token.expose())
        }
        CredChoice::SshMemory {
            username,
            public,
            private,
            pass,
        } => git2::Cred::ssh_key_from_memory(
            username,
            public,
            private.expose(),
            // Written as a closure rather than a function path so an auditor's
            // `grep '\.expose()'` finds exactly the four call sites it expects,
            // all of them inside this one function.
            pass.map(|p| p.expose()),
        ),
        CredChoice::SshPath {
            username,
            public,
            private,
            pass,
        } => git2::Cred::ssh_key(username, public, private, pass.map(|p| p.expose())),
        // Fail here rather than letting libgit2 fall back to the machine's ambient
        // identity. That fallback is what would turn this host into a laundering
        // proxy for whatever the desktop user's own keys and helpers can reach, and
        // it loops offering the same rejected identity until the transport gives up.
        CredChoice::Missing => {
            ctx.record_cred_failure(if ctx.cred_unbound {
                GitErrorCode::AuthUnbound
            } else {
                GitErrorCode::AuthMissing
            });
            Err(git2::Error::from_str(&ctx.auth_missing_message(allowed)))
        }
    }
}
```

- [ ] **Step 54: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::creds`

Expected: PASS — `test result: ok. 28 passed; 0 failed`.

- [ ] **Step 55: Commit**

```bash
git add src/git/creds.rs
git commit -m "feat(git): build credentials without an ambient fallback

The USERNAME probe libgit2 sends before every SSH attempt does not spend the
attempt budget, and a demand nothing resolved can answer fails immediately
rather than falling through to whatever identity the machine happens to have."
```

---

- [ ] **Step 56: Write the failing tests for the callback set and the absent fallback**

Two tests. The first is a compile-level proof that every callback shares one borrow of the
context. The second is the security property this file exists for, tested as a property.

Add to `mod tests` in `src/git/creds.rs`:

```rust
    #[test]
    fn every_callback_shares_one_borrow_of_the_context() {
        let fx = with_token();
        // Each setter stores its own `Box<dyn FnMut + 'a>`. If any callback needed
        // `&mut OpCtx`, building the set would not compile — and building a second
        // set over the same borrow proves the context is shared, not moved.
        let _first = callbacks(&fx.ctx);
        let _second = callbacks(&fx.ctx);
        assert!(!fx.ctx.aborted());
    }

    /// The absence of an ambient-credential fallback is a security property, so it
    /// is tested like one rather than left to review.
    ///
    /// libgit2 offers three ways to authenticate as whoever is logged in: the
    /// machine's default identity (NTLM/Negotiate), the running ssh agent, and the
    /// git credential helper chain. Any of them would let a loopback caller push to
    /// every repository the desktop user's own credentials can reach — this host
    /// would be a laundering proxy for their whole account — and each would make a
    /// failure depend on which machine the app happens to be running on. None is
    /// called here, and this test is what keeps it that way through a refactor.
    #[test]
    fn no_ambient_credential_fallback_exists_in_this_file() {
        const SRC: &str = include_str!("creds.rs");
        // Assembled at run time so this test's own source does not contain the
        // needles it searches for.
        for needle in [
            format!("Cred::{}", "default"),
            format!("ssh_key_from_{}", "agent"),
            format!("credential_{}", "helper"),
        ] {
            assert!(
                !SRC.contains(&needle),
                "creds.rs must never reach for an ambient identity, but it names {needle}"
            );
        }
        // A rename or a move must not make the assertions above pass vacuously.
        assert!(
            SRC.contains("git2::Cred::"),
            "this test only means something while creds.rs still builds git2 credentials"
        );
    }
```

- [ ] **Step 57: Run the tests and watch them fail**

Run: `cargo test --bin chrome-host-app git::creds`

Expected: FAIL — `callbacks` does not exist yet:

```
error[E0425]: cannot find function `callbacks` in this scope
  --> src/git/creds.rs:NNN:22
   |
   |         let _first = callbacks(&fx.ctx);
   |                      ^^^^^^^^^ not found in this scope
error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 2 previous errors
```

- [ ] **Step 58: Implement `callbacks`**

Insert this block into `src/git/creds.rs` immediately above the `#[cfg(test)]` line.

```rust
/// The libgit2 callback set for one operation.
///
/// Every callback borrows the same `&'a OpCtx` immutably and writes through its
/// cells. `RemoteCallbacks` boxes each one separately, so no two of them could ever
/// hold `&mut` to the context; interior mutability is what makes the set buildable
/// at all.
pub fn callbacks<'a>(ctx: &'a OpCtx) -> git2::RemoteCallbacks<'a> {
    let mut cb = git2::RemoteCallbacks::new();
    // `username_from_url` is ignored on purpose: the username to authenticate with
    // comes from the credential that was resolved for this repo, not from a string
    // in a remote URL a caller can rewrite.
    cb.credentials(move |_url, _username_from_url, allowed| credentials_for(ctx, allowed));
    cb.transfer_progress(move |p| ctx.transfer(&p));
    // The sideband is the remote's "counting objects" chatter. There is nothing to
    // record, but returning false is the only way to cancel while it is talking.
    cb.sideband_progress(move |_| !ctx.aborted());
    cb.push_update_reference(move |refname, status| {
        ctx.record_push_status(refname, status);
        Ok(())
    });
    cb.certificate_check(move |cert, host| ctx.host_key.check(cert, host));
    cb
}
```

- [ ] **Step 59: Run the tests and watch them pass**

Run: `cargo test --bin chrome-host-app git::creds`

Expected: PASS — `test result: ok. 30 passed; 0 failed`.

- [ ] **Step 60: Commit**

```bash
git add src/git/creds.rs
git commit -m "feat(git): assemble the libgit2 remote callbacks

One shared borrow of OpCtx feeds all five callbacks. A source-grep test asserts
the file names none of libgit2's ambient-credential constructors, because the
absence of that fallback is a security property and deserves a test."
```

---

- [ ] **Step 61: Verify the whole task and commit any formatting fixes**

Run all four checks in the order CI runs them:

```bash
cargo fmt
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Expected:
- `cargo fmt --check` produces no output.
- `cargo clippy --all-targets --locked -- -D warnings` finishes with `Finished`, no warnings.
  If it reports `dead_code` for anything in `creds.rs` or `ops.rs`, the two
  `#![allow(dead_code)]` / `#![allow(clippy::result_large_err)]` inner attributes at the top of
  `src/git/mod.rs` are missing — they came from task 3 and every module below `git` depends on
  them.
- `cargo test --locked` passes, `0 failed`, and the two modules this task created contribute 48
  tests: `cargo test --bin chrome-host-app git::creds` reports 30 and
  `cargo test --bin chrome-host-app git::ops` reports 18.

Then confirm the two audits this task is responsible for, by hand:

```bash
grep -c '\.expose()' src/git/creds.rs      # expect exactly 4
grep -rn 'Cred::default\|ssh_key_from_agent\|credential_helper' src/git/
grep -rn 'axum\|tokio\|Children\|HostEvent' src/git/creds.rs src/git/ops.rs
```

Expected: `4`; the second and third greps print nothing. (The four `.expose()` calls are the
token, the in-memory private key, and the two passphrases — all inside `credentials_for`.)

If `cargo fmt` rewrote anything:

```bash
git add -u
git commit -m "style(git): apply cargo fmt to creds.rs and ops.rs"
```
