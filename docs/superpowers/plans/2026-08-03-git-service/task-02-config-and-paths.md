### Task 2: Config section and runtime paths

Teaches `app.toml` about a `[git]` section. Nothing in this task opens a repository, creates a
directory, or links libgit2 — it is pure parsing, validation, and path arithmetic. It does not
touch `src/git/`.

**The load-bearing decision in this task:** `RuntimePaths::ensure()` is left **exactly as it is**.
The three new path fields are computed unconditionally (they are just `PathBuf`s), but nothing
creates them. `GitService::start` (task 9) owns the `mkdir` for `repos_dir`. That is the only
reason "`[git]` absent ⇒ no git directories and no git files on disk" is true, so a later task
that "helpfully" adds `create_dir_all(&self.repos_dir)` to `ensure()` silently breaks a locked
design decision. Step 18 has a test that fails if anyone does.

**Files:**
- Modify: `src/config.rs:7-19` — add `pub git: Option<GitSection>` as the last field of `AppConfig`
- Modify: `src/config.rs:128-130` — insert `GitSection`, its four default fns, `SshHostKeyPolicy`,
  `MAX_BRANCH_NAME_LEN` and `validate_branch_name` between `enum FieldType` and `impl AppConfig`
- Modify: `src/config.rs:141-143` — add `git_enabled()` after `settings_enabled()`
- Modify: `src/config.rs:231-232` — add the `[git]` validation arm just before `Ok(())`
- Modify: `src/config.rs:236-243` — three new `RuntimePaths` fields
- Modify: `src/config.rs:246-255` — populate them in `RuntimePaths::under`
- Modify: `src/config.rs:262-266` — `ensure()` **deliberately unchanged**
- Modify: `src/config.rs:385-392` — extend `runtime_paths_layout`, then append nine new tests
- Modify: `app.toml:25-26` — the commented-out `[git]` block, inserted before `# [[settings.fields]]`

**Interfaces:**

- Consumes: nothing. `AppConfig`, `RuntimePaths`, `SettingsSection` and `EMBEDDED_CONFIG` already
  exist in `src/config.rs` on `main`. This task compiles and passes with or without tasks 0 and 1
  having landed.

- Produces (task 4 uses `validate_branch_name` + `MAX_BRANCH_NAME_LEN` + `RuntimePaths::repos_dir`;
  task 6 uses `SshHostKeyPolicy`; task 9 clones `GitSection` into `GitService.cfg` and reads
  `registry_file` / `git_state_file`; task 10 calls `git_enabled()`):

```rust
// src/config.rs
pub const MAX_BRANCH_NAME_LEN: usize = 200;
pub fn validate_branch_name(name: &str) -> bool;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSection {
    pub tray_sync: bool,              // default false
    pub error_dialogs: bool,          // default false
    pub status_api: bool,             // default false
    pub registry_writes: bool,        // default TRUE
    pub default_branch: String,       // default "main"
    pub author_name: String,          // default ""
    pub author_email: String,         // default ""
    pub network_timeout_secs: u64,    // default 120
    pub quit_sync_timeout_secs: u64,  // default 10
    pub allow_http: bool,             // default false
    pub ssh_host_key_policy: SshHostKeyPolicy,   // default Tofu
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SshHostKeyPolicy { #[default] Tofu, Accept }   // toml: "tofu" | "accept"

// AppConfig gains, as the LAST field, after `settings`:
//     #[serde(default)] pub git: Option<GitSection>,
impl AppConfig {
    pub fn git_enabled(&self) -> bool;    // self.git.is_some()
}

// RuntimePaths gains three fields, populated in `under()`, ignored by `ensure()`:
//     pub repos_dir: PathBuf,        // data_dir.join("repos")
//     pub registry_file: PathBuf,    // data_dir.join("repos.json")
//     pub git_state_file: PathBuf,   // data_dir.join("git-state.json")
```

Five stable error strings returned by `AppConfig::validate`, byte for byte:

