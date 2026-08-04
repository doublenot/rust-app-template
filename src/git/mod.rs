//! Git service.
//!
//! Root of the host-side git subsystem. Nothing lives here yet beyond the build-spine
//! tripwire below; each later module registers itself by adding its own
//! `pub mod <name>;` line to this file as it lands.

// The subsystem lands over a dozen commits and is not reachable from `main` until the
// host is wired up in task 10; without this, rustc's dead-code pass flags every item a
// module publishes for the *next* module to consume, and CI runs `clippy -D warnings`.
#![allow(dead_code)]
// `GitError` is 152 bytes: a code plus four optional context strings, all of which the
// HTTP envelope and the job record are contractually required to carry. Boxing it to
// satisfy clippy's 128-byte `Result` budget would put an allocation on every `?` in the
// subsystem to save a move on paths that are already doing filesystem or network I/O.
#![allow(clippy::result_large_err)]

pub mod creds;
pub mod error;
pub mod jobs;
pub mod ops;
pub mod registry;
pub mod secret;
pub mod state;
pub mod util;

/// How long libgit2 may spend establishing a connection before giving up.
///
/// Deliberately not a config key: it is a liveness floor, not a policy, and a caller who
/// wants to wait longer for a slow server wants `[git].network_timeout_secs`.
pub const CONNECT_TIMEOUT_MS: i32 = 10_000;

/// Install the process-global libgit2 network timeouts. Call once, from
/// `GitService::start`, before any repository is opened.
///
/// Without these, a black-holed remote pins a `spawn_blocking` thread indefinitely: the
/// job watchdog lives inside the transfer callbacks, which never fire before the
/// transport has data. Both libgit2 options default to 0, meaning "the OS default".
pub fn init_libgit2_timeouts(network_timeout_secs: u64) -> Result<(), crate::git::error::GitError> {
    let total = i32::try_from(network_timeout_secs.saturating_mul(1000)).unwrap_or(i32::MAX);
    // SAFETY: both setters mutate libgit2 process-global state and are documented as
    // needing external synchronisation. The single caller runs once, on the main thread,
    // during startup and before any repository has been opened, so no other thread can
    // be inside libgit2 at this point.
    unsafe {
        git2::opts::set_server_connect_timeout_in_milliseconds(CONNECT_TIMEOUT_MS).map_err(
            |e| crate::git::error::GitError::internal(format!("libgit2 connect timeout: {e}")),
        )?;
        git2::opts::set_server_timeout_in_milliseconds(total).map_err(|e| {
            crate::git::error::GitError::internal(format!("libgit2 server timeout: {e}"))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Guards the `git2` feature list in `Cargo.toml`, not git2 itself.
    ///
    /// git2 0.21 declares `default = []`. A bare `git2 = "0.21"` therefore compiles,
    /// links, and passes every local-only test while producing a libgit2 with no HTTPS
    /// and no SSH transport: `clone`/`fetch`/`push` against a real remote fail at
    /// runtime with "unsupported URL protocol", possibly months after the dependency
    /// was added. Dropping `vendored-libgit2` is the same trap pointed the other way —
    /// the build silently links whatever libgit2 the build machine happens to have, so
    /// the shipped .deb/.dmg/.msi depends on a system library the user does not have.
    /// libgit2 reports all four capabilities at runtime, so one assertion converts both
    /// silent regressions into a red test.
    /// Ties `config::validate_branch_name` to the library that actually enforces the
    /// rule, in the one direction that matters: **everything we accept, libgit2 must
    /// accept.** The reverse is deliberately false — we reject a leading `-`, `@`, `~`
    /// and friends that libgit2 is happy with, because they are argv and shell hazards.
    ///
    /// This exists because the validator is the single admission gate for
    /// `[git].default_branch`, `repos[].branch` and `POST /api/git/repos/<id>/branch`.
    /// A name that slips through is not caught anywhere else: it reaches libgit2 deep
    /// inside a job, where it surfaces as a generic ref error instead of the
    /// admission-time rejection the API contract promises. An earlier revision checked
    /// `.lock` against the whole name rather than each path component, so `a.lock/b`,
    /// `a/.b` and `x/.` all passed — this test is what would have caught it.
    ///
    /// `is_valid_name` wants a full refname; branches live under `refs/heads/`.
    #[test]
    fn validate_branch_name_never_admits_a_name_libgit2_refuses() {
        // Shapes chosen to probe each rule and its boundary, not to be exhaustive:
        // component dots, `.lock` in every position, separators, and the length limit.
        let corpus = [
            "main",
            "a",
            "feature/x",
            "v1.2.3",
            "a-b_c",
            "release/2026.08",
            "a.b.c",
            "refs/heads/x",
            "x.locked",
            "lock",
            ".",
            "..",
            "...",
            ".x",
            "a.",
            "x.lock",
            "x.lock.",
            "a/.b",
            "x/.",
            "a.lock/b",
            "a/b.lock/c",
            "a..b",
            "a//b",
            "-x",
            "/x",
            "x/",
            "@",
            "a b",
            "a~b",
            "héllo",
            "a\tb",
            "",
        ];
        for name in corpus {
            if crate::config::validate_branch_name(name) {
                assert!(
                    git2::Reference::is_valid_name(&format!("refs/heads/{name}")),
                    "validate_branch_name accepted {name:?}, which libgit2 refuses as a refname"
                );
            }
        }
        // Guard the guard: a corpus that no longer reaches the interesting branch
        // would make the loop above vacuously true.
        assert!(
            corpus.iter().filter(|n| validate(n)).count() >= 8,
            "corpus no longer exercises the accept path"
        );
        assert!(
            corpus.iter().filter(|n| !validate(n)).count() >= 8,
            "corpus no longer exercises the reject path"
        );
        assert!(
            validate(&"a".repeat(200)) && !validate(&"a".repeat(201)),
            "MAX_BRANCH_NAME_LEN boundary moved"
        );
    }

    fn validate(name: &str) -> bool {
        crate::config::validate_branch_name(name)
    }

    #[test]
    fn libgit2_is_vendored_with_network_transports() {
        let v = git2::Version::get();
        assert!(
            v.vendored(),
            "libgit2 must be the vendored build; Cargo.toml needs the `vendored-libgit2` feature: {v:?}"
        );
        assert!(
            v.https(),
            "libgit2 must be built with HTTPS; Cargo.toml needs the `https` feature: {v:?}"
        );
        assert!(
            v.ssh(),
            "libgit2 must be built with SSH; Cargo.toml needs the `ssh` feature: {v:?}"
        );
        assert!(
            v.threads(),
            "libgit2 must be thread-aware; git work runs on tokio's blocking pool: {v:?}"
        );
    }

    /// Without these, a black-holed remote pins a `spawn_blocking` thread indefinitely:
    /// our own watchdog lives inside the transfer callbacks, and those never fire before
    /// the transport has produced data. Both libgit2 options default to 0, meaning
    /// "whatever the OS decides", which on Linux is around two hours.
    #[test]
    fn network_timeouts_are_installed_from_the_configured_value() {
        super::init_libgit2_timeouts(45).expect("libgit2 accepts both timeouts");
        // SAFETY: reads libgit2 process-global state. Nothing else in this test binary
        // writes it, and `init_libgit2_timeouts` has already returned.
        unsafe {
            assert_eq!(
                git2::opts::get_server_connect_timeout_in_milliseconds().expect("connect"),
                super::CONNECT_TIMEOUT_MS
            );
            assert_eq!(
                git2::opts::get_server_timeout_in_milliseconds().expect("server"),
                45_000
            );
        }
    }
}
