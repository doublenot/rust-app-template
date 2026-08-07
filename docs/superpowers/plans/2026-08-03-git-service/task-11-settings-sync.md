### Task 11: Settings sync helper

`sync_settings = true` is the one built-in behaviour the git service ships: the host mirrors its
own `settings.json` into the work tree, and adopts the copy that comes back from the remote.

Four rules run through every line of this task, and the comments in the code say so:

1. **Only `settings::save` ever writes a `settings.json`, and only `settings::validate_incoming`
   ever validates one.** Git does not get a second format and does not get a second validator. A
   private `write_settings` wrapper delegates to `settings::save` so there is exactly one call
   site to audit.
2. **A teammate's typo must not fail an entire repo's sync forever.** Invalid pulled settings are
   a *warning on a successful job*, never an error: the local `settings.json` is left completely
   untouched and the host's own valid copy is written back over the tree copy, so the next sync
   commits it and the next push heals the remote.
3. **A restart is gated on the validated values actually differing**, not on the file changing.
   Every restart tears down the user's Chrome window and discards scroll position and in-page
   state. Because the values round-trip through `settings::save`, key order and whitespace are
   normalised, so two hosts that agree on the values produce identical bytes and never fight over
   formatting — and never restart each other's windows over it.
4. **`sync` only.** `pull` is deliberately untouched: copying the settings in is what would make
   the tree dirty, and a `pull` on a dirty tree fails `dirty_tree` by design — every pull on a
   `sync_settings` repo would fail. Callers who want a settings pull without publishing use
   `POST /repos/{id}/sync` with `push: false`, which is the same code path.

**House rules for every step below:** unit tests live in a `#[cfg(test)] mod tests` at the bottom
of the file they test; `tempfile` for filesystem tests; comments explain WHY (an invariant, a
race, a verified behaviour), never what; no `unwrap()` outside `#[cfg(test)]` unless a panic is
the correct behaviour and a comment says so; `cargo fmt` and
`cargo clippy --all-targets -- -D warnings` clean at every commit.

**This crate has no `--lib` target** (`src/main.rs` is the only entry point and
`src/bin/gen_icons.rs` is a second binary), so unit tests run in the `hitch` bin
target: `cargo test --bin hitch <filter>`.

**No `#![allow(dead_code)]` of its own is needed anywhere in this task.** `src/git/mod.rs` carries
`#![allow(dead_code)]` and `#![allow(clippy::result_large_err)]` from task 3, and lint levels
propagate down the module tree.

---

**Files:**

- Modify: `src/git/ops.rs` — one new `use` line; four new private helpers and the two contract
  functions, appended after `push_branch` and before the `#[cfg(test)] mod tests` line; two new
  call sites inside `sync`; new tests in `mod tests`.
- Modify: `src/git/mod.rs` — tests only: two `#[tokio::test]`s and their fixture, appended to the
  existing `#[cfg(test)] mod tests`. Possibly one line inside `GitService::after_job` (Step 18,
  conditional — it may already be right).

Line numbers depend on how tasks 6–8 laid the file out, so every edit below is anchored on exact
existing text rather than on a line number.

**Interfaces:**

