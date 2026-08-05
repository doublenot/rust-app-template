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

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use crate::config::SshHostKeyPolicy;
use crate::git::error::{GitError, GitErrorCode};
use crate::git::ops::OpCtx;
use crate::git::registry::{self, CredentialSpec};
use crate::git::secret::Secret;
use crate::git::util::b64_nopad;

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
        cert: &git2::cert::Cert<'_>,
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

    /// Drive the TOFU decision from a raw hash. `#[cfg(test)]` because production
    /// code only ever arrives here through `check`, from a real `git2::Cert` — which
    /// no test can build, since libgit2 constructs them behind a private trait.
    #[cfg(test)]
    pub fn check_for_test_sha256(&self, sha: &[u8; 32], host: &str) -> Result<(), git2::Error> {
        self.decide(HostKeyHash::Sha256(sha), host).map(|_| ())
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
            Some(pinned) if *pinned == fingerprint => {
                Ok(git2::CertificateCheckStatus::CertificateOk)
            }
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

/// The body of the `credentials` callback, named rather than inlined so it can be
/// exercised directly: `RemoteCallbacks` keeps its closures in private fields, and
/// a test can never call one back out of a built callback set.
fn credentials_for(ctx: &OpCtx, allowed: git2::CredentialType) -> Result<git2::Cred, git2::Error> {
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
    // `transfer_progress` fires only on FETCH. Without this a push reports no
    // progress at all and a large first push looks hung for its whole duration.
    // libgit2 declares this one as returning `()`, so unlike `transfer_progress` it
    // **cannot cancel** — `sideband_progress` below is the only abort hook that runs
    // while a push is in flight.
    cb.push_transfer_progress(move |current, total, bytes| {
        ctx.push_transfer(current, total, bytes)
    });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::jobs::{Admission, JobOp, JobStore, RepoLease};
    use crate::git::ops::{Identity, OpRequest, OpScratch};
    use crate::git::registry::{AuthorSpec, RepoDef};
    use std::time::{Duration, Instant};

    fn spec(json: &str) -> CredentialSpec {
        serde_json::from_str(json).expect("test fixture must parse")
    }

    const STORED_TOKEN: &str =
        r#"{"kind":"token","token":"stored-token-value","bound_host":"github.com"}"#;
    const STORED_SSH_MEM: &str = r#"{"kind":"ssh_key","private_key":"-----BEGIN KEY-----stored",
        "passphrase":"stored-passphrase","bound_host":"github.com"}"#;

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

    fn resolved(json: &str) -> ResolvedCred {
        resolve(Some(&spec(json)), None, None)
            .expect("test fixture must resolve")
            .cred
    }

    fn with_token() -> Fixture {
        fixture(
            r#"{"id":"notes","remote":"https://github.com/a/b.git"}"#,
            resolved(r#"{"kind":"token","token":"token-body-value"}"#),
            false,
        )
    }

    /// `CertificateCheckStatus` derives nothing — not even `Debug` — so
    /// `expect_err` will not compile against a `decide` result.
    fn refuse(checker: &HostKeyChecker, hash: HostKeyHash<'_>, host: &str) -> git2::Error {
        match checker.decide(hash, host) {
            Ok(_) => panic!("the checker was expected to refuse"),
            Err(e) => e,
        }
    }

    fn refusal(fx: &Fixture, allowed: git2::CredentialType) -> git2::Error {
        // `git2::Cred` has no Debug, so `expect_err` will not compile here.
        match credentials_for(&fx.ctx, allowed) {
            Ok(_) => panic!("the callback was expected to refuse"),
            Err(e) => e,
        }
    }

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
    fn a_credential_bound_to_nothing_is_withheld_from_a_remote_with_a_host() {
        // The other half of those two Nones, and the half `Registry::put`'s carry-forward rule
        // now cites by name: unbound is not a wildcard. Correct here since this file was
        // written, which is why the theft needed the registry's rebind to be exploitable at all.
        let stored = spec(r#"{"kind":"token","token":"local-token-value"}"#);
        let r = resolve(None, Some(&stored), Some("https://evil.example/a/b.git"))
            .expect("withholding is not an error");
        assert!(r.cred.is_none());
        assert!(r.unbound);
        assert!(r.cred.secrets().is_empty());
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
        let on_disk = spec(
            r#"{"kind":"ssh_key","private_key_path":"/home/u/.ssh/id_ed25519",
            "public_key_path":"/home/u/.ssh/id_ed25519.pub","passphrase":"disk-passphrase"}"#,
        );
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

        let in_memory = spec(
            r#"{"kind":"ssh_key","username":"deploy",
            "private_key":"-----BEGIN KEY-----body","public_key":"ssh-ed25519 AAAA"}"#,
        );
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

        let mem = spec(
            r#"{"kind":"ssh_key","private_key":"key-body-value",
            "passphrase":"pass-body-value"}"#,
        );
        let resolved = resolve(Some(&mem), None, None).expect("valid").cred;
        assert_eq!(resolved.username(), Some("git"));
        assert_eq!(
            resolved.secrets(),
            vec!["key-body-value", "pass-body-value"]
        );

        assert!(ResolvedCred::None.username().is_none());
        assert!(ResolvedCred::None.secrets().is_empty());
        assert!(ResolvedCred::None.is_none());
    }

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
        let e = refuse(&checker, HostKeyHash::Sha256(&[0xffu8; 32]), "github.com");
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
        let e = refuse(&checker, HostKeyHash::Missing, "github.com");
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
}
