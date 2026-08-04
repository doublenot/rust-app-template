//! A string that must never be printed, serialised, or logged.
//!
//! Every credential the host holds lives in one of these. Three properties are
//! load-bearing, and each of them is a *missing* impl rather than a rule someone
//! has to remember:
//!
//! * **No serde serializer, ever.** `RepoDef` holds `Secret`s, so a future
//!   `#[derive(Serialize)]` on `RepoDef` is a compile error instead of a token
//!   leaking into `GET /api/git/repos`. Giving this type a serializer — even a
//!   redacting one — removes that barrier and is a security regression; the
//!   hand-written `CredentialView` is the only sanctioned path from a credential
//!   to JSON.
//! * **No `Display`.** `format!("{token}")` does not compile.
//! * **A hand-written `Debug` that prints `Secret(***)`.** `RepoDef` derives
//!   `Debug` on top of it, so a stray `dbg!(&def)` or a panic message carrying a
//!   definition cannot leak.
//!
//! The value is reachable only through the three `expose_*` accessors. They are
//! split by purpose so that the audit in spec §8.5 — grep for `.expose()` and
//! expect only credential callbacks — stays literally true as the subsystem grows.

use serde::Deserialize;

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Secret(value)
    }

    /// The value, to hand to libgit2.
    ///
    /// ONLY legal caller: the `cb.credentials` closure in `creds::callbacks`.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The value, to persist into `repos.json` (written `0600`, atomically).
    ///
    /// ONLY legal caller: `registry::to_wire`.
    pub fn expose_for_disk(&self) -> &str {
        &self.0
    }

    /// The value, to find and replace with `***`.
    ///
    /// ONLY legal caller: `error::scrub`, fed by `creds::ResolvedCred::secrets`.
    pub fn expose_for_scrub(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::Secret;

    #[test]
    fn debug_never_prints_the_value() {
        let s = Secret::new("ghp_averyrealtoken".to_string());
        assert_eq!(format!("{s:?}"), "Secret(***)");

        // The realistic leak is not `{s:?}` but a derived `Debug` several layers up —
        // `dbg!(&repo_def)`, or a panic payload that happens to carry one.
        let nested = Some(vec![("credential", s)]);
        assert_eq!(
            format!("{nested:?}"),
            r#"Some([("credential", Secret(***))])"#
        );
    }

    #[test]
    fn deserializes_from_a_bare_json_string() {
        let s: Secret =
            serde_json::from_str("\"ghp_abc\"").expect("Secret is #[serde(transparent)]");
        assert_eq!(s.expose(), "ghp_abc");
        assert_eq!(s.expose_for_disk(), "ghp_abc");
        assert_eq!(s.expose_for_scrub(), "ghp_abc");
        assert_eq!(s, Secret::new("ghp_abc".to_string()));
    }

    /// Redaction is structural: `RepoDef` cannot derive a serializer while it holds a
    /// `Secret` that has none. Adding one here would silently re-open that path and no
    /// other test in the tree would go red, so assert on this file's own source text.
    #[test]
    fn secret_has_no_serializer() {
        // Spelled in two halves so this test does not trip over its own source.
        let banned = concat!("Seri", "alize");
        let code = include_str!("secret.rs")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains(banned),
            "src/git/secret.rs must never derive or implement serde's serializer trait for \
             Secret: it is the compile-time barrier that keeps RepoDef un-serialisable \
             (spec §4.6). Expose credential shape through CredentialView instead."
        );
    }
}