| trigger | string |
|---|---|
| `default_branch` fails `validate_branch_name` | ``git.default_branch "{v}" is not a valid branch name`` |
| `author_name` contains `<`, `>` or `\n` | `git.author_name must not contain '<', '>' or newlines (git signatures cannot represent them)` |
| non-empty `author_email` lacking `@`, or containing `<`/`>`/whitespace | ``git.author_email "{v}" must look like a plain address, e.g. app@example.com`` |
| `network_timeout_secs` outside `5..=3600` | `git.network_timeout_secs must be between 5 and 3600 (got {v})` |
| `quit_sync_timeout_secs > 120` | `git.quit_sync_timeout_secs must be 120 or less (0 disables sync_on_quit)` |

`validate_branch_name` is the single source of truth for every branch name the host accepts:
`[git].default_branch` here, `repos[].branch` in task 4, and `POST /api/git/repos/<id>/branch` in
task 7. Rules: non-empty; `<= MAX_BRANCH_NAME_LEN` bytes; every char in `[A-Za-z0-9._/-]`; no `..`;
no `//`; no leading `-` or `/`; no trailing `/`; does not end in `.lock`.

Two `#[allow(dead_code)]` attributes land in this task (on `GitSection` and on `git_enabled`), plus
three on the new `RuntimePaths` fields. They are required — `cargo clippy -- -D warnings` fails
without them, because nothing outside `#[cfg(test)]` reads those items until tasks 9 and 10. Tasks 9
and 10 should delete each one as they start reading the item it guards.

---

**Orientation for the engineer.** `src/config.rs` is the crate's config layer: one
`#[serde(deny_unknown_fields)]` struct per `app.toml` section, `AppConfig::from_str` parses then
calls the private `validate()`, and `validate()` returns `Result<(), String>` where the `String` is
plain English shown to the person editing `app.toml` — not a `thiserror` enum. `EMBEDDED_CONFIG` is
`include_str!("../app.toml")`, so `app.toml` is baked into the binary at compile time and changing
it forces a rebuild. Tests live in the `#[cfg(test)] mod tests` at the bottom of the same file and
share a `fn minimal() -> &'static str` helper that yields the smallest valid config.

The crate has **no lib target** — unit tests live in the binary — so the test command is
`cargo test config::tests::<name>`, never `cargo test --lib …`.

Baseline before you start: `cargo test` reports `33 passed; 0 failed; 1 ignored`, of which 9 are in
`config::tests`.

---

- [ ] **Step 1: Write the failing tests for `[git]` parsing**

Append these four tests inside the existing `#[cfg(test)] mod tests` block at the bottom of
`src/config.rs` (after `runtime_paths_layout`, before the closing `}` of the module). They pin the
default of every one of the eleven keys, pin that an absent section stays `None`, and pin that
`deny_unknown_fields` is actually on.

```rust
    #[test]
    fn git_section_defaults() {
        let c = AppConfig::from_str(&format!("{}[git]\n", minimal())).unwrap();
        let g = c.git.as_ref().unwrap();
        assert!(!g.tray_sync);
        assert!(!g.error_dialogs);
        assert!(!g.status_api);
        assert!(g.registry_writes);
        assert_eq!(g.default_branch, "main");
        assert_eq!(g.author_name, "");
        assert_eq!(g.author_email, "");
        assert_eq!(g.network_timeout_secs, 120);
        assert_eq!(g.quit_sync_timeout_secs, 10);
        assert!(!g.allow_http);
        assert_eq!(g.ssh_host_key_policy, SshHostKeyPolicy::Tofu);
        assert!(c.git_enabled());
    }

    #[test]
    fn absent_git_section_is_none() {
        let c = AppConfig::from_str(minimal()).unwrap();
        assert!(c.git.is_none());
        assert!(!c.git_enabled());
    }

    #[test]
    fn git_section_rejects_unknown_key() {
        let s = format!("{}[git]\ntray_synk = true\n", minimal());
        let err = AppConfig::from_str(&s).unwrap_err();
        assert!(err.contains("tray_synk"), "unexpected error: {err}");
    }

    #[test]
    fn ssh_host_key_policy_parses_and_rejects_unknown() {
        let s = format!("{}[git]\nssh_host_key_policy = \"accept\"\n", minimal());
        let c = AppConfig::from_str(&s).unwrap();
        assert_eq!(c.git.unwrap().ssh_host_key_policy, SshHostKeyPolicy::Accept);
        let s = format!("{}[git]\nssh_host_key_policy = \"strict\"\n", minimal());
        assert!(AppConfig::from_str(&s).is_err());
    }
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test config::tests::git_section_defaults`

