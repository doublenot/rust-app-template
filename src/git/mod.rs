//! Git service.
//!
//! Root of the host-side git subsystem. Nothing lives here yet beyond the build-spine
//! tripwire below; each later module registers itself by adding its own
//! `pub mod <name>;` line to this file as it lands.

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
}