- **Consumes** (all already landed; signatures are exact):
  - *existing host code* — `crate::settings`:
    `pub fn defaults(schema: &SettingsSection) -> serde_json::Map<String, serde_json::Value>`,
    `pub fn load(schema: &SettingsSection, path: &Path) -> serde_json::Map<String, serde_json::Value>`
    (total: an absent file, unparseable bytes, an unknown key or a wrong-typed value all collapse
    to the schema's defaults),
    `pub fn validate_incoming(schema: &SettingsSection, incoming: &serde_json::Value)
    -> Result<serde_json::Map<String, serde_json::Value>, String>` (unknown key or bad value →
    `Err`; a *missing* key falls back to that field's default),
    `pub fn save(path: &Path, values: &serde_json::Map<String, serde_json::Value>)
    -> std::io::Result<()>` (creates parent dirs, writes `serde_json::to_string_pretty`).
  - *existing host code* — `crate::config::{AppConfig, SettingsSection}`; `SettingsSection` is
    `Debug + Clone + Deserialize`; `AppConfig::from_str(&str) -> Result<AppConfig, String>` and
    `AppConfig::settings_enabled(&self) -> bool` (`menu.settings && settings.is_some()`).
  - *task 3* — `crate::git::error::{GitError, GitErrorCode}` with
    `GitError::settings_sync_unavailable() -> GitError`, `GitError::io(impl Into<String>) -> GitError`,
    `.with_repo(&str) -> GitError`, `.code() -> GitErrorCode`, and
    `GitErrorCode::SettingsSyncUnavailable`.
  - *task 4* — `crate::git::registry::{RepoDef, Warning}`; `RepoDef` has the public fields
    `id: String`, `sync_settings: bool` and `settings_path: String` (registry-validated as
    relative, `/`-separated, no `..`, no leading `/`; defaulting to `"settings.json"`);
    `Warning { pub code: &'static str, pub message: String }`.
  - *task 5* — `crate::git::jobs::JobOp`.
  - *task 6* — `crate::git::ops::{OpCtx, OpOutcome, OpRequest, SettingsCtx, SettingsRejected}`:
    `SettingsCtx { pub schema: crate::config::SettingsSection, pub settings_file: std::path::PathBuf }`
    (`Clone`), `OpCtx`'s public fields `tree: PathBuf`, `def: RepoDef`, `settings: Option<SettingsCtx>`,
    `OpOutcome::new(&'static str, &str) -> OpOutcome` with the public fields `settings_synced: bool`,
    `settings_rejected: Option<SettingsRejected>`, `settings_changed: bool`, `warnings: Vec<Warning>`,
    `committed: bool`; `SettingsRejected { pub error: String }`; `OpRequest::auto()` (which
    hard-codes `restart_children: Some(false)`).
  - *task 8* — `crate::git::ops::sync(&OpCtx) -> Result<OpOutcome, GitError>` and the test harness
    `crate::git::merge::testkit` with `Origin { pub root: tempfile::TempDir, pub bare: PathBuf }`,
    `origin_with_main() -> Origin`, `clone_at(&Origin, &str) -> PathBuf`,
    `commit_all(&Path, &str) -> git2::Oid`, `push_main(&Path)`,
    `Job { pub ctx: OpCtx, .. }`, `job(&Origin, &Path, JobOp) -> Job`,
    `job_local(&Path, JobOp) -> Job`.
  - *task 9* — `crate::git::{GitService, GitOps, StartOutcome}` with
    `GitService::with_ops(&AppConfig, &RuntimePaths, tokio::runtime::Handle,
    tokio::sync::mpsc::UnboundedSender<HostEvent>, Arc<tokio::sync::RwLock<AppStatus>>,
    Arc<dyn GitOps>) -> Arc<GitService>` (`#[cfg(test)]`), `GitService::start(&Arc<Self>)`,
    `GitService::put_repo(&Arc<Self>, &str, RepoDef) -> Result<PutOutcome, GitError>`,
    `GitService::start_job(&Arc<Self>, &str, JobOp, OpRequest) -> Result<StartOutcome, GitError>`,
    `GitService::jobs(&self) -> &Arc<JobStore>`, `JobStore::busy(&self, &str) -> Option<Arc<JobSlot>>`.
  - *task 9/10* — `crate::internal_server::{AppStatus, HostEvent}` with
    `HostEvent::GitRestartChildren { repo_id: String, reason: &'static str }`.

- **Produces** (`src/git/ops.rs`, both named by the interface contract §5.8):
  - `pub fn settings_copy_in(ctx: &OpCtx, out: &mut OpOutcome) -> Result<Option<u64>, GitError>` —
    `Ok(None)` when the repo does not mirror settings, otherwise `Ok(Some(hash))` of the bytes now
    in the tree.
  - `pub fn settings_apply_back(ctx: &OpCtx, before_hash: Option<u64>, out: &mut OpOutcome)
    -> Result<(), GitError>` — never returns `Err` for bad *content*; only for I/O and for a
    refused path.
  - Private to `ops.rs`, not part of the contract, internal to this task:
    `fn settings_tree_path(ctx: &OpCtx) -> PathBuf`,
    `fn settings_tree_target(ctx: &OpCtx, out: &mut OpOutcome) -> Result<PathBuf, GitError>`,
    `fn hash_bytes(bytes: &[u8]) -> u64`,
    `fn write_settings(path: &std::path::Path, values: &serde_json::Map<String, serde_json::Value>)
    -> Result<(), GitError>`,
    `fn heal_tree_copy(ctx: &OpCtx, sc: &SettingsCtx, out: &mut OpOutcome) -> Result<(), GitError>`.

> **Amended by the post-execution audit (second round).** `settings_copy_in` and
> `heal_tree_copy` gained `&mut OpOutcome`, and `settings_tree_target` is new. Both changes
> exist for the same reason: nothing may reach `settings_path` on disk without first
> checking what the remote planted there, and a job that disarms something has to say so
> (`Warning{code: "settings_symlink_replaced"}`, contract §3.11). See step 7.
  - No new public types. `OpOutcome.settings_synced` / `.settings_rejected` / `.settings_changed`
    stop being permanently `false`/`None`, which is what task 9's `after_job` reads.

---

## Cycle A — copy the host's settings into the tree

- [ ] **Step 1: Write the failing tests for `settings_copy_in`**

Append to the `#[cfg(test)] mod tests` block at the bottom of `src/git/ops.rs`. `use super::*;`
at the top of that module already brings in `settings_copy_in`, `SettingsCtx`, `OpOutcome`,
`GitErrorCode`, `PathBuf` and `JobOp`; `use crate::git::merge::testkit;` was added there by
task 8. Everything else is fully qualified so nothing new has to be imported.

```rust
    /// Two fields is the smallest schema that can prove both "an omitted key
    /// falls back to its default" and "a bad value is rejected".
    fn settings_schema() -> crate::config::SettingsSection {
        crate::config::AppConfig::from_str(
            "[app]\nname = \"X\"\nidentifier = \"com.example.x\"\n\
             [menu]\nsettings = true\n\
             [[settings.fields]]\n\
             key = \"theme\"\nlabel = \"Theme\"\ntype = \"select\"\n\
             default = \"light\"\noptions = [\"light\", \"dark\"]\n\
             [[settings.fields]]\n\
             key = \"notify\"\nlabel = \"Notifications\"\ntype = \"boolean\"\n\
             default = true\n",
        )
        .expect("the fixture config must parse")
        .settings
        .expect("the fixture config declares a settings section")
    }

    /// A `sync_settings` repo whose work tree is not a git repository at all.
    ///
    /// Neither settings helper ever opens one: they move bytes between the
    /// host's `settings.json` and the tree copy, and driving them directly is
    /// what keeps these assertions about the *files* and nothing else. The
    /// host's settings file lives outside the tree, as it does in production
    /// (`<data-dir>/settings.json`).
    struct SettingsFx {
        _dir: tempfile::TempDir,
        tree: PathBuf,
        host_settings: PathBuf,
        job: testkit::Job,
    }

    fn settings_fx() -> SettingsFx {
        let dir = tempfile::tempdir().expect("tempdir");
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(&tree).expect("create the work tree dir");
        let host_settings = dir.path().join("data/settings.json");
        let mut job = testkit::job_local(&tree, JobOp::Sync);
        job.ctx.def.sync_settings = true;
        job.ctx.settings = Some(SettingsCtx {
            schema: settings_schema(),
            settings_file: host_settings.clone(),
        });
        SettingsFx {
            _dir: dir,
            tree,
            host_settings,
            job,
        }
    }

    /// Somewhere for a direct `settings_copy_in` to report what it disarmed, in
    /// the tests that assert on the files rather than on the job result.
    fn ignored_outcome() -> OpOutcome {
        OpOutcome::new("no_changes", "main")
    }

    #[test]
    fn settings_copy_in_materializes_the_host_file_and_copies_it_in() {
        let fx = settings_fx();
        assert!(
            !fx.host_settings.exists(),
            "the fixture must start with no host settings.json"
        );

        let hash = settings_copy_in(&fx.job.ctx, &mut ignored_outcome()).expect("copy in");

        assert!(hash.is_some(), "a sync_settings repo must report a hash");
        assert!(
            fx.host_settings.exists(),
            "a first sync must materialize the host's settings.json from the schema"
        );
        assert_eq!(
            std::fs::read(fx.tree.join("settings.json")).expect("read the tree copy"),
            std::fs::read(&fx.host_settings).expect("read the host copy"),
            "the tree copy must be byte-identical to the host copy"
        );
        let values = crate::settings::load(&settings_schema(), &fx.host_settings);
        assert_eq!(values["theme"], serde_json::json!("light"));
        assert_eq!(values["notify"], serde_json::json!(true));
    }

    #[test]
    fn settings_copy_in_is_a_no_op_without_sync_settings() {
        let mut fx = settings_fx();
        fx.job.ctx.def.sync_settings = false;

        assert_eq!(
            settings_copy_in(&fx.job.ctx, &mut ignored_outcome()).expect("copy in"),
            None
        );
        assert!(
            !fx.tree.join("settings.json").exists(),
            "a repo that did not opt in must never gain a settings.json"
        );
        assert!(!fx.host_settings.exists());
    }

    #[test]
    fn sync_settings_without_a_schema_is_settings_sync_unavailable() {
        // The registry rejects this combination, so it is reachable only when
        // [settings] was deleted from app.toml under a live repos.json. A
        // silently no-op data-sync feature is how people lose data.
        let mut fx = settings_fx();
        fx.job.ctx.settings = None;

        let e = settings_copy_in(&fx.job.ctx, &mut ignored_outcome())
            .expect_err("a schemaless mirror must not be silent");
        assert_eq!(e.code(), GitErrorCode::SettingsSyncUnavailable);
    }
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::ops::tests::settings_copy_in git::ops::tests::sync_settings_without`

Expected: FAIL to compile — the function does not exist yet:

```
error[E0425]: cannot find function `settings_copy_in` in this scope
   --> src/git/ops.rs:NNN:20
    |
NNN |         let hash = settings_copy_in(&fx.job.ctx, &mut ignored_outcome()).expect("copy in");
    |                    ^^^^^^^^^^^^^^^^ not found in this scope

error: could not compile `hitch` (bin "hitch" test) due to 3 previous errors
```

- [ ] **Step 3: Implement `settings_copy_in` and its three helpers**

In `src/git/ops.rs`, add one line to the `use` block, after the `crate::git::…` lines (rustfmt
sorts the crate group alphabetically, so `crate::settings` goes last):

```rust
use crate::settings;
```

Then append this block after `push_branch` and immediately above the `#[cfg(test)]` line:

> **Amended by the post-execution audit (second round).** Every write here used to go
> straight to `settings_tree_path`, and `settings::save` bottoms out in `std::fs::write`,
> which follows a symlink. A remote chooses the *file type* at `settings_path`: track it as
> a mode-120000 blob and libgit2 materializes a real symlink (`blob_content_to_link` →
> `p_symlink`, checkout.c:1596), after which every sync overwrites whatever the remote
> named — a dotfile, a shell rc, or `<data-dir>/repos.json` with every stored PAT in it.
> Silently and forever, because the index and HEAD still hold mode 120000 so nothing is
> ever staged and the job answers `no_changes`. `validate_settings_path` constrains the
> *string* and says nothing about the *file*. `settings_tree_target` below is now the only
> way to that path, on the read side as well as the write, and `settings_copy_in` /
> `settings_apply_back` / `heal_tree_copy` all take `&mut OpOutcome` so the job can report
> what it disarmed. Do not reintroduce a bare `settings_tree_path` call site.

```rust
/// The work-tree copy of `settings.json` for a repo that mirrors settings.
///
/// `settings_path` is registry-validated as relative, `/`-separated and free of
/// `..`, and `Path::join` treats `/` as a separator on Windows too, so no
/// per-platform splitting is needed here.
///
/// **Nothing may read or write this path without going through
/// `settings_tree_target` first**, which is the function that makes the
/// validated *string* mean anything about the *file* it names.
fn settings_tree_path(ctx: &OpCtx) -> PathBuf {
    ctx.tree.join(&ctx.def.settings_path)
}

/// Resolve the tree copy's path, disarm what a remote can plant at it, and say so.
///
/// Two dispositions, because the two cases are not the same object. **The leaf is
/// replaced**: `heal_tree_copy`'s doctrine is that the tree copy is the disposable
/// one, unlinking keeps the repo syncing, and the next commit publishes a regular
/// blob that heals every other clone. **A directory component is refused** (403):
/// it is not the settings copy, the write that follows would not recreate it, and
/// `settings::save` `create_dir_all`s the parent — so a nested `settings_path`
/// would otherwise escape through a linked directory with the leaf never being a
/// link at all.
///
/// Component by component rather than by canonicalizing: with `..` already excluded
/// upstream this also refuses a link that stays *inside* the tree, where
/// `.git/config` lives.
///
/// A SAFE checkout does not close this. `checkout_action_with_wd`
/// (checkout.c:530-551) sends a blob→link TYPECHANGE to CONFLICT only when the
/// workdir copy is *modified*; when it matches the baseline — which `settings_copy_in`
/// plus the commit after it guarantee — the arm is a plain `REMOVE_AND_UPDATE`. So the
/// link can also land mid-job, between the copy-in and the apply-back, which is why the
/// *read* goes through here too.
fn settings_tree_target(ctx: &OpCtx, out: &mut OpOutcome) -> Result<PathBuf, GitError> {
    let mut walked = ctx.tree.clone();
    let mut parts = ctx.def.settings_path.split('/').peekable();
    while let Some(part) = parts.next() {
        walked.push(part);
        // A component that does not exist yet cannot be a link, and nothing under
        // it can exist either: `create_dir_all` will build the rest inside the tree.
        let Ok(meta) = std::fs::symlink_metadata(&walked) else {
            break;
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        if parts.peek().is_some() {
            return Err(GitError::path_refused(&walked, "is a symlink").with_repo(&ctx.def.id));
        }
        // Both calls unlink the LINK and never touch the file it names;
        // `remove_dir` is the one Windows needs for a directory symlink, where
        // `remove_file` fails.
        std::fs::remove_file(&walked)
            .or_else(|_| std::fs::remove_dir(&walked))
            .map_err(|e| GitError::io(format!("unlink {}: {e}", walked.display())))?;
        // Repairing it silently would leave the operator with a repo that syncs
        // fine and a remote that is still aiming at their filesystem.
        out.warnings.push(Warning {
            code: "settings_symlink_replaced",
            message: format!(
                "{} was a symlink in the work tree and was replaced with a regular file; \
                 the remote is tracking it as one and this host will not write through it",
                ctx.def.settings_path
            ),
        });
    }
    Ok(settings_tree_path(ctx))
}

/// A change detector, not a checksum.
///
/// Both hashes are produced by the same process inside one job and neither is
/// ever persisted or transmitted, so a fast non-cryptographic 64-bit hash is
/// exactly the right tool: the question is only "did these bytes move while we
/// were fetching and merging".
fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{DefaultHasher, Hasher};
    let mut hasher = DefaultHasher::new();
    hasher.write(bytes);
    hasher.finish()
}

/// The single delegation point to the one writer of any `settings.json`.
///
/// Routing the tree copy through `settings::save` too — rather than copying the
/// host file byte-for-byte — is what normalizes key order and whitespace, so two
/// hosts that agree on the values produce identical bytes and never fight over
/// formatting.
fn write_settings(
    path: &std::path::Path,
    values: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), GitError> {
    settings::save(path, values).map_err(|e| GitError::io(format!("write {}: {e}", path.display())))
}

/// Materialize the host's `settings.json` and copy it into the work tree.
///
/// Returns the hash of the bytes now in the tree, or `None` when this repo does
/// not mirror settings. Prefer-local applies to settings exactly as it does to
/// every other file: this machine's `settings.json` is what gets published, and
/// the merge decides the rest.
///
/// Takes `out` for one reason: `settings_tree_target` can disarm a hostile tree
/// entry, and the job that did it has to say so.
pub fn settings_copy_in(ctx: &OpCtx, out: &mut OpOutcome) -> Result<Option<u64>, GitError> {
    if !ctx.def.sync_settings {
        return Ok(None);
    }
    // `sync_settings` with no schema is a registry-validation error, so this is
    // reachable only if `[settings]` was removed from app.toml under a live
    // repos.json. Failing loudly beats a data-sync feature that quietly stops.
    let Some(sc) = ctx.settings.as_ref() else {
        return Err(GitError::settings_sync_unavailable().with_repo(&ctx.def.id));
    };

    // `load` is total: an absent file, junk, an unknown key or a wrong-typed
    // value all collapse to the schema's defaults, which is precisely what
    // "materialize it from the defaults first" means.
    let values = settings::load(&sc.schema, &sc.settings_file);
    if !sc.settings_file.exists() {
        write_settings(&sc.settings_file, &values)?;
    }

    let target = settings_tree_target(ctx, out)?;
    write_settings(&target, &values)?;
    let bytes = std::fs::read(&target)
        .map_err(|e| GitError::io(format!("read {}: {e}", target.display())))?;
    Ok(Some(hash_bytes(&bytes)))
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::ops::tests::settings_copy_in git::ops::tests::sync_settings_without`

Expected: PASS — `test result: ok. 3 passed; 0 failed`.

- [ ] **Step 5: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): copy the host settings.json into the work tree

The tree copy is written through settings::save rather than copied
byte-for-byte, so it always carries to_string_pretty's key order and
spacing. Two hosts that agree on the values then produce identical bytes
and never fight over formatting - which matters because a settings change
is what restarts the user's Chrome window.

sync_settings with no schema errors as settings_sync_unavailable instead
of quietly doing nothing: a silently no-op data-sync feature is how people
lose data."
```

---

## Cycle B — validate what came back and adopt it

- [ ] **Step 6: Write the failing tests for `settings_apply_back`**

Append to `mod tests` in `src/git/ops.rs`. Each test stands in for the merge by writing the tree
copy directly — the merge itself is task 8's, already tested, and mixing the two would test
neither.

```rust
    #[test]
    fn unchanged_tree_settings_are_not_reapplied() {
        let fx = settings_fx();
        let mut out = OpOutcome::new("up_to_date", "main");
        let before = settings_copy_in(&fx.job.ctx, &mut out).expect("copy in");
        let host_bytes = std::fs::read(&fx.host_settings).expect("read the host copy");

        settings_apply_back(&fx.job.ctx, before, &mut out).expect("apply back");

        assert!(!out.settings_synced);
        assert!(!out.settings_changed, "an untouched file must never restart children");
        assert!(out.settings_rejected.is_none());
        assert!(out.warnings.is_empty());
        assert_eq!(
            std::fs::read(&fx.host_settings).expect("read the host copy"),
            host_bytes
        );
    }

    #[test]
    fn valid_pulled_settings_are_saved_and_flag_a_restart() {
        let fx = settings_fx();
        let mut out = OpOutcome::new("merged", "main");
        let before = settings_copy_in(&fx.job.ctx, &mut out).expect("copy in");
        std::fs::write(fx.tree.join("settings.json"), r#"{"theme":"dark"}"#)
            .expect("write the tree copy");

        settings_apply_back(&fx.job.ctx, before, &mut out).expect("apply back");

        assert!(out.settings_synced);
        assert!(out.settings_changed, "a changed value is what earns a restart");
        assert!(out.settings_rejected.is_none());
        let values = crate::settings::load(&settings_schema(), &fx.host_settings);
        assert_eq!(values["theme"], serde_json::json!("dark"));
        // The key the remote omitted came back as the schema's default rather
        // than disappearing from the host's file.
        assert_eq!(values["notify"], serde_json::json!(true));
    }

    #[test]
    fn valid_but_identical_settings_do_not_flag_a_restart() {
        // Same values, different formatting. A restart tears down the user's
        // window and discards in-page state, so whitespace must never buy one.
        let fx = settings_fx();
        let mut out = OpOutcome::new("merged", "main");
        let before = settings_copy_in(&fx.job.ctx, &mut out).expect("copy in");
        std::fs::write(
            fx.tree.join("settings.json"),
            r#"{"notify":true,"theme":"light"}"#,
        )
        .expect("write the tree copy");

        settings_apply_back(&fx.job.ctx, before, &mut out).expect("apply back");

        assert!(out.settings_synced, "the file was still adopted");
        assert!(
            !out.settings_changed,
            "identical values reformatted must not restart children"
        );
    }

    #[test]
    fn rejected_settings_leave_the_local_file_untouched_and_heal_the_tree() {
        let fx = settings_fx();
        let mut out = OpOutcome::new("merged", "main");
        let before = settings_copy_in(&fx.job.ctx, &mut out).expect("copy in");
        let host_bytes = std::fs::read(&fx.host_settings).expect("read the host copy");
        std::fs::write(fx.tree.join("settings.json"), r#"{"theme":"purple"}"#)
            .expect("write the tree copy");

        settings_apply_back(&fx.job.ctx, before, &mut out)
            .expect("a teammate's typo must not fail the job");

        assert!(!out.settings_synced);
        assert!(!out.settings_changed);
        let rejected = out
            .settings_rejected
            .expect("settings_rejected must carry the reason");
        assert!(
            rejected.error.contains("purple"),
            "the message must name the offending value: {}",
            rejected.error
        );
        assert_eq!(out.warnings.len(), 1);
        assert_eq!(out.warnings[0].code, "settings_rejected");
        assert_eq!(
            std::fs::read(&fx.host_settings).expect("read the host copy"),
            host_bytes,
            "a rejected pull must not touch the local settings.json at all"
        );
        assert_eq!(
            std::fs::read(fx.tree.join("settings.json")).expect("read the tree copy"),
            host_bytes,
            "the tree copy must be healed so the next push fixes the remote"
        );
    }

    #[test]
    fn unparseable_tree_settings_are_rejected_rather_than_fatal() {
        // Broken JSON and a schema violation are the same event from the user's
        // side - someone committed a file this host cannot use - and both must
        // leave the repo syncing.
        let fx = settings_fx();
        let mut out = OpOutcome::new("merged", "main");
        let before = settings_copy_in(&fx.job.ctx, &mut out).expect("copy in");
        std::fs::write(fx.tree.join("settings.json"), "{not json").expect("write the tree copy");

        settings_apply_back(&fx.job.ctx, before, &mut out).expect("broken JSON must not fail");

        assert!(out.settings_rejected.is_some());
        assert!(!out.settings_synced);
        assert_eq!(out.warnings[0].code, "settings_rejected");
    }

    #[test]
    fn apply_back_without_a_copy_in_does_nothing() {
        let mut fx = settings_fx();
        fx.job.ctx.def.sync_settings = false;

        let mut out = OpOutcome::new("up_to_date", "main");
        settings_apply_back(&fx.job.ctx, None, &mut out).expect("apply back");

        assert!(!out.settings_synced);
        assert!(out.settings_rejected.is_none());
        assert!(!fx.host_settings.exists());
    }
```

- [ ] **Step 7: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::ops::tests::settings git::ops::tests::valid git::ops::tests::rejected git::ops::tests::unchanged git::ops::tests::unparseable git::ops::tests::apply_back`

Expected: FAIL to compile:

```
error[E0425]: cannot find function `settings_apply_back` in this scope
   --> src/git/ops.rs:NNN:9
    |
NNN |         settings_apply_back(&fx.job.ctx, before, &mut out).expect("apply back");
    |         ^^^^^^^^^^^^^^^^^^^ not found in this scope

error: could not compile `hitch` (bin "hitch" test) due to 6 previous errors
```

- [ ] **Step 8: Implement `settings_apply_back` and `heal_tree_copy`**

In `src/git/ops.rs`, append below `settings_copy_in` (still above the `#[cfg(test)]` line):

> **Amended by the post-execution audit (second round).** Both functions reached
> `settings_tree_path` directly. The *read* matters as much as the write: these bytes are
> parsed, validated, adopted as this host's settings and quoted into a warning, so a link
> at that path chooses what this host runs with. And the merge is a way in — a SAFE
> checkout only sends a blob→link TYPECHANGE to CONFLICT when the workdir copy is
> *modified*, which `settings_copy_in` plus the commit after it guarantee it is not — so
> the link can land after the copy-in guard has already run. Both now go through
> `settings_tree_target` and both take `&mut OpOutcome`.

```rust
/// Validate the work tree's `settings.json` after the merge and adopt it.
///
/// Never fails the job over bad *content*: a teammate's typo must not wedge an
/// entire repo's sync forever. A rejected file is reported as a warning on an
/// otherwise successful job and the host's own valid copy is written back into
/// the tree, so the next sync commits it and the next push heals the remote.
pub fn settings_apply_back(
    ctx: &OpCtx,
    before_hash: Option<u64>,
    out: &mut OpOutcome,
) -> Result<(), GitError> {
    // `None` means `settings_copy_in` did nothing: this repo does not mirror
    // settings and there is no copy of ours to compare against.
    let (Some(before), Some(sc)) = (before_hash, ctx.settings.as_ref()) else {
        return Ok(());
    };

    // Before the read, not only before a write: the bytes at this path are parsed,
    // validated, adopted as this host's settings and quoted into a warning, so a
    // link here chooses what this host runs with and what its logs print.
    let target = settings_tree_target(ctx, out)?;
    let Ok(after) = std::fs::read(&target) else {
        // The copy above was committed before the fetch and the merge prefers
        // local, so the file cannot normally vanish. It does vanish when the
        // guard has just unlinked a planted symlink, and that lands here on
        // purpose — the heal below is exactly the repair that case wants.
        return heal_tree_copy(ctx, sc, out);
    };
    if hash_bytes(&after) == before {
        return Ok(());
    }

    let validated = serde_json::from_slice::<serde_json::Value>(&after)
        .map_err(|e| e.to_string())
        .and_then(|incoming| settings::validate_incoming(&sc.schema, &incoming));

    let rejected = match validated {
        Ok(values) => {
            // Read before the write, because this comparison is the only thing
            // standing between an auto-sync and a Chrome restart every five
            // minutes: the file changing is not enough, the values must differ.
            let current = settings::load(&sc.schema, &sc.settings_file);
            write_settings(&sc.settings_file, &values)?;
            out.settings_synced = true;
            out.settings_changed = values != current;
            return Ok(());
        }
        Err(e) => e,
    };

    let message = format!(
        "{} was rejected and the local settings were left unchanged: {rejected}",
        ctx.def.settings_path
    );
    out.settings_rejected = Some(SettingsRejected { error: rejected });
    out.warnings.push(Warning {
        code: "settings_rejected",
        message,
    });
    heal_tree_copy(ctx, sc, out)
}

/// Write the host's own valid settings back over the tree copy.
///
/// The next sync stages and commits it, and that push is what heals the remote
/// for everyone else.
fn heal_tree_copy(ctx: &OpCtx, sc: &SettingsCtx, out: &mut OpOutcome) -> Result<(), GitError> {
    let values = settings::load(&sc.schema, &sc.settings_file);
    // Through the guard, like every other write: one of the two ways to reach
    // this function is a tree copy that turned into a symlink.
    let target = settings_tree_target(ctx, out)?;
    write_settings(&target, &values)
}
```

- [ ] **Step 9: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::ops::tests::settings git::ops::tests::valid git::ops::tests::rejected git::ops::tests::unchanged git::ops::tests::unparseable git::ops::tests::apply_back`

Expected: PASS — `test result: ok. 9 passed; 0 failed` (the three from Cycle A plus these six).

- [ ] **Step 10: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): validate and adopt settings that arrived with a merge

Invalid pulled settings are a warning on a successful job, never an error:
the local settings.json is left completely untouched and the host's own
valid copy is written back over the tree copy so the next push heals the
remote. One teammate's typo must not wedge a repo's sync forever.

The restart flag is gated on the validated values differing, not on the
file differing, so a reformat cannot restart the user's Chrome window."
```

---

## Cycle C — wire both halves into `sync`

- [ ] **Step 11: Write the failing end-to-end tests**

These drive the real merge through `ops::sync` against a local bare remote, which is the only
honest way to assert that the copy lands *before* the commit and the adoption happens *after* the
merge. Append to `mod tests` in `src/git/ops.rs`:

```rust
    /// An origin whose `main` already carries this host's exact settings bytes,
    /// plus the `seed` clone that stands in for a teammate.
    ///
    /// Seeding through `settings::save` is what makes the bytes identical, so a
    /// later `settings_copy_in` produces no diff and the only change in the job
    /// under test is the one the teammate pushed.
    fn origin_with_settings() -> (testkit::Origin, PathBuf) {
        let o = testkit::origin_with_main();
        let seed = testkit::clone_at(&o, "seed");
        crate::settings::save(
            &seed.join("settings.json"),
            &crate::settings::defaults(&settings_schema()),
        )
        .expect("seed settings.json");
        testkit::commit_all(&seed, "seed settings.json");
        testkit::push_main(&seed);
        (o, seed)
    }

    fn mirror_job(o: &testkit::Origin, tree: &std::path::Path, host_settings: &PathBuf) -> testkit::Job {
        let mut fx = testkit::job(o, tree, JobOp::Sync);
        fx.ctx.def.sync_settings = true;
        fx.ctx.settings = Some(SettingsCtx {
            schema: settings_schema(),
            settings_file: host_settings.clone(),
        });
        fx
    }

    #[test]
    fn sync_adopts_settings_that_arrived_with_the_merge() {
        let (o, seed) = origin_with_settings();
        let a = testkit::clone_at(&o, "a");

        let mut wanted = crate::settings::defaults(&settings_schema());
        wanted.insert("theme".to_string(), serde_json::json!("dark"));
        crate::settings::save(&seed.join("settings.json"), &wanted).expect("teammate edit");
        testkit::commit_all(&seed, "switch the theme to dark");
        testkit::push_main(&seed);

        let host_settings = o.root.path().join("data/settings.json");
        let fx = mirror_job(&o, &a, &host_settings);

        let out = sync(&fx.ctx).expect("sync");

        assert!(
            !out.committed,
            "the copy-in wrote byte-identical bytes, so there was nothing to commit"
        );
        assert!(out.settings_synced);
        assert!(out.settings_changed, "a pulled value change must earn a restart");
        assert!(out.settings_rejected.is_none());
        assert_eq!(
            crate::settings::load(&settings_schema(), &host_settings)["theme"],
            serde_json::json!("dark")
        );
    }

    #[test]
    fn sync_survives_settings_a_teammate_broke() {
        let (o, seed) = origin_with_settings();
        let a = testkit::clone_at(&o, "a");

        std::fs::write(seed.join("settings.json"), "{\"theme\": \"purple\"}\n")
            .expect("teammate typo");
        testkit::commit_all(&seed, "typo the theme");
        testkit::push_main(&seed);

        let host_settings = o.root.path().join("data/settings.json");
        let fx = mirror_job(&o, &a, &host_settings);

        let out = sync(&fx.ctx).expect("a teammate's typo must not fail the whole sync");

        assert!(out.settings_rejected.is_some());
        assert!(!out.settings_synced);
        assert!(!out.settings_changed);
        assert!(out.warnings.iter().any(|w| w.code == "settings_rejected"));
        assert_eq!(
            crate::settings::load(&settings_schema(), &host_settings)["theme"],
            serde_json::json!("light"),
            "the local settings must survive the pull untouched"
        );
        assert_eq!(
            std::fs::read(a.join("settings.json")).expect("read the tree copy"),
            std::fs::read(&host_settings).expect("read the host copy"),
            "the tree copy must be healed so the next push fixes the remote"
        );
    }

    #[test]
    fn first_sync_publishes_the_host_settings_without_a_restart() {
        let o = testkit::origin_with_main();
        let a = testkit::clone_at(&o, "a");
        let host_settings = o.root.path().join("data/settings.json");
        let fx = mirror_job(&o, &a, &host_settings);

        let out = sync(&fx.ctx).expect("sync");

        assert!(
            out.committed,
            "the materialized settings.json must be staged with everything else"
        );
        assert!(!out.settings_synced);
        assert!(
            !out.settings_changed,
            "publishing our own settings must never restart our own children"
        );

        let bare = git2::Repository::open_bare(&o.bare).expect("open the bare origin");
        let head = bare
            .find_reference("refs/heads/main")
            .expect("origin has main")
            .peel_to_commit()
            .expect("main has a commit");
        assert!(
            head.tree()
                .expect("commit tree")
                .get_path(std::path::Path::new("settings.json"))
                .is_ok(),
            "the first sync must publish settings.json to the remote"
        );
    }
```

- [ ] **Step 12: Run the tests and watch them fail**

Run: `cargo test --bin hitch git::ops::tests::sync_adopts git::ops::tests::sync_survives git::ops::tests::first_sync`

Expected: FAIL — three assertion failures, because `sync` does not call either helper yet:

```
---- git::ops::tests::sync_adopts_settings_that_arrived_with_the_merge stdout ----
thread '...' panicked at src/git/ops.rs:NNN:
assertion failed: out.settings_synced

---- git::ops::tests::sync_survives_settings_a_teammate_broke stdout ----
thread '...' panicked at src/git/ops.rs:NNN:
assertion failed: out.settings_rejected.is_some()

---- git::ops::tests::first_sync_publishes_the_host_settings_without_a_restart stdout ----
thread '...' panicked at src/git/ops.rs:NNN:
the materialized settings.json must be staged with everything else

test result: FAILED. 0 passed; 3 failed
```

- [ ] **Step 13: Call both helpers from `sync`**

`sync` is `[settings copy] → commit-all → fetch → merge → [settings write-back] → push`. Two
edits in `src/git/ops.rs`, both inside `pub fn sync`.

First, insert the copy between `head_before` and the commit. Find this exact text — it is the one
occurrence followed by the "Committing everything BEFORE the fetch" comment:

```rust
    let mut out = OpOutcome::new("no_changes", &ctx.def.branch);
    out.head_before = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|o| o.to_string());

    // Committing everything BEFORE the fetch is load-bearing, not convenient.
```

and make it:

```rust
    let mut out = OpOutcome::new("no_changes", &ctx.def.branch);
    out.head_before = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|o| o.to_string());

    // Before staging, so this host's settings ride out on the same commit as
    // everything else in the tree. `sync` is the only verb that does this: a
    // `pull` refuses a dirty tree, and copying in is what would make it dirty.
    let settings_before = settings_copy_in(ctx, &mut out)?;

    // Committing everything BEFORE the fetch is load-bearing, not convenient.
```

Second, insert the write-back after the merge. Find this exact pair of statements — `pull` also
calls `apply_merge`, but only `sync` follows it with `record_ahead_behind`:

```rust
    apply_merge(&repo, ctx, &mut out)?;

    record_ahead_behind(&repo, ctx, &mut out);
```

and make it:

```rust
    apply_merge(&repo, ctx, &mut out)?;

    // After the merge and before the push, so settings that arrived with the
    // merge are validated before this host acts on them, and so a rejected file
    // is healed in the tree the next sync will commit.
    settings_apply_back(ctx, settings_before, &mut out)?;

    record_ahead_behind(&repo, ctx, &mut out);
```

Note what needs no edit: the early `return` for a repo with `remote: None` sits between the two
insertions, so a local-only repo still commits its settings and correctly never runs the
write-back — nothing could have changed the tree copy without a fetch.

- [ ] **Step 14: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::`

Expected: PASS — every `git::ops` and `git::merge` test is green, including task 8's
`sync_commits_and_publishes_in_one_pass` and `sync_without_a_remote_only_commits` (both use repos
with `sync_settings = false`, so both helpers are no-ops for them).

- [ ] **Step 15: Commit**

```bash
git add src/git/ops.rs
git commit -m "feat(git): run the settings mirror inside sync

The copy lands before staging so this host's settings ride out on the same
commit as everything else, and the write-back lands after the merge and
before the push so a file that arrived with the merge is validated before
this host acts on it and a rejected one is healed in the tree the next
sync will commit.

pull is deliberately left alone: copying in is what would make the tree
dirty, and pull refuses a dirty tree by design."
```

---

## Cycle D — a settings change is what restarts the children

§9.7 requires **all three** of: someone asked for a restart **or** `sync_settings` produced a real
change; the operation actually moved HEAD or rewrote `settings.json`; and the app status is
`Ready`. The middle and last conditions are task 9's and already tested there. The first
condition's *second half* is this task's, and it is the half that makes the feature work at all:
`OpRequest::auto()` hard-codes `restart_children: Some(false)`, so every timer sync and every
`sync_on_start` says "do not restart" — if `settings_changed` is not an independent OR-term, a
settings mirror can only ever fire from an explicit HTTP call, which is not what a mirror is for.

- [ ] **Step 16: Write the restart tests**

Append to the `#[cfg(test)] mod tests` at the bottom of `src/git/mod.rs`. `use super::*;` there
already brings in `GitService`, `GitOps`, `StartOutcome`, `JobOp`, `OpCtx`, `OpOutcome`,
`OpRequest`, `GitError`, `AppConfig`, `RuntimePaths`, `AppStatus` and `HostEvent`. If rustc
reports `cannot find type X in this scope` for any of them, add that one import
(`use crate::git::jobs::JobOp;`, `use crate::git::ops::{OpCtx, OpOutcome, OpRequest};`,
`use crate::git::error::GitError;`, `use crate::config::{AppConfig, RuntimePaths};`,
`use crate::internal_server::{AppStatus, HostEvent};`) rather than a glob.

```rust
    /// A `GitOps` that reports a settings outcome without touching a work tree,
    /// so `after_job`'s restart decision can be driven directly with no remote,
    /// no libgit2 and no filesystem.
    struct SettingsOps {
        changed: bool,
    }

    impl GitOps for SettingsOps {
        fn run(&self, _op: JobOp, ctx: &OpCtx) -> Result<OpOutcome, GitError> {
            let mut out = OpOutcome::new("merged", &ctx.def.branch);
            // HEAD moved in both cases, so the only variable between the two
            // tests is whether the settings values actually changed.
            out.head_before = Some("1111111111111111111111111111111111111111".to_string());
            out.head_after = Some("2222222222222222222222222222222222222222".to_string());
            out.settings_synced = true;
            out.settings_changed = self.changed;
            Ok(out)
        }
    }

    /// Run one auto-triggered sync on a `sync_settings` repo and return every
    /// host event it produced.
    ///
    /// `OpRequest::auto()` is the point of the test: it hard-codes
    /// `restart_children: Some(false)`, which is what every timer and every
    /// `sync_on_start` uses.
    async fn settings_sync_events(changed: bool) -> Vec<HostEvent> {
        let dir = tempfile::tempdir().unwrap();
        let cfg = AppConfig::from_str(
            "[app]\nname = \"X\"\nidentifier = \"com.example.x\"\n\
             [menu]\nsettings = true\n\
             [[settings.fields]]\n\
             key = \"theme\"\nlabel = \"Theme\"\ntype = \"select\"\n\
             default = \"light\"\noptions = [\"light\", \"dark\"]\n\
             [git]\n",
        )
        .unwrap();
        let paths = RuntimePaths::under(dir.path(), "com.example.x");
        paths.ensure().unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let status = std::sync::Arc::new(tokio::sync::RwLock::new(AppStatus::Ready {
            target_url: "http://x/".to_string(),
        }));
        let svc = GitService::with_ops(
            &cfg,
            &paths,
            tokio::runtime::Handle::current(),
            tx,
            status,
            std::sync::Arc::new(SettingsOps { changed }),
        );
        svc.start();
        svc.put_repo(
            "notes",
            serde_json::from_value(serde_json::json!({ "id": "notes", "sync_settings": true }))
                .unwrap(),
        )
        .unwrap();

        match svc.start_job("notes", JobOp::Sync, OpRequest::auto()) {
            Ok(StartOutcome::Started(_)) => {}
            Ok(_) => panic!("expected a freshly started job, got a replay or a busy repo"),
            Err(e) => panic!("start_job refused the job: {e}"),
        }
        // The busy slot clears when the RepoLease drops, which is after the job
        // and its notification have both run.
        let mut finished = false;
        for _ in 0..400 {
            if svc.jobs().busy("notes").is_none() {
                finished = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(finished, "job did not finish within 2s");

        let mut events = Vec::new();
        while let Ok(Some(e)) =
            tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
        {
            events.push(e);
        }
        events
    }

    #[tokio::test]
    async fn a_settings_change_restarts_children_even_when_the_request_said_no() {
        let events = settings_sync_events(true).await;
        assert_eq!(
            events.len(),
            1,
            "expected exactly one restart request, got {events:?}"
        );
        match &events[0] {
            HostEvent::GitRestartChildren { repo_id, reason } => {
                assert_eq!(repo_id, "notes");
                assert_eq!(
                    *reason, "settings",
                    "a restart caused by the settings mirror must say so"
                );
            }
            other => panic!("expected GitRestartChildren, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_sync_that_changed_no_settings_restarts_nothing() {
        // The auto-sync case: HEAD moved, the request said not to restart, and
        // the settings were identical. A five-minute timer must not relaunch
        // the user's window 288 times a day.
        let events = settings_sync_events(false).await;
        assert!(events.is_empty(), "expected no host events, got {events:?}");
    }
```

- [ ] **Step 17: Run the tests and read the result carefully**

Run: `cargo test --bin hitch git::tests::a_settings_change git::tests::a_sync_that_changed -- --nocapture`

This is a characterization test over task 9's `after_job`, so there are two legitimate outcomes
and they need different responses.

**If both PASS**, task 9 already wired the settings term correctly; the tests now prevent it from
regressing. Skip Step 18 and go to Step 19.

**If `a_settings_change_restarts_children_even_when_the_request_said_no` FAILS**, the expected
failure is one of:

```
thread 'git::tests::a_settings_change_restarts_children_even_when_the_request_said_no' panicked at src/git/mod.rs:NNN:
expected exactly one restart request, got []
```

(the `settings_changed` term is missing, so `restart_children: Some(false)` suppressed it) or:

```
thread 'git::tests::a_settings_change_restarts_children_even_when_the_request_said_no' panicked at src/git/mod.rs:NNN:
assertion `left == right` failed: a restart caused by the settings mirror must say so
  left: "requested"
 right: "settings"
```

(the event fires but is attributed to the request). That means task 9's `decide_restart` was
changed after it landed — see Step 18.

- [ ] **Step 18: Confirm task 9's `decide_restart` still treats `settings_changed` as an OR-term**

Both tests in Step 17 must pass with **no edit to `src/git/mod.rs`**. Task 9 already implements
this; this step is a review check, not a patch. Read `GitService::decide_restart` and confirm the
two properties still hold:

- `out.settings_changed` is an **independent OR-term**, never ANDed with the request's
  `restart_children`. The `Some(false)` that `OpRequest::auto()` and `OpRequest::manual()` both set
  must not be able to suppress it. Task 9 writes this as:

  ```rust
  let asked = ctx
      .request
      .restart_children
      .unwrap_or(ctx.def.restart_children_on_pull);
  let moved = out.head_after.is_some() && out.head_before != out.head_after;
  if !((asked && moved) || out.settings_changed) {
      return;
  }
  ```

- `reason` is `"settings"` when the settings change triggered the restart and `"requested"` when
  the caller asked — both `&'static str`s from §3.13. Task 9 selects it with
  `let reason = if out.settings_changed { "settings" } else { "requested" };`.

Why this is a check and not a patch: this task is the first thing that ever sets
`settings_changed` to `true`, so until now that OR-term has been dead code. If either test in
Step 17 fails, the bug is a regression in task 9's `decide_restart` — fix it *there*, against the
real code, rather than writing a second copy of the condition here.

Run: `rg -n 'out\.settings_changed' src/git/mod.rs`
Expected: two hits — the `decide_restart` guard and the `reason` selection.

- [ ] **Step 19: Run the tests and watch them pass**

Run: `cargo test --bin hitch git::tests::a_settings_change git::tests::a_sync_that_changed`

Expected: PASS — `test result: ok. 2 passed; 0 failed`.

- [ ] **Step 20: Commit**

```bash
git add src/git/mod.rs
git commit -m "test(git): lock the settings-driven restart to a real value change

OpRequest::auto() hard-codes restart_children = false, so a settings change
has to be an independent OR-term or the mirror can only ever fire from an
explicit HTTP call - which is not what a mirror is for. The companion test
holds the other edge: HEAD moved, the request said no, the values were
identical, and nothing restarts. A five-minute timer must not relaunch the
user's window 288 times a day."
```

If Step 18 was needed, `git add src/git/mod.rs` already covers it — the test and the fix land in
the one file, in one commit.

---

## Cycle E — leave the tree green

- [ ] **Step 21: Run the whole suite, the formatter and clippy**

Run, in order:

```bash
cargo fmt
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: `cargo fmt --check` prints nothing; clippy reports no warnings; `cargo test` is fully
green, including the pre-existing `settings::tests::*`, `config::tests::*` and
`internal_server::tests::*` — this task changed no behaviour outside `sync_settings` repos, and
every existing fixture leaves `sync_settings` at its `false` default.

If `cargo fmt` rewrote anything, fold it in:

```bash
git add -A
git commit --amend --no-edit
```