Expected: FAIL — it does not compile:

```
error[E0609]: no field `git` on type `config::AppConfig`
   --> src/config.rs:...
    |
    |         let g = c.git.as_ref().unwrap();
    |                   ^^^ unknown field
    |
    = note: available fields are: `app`, `server`, `window`, `menu`, `settings`

error[E0433]: cannot find type `SshHostKeyPolicy` in this scope
    |         assert_eq!(g.ssh_host_key_policy, SshHostKeyPolicy::Tofu);
    |                                           ^^^^^^^^^^^^^^^^ use of undeclared type `SshHostKeyPolicy`

error[E0599]: no method named `git_enabled` found for struct `config::AppConfig` in the current scope
    |         assert!(c.git_enabled());
    |                   ^^^^^^^^^^^ method not found in `config::AppConfig`

error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 6 previous errors
```

- [ ] **Step 3: Add `GitSection`, `SshHostKeyPolicy`, `AppConfig::git` and `git_enabled()`**

Insert this block into `src/config.rs` between `pub enum FieldType { … }` (ends at line 128) and
`impl AppConfig {` (line 130):

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
// Consumed by the git service and the host wiring, neither of which exists yet.
// The section is parsed and validated here regardless of whether anything reads
// it, so that turning a git feature on later can never surface a *new* config
// error at an awkward moment.
#[allow(dead_code)]
pub struct GitSection {
    #[serde(default)]
    pub tray_sync: bool,
    #[serde(default)]
    pub error_dialogs: bool,
    #[serde(default)]
    pub status_api: bool,
    #[serde(default = "default_registry_writes")]
    pub registry_writes: bool,
    #[serde(default = "default_branch_name")]
    pub default_branch: String,
    #[serde(default)]
    pub author_name: String,
    #[serde(default)]
    pub author_email: String,
    #[serde(default = "default_network_timeout_secs")]
    pub network_timeout_secs: u64,
    #[serde(default = "default_quit_sync_timeout_secs")]
    pub quit_sync_timeout_secs: u64,
    #[serde(default)]
    pub allow_http: bool,
    #[serde(default)]
    pub ssh_host_key_policy: SshHostKeyPolicy,
}

fn default_registry_writes() -> bool {
    true
}

fn default_branch_name() -> String {
    "main".to_string()
}

fn default_network_timeout_secs() -> u64 {
    120
}

fn default_quit_sync_timeout_secs() -> u64 {
    10
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SshHostKeyPolicy {
    #[default]
    Tofu,
    Accept,
}
```

Add the field to `AppConfig` (it must be **last**, after `settings`) — `src/config.rs:18-19`:

```rust
    #[serde(default)]
    pub settings: Option<SettingsSection>,
    #[serde(default)]
    pub git: Option<GitSection>,
}
```

And the accessor, directly after `settings_enabled()` in `impl AppConfig`:

```rust
    // Called by the git service constructor and by `supervisor::build_env`, neither
    // of which exists yet.
    #[allow(dead_code)]
    pub fn git_enabled(&self) -> bool {
        self.git.is_some()
    }
```

The `#[allow(dead_code)]` on `GitSection` is not optional: without it
`cargo clippy -- -D warnings` fails with `error: fields \`tray_sync\`, \`error_dialogs\`,
\`status_api\`, \`registry_writes\`, \`allow_http\`, and \`ssh_host_key_policy\` are never read`.
Placing it on the struct also keeps `AppConfig.git` itself alive: rustc's dead-code pass treats an
`#[allow(dead_code)]` item as live, so the read of `self.git` inside `git_enabled` counts.

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test config::`

Expected: PASS — `test result: ok. 13 passed; 0 failed; 0 ignored`.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(git): parse the [git] section of app.toml

Presence of the section is the on switch, exactly like [server]; there is
no enabled key. Every field defaults, so a bare \`[git]\` is a valid,
fully-defaulted configuration."
```

- [ ] **Step 6: Write the failing tests for `validate_branch_name`**

Two tests: one exercising the rule set directly, one proving `[git].default_branch` is wired to it
and produces the exact operator-facing string. Append both to `mod tests`.

```rust
    #[test]
    fn branch_names() {
        for good in [
            "main",
            "a",
            "feature/x",
            "v1.2.3",
            "a-b_c",
            "release/2026.08",
        ] {
            assert!(validate_branch_name(good), "rejected {good:?}");
        }
        for bad in [
            "", "-x", "/x", "x/", "a..b", "a//b", "@", "x.lock", "a b", "a~b", "héllo", "a\tb",
        ] {
            assert!(!validate_branch_name(bad), "accepted {bad:?}");
        }
        assert!(validate_branch_name(&"a".repeat(MAX_BRANCH_NAME_LEN)));
        assert!(!validate_branch_name(&"a".repeat(MAX_BRANCH_NAME_LEN + 1)));
    }

    #[test]
    fn git_default_branch_message() {
        let s = format!("{}[git]\ndefault_branch = \"bad branch\"\n", minimal());
        assert_eq!(
            AppConfig::from_str(&s).unwrap_err(),
            "git.default_branch \"bad branch\" is not a valid branch name"
        );
    }
```

- [ ] **Step 7: Run the tests and watch them fail**

Run: `cargo test config::tests::branch_names`

Expected: FAIL — it does not compile:

```
error[E0425]: cannot find function `validate_branch_name` in this scope
   --> src/config.rs:...
    |
    |         assert!(validate_branch_name(good), "rejected {good:?}");
    |                 ^^^^^^^^^^^^^^^^^^^^ not found in this scope

error[E0425]: cannot find value `MAX_BRANCH_NAME_LEN` in this scope
    |         assert!(validate_branch_name(&"a".repeat(MAX_BRANCH_NAME_LEN)));
    |                                                  ^^^^^^^^^^^^^^^^^^^ not found in this scope

error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 5 previous errors
```

- [ ] **Step 8: Add `MAX_BRANCH_NAME_LEN`, `validate_branch_name`, and the `default_branch` rule**

Append to the block you added in step 3, immediately after `enum SshHostKeyPolicy`:

```rust
pub const MAX_BRANCH_NAME_LEN: usize = 200;

/// Single source of truth for every branch name this host will accept:
/// `[git].default_branch`, `repos[].branch` in `repos.json`, and the body of
/// `POST /api/git/repos/<id>/branch`.
///
/// Deliberately stricter than git's own `check_ref_format`, and a whitelist rather
/// than a blacklist: `[A-Za-z0-9._/-]` already excludes ASCII control characters,
/// whitespace, `@`, `~`, `^`, `:`, `?`, `*`, `[`, `\` and every shell metacharacter,
/// so a future git relaxing its own rules cannot widen ours by accident.
pub fn validate_branch_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_BRANCH_NAME_LEN {
        return false;
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
    {
        return false;
    }
    if name.contains("..") || name.contains("//") {
        return false;
    }
    if name.starts_with('-') || name.starts_with('/') || name.ends_with('/') {
        return false;
    }
    !name.ends_with(".lock")
}
```

Then open the `[git]` arm in `AppConfig::validate`, immediately before the closing `Ok(())`
(line 232 of the original file):

```rust
        if let Some(git) = &self.git {
            if !validate_branch_name(&git.default_branch) {
                return Err(format!(
                    "git.default_branch {:?} is not a valid branch name",
                    git.default_branch
                ));
            }
        }
        Ok(())
```

`{:?}` on a `String` is what produces the `"bad branch"` quoting in the expected message, and it
matches the voice of the existing `app.identifier {id:?} must be reverse-domain` arm two blocks up.

- [ ] **Step 9: Run the tests and watch them pass**

Run: `cargo test config::`

Expected: PASS — `test result: ok. 15 passed; 0 failed; 0 ignored`.

- [ ] **Step 10: Commit**

```bash
git add src/config.rs
git commit -m "feat(git): add validate_branch_name and check [git].default_branch

One whitelist shared by app.toml, repos.json and POST /branch, so the three
entry points cannot drift apart on what a branch name is."
```

- [ ] **Step 11: Write the failing tests for the remaining four validation rules**

Append both tests to `mod tests`. Each asserts the message with `assert_eq!`, not `contains` — the
strings are a contract surface, and a test that only checks a substring will not notice a reworded
message.

```rust
    #[test]
    fn git_author_messages() {
        let s = format!("{}[git]\nauthor_name = \"A <a@b.c>\"\n", minimal());
        assert_eq!(
            AppConfig::from_str(&s).unwrap_err(),
            "git.author_name must not contain '<', '>' or newlines (git signatures cannot \
             represent them)"
        );
        for bad in ["nobody", "a b@c.d", "<a@b.c>"] {
            let s = format!("{}[git]\nauthor_email = \"{bad}\"\n", minimal());
            assert_eq!(
                AppConfig::from_str(&s).unwrap_err(),
                format!(
                    "git.author_email \"{bad}\" must look like a plain address, \
                     e.g. app@example.com"
                )
            );
        }
        let ok = format!(
            "{}[git]\nauthor_name = \"App\"\nauthor_email = \"app@example.com\"\n",
            minimal()
        );
        assert!(AppConfig::from_str(&ok).is_ok());
    }

    #[test]
    fn git_timeout_messages() {
        for bad in [0u64, 4, 3601] {
            let s = format!("{}[git]\nnetwork_timeout_secs = {bad}\n", minimal());
            assert_eq!(
                AppConfig::from_str(&s).unwrap_err(),
                format!("git.network_timeout_secs must be between 5 and 3600 (got {bad})")
            );
        }
        let s = format!("{}[git]\nquit_sync_timeout_secs = 121\n", minimal());
        assert_eq!(
            AppConfig::from_str(&s).unwrap_err(),
            "git.quit_sync_timeout_secs must be 120 or less (0 disables sync_on_quit)"
        );
        let ok = format!(
            "{}[git]\nnetwork_timeout_secs = 5\nquit_sync_timeout_secs = 0\n",
            minimal()
        );
        assert!(AppConfig::from_str(&ok).is_ok());
    }
```

Note `quit_sync_timeout_secs = 0` is **valid** — it is the documented way to switch
`sync_on_quit` off entirely, which is why the rule is a one-sided `> 120` and not a range.

- [ ] **Step 12: Run the tests and watch them fail**

Run: `cargo test config::tests::git_`

Expected: FAIL — these compile but panic, because `validate()` currently accepts the bad values:

```
---- config::tests::git_author_messages stdout ----
thread 'config::tests::git_author_messages' panicked at src/config.rs:...:
called `Result::unwrap_err()` on an `Ok` value: AppConfig { app: AppSection { name: "X",
identifier: "com.example.x", url: "" }, server: None, ..., git: Some(GitSection { tray_sync:
false, ..., author_name: "A <a@b.c>", author_email: "", network_timeout_secs: 120, ... }) }

---- config::tests::git_timeout_messages stdout ----
thread 'config::tests::git_timeout_messages' panicked at src/config.rs:...:
called `Result::unwrap_err()` on an `Ok` value: AppConfig { ..., git: Some(GitSection { ...,
network_timeout_secs: 0, quit_sync_timeout_secs: 10, ... }) }

test result: FAILED. 3 passed; 2 failed; 0 ignored; 0 measured
```

- [ ] **Step 13: Extend the `[git]` validation arm with the four remaining rules**

In `AppConfig::validate`, inside the `if let Some(git) = &self.git {` block you opened in step 8,
after the `default_branch` check:

```rust
            if git.author_name.contains(['<', '>', '\n']) {
                return Err(
                    "git.author_name must not contain '<', '>' or newlines (git \
                            signatures cannot represent them)"
                        .into(),
                );
            }
            if !git.author_email.is_empty()
                && (!git.author_email.contains('@')
                    || git.author_email.contains(['<', '>'])
                    || git.author_email.chars().any(char::is_whitespace))
            {
                return Err(format!(
                    "git.author_email {:?} must look like a plain address, e.g. app@example.com",
                    git.author_email
                ));
            }
            if !(5..=3600).contains(&git.network_timeout_secs) {
                return Err(format!(
                    "git.network_timeout_secs must be between 5 and 3600 (got {})",
                    git.network_timeout_secs
                ));
            }
            if git.quit_sync_timeout_secs > 120 {
                return Err(
                    "git.quit_sync_timeout_secs must be 120 or less (0 disables sync_on_quit)"
                        .into(),
                );
            }
```

The odd-looking indentation inside the `author_name` literal is what `cargo fmt` produces. A
backslash at end-of-line in a Rust string literal eats the newline *and* the following line's
leading whitespace, so the runtime string is still the single line
`git.author_name must not contain '<', '>' or newlines (git signatures cannot represent them)`.
Run `cargo fmt` and leave whatever it emits alone.

Two rules are deliberately asymmetric and should not be "tidied":
`author_name` and `author_email` default to `""`, and empty is legal — task 9 fills the blanks
from `[app].name` and `<identifier>@<hostname>`. Only `author_email` guards on `is_empty()`
because `""` trivially passes the `author_name` check anyway.

- [ ] **Step 14: Run the tests and watch them pass**

Run: `cargo test config::`

Expected: PASS — `test result: ok. 17 passed; 0 failed; 0 ignored`.

- [ ] **Step 15: Commit**

```bash
git add src/config.rs
git commit -m "feat(git): validate the [git] author and timeout keys

Runs whether or not the section is later used, so enabling a repo never
surfaces a new config error at an awkward moment."
```

- [ ] **Step 16: Write the failing tests for the three new `RuntimePaths` fields**

Extend the existing `runtime_paths_layout` test (`src/config.rs:385-392`) with three assertions,
and append a new test that pins the negative half of the contract.

```rust
    #[test]
    fn runtime_paths_layout() {
        let p = RuntimePaths::under(std::path::Path::new("/base"), "com.example.x");
        assert_eq!(p.data_dir, std::path::PathBuf::from("/base/com.example.x"));
        assert_eq!(p.chrome_profile, p.data_dir.join("chrome-profile"));
        assert_eq!(p.logs_dir, p.data_dir.join("logs"));
        assert_eq!(p.settings_file, p.data_dir.join("settings.json"));
        assert_eq!(p.lock_file, p.data_dir.join("app.lock"));
        assert_eq!(p.repos_dir, p.data_dir.join("repos"));
        assert_eq!(p.registry_file, p.data_dir.join("repos.json"));
        assert_eq!(p.git_state_file, p.data_dir.join("git-state.json"));
    }

    #[test]
    fn ensure_does_not_create_git_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let p = RuntimePaths::under(tmp.path(), "com.example.x");
        p.ensure().unwrap();
        assert!(p.data_dir.is_dir());
        assert!(p.chrome_profile.is_dir());
        assert!(p.logs_dir.is_dir());
        assert!(!p.repos_dir.exists());
        assert!(!p.registry_file.exists());
        assert!(!p.git_state_file.exists());
    }
```

`tempfile` is already a `[dev-dependencies]` entry in `Cargo.toml`; no manifest change is needed.

- [ ] **Step 17: Run the tests and watch them fail**

Run: `cargo test config::tests::runtime_paths_layout`

Expected: FAIL — it does not compile:

```
error[E0609]: no field `repos_dir` on type `config::RuntimePaths`
   --> src/config.rs:...
    |
    |         assert_eq!(p.repos_dir, p.data_dir.join("repos"));
    |                      ^^^^^^^^^ unknown field
    |
    = note: available fields are: `data_dir`, `chrome_profile`, `logs_dir`, `settings_file`, `lock_file`

error[E0609]: no field `registry_file` on type `config::RuntimePaths`
error[E0609]: no field `git_state_file` on type `config::RuntimePaths`

error: could not compile `chrome-host-app` (bin "chrome-host-app" test) due to 6 previous errors
```

- [ ] **Step 18: Add the three fields — and leave `ensure()` alone**

`src/config.rs:236-243`, the `RuntimePaths` struct:

```rust
#[derive(Debug, Clone)]
pub struct RuntimePaths {
    pub data_dir: PathBuf,
    pub chrome_profile: PathBuf,
    pub logs_dir: PathBuf,
    pub settings_file: PathBuf,
    pub lock_file: PathBuf,
    // Computed unconditionally — they are only paths. Nothing here creates them:
    // `ensure()` deliberately ignores all three so that a host with no `[git]`
    // section leaves no git files on disk at all. The mkdir for `repos_dir`
    // belongs to `GitService::start`, and that placement is the entire reason
    // the "nothing on disk when git is off" guarantee holds.
    #[allow(dead_code)]
    pub repos_dir: PathBuf,
    #[allow(dead_code)]
    pub registry_file: PathBuf,
    #[allow(dead_code)]
    pub git_state_file: PathBuf,
}
```

`RuntimePaths::under`, keeping `data_dir` last so the earlier `data_dir.join(..)` calls still
borrow it before the move:

```rust
    pub fn under(base: &Path, identifier: &str) -> Self {
        let data_dir = base.join(identifier);
        Self {
            chrome_profile: data_dir.join("chrome-profile"),
            logs_dir: data_dir.join("logs"),
            settings_file: data_dir.join("settings.json"),
            lock_file: data_dir.join("app.lock"),
            repos_dir: data_dir.join("repos"),
            registry_file: data_dir.join("repos.json"),
            git_state_file: data_dir.join("git-state.json"),
            data_dir,
        }
    }
```

Make **no change** to `ensure()`. It stays exactly:

```rust
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.chrome_profile)?;
        std::fs::create_dir_all(&self.logs_dir)
    }
```

- [ ] **Step 19: Run the tests and watch them pass**

Run: `cargo test config::`

Expected: PASS — `test result: ok. 18 passed; 0 failed; 0 ignored`.

- [ ] **Step 20: Commit**

```bash
git add src/config.rs
git commit -m "feat(git): add repos_dir, registry_file and git_state_file to RuntimePaths

ensure() is deliberately untouched. The mkdir for repos_dir belongs to
GitService::start, which is what makes \"no git files on disk when [git]
is absent\" true rather than aspirational."
```

- [ ] **Step 21: Write the failing test for the shipped `[git]` comment block**

`app.toml` is `include_str!`d, so the commented `[git]` block is documentation that ships inside
the binary. This test un-comments it at runtime and parses it, so a key renamed in `GitSection`
without a matching edit to the comment fails here instead of failing the first user who uncomments
it. Append to `mod tests`.

```rust
    #[test]
    fn shipped_git_block_is_commented_out_but_valid() {
        assert!(!AppConfig::load().unwrap().git_enabled());
        // The shipped `[git]` block is documentation users uncomment. Rename a key
        // without updating the comment and `deny_unknown_fields` bites the user on
        // their first run, not us. Uncomment it here and parse it instead.
        let mut out = String::new();
        let mut in_git = false;
        for line in EMBEDDED_CONFIG.lines() {
            let body = line.strip_prefix("# ").unwrap_or("").trim();
            if body == "[git]" {
                in_git = true;
            } else if in_git && !line.starts_with('#') {
                in_git = false;
            }
            if in_git && !body.is_empty() && !body.starts_with('#') {
                out.push_str(body);
                out.push('\n');
            }
        }
        assert!(
            out.starts_with("[git]\n"),
            "app.toml has no commented-out [git] block"
        );
        let s = format!("{}{out}", minimal());
        let c = AppConfig::from_str(&s).unwrap_or_else(|e| panic!("{e}\n--- built from ---\n{s}"));
        let g = c.git.unwrap();
        assert!(g.registry_writes);
        assert_eq!(g.default_branch, "main");
        assert_eq!(g.network_timeout_secs, 120);
        assert_eq!(g.quit_sync_timeout_secs, 10);
        assert_eq!(g.ssh_host_key_policy, SshHostKeyPolicy::Tofu);
    }
```

The extractor starts collecting at the `# [git]` line and stops at the first line that is not a
comment (the blank line after the block). Prose lines above `# [git]` are outside the block; a
continuation line whose body is itself a `#` comment (the wrapped `error_dialogs` note) is skipped
by the `!body.starts_with('#')` guard; trailing `# …` comments on a key line are handled by TOML
itself.

- [ ] **Step 22: Run the test and watch it fail**

Run: `cargo test config::tests::shipped_git_block`

Expected: FAIL — the assertion, because `app.toml` has no `[git]` block yet:

```
---- config::tests::shipped_git_block_is_commented_out_but_valid stdout ----
thread 'config::tests::shipped_git_block_is_commented_out_but_valid' panicked at src/config.rs:...:
app.toml has no commented-out [git] block

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured
```

- [ ] **Step 23: Add the commented `[git]` block to `app.toml`**

Insert this immediately before the existing `# [[settings.fields]]` block (currently `app.toml:26`),
separated from `[menu]` above by the blank line that is already there:

```toml
# Uncomment to give the host a git service. The [server] child reaches it at
# $APP_HOST_URL/api/git/* with header  x-host-token: $APP_HOST_TOKEN.
# Repos are defined in <data-dir>/repos.json; trees live in <data-dir>/repos/<id>/.
# See README §9.
# [git]
# tray_sync = false                     # add a "Sync now" tray entry
# error_dialogs = false                 # native dialog when a sync starts failing,
#                                       # and when a sync overwrote a remote edit
# status_api = false                    # add a "git" key to /api/status
# registry_writes = true                # false → repos.json is author-owned; PUT/DELETE 403
# default_branch = "main"
# author_name = ""                      # "" → [app].name
# author_email = ""                     # "" → "<identifier>@<hostname>"
# network_timeout_secs = 120            # 5..=3600
# quit_sync_timeout_secs = 10           # 0 disables sync_on_quit entirely
# allow_http = false                    # permit plaintext http:// remotes
# ssh_host_key_policy = "tofu"          # "tofu" | "accept"
```

Every value shown is the actual default, which is what step 21's test enforces. `README §9` does
not exist yet — task 12 writes it. The `#` column is aligned at 41 to match `[window]` and `[menu]`
above.

- [ ] **Step 24: Run the test and watch it pass**

Run: `cargo test config::`

Expected: PASS — `test result: ok. 19 passed; 0 failed; 0 ignored`.

- [ ] **Step 25: Commit**

```bash
git add app.toml
git commit -m "docs(git): ship a commented-out [git] block in app.toml

Every value in the block is the real default, and a test un-comments and
parses it so the documentation cannot drift from GitSection."
```

- [ ] **Step 26: Run the full gate**

Run:

```bash
cargo fmt
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

Expected: `cargo fmt --check` silent; clippy `Finished` with no warnings; `cargo test` reports
`test result: ok. 42 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out` for the
`chrome-host-app` binary (up from 33 passed at the start of the task) plus `0 passed` for
`gen_icons`.

If `cargo fmt` rewrote anything, fold it into the last commit:

```bash
git add -A && git commit --amend --no-edit
```
